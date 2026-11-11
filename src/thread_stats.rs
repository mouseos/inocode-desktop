//! The line under the composer: what the thread has used so far ("3 ターン · 7 ステップ | LLM 7.2 秒
//! | 初回応答 平均 3.3 秒 · 95 tok/s | キャッシュ 12% | 入力 8.7K · 出力 368").
//!
//! The speed and the time to the first answer are of the model that is selected now: a thread
//! that changed the model counts the model's own turns, never a mix. Nothing that is not known is
//! written. The generating seconds and output tokens are the ones metrics.rs counts for the
//! answer's own line; this file adds them up per turn.

use crate::state::STATE;
use crate::view::messages_with;
use crate::*;
use slint::Model;
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// A send that is older than this was not the start of the turn that is running.
const SEND_MAX_AGE: Duration = Duration::from_secs(30);

#[derive(Default, Clone)]
struct Agg {
    model: String,
    gen_secs: f64,
    out: i64,
    ttft_sum: f64,
    ttft_n: u32,
}

#[derive(Default)]
struct T {
    aggs: Vec<Agg>,
    steps: u32,
    last_total: i64,
    input: i64,
    cached: i64,
    output: i64,
    /// When the message was sent (turn/start), until the first output arrives.
    sent_at: Option<Instant>,
    /// Seconds from the send to the first output of the running turn.
    turn_ttft: Option<f64>,
}

thread_local! {
    static T: RefCell<T> = RefCell::new(T::default());
}

// ------------------------------------------------------------------ pure helpers (tested)

/// 368 -> "368", 8700 -> "8.7K", 1_250_000 -> "1.3M".
pub(crate) fn short_count(n: i64) -> String {
    let trim = |x: f64| {
        let t = format!("{x:.1}");
        t.strip_suffix(".0").map(str::to_string).unwrap_or(t)
    };
    if n >= 1_000_000 {
        format!("{}M", trim(n as f64 / 1_000_000.0))
    } else if n >= 10_000 {
        format!("{}K", (n as f64 / 1000.0).round() as i64)
    } else if n >= 1000 {
        format!("{}K", trim(n as f64 / 1000.0))
    } else {
        n.to_string()
    }
}

/// 7.2 -> "7.2 秒", 72 -> "1 分 12 秒", 3900 -> "1 時間 5 分".
pub(crate) fn duration_label(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.1} 秒")
    } else if secs < 3600.0 {
        let s = secs.round() as i64;
        format!("{} 分 {} 秒", s / 60, s % 60)
    } else {
        let m = (secs / 60.0).round() as i64;
        format!("{} 時間 {} 分", m / 60, m % 60)
    }
}

/// What is known about the thread; None: not known, not written.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct Figures {
    pub turns: usize,
    pub steps: u32,
    pub llm_secs: f64,
    pub ttft: Option<f64>,
    pub tok_per_s: Option<f64>,
    pub cache_pct: Option<f64>,
    pub input: i64,
    pub output: i64,
}

pub(crate) fn line(f: &Figures) -> String {
    if f.turns == 0 {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut first = format!("{} ターン", f.turns);
    if f.steps > 0 {
        first.push_str(&format!(" · {} ステップ", f.steps));
    }
    parts.push(first);
    if f.llm_secs > 0.0 {
        parts.push(format!("LLM {}", duration_label(f.llm_secs)));
    }
    let speed = f.tok_per_s.filter(|s| *s > 0.0).map(|s| format!("{} tok/s", s.round() as i64));
    match (f.ttft, speed) {
        (Some(t), Some(s)) => parts.push(format!("初回応答 平均 {t:.1} 秒 · {s}")),
        (Some(t), None) => parts.push(format!("初回応答 平均 {t:.1} 秒")),
        (None, Some(s)) => parts.push(s),
        (None, None) => {}
    }
    if let Some(p) = f.cache_pct {
        parts.push(format!("キャッシュ {}%", p.round() as i64));
    }
    if f.input > 0 || f.output > 0 {
        parts.push(format!("入力 {} · 出力 {}", short_count(f.input), short_count(f.output)));
    }
    parts.join(" | ")
}

/// The figures of `model` from the sums that were kept.
fn figures_of(aggs: &[Agg], model: &str, steps: u32, input: i64, cached: i64, output: i64, turns: usize) -> Figures {
    let mine = aggs.iter().find(|a| a.model == model);
    Figures {
        turns,
        steps,
        llm_secs: aggs.iter().map(|a| a.gen_secs).sum(),
        ttft: mine.filter(|a| a.ttft_n > 0).map(|a| a.ttft_sum / a.ttft_n as f64),
        tok_per_s: mine.filter(|a| a.gen_secs > 1.0 && a.out > 0).map(|a| a.out as f64 / a.gen_secs),
        cache_pct: if input > 0 { Some((cached as f64 / input as f64 * 100.0).clamp(0.0, 100.0)) } else { None },
        input,
        output,
    }
}

// ------------------------------------------------------------------ feeding

/// The message is being sent (turn/start).
pub(crate) fn note_send() {
    T.with(|t| t.borrow_mut().sent_at = Some(Instant::now()));
}

/// The first text of the running turn came in.
pub(crate) fn first_output() {
    T.with(|t| {
        let mut t = t.borrow_mut();
        if t.turn_ttft.is_none() {
            if let Some(at) = t.sent_at.take() {
                let secs = at.elapsed();
                if secs <= SEND_MAX_AGE {
                    t.turn_ttft = Some(secs.as_secs_f64());
                }
            }
        }
    });
}

/// thread/tokenUsage/updated: the thread's totals (a change of the total is one model call).
pub(crate) fn usage(app: &AppWindow, input: i64, cached: i64, output: i64, total: i64) {
    T.with(|t| {
        let mut t = t.borrow_mut();
        if total != t.last_total {
            t.steps += 1;
        }
        t.last_total = total;
        t.input = input;
        t.cached = cached;
        t.output = output;
    });
    refresh(app);
}

/// A turn ended: its generating seconds and output tokens (as metrics.rs has them) and its first
/// answer time go to the model that was selected.
pub(crate) fn turn_finished(app: &AppWindow, gen_secs: f64, out_tokens: i64) {
    let model = current_model();
    T.with(|t| {
        let mut t = t.borrow_mut();
        let ttft = t.turn_ttft.take();
        t.sent_at = None;
        if model.is_empty() {
            return;
        }
        let i = match t.aggs.iter().position(|a| a.model == model) {
            Some(i) => i,
            None => {
                t.aggs.push(Agg { model: model.clone(), ..Agg::default() });
                t.aggs.len() - 1
            }
        };
        let a = &mut t.aggs[i];
        a.gen_secs += gen_secs.max(0.0);
        a.out += out_tokens.max(0);
        if let Some(s) = ttft {
            a.ttft_sum += s;
            a.ttft_n += 1;
        }
    });
    refresh(app);
}

/// Another thread (or the history of this one) was put on the screen.
pub(crate) fn reset(app: &AppWindow) {
    T.with(|t| *t.borrow_mut() = T::default());
    refresh(app);
}

fn current_model() -> String {
    STATE.with(|s| {
        let st = s.borrow();
        st.settings.model.clone().filter(|m| !m.is_empty()).unwrap_or_else(|| st.applied_model.clone())
    })
}

fn turns(app: &AppWindow) -> usize {
    messages_with(app, |m| (0..m.row_count()).filter(|i| m.row_data(*i).map(|r| r.role == "user").unwrap_or(false)).count()).unwrap_or(0)
}

/// Write the line under the composer again.
pub(crate) fn refresh(app: &AppWindow) {
    let model = current_model();
    let n = turns(app);
    let text = T.with(|t| {
        let t = t.borrow();
        line(&figures_of(&t.aggs, &model, t.steps, t.input, t.cached, t.output, n))
    });
    app.global::<MetricsState>().set_thread_text(text.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_line() {
        let f = Figures { turns: 3, steps: 7, llm_secs: 7.2, ttft: Some(3.3), tok_per_s: Some(95.0), cache_pct: Some(12.0), input: 8700, output: 368 };
        assert_eq!(line(&f), "3 ターン · 7 ステップ | LLM 7.2 秒 | 初回応答 平均 3.3 秒 · 95 tok/s | キャッシュ 12% | 入力 8.7K · 出力 368");
    }

    #[test]
    fn what_is_not_known_is_not_written() {
        // a resumed thread: the turns are known, nothing else
        assert_eq!(line(&Figures { turns: 2, ..Figures::default() }), "2 ターン");
        // no turn: no line
        assert_eq!(line(&Figures::default()), "");
        // speed without a first answer time, and the other way round
        let only_speed = Figures { turns: 1, tok_per_s: Some(40.4), ..Figures::default() };
        assert_eq!(line(&only_speed), "1 ターン | 40 tok/s");
        let only_ttft = Figures { turns: 1, ttft: Some(1.26), ..Figures::default() };
        assert_eq!(line(&only_ttft), "1 ターン | 初回応答 平均 1.3 秒");
        // no input: no cache figure
        let f = Figures { turns: 1, output: 12, ..Figures::default() };
        assert_eq!(line(&f), "1 ターン | 入力 0 · 出力 12");
    }

    #[test]
    fn counts_and_durations_are_short() {
        assert_eq!(short_count(368), "368");
        assert_eq!(short_count(8700), "8.7K");
        assert_eq!(short_count(12_345), "12K");
        assert_eq!(short_count(2_000_000), "2M");
        assert_eq!(duration_label(7.24), "7.2 秒");
        assert_eq!(duration_label(72.0), "1 分 12 秒");
        assert_eq!(duration_label(3900.0), "1 時間 5 分");
    }

    #[test]
    fn speed_and_first_answer_are_of_one_model_only() {
        let aggs = vec![
            Agg { model: "fast".into(), gen_secs: 10.0, out: 1000, ttft_sum: 2.0, ttft_n: 2 },
            Agg { model: "slow".into(), gen_secs: 100.0, out: 1000, ttft_sum: 40.0, ttft_n: 2 },
        ];
        let f = figures_of(&aggs, "fast", 5, 1000, 250, 400, 2);
        assert_eq!(f.tok_per_s, Some(100.0));
        assert_eq!(f.ttft, Some(1.0));
        // the time of the thread is the sum, not an average of the models
        assert_eq!(f.llm_secs, 110.0);
        assert_eq!(f.cache_pct, Some(25.0));
        // a model that has no turn yet: no speed, no first answer time
        let g = figures_of(&aggs, "new", 5, 0, 0, 0, 2);
        assert_eq!((g.tok_per_s, g.ttft, g.cache_pct), (None, None, None));
    }
}
