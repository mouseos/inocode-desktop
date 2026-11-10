//! チームタブ: 社員(担当)、見積もり、測定の概要。
//! inocode/team/status と通知 inocode/team/updated、inocode/metrics/summary を使う。

use super::Host;
use crate::client::Event;
use crate::{AgentRow, AppWindow, MetricRow, TeamState};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

const TAG_STATUS: &str = "team.status";
const TAG_METRICS: &str = "team.metrics";

#[derive(Default)]
struct Data {
    thread_id: String,
    agents: Vec<Value>,
    estimate: Option<Value>,
}

thread_local! {
    static DATA: RefCell<Data> = RefCell::new(Data::default());
}

pub fn install(app: &AppWindow, host: &Host) {
    let host = host.clone();
    app.global::<TeamState>().on_refresh(move || request(&host));
}

pub fn opened(_app: &AppWindow, host: &Host) {
    request(host);
}

fn request(host: &Host) {
    let thread_id = DATA.with(|d| d.borrow().thread_id.clone());
    if !thread_id.is_empty() {
        host.rpc(TAG_STATUS, "inocode/team/status", json!({ "threadId": thread_id }));
    }
    host.rpc(TAG_METRICS, "inocode/metrics/summary", json!({ "since": rfc3339(now_secs() - 24 * 3600) }));
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
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
                request(host);
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
        ts.set_agents(ModelRc::from(Rc::new(VecModel::from(rows))));
        match &d.estimate {
            Some(e) => {
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
                ts.set_reason(e.get("reason").and_then(Value::as_str).unwrap_or("").into());
            }
            None => ts.set_has_estimate(false),
        }
    });
}

fn set_metrics(app: &AppWindow, v: &Value) {
    let c = v.get("counters").unwrap_or(&Value::Null);
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
    add("誤検知", n("falsePositives").map(|x| x.to_string()));
    app.global::<TeamState>().set_metrics(ModelRc::from(Rc::new(VecModel::from(rows))));
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

fn rfc3339(t: i64) -> String {
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
}
