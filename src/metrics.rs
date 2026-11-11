//! Tokens and speed of the turn on the chat screen ("1,234 トークン · 45 tok/s").
//!
//! Tokens come from `thread/tokenUsage/updated` (cumulative totals; the turn's share is the
//! difference to the totals at turn start). Speed counts only the time the model was producing
//! text: a pause longer than `GAP` (a tool running, waiting) does not count. While no usage
//! notification has arrived the output is estimated from the text that streamed in, and the
//! numbers carry a "~". After the turn the speed is output tokens / generating seconds.

use crate::*;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A pause between two streamed chunks longer than this is not generation time.
const GAP: Duration = Duration::from_millis(1500);
/// The live speed is the average over this much generating time.
const WINDOW: f64 = 5.0;

#[derive(Default)]
struct M {
    /// Cumulative total tokens of the thread as last reported (to skip repeated reports).
    last_total: i64,
    /// This turn's tokens: the sum of the `last` figures of each report.
    turn_tokens: i64,
    turn_out: i64,
    /// True once a usage notification arrived during this turn.
    exact: bool,
    running: bool,
    gen_secs: f64,
    last_chunk: Option<Instant>,
    est_tokens: f64,
    /// (generating seconds, output tokens) samples for the moving average.
    samples: VecDeque<(f64, f64)>,
    last_paint: Option<Instant>,
}

thread_local! {
    static M: RefCell<M> = RefCell::new(M::default());
}

fn estimate(text: &str) -> f64 {
    text.chars().map(|c| if c.is_ascii() { 0.25 } else { 0.7 }).sum()
}

fn fmt_int(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    let out: String = out.chars().rev().collect();
    if n < 0 { format!("-{out}") } else { out }
}

fn paint(app: &AppWindow, m: &M, final_: bool) {
    let (out, total, approx) = if m.exact {
        (m.turn_out as f64, m.turn_tokens as f64, false)
    } else {
        (m.est_tokens, m.est_tokens, true)
    };
    if total <= 0.0 && out <= 0.0 {
        app.global::<MetricsState>().set_text("".into());
        return;
    }
    let rate = if final_ {
        if m.gen_secs > 0.2 { out / m.gen_secs } else { 0.0 }
    } else {
        match (m.samples.front(), m.samples.back()) {
            (Some(a), Some(b)) if b.0 - a.0 > 0.2 => (b.1 - a.1) / (b.0 - a.0),
            _ => 0.0,
        }
    };
    // The estimate during the turn is marked with "~"; the exact figures carry no mark.
    let mark = if approx { "~" } else { "" };
    let mut text = format!("{mark}{} トークン", fmt_int(total.round() as i64));
    if rate > 0.0 {
        text.push_str(&format!(" · {mark}{} tok/s", rate.round() as i64));
    }
    app.global::<MetricsState>().set_text(text.into());
}

pub(crate) fn turn_started(app: &AppWindow) {
    M.with(|m| {
        let mut m = m.borrow_mut();
        m.turn_tokens = 0;
        m.turn_out = 0;
        m.exact = false;
        m.running = true;
        m.gen_secs = 0.0;
        m.last_chunk = None;
        m.est_tokens = 0.0;
        m.samples.clear();
    });
    app.global::<MetricsState>().set_text("".into());
}

/// Text streamed in (an answer or a reasoning summary).
pub(crate) fn chunk(app: &AppWindow, text: &str) {
    let now = Instant::now();
    M.with(|m| {
        let mut m = m.borrow_mut();
        if !m.running {
            return;
        }
        if let Some(prev) = m.last_chunk {
            let dt = now.duration_since(prev);
            if dt <= GAP {
                m.gen_secs += dt.as_secs_f64();
            }
        } else {
            // the first text of the turn: the time to the first answer ends here
            crate::thread_stats::first_output();
        }
        m.last_chunk = Some(now);
        m.est_tokens += estimate(text);
        let out = if m.exact { m.turn_out as f64 } else { m.est_tokens };
        let g = m.gen_secs;
        m.samples.push_back((g, out));
        while m.samples.len() > 2 && g - m.samples[1].0 >= WINDOW {
            m.samples.pop_front();
        }
        let due = m.last_paint.map(|p| now.duration_since(p) > Duration::from_millis(300)).unwrap_or(true);
        if due {
            m.last_paint = Some(now);
            paint(app, &m, false);
        }
    });
}

/// `thread/tokenUsage/updated`: `total` is the thread's cumulative total, `last_*` is what the
/// latest model call used.
pub(crate) fn totals(app: &AppWindow, total: i64, last_total: i64, last_output: i64) {
    M.with(|m| {
        let mut m = m.borrow_mut();
        let changed = total != m.last_total;
        m.last_total = total;
        if m.running && changed {
            m.turn_tokens += last_total;
            m.turn_out += last_output;
            m.exact = true;
            paint(app, &m, false);
        }
    });
}

pub(crate) fn turn_done(app: &AppWindow) {
    M.with(|m| {
        let mut m = m.borrow_mut();
        if !m.running {
            return;
        }
        m.running = false;
        paint(app, &m, true);
        // the same seconds and tokens go to the thread's sums
        let out = if m.exact { m.turn_out } else { m.est_tokens.round() as i64 };
        crate::thread_stats::turn_finished(app, m.gen_secs, out);
    });
}

/// A thread was opened: the totals belong to another thread.
pub(crate) fn reset(app: &AppWindow) {
    M.with(|m| *m.borrow_mut() = M::default());
    app.global::<MetricsState>().set_text("".into());
    crate::thread_stats::reset(app);
}
