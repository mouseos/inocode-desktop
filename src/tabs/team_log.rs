//! チームタブの記録: 停滞と相談役(S4)、エージェント間メッセージ(S5)、圧縮と退避した出力(S6)。
//! inocode/advisor/events と通知 inocode/advisor/event、inocode/messages/list と通知 inocode/messages/appended、
//! inocode/compaction/state と通知 inocode/compaction/event、inocode/toolOutput/list|read、
//! inocode/metrics/markFalsePositive を使う。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, LogRow, MetricRow, TeamState};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

const TAG_ADVISOR: &str = "team.advisor";
const TAG_MESSAGES: &str = "team.messages";
const TAG_COMPACTION: &str = "team.compaction";
const TAG_OUTPUTS: &str = "team.outputs";
const TAG_OUTPUT_READ: &str = "team.output.read";
const TAG_FALSE_POSITIVE: &str = "team.falsePositive";

#[derive(Default)]
struct L {
    thread_id: String,
    advisor: Vec<Value>,
    messages: Vec<Value>,
    /// 0 = すべて、1.. = agents の並び
    filter: usize,
    outputs: Vec<Value>,
    /// 退避した出力の本文(id -> 読み込んだ本文)
    output_text: Vec<(String, String)>,
    open: HashSet<String>,
    /// 画面に出している行のキー(開閉とボタンの対象を引くため)
    advisor_keys: Vec<usize>,
    message_keys: Vec<usize>,
    output_ids: Vec<String>,
}

thread_local! {
    static L: RefCell<L> = RefCell::new(L::default());
}

// ------------------------------------------------------------------ pure helpers (tested)

/// 2026-11-11 の UTC "11-11 05:00"
pub fn time_label(ms: i64) -> String {
    let t = super::team::rfc3339(ms / 1000);
    format!("{} {}", &t[5..10], &t[11..16])
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn first_line(t: &str, max: usize) -> String {
    let line = t.lines().next().unwrap_or("");
    let cut: String = line.chars().take(max).collect();
    if line.chars().count() > max { format!("{cut}…") } else { cut }
}

fn trigger_label(t: &str) -> &str {
    match t {
        "same_failure" => "同じ失敗",
        "repeated_action" => "同じ操作の繰り返し",
        "alternation" => "往復",
        "no_progress" => "進捗なし",
        other => other,
    }
}

/// 停滞と相談の 1 件 -> 行 (head, meta, detail, tone, action)
fn advisor_row(e: &Value, thread: &str) -> (String, String, String, i32, String) {
    let meta = format!("{} · {}", time_label(e.get("tsMs").and_then(Value::as_i64).unwrap_or(0)), thread);
    match s(e, "kind") {
        "stall" => {
            let mut detail = String::new();
            if !s(e, "signature").is_empty() {
                detail.push_str(&format!("署名: {}\n", s(e, "signature")));
            }
            let recent: Vec<&str> =
                e.get("recentActions").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            if !recent.is_empty() {
                detail.push_str("直近の操作:\n");
                for r in recent {
                    detail.push_str(&format!("- {r}\n"));
                }
            }
            (format!("停滞を検出: {}", trigger_label(s(e, "trigger"))), meta, detail.trim_end().to_string(), 1, "誤検知として記録".into())
        }
        "advice" => {
            let model = if s(e, "model").is_empty() { String::new() } else { format!(" · {}", s(e, "model")) };
            (format!("相談役の助言: {}", first_line(s(e, "text"), 60)), format!("{meta}{model}"), s(e, "text").to_string(), 0, String::new())
        }
        _ => (format!("相談役の失敗: {}", first_line(s(e, "text"), 60)), meta, s(e, "text").to_string(), 2, String::new()),
    }
}

fn message_kind_label(kind: &str) -> &str {
    match kind {
        "ASK" => "質問",
        "ANSWER" => "回答",
        "BLOCKED" => "ブロック",
        "RESULT" => "結果",
        "HANDOFF" => "引き継ぎ",
        other => other,
    }
}

fn field_label(key: &str) -> &str {
    match key {
        "q" => "質問",
        "a" => "回答",
        "reason" => "理由",
        "summary" => "要約",
        "task" => "作業",
        "files" => "ファイル",
        "next" => "次の作業",
        "result" => "結果",
        other => other,
    }
}

fn conf_label(c: &str) -> &str {
    match c {
        "high" => "高",
        "med" | "medium" => "中",
        "low" => "低",
        other => other,
    }
}

/// "/root/app-server" -> 深さ(root = 0)
fn depth(path: &str) -> usize {
    path.matches('/').count().saturating_sub(1)
}

fn short_path(p: &str) -> &str {
    p.rsplit('/').find(|x| !x.is_empty()).unwrap_or(p)
}

fn is_conflict(text: &str) -> bool {
    let t = text.to_lowercase();
    ["refus", "reject", "declin", "disagree", "conflict", "拒否", "反対", "衝突"].iter().any(|w| t.contains(w))
}

/// メッセージ 1 件 -> 行 (head, meta, detail, tone)。人間向けは見出し + 分解。逐語は開いたときの末尾。
fn message_row(m: &Value) -> (String, String, String, i32) {
    let parsed = m.get("parsed");
    let kind = parsed.map(|p| s(p, "kind")).unwrap_or("");
    let fields: Vec<(&str, &str)> = parsed
        .and_then(|p| p.get("fields"))
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|f| (s(f, "key"), s(f, "value"))).collect())
        .unwrap_or_default();
    let heading = ["q", "a", "reason", "summary", "task"]
        .iter()
        .find_map(|k| fields.iter().find(|(key, v)| key == k && !v.is_empty()).map(|(_, v)| *v))
        .unwrap_or("");
    let label = if kind.is_empty() { "メッセージ" } else { message_kind_label(kind) };
    let indent = "  ".repeat(depth(s(m, "from")));
    let head = if heading.is_empty() { format!("{indent}{label}") } else { format!("{indent}{label}: {heading}") };
    let wake = match s(m, "delivery") {
        "followup_task" => "起こす",
        "send_message" => "起こさない",
        "spawn" => "起動時",
        "final" => "最終結果",
        other => other,
    };
    let mut meta = format!(
        "{} · {} → {} · {}",
        time_label(m.get("tsMs").and_then(Value::as_i64).unwrap_or(0)),
        short_path(s(m, "from")),
        short_path(s(m, "to")),
        wake
    );
    if let Some(p) = parsed {
        if !s(p, "conf").is_empty() {
            meta.push_str(&format!(" · 確度 {}", conf_label(s(p, "conf"))));
        }
    }
    let mut detail = String::new();
    for (k, v) in &fields {
        detail.push_str(&format!("{}: {}\n", field_label(k), v));
    }
    if !detail.is_empty() {
        detail.push('\n');
    }
    detail.push_str("逐語:\n");
    detail.push_str(s(m, "text"));
    let tone = if kind == "BLOCKED" || is_conflict(s(m, "text")) { 2 } else { 0 };
    (head, meta, detail, tone)
}

fn message_agents(messages: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in messages {
        for k in ["from", "to"] {
            let a = s(m, k);
            if !a.is_empty() && !out.iter().any(|x| x == a) {
                out.push(a.to_string());
            }
        }
    }
    out
}

fn bytes_label(n: i64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn compaction_rows(v: &Value) -> (Vec<(String, String)>, bool) {
    let n = |k: &str| v.get(k).and_then(Value::as_i64);
    let tripped = v.get("breakerTripped").and_then(Value::as_bool).unwrap_or(false);
    let mut rows = Vec::new();
    rows.push(("自動圧縮".to_string(), if tripped { "停止中(/compact で再開)" } else { "有効" }.to_string()));
    if let (Some(c), Some(m)) = (n("consecutiveFailures"), n("maxFailures")) {
        rows.push(("連続失敗".to_string(), format!("{c} / {m}")));
    }
    if let Some(e) = v.get("lastEvent").filter(|e| e.is_object()) {
        let kind = match s(e, "kind") {
            "completed" => "完了",
            "failed" => "失敗",
            "breakerTripped" => "遮断",
            other => other,
        };
        let auto = if e.get("auto").and_then(Value::as_bool).unwrap_or(false) { "自動" } else { "手動" };
        rows.push(("直近の圧縮".to_string(), format!("{kind}({auto}) · {}", s(e, "reason"))));
        let files = e.get("reattachedFiles").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
        let rules = e.get("pinnedRuleIds").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
        rows.push(("再添付したファイル".to_string(), format!("{files} 件")));
        rows.push(("再注入した禁止リスト".to_string(), format!("{rules} 件")));
    }
    (rows, tripped)
}

// ------------------------------------------------------------------ rendering

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn render_advisor(app: &AppWindow) {
    L.with(|l| {
        let mut l = l.borrow_mut();
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        for (i, e) in l.advisor.iter().enumerate() {
            let (head, meta, detail, tone, action) = advisor_row(e, &super::team::thread_name(s(e, "threadId")));
            let key = format!("a{}{}{}", e.get("tsMs").and_then(Value::as_i64).unwrap_or(0), s(e, "kind"), s(e, "threadId"));
            rows.push(LogRow {
                head: head.into(),
                meta: meta.into(),
                detail: detail.into(),
                tone,
                open: l.open.contains(&key),
                action_label: action.into(),
            });
            keys.push(i);
        }
        l.advisor_keys = keys;
        app.global::<TeamState>().set_advisor(model(rows));
    });
}

fn msg_key(m: &Value) -> String {
    format!("m{}{}{}", m.get("tsMs").and_then(Value::as_i64).unwrap_or(0), s(m, "from"), s(m, "to"))
}

fn render_messages(app: &AppWindow) {
    L.with(|l| {
        let mut l = l.borrow_mut();
        let agents = message_agents(&l.messages);
        let filter = if l.filter <= agents.len() { l.filter } else { 0 };
        let mut labels = vec!["すべて".to_string()];
        labels.extend(agents.iter().map(|a| short_path(a).to_string()));
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        for (i, m) in l.messages.iter().enumerate() {
            if filter > 0 {
                let a = &agents[filter - 1];
                if s(m, "from") != a && s(m, "to") != a {
                    continue;
                }
            }
            let (head, meta, detail, tone) = message_row(m);
            rows.push(LogRow { head: head.into(), meta: meta.into(), detail: detail.into(), tone, open: l.open.contains(&msg_key(m)), action_label: "".into() });
            keys.push(i);
        }
        l.message_keys = keys;
        let st = app.global::<TeamState>();
        st.set_message_agents(model(labels.into_iter().map(Into::into).collect()));
        st.set_message_filter(filter as i32);
        st.set_messages(model(rows));
    });
}

fn render_outputs(app: &AppWindow) {
    L.with(|l| {
        let mut l = l.borrow_mut();
        let mut rows = Vec::new();
        let mut ids = Vec::new();
        for o in &l.outputs {
            let id = s(o, "id").to_string();
            let bytes = o.get("bytes").and_then(Value::as_i64).unwrap_or(0);
            let tokens = o.get("approxTokens").and_then(Value::as_i64).unwrap_or(0);
            let tool = if s(o, "tool").is_empty() { "出力" } else { s(o, "tool") };
            let thread = if s(o, "threadId").is_empty() { String::new() } else { format!(" · {}", super::team::thread_name(s(o, "threadId"))) };
            let text = l.output_text.iter().find(|(i, _)| *i == id).map(|(_, t)| t.clone());
            rows.push(LogRow {
                head: format!("{tool} · {} (約 {tokens} トークン)", bytes_label(bytes)).into(),
                meta: format!("{}{thread}", time_label(o.get("tsMs").and_then(Value::as_i64).unwrap_or(0))).into(),
                detail: text.unwrap_or_else(|| "読み込み中…".into()).into(),
                tone: 0,
                open: l.open.contains(&format!("o{id}")),
                action_label: "".into(),
            });
            ids.push(id);
        }
        l.output_ids = ids;
        app.global::<TeamState>().set_outputs(model(rows));
    });
}

fn render_compaction(app: &AppWindow, v: &Value) {
    let (rows, tripped) = compaction_rows(v);
    let st = app.global::<TeamState>();
    st.set_compaction(model(rows.into_iter().map(|(label, value)| MetricRow { label: label.into(), value: value.into() }).collect()));
    st.set_breaker_tripped(tripped);
}

// ------------------------------------------------------------------ tab hooks (called from team.rs)

pub fn install(app: &AppWindow, host: &Host) {
    let st = app.global::<TeamState>();

    let weak = app.as_weak();
    st.on_advisor_toggled(move |i| {
        let Some(app) = weak.upgrade() else { return };
        toggle(|l| l.advisor_keys.get(i as usize).and_then(|&k| l.advisor.get(k)).map(|e| {
            format!("a{}{}{}", e.get("tsMs").and_then(Value::as_i64).unwrap_or(0), s(e, "kind"), s(e, "threadId"))
        }));
        render_advisor(&app);
    });
    let h = host.clone();
    st.on_advisor_action(move |i| {
        let target = L.with(|l| {
            let l = l.borrow();
            l.advisor_keys.get(i as usize).and_then(|&k| l.advisor.get(k)).map(|e| s(e, "threadId").to_string())
        });
        if let Some(thread) = target {
            h.rpc(TAG_FALSE_POSITIVE, "inocode/metrics/markFalsePositive", json!({ "threadId": thread, "source": "advisor" }));
        }
    });
    let weak = app.as_weak();
    st.on_message_toggled(move |i| {
        let Some(app) = weak.upgrade() else { return };
        toggle(|l| l.message_keys.get(i as usize).and_then(|&k| l.messages.get(k)).map(msg_key));
        render_messages(&app);
    });
    let weak = app.as_weak();
    st.on_message_filter_changed(move |i| {
        let Some(app) = weak.upgrade() else { return };
        L.with(|l| l.borrow_mut().filter = i.max(0) as usize);
        render_messages(&app);
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_output_toggled(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let id = L.with(|l| l.borrow().output_ids.get(i as usize).cloned());
        let Some(id) = id else { return };
        let (opened, loaded) = L.with(|l| {
            let mut l = l.borrow_mut();
            let key = format!("o{id}");
            let opened = if l.open.remove(&key) { false } else { l.open.insert(key) };
            (opened, l.output_text.iter().any(|(i, _)| *i == id))
        });
        if opened && !loaded {
            h.rpc(TAG_OUTPUT_READ, "inocode/toolOutput/read", json!({ "id": id, "limit": 20000 }));
        }
        render_outputs(&app);
    });
}

fn toggle(key_of: impl Fn(&L) -> Option<String>) {
    L.with(|l| {
        let mut l = l.borrow_mut();
        if let Some(key) = key_of(&l) {
            if !l.open.remove(&key) {
                l.open.insert(key);
            }
        }
    });
}

pub fn request(host: &Host, thread_id: &str) {
    L.with(|l| l.borrow_mut().thread_id = thread_id.to_string());
    host.rpc(TAG_ADVISOR, "inocode/advisor/events", json!({ "limit": 50 }));
    host.rpc(TAG_OUTPUTS, "inocode/toolOutput/list", json!({ "limit": 50 }));
    if !thread_id.is_empty() {
        host.rpc(TAG_MESSAGES, "inocode/messages/list", json!({ "threadId": thread_id, "limit": 200 }));
        host.rpc(TAG_COMPACTION, "inocode/compaction/state", json!({ "threadId": thread_id }));
    }
}

fn payload(v: &Value) -> &Value {
    v.get("response").unwrap_or(v)
}

fn items<'a>(v: &'a Value, key: &str) -> Vec<Value> {
    payload(v).get(key).and_then(Value::as_array).cloned().unwrap_or_default()
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    match event {
        Event::ThreadActive { id, .. } => {
            L.with(|l| {
                let mut l = l.borrow_mut();
                if l.thread_id != *id {
                    l.messages.clear();
                    l.filter = 0;
                }
            });
            render_messages(app);
        }
        Event::Rpc { tag, result } if tag == TAG_ADVISOR => {
            if let Ok(v) = result {
                let mut list = items(v, "events");
                list.sort_by_key(|e| std::cmp::Reverse(e.get("tsMs").and_then(Value::as_i64).unwrap_or(0)));
                L.with(|l| l.borrow_mut().advisor = list);
                render_advisor(app);
            }
        }
        Event::Rpc { tag, result } if tag == TAG_MESSAGES => {
            if let Ok(v) = result {
                let mut list = items(v, "messages");
                list.sort_by_key(|m| m.get("tsMs").and_then(Value::as_i64).unwrap_or(0));
                L.with(|l| l.borrow_mut().messages = list);
                render_messages(app);
            }
        }
        Event::Rpc { tag, result } if tag == TAG_COMPACTION => {
            if let Ok(v) = result {
                render_compaction(app, payload(v));
            }
        }
        Event::Rpc { tag, result } if tag == TAG_OUTPUTS => {
            if let Ok(v) = result {
                let mut list = items(v, "items");
                list.sort_by_key(|o| std::cmp::Reverse(o.get("tsMs").and_then(Value::as_i64).unwrap_or(0)));
                L.with(|l| l.borrow_mut().outputs = list);
                render_outputs(app);
            }
        }
        Event::Rpc { tag, result } if tag == TAG_OUTPUT_READ => {
            if let Ok(v) = result {
                let p = payload(v);
                let mut text = s(p, "text").to_string();
                if p.get("truncated").and_then(Value::as_bool).unwrap_or(false) {
                    text.push_str("\n…(続きがあります)");
                }
                L.with(|l| l.borrow_mut().output_text.push((s(p, "id").to_string(), text)));
                render_outputs(app);
            } else if let Err(e) = result {
                app.global::<TeamState>().set_error(e.as_str().into());
            }
        }
        Event::Rpc { tag, result } if tag == TAG_FALSE_POSITIVE => {
            if let Err(e) = result {
                app.global::<TeamState>().set_error(e.as_str().into());
            }
        }
        Event::Notify { method, params } if method == "inocode/advisor/event" => {
            L.with(|l| l.borrow_mut().advisor.insert(0, params.clone()));
            render_advisor(app);
        }
        Event::Notify { method, params } if method == "inocode/messages/appended" => {
            let mine = L.with(|l| l.borrow().thread_id == s(params, "threadId"));
            if mine {
                if let Some(m) = params.get("message") {
                    L.with(|l| l.borrow_mut().messages.push(m.clone()));
                    render_messages(app);
                }
            }
        }
        Event::Notify { method, params } if method == "inocode/compaction/event" => {
            let (tid, mine) = L.with(|l| {
                let l = l.borrow();
                (l.thread_id.clone(), l.thread_id == s(params, "threadId"))
            });
            if mine {
                host.rpc(TAG_COMPACTION, "inocode/compaction/state", json!({ "threadId": tid }));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ref/contract-fixtures").join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn advisor_events_become_rows() {
        let list = items(&fixture("advisor-events.json"), "events");
        let advice = advisor_row(&list[0], "T");
        assert!(advice.0.starts_with("相談役の助言"));
        let stall = advisor_row(&list[1], "T");
        assert_eq!(stall.0, "停滞を検出: 同じ失敗");
        assert!(stall.2.contains("署名: cargo test") && stall.2.contains("- cargo test"));
        assert_eq!(stall.4, "誤検知として記録");
        let err = advisor_row(&list[2], "T");
        assert_eq!(err.3, 2);
    }

    #[test]
    fn messages_show_summary_first_and_verbatim_inside() {
        let list = items(&fixture("messages-list.json"), "messages");
        let (head, meta, detail, tone) = message_row(&list[0]);
        assert_eq!(head, "結果: done");
        assert!(meta.contains("起こす") && meta.contains("確度 高"));
        assert!(detail.contains("要約: done") && detail.ends_with("RESULT summary=done conf:high"));
        assert_eq!(tone, 0);
        let blocked = message_row(&list[2]);
        assert_eq!(blocked.3, 2, "BLOCKED stands out");
        let plain = message_row(&list[3]);
        assert_eq!(plain.0, "  メッセージ");
        assert!(plain.2.starts_with("逐語:"));
        assert_eq!(message_agents(&list).len(), 4);
    }

    #[test]
    fn compaction_breaker_is_shown() {
        let (rows, tripped) = compaction_rows(payload(&fixture("compaction-state.json")));
        assert!(!tripped);
        assert!(rows.iter().any(|(k, v)| k == "連続失敗" && v == "1 / 3"));
        let (rows, tripped) = compaction_rows(&json!({ "consecutiveFailures": 3, "maxFailures": 3, "breakerTripped": true }));
        assert!(tripped && rows[0].1.contains("停止中"));
    }

    #[test]
    fn byte_labels() {
        assert_eq!(bytes_label(182340), "178.1 KB");
        assert_eq!(bytes_label(12), "12 B");
    }
}
