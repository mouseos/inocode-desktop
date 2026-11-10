//! JSON parsing and request-parameter builders.

use serde_json::{json, Value};
use std::process::Command as Proc;
use super::*;

// ------------------------------------------------------------------ helpers

pub(super) fn is_auth_error(message: &str) -> bool {
    message.contains("401") || message.to_ascii_lowercase().contains("unauthorized")
}

pub(super) fn item_id(item: &Value) -> Option<String> {
    item.get("id").and_then(Value::as_str).map(str::to_string)
}

pub(super) fn status_label(status: &str) -> &'static str {
    match status {
        "inProgress" => "実行中",
        "completed" => "完了",
        "failed" => "失敗",
        "declined" => "拒否",
        _ => "",
    }
}

/// One-line description of a command / tool item, or None for item kinds that
/// are not shown as rows.
pub fn tool_text(item: &Value) -> Option<String> {
    let status = status_label(item.get("status").and_then(Value::as_str).unwrap_or(""));
    let suffix = if status.is_empty() { String::new() } else { format!(" — {status}") };
    match item.get("type").and_then(Value::as_str)? {
        "commandExecution" => {
            let cmd = item.get("command").and_then(Value::as_str).unwrap_or("");
            let exit = item
                .get("exitCode")
                .and_then(Value::as_i64)
                .filter(|c| *c != 0)
                .map(|c| format!(" (終了コード {c})"))
                .unwrap_or_default();
            Some(format!("実行: {cmd}{suffix}{exit}"))
        }
        "fileChange" => {
            let n = item.get("changes").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            Some(format!("ファイル変更 {n} 件{suffix}"))
        }
        "mcpToolCall" => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or("");
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("");
            Some(format!("ツール {server}/{tool}{suffix}"))
        }
        "dynamicToolCall" => {
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("");
            Some(format!("ツール {tool}{suffix}"))
        }
        _ => None,
    }
}

pub(super) fn thread_title(thread: &Value) -> String {
    let name = thread.get("name").and_then(Value::as_str).unwrap_or("").trim();
    let text = if name.is_empty() {
        thread.get("preview").and_then(Value::as_str).unwrap_or("").trim()
    } else {
        name
    };
    let first_line = text.lines().next().unwrap_or("");
    if first_line.is_empty() {
        "(無題)".to_string()
    } else {
        first_line.chars().take(60).collect()
    }
}

pub fn parse_threads(result: &Value) -> Vec<ThreadInfo> {
    result
        .get("data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|t| {
                    Some(ThreadInfo {
                        id: t.get("id")?.as_str()?.to_string(),
                        title: thread_title(t),
                        cwd: t.get("cwd").and_then(Value::as_str).unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn history_from_thread(thread: &Value) -> Vec<Msg> {
    let mut out = Vec::new();
    let turns = thread.get("turns").and_then(Value::as_array).cloned().unwrap_or_default();
    for turn in turns {
        for item in turn.get("items").and_then(Value::as_array).cloned().unwrap_or_default() {
            let id = item_id(&item).unwrap_or_default();
            match item.get("type").and_then(Value::as_str) {
                Some("userMessage") => {
                    let text = item
                        .get("content")
                        .and_then(Value::as_array)
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|p| p.get("text").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    out.push(Msg { id, role: "user".into(), text });
                }
                Some("agentMessage") => {
                    let text = item.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                    out.push(Msg { id, role: "assistant".into(), text });
                }
                _ => {
                    if let Some(text) = tool_text(&item) {
                        out.push(Msg { id, role: "tool".into(), text });
                    }
                }
            }
        }
    }
    out
}

pub(super) fn thread_start_params(s: &Settings) -> Value {
    let mut p = json!({});
    if let Some(perm) = s.perm {
        if let Some(v) = perm.policy() {
            p["approvalPolicy"] = json!(v);
        }
        if let Some(v) = perm.sandbox_mode() {
            p["sandbox"] = json!(v);
        }
        if let Some(v) = perm.reviewer(false) {
            p["approvalsReviewer"] = json!(v);
        }
    }
    if let Some(m) = &s.model {
        p["model"] = json!(m);
    }
    if let Some(t) = &s.service_tier {
        p["serviceTier"] = match t {
            Some(id) => json!(id),
            None => Value::Null,
        };
    }
    if let Some(c) = &s.cwd {
        p["cwd"] = json!(c);
    }
    if s.daybreak {
        p["daybreakEnabled"] = json!(true);
    }
    p
}

pub(super) fn sandbox_policy(mode: &str) -> Value {
    match mode {
        "read-only" => json!({ "type": "readOnly" }),
        "danger-full-access" => json!({ "type": "dangerFullAccess" }),
        _ => json!({ "type": "workspaceWrite" }),
    }
}

/// Per-turn fields. Model, effort and speed go with every turn (as in the desktop
/// client); permissions only when thread/settings/update is unavailable.
pub(super) fn turn_overrides(s: &Settings, with_permissions: bool, params: &mut Value) {
    if with_permissions {
        if let Some(perm) = s.perm {
            if let Some(v) = perm.policy() {
                params["approvalPolicy"] = json!(v);
            }
            if let Some(v) = perm.sandbox_mode() {
                params["sandboxPolicy"] = sandbox_policy(v);
            }
            if let Some(v) = perm.reviewer(true) {
                params["approvalsReviewer"] = json!(v);
            }
        }
    }
    if let Some(t) = &s.service_tier {
        params["serviceTier"] = match t {
            Some(id) => json!(id),
            None => Value::Null,
        };
    }
    if let Some(m) = &s.model {
        params["model"] = json!(m);
        if let Some(program) = s.programs.iter().find(|x| &x.id == m).and_then(|x| x.program_for(s.daybreak)) {
            params["cyberAccessProgram"] = json!(program);
        }
    }
    if let Some(e) = &s.effort {
        params["effort"] = json!(e);
    }
}

pub(super) fn applied(result: &Value) -> Event {
    let text = |p: &str| result.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
    let approval = match result.get("approvalPolicy") {
        Some(Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => "granular".to_string(),
        _ => String::new(),
    };
    Event::Applied {
        model: text("/model"),
        effort: text("/reasoningEffort"),
        approval,
        sandbox: text("/sandbox/type"),
        reviewer: text("/approvalsReviewer"),
        cwd: text("/cwd"),
    }
}

pub fn parse_models(result: &Value) -> Vec<ModelInfo> {
    result
        .get("data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|m| {
                    let id = m.get("model").or_else(|| m.get("id"))?.as_str()?.to_string();
                    let name = m.get("displayName").and_then(Value::as_str).unwrap_or(&id).to_string();
                    let efforts = m
                        .get("supportedReasoningEfforts")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|e| e.get("reasoningEffort").and_then(Value::as_str))
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    let default_effort =
                        m.get("defaultReasoningEffort").and_then(Value::as_str).unwrap_or("").to_string();
                    let programs = m
                        .pointer("/availableAccessPrograms/cyber")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                        .unwrap_or_default();
                    let hidden = m.get("hidden").and_then(Value::as_bool).unwrap_or(false);
                    let is_default = m.get("isDefault").and_then(Value::as_bool).unwrap_or(false);
                    let description = m.get("description").and_then(Value::as_str).unwrap_or("").to_string();
                    let service_tiers = m
                        .get("serviceTiers")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| {
                                    Some((
                                        t.get("id")?.as_str()?.to_string(),
                                        t.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                                        t.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let default_service_tier =
                        m.get("defaultServiceTier").and_then(Value::as_str).map(str::to_string);
                    Some(ModelInfo {
                        id,
                        name,
                        description,
                        efforts,
                        default_effort,
                        hidden,
                        is_default,
                        programs,
                        service_tiers,
                        default_service_tier,
                        min_version: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Account state for the screen: logged in or not, and a label (the plan). The e-mail
/// address in the `account/read` reply is never read. The reply has no display-name field
/// (only the workspace name), so, as before, no name is shown.
pub fn parse_account(result: &Value) -> (bool, String) {
    match result.pointer("/account/type").and_then(Value::as_str) {
        Some("chatgpt") => {
            let plan = result.pointer("/account/planType").and_then(Value::as_str).unwrap_or("");
            (true, if plan.is_empty() { "アカウント".to_string() } else { plan.to_string() })
        }
        Some("apiKey") => (true, "API キー".to_string()),
        Some(_) => (true, "アカウント".to_string()),
        None => {
            let need = result.get("requiresOpenaiAuth").and_then(Value::as_bool).unwrap_or(true);
            (!need, if need { "未ログイン".to_string() } else { "ローカル".to_string() })
        }
    }
}

pub(super) fn window_label(mins: Option<i64>) -> String {
    match mins {
        Some(m) if m >= 10_000 => "週".to_string(),
        Some(m) if m >= 60 => format!("{}時間", m / 60),
        Some(m) => format!("{m}分"),
        None => "枠".to_string(),
    }
}

pub fn rate_limit_text(snapshot: &Value) -> Option<String> {
    let mut parts = Vec::new();
    for key in ["primary", "secondary"] {
        if let Some(w) = snapshot.get(key).filter(|w| !w.is_null()) {
            let used = w.get("usedPercent").and_then(Value::as_i64).unwrap_or(0);
            let label = window_label(w.get("windowDurationMins").and_then(Value::as_i64));
            parts.push(format!("{label} {used}%"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

pub(super) fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Proc::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .creation_flags(0x0800_0000)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = Proc::new("xdg-open").arg(url).spawn();
    }
}

pub(super) fn approval_result(method: &str, params: &Value, decision: Decision) -> Value {
    if method == "item/permissions/requestApproval" {
        return match decision {
            Decision::Accept | Decision::AcceptForSession => json!({
                "permissions": params.get("permissions").cloned().unwrap_or_else(|| json!({})),
                "scope": if decision == Decision::AcceptForSession { "session" } else { "turn" },
            }),
            _ => json!({ "permissions": {}, "scope": "turn" }),
        };
    }
    json!({ "decision": decision.wire() })
}

