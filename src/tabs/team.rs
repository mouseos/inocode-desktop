//! チームタブ: 社員(担当)、見積もり、測定の概要。
//! inocode/team/status と通知 inocode/team/updated、inocode/metrics/summary を使う。

use super::Host;
use crate::client::Event;
use crate::{AgentRow, AppWindow, MetricRow, TeamState, ThreadMetricRow};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

const TAG_STATUS: &str = "team.status";
const TAG_METRICS: &str = "team.metrics";
const TAG_HISTORY: &str = "team.history";

#[derive(Default)]
struct Data {
    thread_id: String,
    agents: Vec<Value>,
    estimate: Option<Value>,
    /// thread id -> title, from Event::Threads (names for the per-thread table).
    titles: Vec<(String, String)>,
}

thread_local! {
    static DATA: RefCell<Data> = RefCell::new(Data::default());
}

pub fn install(app: &AppWindow, host: &Host) {
    super::team_log::install(app, host);
    let h = host.clone();
    app.global::<TeamState>().on_refresh(move || request(&h, 0));
    let h = host.clone();
    app.global::<TeamState>().on_period_changed(move |i| request_metrics(&h, i));
}

pub fn opened(app: &AppWindow, host: &Host) {
    request(host, app.global::<TeamState>().get_period());
}

/// since (RFC 3339) and the history bucket for a period index.
fn period_params(period: i32, now: i64) -> (String, &'static str) {
    match period {
        1 => (rfc3339(now - 7 * 86400), "day"),
        2 => (rfc3339(now - 30 * 86400), "day"),
        3 => (rfc3339(0), "day"),
        _ => (rfc3339(now - 24 * 3600), "hour"),
    }
}

fn request_metrics(host: &Host, period: i32) {
    let (since, bucket) = period_params(period, now_secs());
    host.rpc(TAG_METRICS, "inocode/metrics/summary", json!({ "since": since }));
    host.rpc(TAG_HISTORY, "inocode/metrics/history", json!({ "since": since, "bucket": bucket }));
}

fn request(host: &Host, period: i32) {
    let thread_id = DATA.with(|d| d.borrow().thread_id.clone());
    if !thread_id.is_empty() {
        host.rpc(TAG_STATUS, "inocode/team/status", json!({ "threadId": thread_id }));
    }
    request_metrics(host, period);
    super::team_log::request(host, &thread_id);
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    super::team_log::event(app, host, event);
    match event {
        Event::ThreadActive { id, .. } => {
            DATA.with(|d| {
                let mut d = d.borrow_mut();
                if d.thread_id != *id {
                    d.thread_id = id.clone();
                    d.agents.clear();
                    d.estimate = None;
                }
            });
            render(app);
            // Only ask while the tab is showing.
            if app.global::<crate::Tabs>().get_current() == 3 {
                request(host, app.global::<TeamState>().get_period());
            }
        }
        Event::Threads(list) => {
            DATA.with(|d| d.borrow_mut().titles = list.iter().map(|t| (t.id.clone(), t.title.clone())).collect());
        }
        Event::Rpc { tag, result } if tag == TAG_HISTORY => {
            if let Ok(v) = result {
                set_history(app, v, app.global::<TeamState>().get_period());
            }
        }
        Event::Rpc { tag, result } if tag == TAG_STATUS => match result {
            Ok(v) => {
                DATA.with(|d| {
                    let mut d = d.borrow_mut();
                    d.agents = v.get("agents").and_then(Value::as_array).cloned().unwrap_or_default();
                    d.estimate = v.get("estimate").cloned();
                });
                app.global::<TeamState>().set_error("".into());
                render(app);
            }
            Err(e) => app.global::<TeamState>().set_error(e.as_str().into()),
        },
        Event::Rpc { tag, result } if tag == TAG_METRICS => match result {
            Ok(v) => set_metrics(app, v),
            Err(e) => app.global::<TeamState>().set_error(e.as_str().into()),
        },
        Event::Notify { method, params } if method == "inocode/team/updated" => {
            let for_this_thread = params
                .get("threadId")
                .and_then(Value::as_str)
                .map(|t| DATA.with(|d| d.borrow().thread_id == t))
                .unwrap_or(true);
            if !for_this_thread {
                return;
            }
            DATA.with(|d| {
                let mut d = d.borrow_mut();
                // The notification carries only the agents that changed.
                if let Some(changed) = params.get("agents").and_then(Value::as_array) {
                    for a in changed {
                        let id = a.get("id").and_then(Value::as_str).unwrap_or("");
                        match d.agents.iter_mut().find(|x| x.get("id").and_then(Value::as_str) == Some(id)) {
                            Some(slot) => *slot = a.clone(),
                            None => d.agents.push(a.clone()),
                        }
                    }
                }
                if let Some(e) = params.get("estimate") {
                    d.estimate = Some(e.clone());
                }
            });
            render(app);
        }
        _ => {}
    }
}

fn payload(v: &Value) -> &Value {
    v.get("response").unwrap_or(v)
}

fn state_label(state: &str) -> &str {
    match state {
        "running" => "実行中",
        "waiting" => "待機中",
        "blocked" => "ブロック中",
        "done" => "完了",
        other => other,
    }
}

fn render(app: &AppWindow) {
    let ts = app.global::<TeamState>();
    DATA.with(|d| {
        let d = d.borrow();
        let now = now_secs();
        let rows: Vec<AgentRow> = d
            .agents
            .iter()
            .map(|a| {
                let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("");
                let paths = a
                    .get("ownedPaths")
                    .and_then(Value::as_array)
                    .map(|p| p.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("、"))
                    .filter(|p| !p.is_empty())
                    .unwrap_or_default();
                let tokens = a
                    .get("tokens")
                    .and_then(Value::as_i64)
                    .map(|t| format!("{} トークン", group(t)))
                    .unwrap_or_default();
                let started = parse_rfc3339(s("startedAt")).map(|t| ago(now - t)).unwrap_or_default();
                AgentRow {
                    name: s("name").into(),
                    paths: paths.into(),
                    state: s("state").into(),
                    state_label: state_label(s("state")).into(),
                    tokens: tokens.into(),
                    started: started.into(),
                }
            })
            .collect();
        // the grid takes them all; the lists take them by state
        let part = |f: &dyn Fn(&str) -> bool| -> ModelRc<AgentRow> {
            ModelRc::from(Rc::new(VecModel::from(rows.iter().filter(|r| f(r.state.as_str())).cloned().collect::<Vec<_>>())))
        };
        ts.set_attention(part(&|s| s == "blocked" || s == "waiting"));
        ts.set_active(part(&|s| s == "running"));
        ts.set_finished(part(&|s| s == "done"));
        let done = rows.iter().filter(|r| r.state == "done").count();
        ts.set_progress(if rows.is_empty() { String::new() } else { format!("{done}/{}", rows.len()) }.into());
        ts.set_agents(ModelRc::from(Rc::new(VecModel::from(rows))));
        match &d.estimate {
            Some(e) if estimate_says_something(e) => {
                let num = |k: &str| e.get(k).and_then(Value::as_f64);
                ts.set_has_estimate(true);
                ts.set_speedup(num("speedup").map(|v| format!("×{v:.1}")).unwrap_or_default().into());
                ts.set_cost(num("costMultiplier").map(|v| format!("×{v:.1}")).unwrap_or_default().into());
                let decision = match e.get("decision").and_then(Value::as_str) {
                    Some("multi") => "複数",
                    Some("single") => "1 人",
                    Some(other) => other,
                    None => "",
                };
                ts.set_decision(decision.into());
                ts.set_reason(reason_text(e.get("reason").and_then(Value::as_str).unwrap_or("")).into());
                let tasks: Vec<MetricRow> =
                    e.get("tasks").and_then(Value::as_array).map(|a| a.iter().map(task_row).collect()).unwrap_or_default();
                ts.set_tasks(ModelRc::from(Rc::new(VecModel::from(tasks))));
            }
            _ => {
                ts.set_has_estimate(false);
                ts.set_tasks(ModelRc::from(Rc::new(VecModel::from(Vec::<MetricRow>::new()))));
            }
        }
    });
}

/// The estimate of a thread without a team is "single, x1.0, x1.0, no estimate yet": that says
/// nothing and is not shown.
fn estimate_says_something(e: &Value) -> bool {
    let num = |k: &str| e.get(k).and_then(Value::as_f64);
    let has_tasks = e.get("tasks").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
    has_tasks || num("speedup").map(|v| (v - 1.0).abs() > 1e-6).unwrap_or(false) || num("costMultiplier").map(|v| (v - 1.0).abs() > 1e-6).unwrap_or(false)
        || !reason_text(e.get("reason").and_then(Value::as_str).unwrap_or("")).is_empty()
}

/// The reason of the estimate in Japanese; a text the server wrote in English only is not shown.
fn reason_text(reason: &str) -> String {
    let r = reason.trim();
    if r.is_empty() || r.eq_ignore_ascii_case("no estimate yet") || r.is_ascii() {
        String::new()
    } else {
        r.to_string()
    }
}

fn task_row(t: &Value) -> MetricRow {
    let secs = t.get("estSeconds").and_then(Value::as_f64).unwrap_or(0.0) as i64;
    let paths = t
        .get("writePaths")
        .and_then(Value::as_array)
        .map(|p| p.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("、"))
        .unwrap_or_default();
    let mut value = format!("約 {}", duration(secs));
    if !paths.is_empty() {
        value.push_str(&format!(" · {paths}"));
    }
    MetricRow { label: t.get("name").and_then(Value::as_str).unwrap_or("").into(), value: value.into() }
}

fn counter_rows(c: &Value) -> Vec<MetricRow> {
    let n = |k: &str| c.get(k).and_then(Value::as_i64);
    let mut rows = Vec::new();
    let mut add = |label: &str, value: Option<String>| {
        if let Some(value) = value {
            rows.push(MetricRow { label: label.into(), value: SharedString::from(value) });
        }
    };
    add("経過時間", n("wallClockMs").map(|ms| duration(ms / 1000)));
    add("入力トークン", n("tokensIn").map(group));
    add("出力トークン", n("tokensOut").map(group));
    add("wait_agent の呼び出し", n("waitAgentCalls").map(|x| x.to_string()));
    add("聞き返し", n("clarifications").map(|x| x.to_string()));
    add("停滞の検出", n("stallsDetected").map(|x| x.to_string()));
    add("誤検知", n("falsePositives").map(|x| x.to_string()));
    rows
}

pub(super) fn thread_name(id: &str) -> String {
    DATA.with(|d| d.borrow().titles.iter().find(|(i, _)| i == id).map(|(_, t)| t.clone()).filter(|t| !t.is_empty()))
        .unwrap_or_else(|| id.to_string())
}

fn thread_rows(v: &Value) -> Vec<ThreadMetricRow> {
    v.get("perThread")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|t| {
                    let c = t.get("counters").unwrap_or(&Value::Null);
                    let n = |k: &str| c.get(k).and_then(Value::as_i64).unwrap_or(0);
                    ThreadMetricRow {
                        name: thread_name(t.get("threadId").and_then(Value::as_str).unwrap_or("")).into(),
                        wall: duration(n("wallClockMs") / 1000).into(),
                        tokens: format!("{} / {}", group(n("tokensIn")), group(n("tokensOut"))).into(),
                        waits: n("waitAgentCalls").to_string().into(),
                        asks: n("clarifications").to_string().into(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn set_metrics(app: &AppWindow, v: &Value) {
    let v = payload(v);
    let rows = counter_rows(v.get("counters").unwrap_or(&Value::Null));
    let ts = app.global::<TeamState>();
    ts.set_metrics(ModelRc::from(Rc::new(VecModel::from(rows))));
    ts.set_thread_metrics(ModelRc::from(Rc::new(VecModel::from(thread_rows(v)))));
}

/// "11-11 05:00" (UTC) for an hour bucket, "11-11" for a day bucket.
fn bucket_label(ms: i64, daily: bool) -> String {
    let t = rfc3339(ms / 1000);
    if daily { t[5..10].to_string() } else { format!("{} {}:00", &t[5..10], &t[11..13]) }
}

fn history_rows(v: &Value, period: i32) -> Vec<MetricRow> {
    let daily = period != 0;
    let mut out: Vec<MetricRow> = payload(v)
        .get("buckets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|b| {
                    let c = b.get("counters").unwrap_or(&Value::Null);
                    let n = |k: &str| c.get(k).and_then(Value::as_i64).unwrap_or(0);
                    let value = format!(
                        "経過 {} · 入力 {} · 出力 {} · 停滞 {}",
                        duration(n("wallClockMs") / 1000),
                        group(n("tokensIn")),
                        group(n("tokensOut")),
                        n("stallsDetected")
                    );
                    MetricRow {
                        label: bucket_label(b.get("startMs").and_then(Value::as_i64).unwrap_or(0), daily).into(),
                        value: value.into(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    out.reverse();
    out
}

fn set_history(app: &AppWindow, v: &Value, period: i32) {
    app.global::<TeamState>().set_history(ModelRc::from(Rc::new(VecModel::from(history_rows(v, period)))));
}

fn group(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn duration(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h} 時間 {m} 分")
    } else if m > 0 {
        format!("{m} 分 {s} 秒")
    } else {
        format!("{s} 秒")
    }
}

fn ago(secs: i64) -> String {
    if secs < 0 {
        return String::new();
    }
    if secs < 60 {
        "たった今開始".into()
    } else if secs < 3600 {
        format!("{} 分前に開始", secs / 60)
    } else if secs < 86400 {
        format!("{} 時間前に開始", secs / 3600)
    } else {
        format!("{} 日前に開始", secs / 86400)
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// "2026-11-11T01:10:00Z" (UTC, optional fraction) -> epoch seconds.
fn parse_rfc3339(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, sec) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec)
}

pub(super) fn rfc3339(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86400));
    let r = t.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", r / 3600, r % 3600 / 60, r % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_roundtrip() {
        let t = parse_rfc3339("2026-11-11T01:10:00Z").unwrap();
        assert_eq!(rfc3339(t), "2026-11-11T01:10:00Z");
        assert_eq!(parse_rfc3339("1970-01-01T00:00:10Z"), Some(10));
    }

    #[test]
    fn number_format() {
        assert_eq!(group(412330), "412,330");
        assert_eq!(group(12), "12");
        assert_eq!(duration(3640), "1 時間 0 分");
    }

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract").join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn period_since_and_bucket() {
        assert_eq!(period_params(0, 90_000), ("1970-01-01T01:00:00Z".to_string(), "hour"));
        assert_eq!(period_params(1, 8 * 86400).1, "day");
        assert_eq!(period_params(3, 5).0, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn summary_rows_include_stalls_and_threads() {
        let f = fixture("metrics-summary.json");
        let rows = counter_rows(payload(&f).get("counters").unwrap());
        assert!(rows.iter().any(|r| r.label == "停滞の検出" && r.value == "2"));
        assert_eq!(thread_rows(payload(&f)).len(), 2);
    }

    #[test]
    fn history_is_newest_first_with_stalls() {
        let rows = history_rows(&fixture("metrics-history.json"), 0);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].value.contains("停滞 2"));
        assert!(rows[0].label.ends_with(":00"));
    }

    #[test]
    fn task_row_shows_seconds_and_paths() {
        let r = task_row(&json!({ "name": "GUI", "estSeconds": 600, "writePaths": ["a/", "b/"] }));
        assert_eq!(r.value, "約 10 分 0 秒 · a/、b/");
    }
}
