//! Replying to a message by its number. A message of the user or the assistant has a number
//! `#<turn>.<item>` that the server gives (inocode/messages/resolve); "reply" puts a chip over
//! the composer, and the next message starts with one line `[返信 #12.3「SQLite の並行…」]` per
//! chip. A message that starts with such lines is shown with chips for them.

use crate::state::STATE;
use crate::view::messages_with;
use crate::*;
use serde_json::{json, Value};
use slint::Model;
use std::cell::RefCell;
use std::collections::VecDeque;

/// How many characters of the message are in the marker of a reply.
const HINT_CHARS: usize = 20;
/// How many numbers are asked for at the same time.
const IN_FLIGHT: usize = 6;

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    /// The replies that are ready: (number, marker).
    static CHIPS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// Messages whose number is still to be asked: (turn id, item index).
    static QUEUE: RefCell<VecDeque<(String, i32)>> = const { RefCell::new(VecDeque::new()) };
    static ASKED: RefCell<usize> = const { RefCell::new(0) };
    static DEMO_TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

// ------------------------------------------------------------------ pure helpers (tested)

/// The marker of a message: its start, whitespace as one space, 20 characters.
pub(crate) fn hint_of(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat: String = flat.chars().filter(|c| !matches!(c, '「' | '」' | '[' | ']')).collect();
    let mut chars = flat.chars();
    let head: String = chars.by_ref().take(HINT_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// One line of the message: `[返信 #12.3「…」]`.
pub(crate) fn ref_line(id: &str, hint: &str) -> String {
    format!("[返信 {id}「{hint}」]")
}

/// The reply lines at the start of a message and the text after them.
pub(crate) fn parse_ref_lines(text: &str) -> (Vec<(String, String)>, String) {
    let mut refs = Vec::new();
    let mut rest = text;
    loop {
        let (line, after) = match rest.split_once('\n') {
            Some((l, a)) => (l, a),
            None => (rest, ""),
        };
        match parse_ref_line(line.trim_end_matches('\r')) {
            Some(r) => {
                refs.push(r);
                rest = after;
                if after.is_empty() {
                    break;
                }
            }
            None => break,
        }
    }
    (refs, rest.to_string())
}

fn parse_ref_line(line: &str) -> Option<(String, String)> {
    let body = line.strip_prefix("[返信 #")?;
    let (turn, tail) = body.split_once('.')?;
    let item_len = tail.chars().take_while(|c| c.is_ascii_digit()).count();
    let (item, tail) = tail.split_at(item_len);
    if turn.is_empty() || item.is_empty() || !turn.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hint = tail.strip_prefix('「')?.strip_suffix("」]")?;
    Some((format!("#{turn}.{item}"), hint.to_string()))
}

/// The text to send: the reply lines, then the text.
pub(crate) fn with_refs(chips: &[(String, String)], text: &str) -> String {
    if chips.is_empty() {
        return text.to_string();
    }
    let mut out: Vec<String> = chips.iter().map(|(id, hint)| ref_line(id, hint)).collect();
    out.push(text.to_string());
    out.join("\n")
}

/// For each row of the screen (role, text): the message of the thread (role, text) it is, in
/// order, or None. A row that the thread does not have (a status line) is skipped.
pub(crate) fn align(rows: &[(String, String)], thread: &[(String, String)]) -> Vec<Option<usize>> {
    let mut at = 0;
    rows.iter()
        .map(|(role, text)| {
            let found = (at..thread.len()).find(|j| thread[*j].0 == *role && thread[*j].1 == *text);
            if let Some(j) = found {
                at = j + 1;
            }
            found
        })
        .collect()
}

// ------------------------------------------------------------------ screen

fn render_chips(app: &AppWindow) {
    let rows: Vec<ReplyRef> = CHIPS.with(|c| c.borrow().iter().map(|(id, hint)| ReplyRef { id: id.as_str().into(), hint: hint.as_str().into() }).collect());
    app.global::<ReplyState>().set_chips(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn status(app: &AppWindow, text: &str) {
    app.global::<AppState>().set_status(text.into());
    app.global::<AppState>().set_status_level(2);
}

/// What the screen needs of a message: the reply lines of a user message become chips.
pub(crate) fn decorate(m: &mut Message) {
    if m.role != "user" {
        return;
    }
    let (refs, body) = parse_ref_lines(&m.text);
    if refs.is_empty() {
        return;
    }
    let rows: Vec<ReplyRef> = refs.into_iter().map(|(id, hint)| ReplyRef { id: id.into(), hint: hint.into() }).collect();
    m.refs = ModelRc::from(Rc::new(VecModel::from(rows)));
    m.shown = body.into();
}

/// The text of the composer with the reply lines of the chips in front; the chips are used up.
pub(crate) fn compose(app: &AppWindow, text: String) -> String {
    let chips = CHIPS.with(|c| std::mem::take(&mut *c.borrow_mut()));
    render_chips(app);
    with_refs(&chips, &text)
}

fn add_chip(app: &AppWindow, id: &str, hint: &str) {
    CHIPS.with(|c| {
        let mut c = c.borrow_mut();
        if !c.iter().any(|(i, h)| i == id && h == hint) {
            c.push((id.to_string(), hint.to_string()));
        }
    });
    render_chips(app);
}

/// "Reply" under a message (the whole message, or `part` of its text).
fn reply_to(app: &AppWindow, index: usize, part: Option<(usize, usize)>) {
    let model = app.global::<AppState>().get_messages();
    let Some(m) = model.row_data(index) else { return };
    if m.role != "user" && m.role != "assistant" {
        return;
    }
    let text = m.text.to_string();
    let source = match part {
        Some((a, b)) => text.get(a..b).unwrap_or("").to_string(),
        None => if m.role == "user" { parse_ref_lines(&text).1 } else { text },
    };
    let hint = hint_of(&source);
    if !m.ref_id.is_empty() {
        add_chip(app, m.ref_id.as_str(), &hint);
        return;
    }
    // Not named yet: ask now.
    let thread = STATE.with(|s| s.borrow().active.clone());
    if m.turn_id.is_empty() || thread.is_empty() {
        status(app, "この発言の番号をまだ取得できません");
        return;
    }
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc {
            tag: format!("reply.add|{}|{}|{hint}", m.turn_id, m.item_index),
            method: "inocode/messages/resolve".into(),
            params: json!({ "threadId": thread, "turnId": m.turn_id.as_str(), "itemIndex": m.item_index }),
        });
    }
}

fn number_of(v: &Value) -> Option<String> {
    let p = v.get("response").unwrap_or(v);
    Some(format!("#{}.{}", p.get("turnNumber")?.as_i64()?, p.get("itemNumber")?.as_i64()?))
}

/// Ask for the numbers of the messages that have none yet (a few at a time).
fn pump() {
    let Some(h) = HANDLE.with(|c| c.borrow().clone()) else { return };
    let thread = STATE.with(|s| s.borrow().active.clone());
    if thread.is_empty() {
        QUEUE.with(|q| q.borrow_mut().clear());
        return;
    }
    while ASKED.with(|a| *a.borrow()) < IN_FLIGHT {
        let Some((turn, item)) = QUEUE.with(|q| q.borrow_mut().pop_front()) else { break };
        ASKED.with(|a| *a.borrow_mut() += 1);
        h.send(Command::Rpc {
            tag: format!("reply.num|{turn}|{item}"),
            method: "inocode/messages/resolve".into(),
            params: json!({ "threadId": thread, "turnId": turn, "itemIndex": item }),
        });
    }
}

/// The conversation was replaced (history): number the messages.
pub(crate) fn number_all(app: &AppWindow) {
    QUEUE.with(|q| q.borrow_mut().clear());
    ASKED.with(|a| *a.borrow_mut() = 0);
    let model = app.global::<AppState>().get_messages();
    let jobs: Vec<(String, i32)> = (0..model.row_count())
        .filter_map(|i| model.row_data(i))
        .filter(|m| (m.role == "user" || m.role == "assistant") && m.ref_id.is_empty() && !m.turn_id.is_empty())
        .map(|m| (m.turn_id.to_string(), m.item_index))
        .collect();
    QUEUE.with(|q| q.borrow_mut().extend(jobs));
    pump();
}

/// A turn ended: the rows that were added while it ran learn where they sit in the thread, and
/// get their numbers.
pub(crate) fn after_turn(app: &AppWindow) {
    let thread = STATE.with(|s| s.borrow().active.clone());
    if thread.is_empty() {
        return;
    }
    let _ = app;
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: "reply.read".into(), method: "thread/read".into(), params: json!({ "threadId": thread, "includeTurns": true }) });
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if tag == "reply.read" {
        let Ok(v) = result else { return };
        let hist = client::history_from_thread(v.get("thread").unwrap_or(&Value::Null));
        let thread: Vec<(String, String)> = hist.iter().map(|m| (m.role.clone(), m.text.clone())).collect();
        messages_with(app, |model| {
            let rows: Vec<(String, String)> = (0..model.row_count())
                .map(|i| model.row_data(i).map(|m| (m.role.to_string(), m.text.to_string())).unwrap_or_default())
                .collect();
            let found = align(&rows, &thread);
            for (i, j) in found.into_iter().enumerate() {
                let Some(j) = j else { continue };
                let Some(mut m) = model.row_data(i) else { continue };
                if (m.role == "user" || m.role == "assistant") && m.turn_id.is_empty() {
                    m.turn_id = hist[j].turn_id.as_str().into();
                    m.item_index = hist[j].item_index;
                    model.set_row_data(i, m);
                }
            }
        });
        number_all(app);
        return;
    }
    if let Some(rest) = tag.strip_prefix("reply.num|") {
        ASKED.with(|a| {
            let mut a = a.borrow_mut();
            *a = a.saturating_sub(1);
        });
        if let (Ok(v), Some((turn, item))) = (result, rest.split_once('|')) {
            if let (Some(id), Ok(item)) = (number_of(v), item.parse::<i32>()) {
                messages_with(app, |model| {
                    for i in 0..model.row_count() {
                        if let Some(mut m) = model.row_data(i) {
                            if m.turn_id.as_str() == turn && m.item_index == item && m.ref_id.as_str() != id {
                                m.ref_id = id.as_str().into();
                                model.set_row_data(i, m);
                            }
                        }
                    }
                });
            }
        }
        pump();
        return;
    }
    if let Some(rest) = tag.strip_prefix("reply.add|") {
        let hint = rest.splitn(3, '|').nth(2).unwrap_or("");
        match result {
            Ok(v) => match number_of(v) {
                Some(id) => add_chip(app, &id, hint),
                None => status(app, "この発言の番号を取得できません"),
            },
            Err(e) => status(app, &format!("この発言の番号を取得できません: {e}")),
        }
    }
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    if std::env::var("INOCODE_GUI_DEMO").map(|v| v == "reply").unwrap_or(false) {
        // Mock only: a conversation with numbers, so that the screen can be looked at.
        let weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(6), move || {
            let Some(app) = weak.upgrade() else { return };
            messages_with(&app, |model| {
                let mut rows = vec![
                    Message { role: "user".into(), text: "SQLite の並行書き込みはどうなりますか".into(), ref_id: "#1.1".into(), ..Default::default() },
                    Message { role: "assistant".into(), text: "WAL を使うと、読み取りと書き込みが同時にできます。

書き込みは 1 つずつです。".into(), ref_id: "#1.2".into(), turn_id: "t1".into(), item_index: 1, ..Default::default() },
                    Message { role: "user".into(), text: "[返信 #1.2「WAL を使うと、読み取りと書き込みが同時にできます。 書き込みは 1…」]
ありがとう。書き込みが重いときは？".into(), ref_id: "#2.1".into(), ..Default::default() },
                ];
                for m in rows.iter_mut() {
                    decorate(m);
                    if m.role == "assistant" {
                        m.blocks = crate::chat_md::blocks_of(&m.text);
                    }
                }
                for m in rows {
                    model.push(m);
                }
            });
        });
        DEMO_TIMER.with(|t| *t.borrow_mut() = Some(timer));
    }
    let weak = app.as_weak();
    app.global::<ReplyState>().on_remove(move |i| {
        CHIPS.with(|c| {
            let mut c = c.borrow_mut();
            if (i as usize) < c.len() {
                c.remove(i as usize);
            }
        });
        if let Some(app) = weak.upgrade() {
            render_chips(&app);
        }
    });
    let weak = app.as_weak();
    app.global::<ReplyState>().on_jump(move |id| {
        let Some(app) = weak.upgrade() else { return };
        let model = app.global::<AppState>().get_messages();
        match (0..model.row_count()).find(|i| model.row_data(*i).map(|m| m.ref_id.as_str() == id.as_str()).unwrap_or(false)) {
            Some(i) => app.invoke_scroll_to(i as i32),
            None => status(&app, "その発言は、いまの画面にありません"),
        }
    });
    let weak = app.as_weak();
    app.global::<MessageActions>().on_reply_selection(move |index, start, end| {
        if let Some(app) = weak.upgrade() {
            reply_to(&app, index.max(0) as usize, Some((start.max(0) as usize, end.max(0) as usize)));
        }
    });
}

/// "reply" and "copy-id" of the buttons under a message. True when it was one of them.
pub(crate) fn act(app: &AppWindow, kind: &str, index: usize) -> bool {
    match kind {
        "reply" => reply_to(app, index, None),
        "copy-id" => {
            let model = app.global::<AppState>().get_messages();
            if let Some(m) = model.row_data(index) {
                if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(m.ref_id.to_string())) {
                    status(app, &format!("コピーできませんでした: {e}"));
                }
            }
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_is_the_start_of_the_message_on_one_line() {
        assert_eq!(hint_of("SQLite の並行書き込みについて、WAL を使うと読み取りが止まりません"), "SQLite の並行書き込みについて、W…");
        assert_eq!(hint_of("a\n\n  b\tc"), "a b c");
        assert_eq!(hint_of("短い"), "短い");
        // the characters of the format are not in a marker
        assert_eq!(hint_of("「引用」 [x]"), "引用 x");
        assert_eq!(hint_of(&"あ".repeat(20)), "あ".repeat(20));
        assert_eq!(hint_of(&"あ".repeat(21)), format!("{}…", "あ".repeat(20)));
    }

    #[test]
    fn reply_lines_are_written_and_read_back() {
        let chips = vec![("#12.3".to_string(), "SQLite の並行…".to_string()), ("#2.1".to_string(), "x".to_string())];
        let sent = with_refs(&chips, "ありがとう\n続き");
        assert_eq!(sent, "[返信 #12.3「SQLite の並行…」]\n[返信 #2.1「x」]\nありがとう\n続き");
        let (refs, body) = parse_ref_lines(&sent);
        assert_eq!(refs, chips);
        assert_eq!(body, "ありがとう\n続き");
        assert_eq!(with_refs(&[], "text"), "text");
        // no reply lines
        assert_eq!(parse_ref_lines("hello\n[返信 #1.1「x」]"), (vec![], "hello\n[返信 #1.1「x」]".to_string()));
        // only reply lines
        assert_eq!(parse_ref_lines("[返信 #1.1「x」]").1, "");
        // something that only looks like one
        assert!(parse_ref_line("[返信 #a.1「x」]").is_none());
        assert!(parse_ref_line("[返信 #1.「x」]").is_none());
        assert!(parse_ref_line("[返信 #1.1 x]").is_none());
        assert_eq!(parse_ref_line("[返信 #10.22「a」b」]"), Some(("#10.22".to_string(), "a」b".to_string())));
    }

    #[test]
    fn rows_of_the_screen_find_their_place_in_the_thread() {
        let row = |r: &str, t: &str| (r.to_string(), t.to_string());
        let thread = vec![row("user", "q1"), row("assistant", "a1"), row("user", "q2"), row("assistant", "a2")];
        // a status line in between, and the last answer not in the thread yet
        let rows = vec![row("user", "q1"), row("assistant", "a1"), row("tool", "status"), row("user", "q2"), row("assistant", "a2"), row("assistant", "new")];
        assert_eq!(align(&rows, &thread), vec![Some(0), Some(1), None, Some(2), Some(3), None]);
        // the same text twice: in order
        let twice = vec![row("user", "ok"), row("assistant", "x"), row("user", "ok")];
        let th = vec![row("user", "ok"), row("assistant", "x"), row("user", "ok")];
        assert_eq!(align(&twice, &th), vec![Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn the_server_gives_the_number() {
        let v = json!({ "turnNumber": 12, "itemNumber": 3, "text": "x" });
        assert_eq!(number_of(&v), Some("#12.3".to_string()));
        assert_eq!(number_of(&json!({ "response": { "turnNumber": 1, "itemNumber": 2 } })), Some("#1.2".to_string()));
        assert_eq!(number_of(&json!({})), None);
    }
}
