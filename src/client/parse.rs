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
        // Claude: "Thinking…" while it runs, "Thought for {s}s" afterwards.
        "reasoning" => {
            let ms = |k: &str| item.get(k).and_then(Value::as_i64);
            Some(match (ms("startedAtMs"), ms("completedAtMs")) {
                (Some(a), Some(b)) if b >= a => format!("{}秒間思考しました", ((b - a) + 500) / 1000),
                _ if item.get("status").and_then(Value::as_str) == Some("completed") => "思考しました".to_string(),
                _ => "思考中…".to_string(),
            })
        }
        "plan" => Some("プラン".to_string()),
        "contextCompaction" => Some("コンテキストを圧縮しました".to_string()),
        "webSearch" => {
            let q = item
                .get("query")
                .and_then(Value::as_str)
                .or_else(|| item.pointer("/action/query").and_then(Value::as_str))
                .unwrap_or("");
            Some(if q.is_empty() { "ウェブ検索".to_string() } else { format!("ウェブ検索: {q}") })
        }
        "imageView" => Some(format!("画像を表示: {}", item.get("path").and_then(Value::as_str).unwrap_or(""))),
        "collabAgentToolCall" => {
            let states = item.get("agentsStates").and_then(Value::as_object);
            let count = |want: &[&str]| {
                states
                    .map(|m| m.values().filter(|v| want.contains(&v.get("status").and_then(Value::as_str).unwrap_or(""))).count())
                    .unwrap_or(0)
            };
            let done = item.get("status").and_then(Value::as_str) != Some("inProgress");
            Some(match item.get("tool").and_then(Value::as_str).unwrap_or("") {
                "spawnAgent" => "サブエージェントを起動".to_string(),
                "wait" if done => format!("待機が完了 ({} 件完了)", count(&["completed", "errored", "shutdown", "interrupted"])),
                "wait" => format!("待機中 ({} 件実行中)", count(&["running", "pendingInit"])),
                "sendInput" | "sendMessage" | "followupTask" => "サブエージェントへメッセージを送信".to_string(),
                "closeAgent" => "サブエージェントを終了".to_string(),
                "interruptAgent" => "サブエージェントを中断".to_string(),
                "resumeAgent" => "サブエージェントを再開".to_string(),
                _ => "サブエージェントの一覧".to_string(),
            })
        }
        "subAgentActivity" => {
            let path = item.get("agentPath").and_then(Value::as_str).unwrap_or("");
            let what = match item.get("kind").and_then(Value::as_str).unwrap_or("") {
                "started" => "が起動",
                "interacted" => "が応答",
                "interrupted" => "が中断",
                "completed" => "が完了",
                _ => "",
            };
            Some(format!("サブエージェント {path} {what}"))
        }
        _ => None,
    }
}

/// The body shown when a tool row is expanded.
pub fn tool_detail(item: &Value) -> String {
    let text = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match item.get("type").and_then(Value::as_str).unwrap_or("") {
        "commandExecution" => {
            let out = text("aggregatedOutput");
            if out.is_empty() {
                format!("$ {}", text("command"))
            } else {
                format!("$ {}\n{}", text("command"), out)
            }
        }
        "fileChange" => item
            .get("changes")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .map(|c| {
                        let kind = match c.pointer("/kind/type").and_then(Value::as_str) {
                            Some("add") => "追加",
                            Some("delete") => "削除",
                            _ => "変更",
                        };
                        format!(
                            "{kind} {}\n{}",
                            c.get("path").and_then(Value::as_str).unwrap_or(""),
                            c.get("diff").and_then(Value::as_str).unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        "mcpToolCall" | "dynamicToolCall" => {
            let mut out = String::new();
            if let Some(a) = item.get("arguments").filter(|a| !a.is_null()) {
                out.push_str(&format!("引数: {a}\n"));
            }
            if let Some(r) = item.get("result").filter(|r| !r.is_null()) {
                out.push_str(&format!("結果: {r}\n"));
            }
            if let Some(e) = item.get("error").filter(|e| !e.is_null()) {
                out.push_str(&format!("エラー: {e}\n"));
            }
            out.trim_end().to_string()
        }
        "reasoning" => item
            .get("summary")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n\n"))
            .unwrap_or_default(),
        "plan" => text("text"),
        "collabAgentToolCall" => {
            let mut out = text("prompt");
            if let Some(m) = item.get("agentsStates").and_then(Value::as_object) {
                for (agent, st) in m {
                    let status = st.get("status").and_then(Value::as_str).unwrap_or("");
                    let msg = st.get("message").and_then(Value::as_str).unwrap_or("");
                    out.push_str(&format!("\n{agent}: {status} {msg}"));
                }
            }
            out.trim().to_string()
        }
        _ => String::new(),
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
    let states = result.get("readStates");
    result
        .get("data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|t| {
                    let id = t.get("id")?.as_str()?.to_string();
                    let read = states.and_then(|s| s.get(&id));
                    let unread = read.and_then(|r| r.get("firstUnread")).map(|v| !v.is_null()).unwrap_or(false);
                    let revision = read.and_then(|r| r.get("revision")).and_then(Value::as_str).unwrap_or("").to_string();
                    let pinned = t.pointer("/section/name").and_then(Value::as_str) == Some("Pinned");
                    let flags = t.pointer("/status/activeFlags").and_then(Value::as_array);
                    let waiting = flags
                        .map(|f| f.iter().filter_map(Value::as_str).any(|x| x.starts_with("waitingOn")))
                        .unwrap_or(false);
                    let status = match t.pointer("/status/type").and_then(Value::as_str) {
                        Some("active") if waiting => 4,
                        Some("active") => 1,
                        Some("systemError") => 3,
                        _ => 0,
                    };
                    Some(ThreadInfo {
                        title: thread_title(t),
                        cwd: t.get("cwd").and_then(Value::as_str).unwrap_or("").to_string(),
                        id,
                        pinned,
                        unread,
                        revision,
                        status,
                        parent: t.get("parentThreadId").and_then(Value::as_str).unwrap_or("").to_string(),
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
        let turn_id = turn.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        for (item_index, item) in turn.get("items").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().enumerate() {
            let item_index = item_index as i32;
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
                    out.push(Msg { id, role: "user".into(), text, detail: String::new(), turn_id: turn_id.clone(), item_index });
                }
                Some("agentMessage") => {
                    let text = item.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                    out.push(Msg { id, role: "assistant".into(), text, detail: String::new(), turn_id: turn_id.clone(), item_index });
                }
                _ => {
                    if let Some(text) = tool_text(&item) {
                        out.push(Msg { id, role: "tool".into(), text, detail: tool_detail(&item), turn_id: turn_id.clone(), item_index });
                    }
                }
            }
        }
    }
    out
}

/// The id of the turn of a thread (thread/resume, thread/read) that is still in progress.
pub(super) fn running_turn(thread: &Value) -> Option<String> {
    thread
        .get("turns")
        .and_then(Value::as_array)?
        .iter()
        .rev()
        .find(|t| t.get("status").and_then(Value::as_str) == Some("inProgress"))
        .and_then(|t| t.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

pub(super) fn thread_start_params(s: &Settings) -> Value {
    let mut p = json!({});
    let (policy, sandbox, reviewer) = s.permission(false);
    if let Some(v) = policy {
        p["approvalPolicy"] = json!(v);
    }
    if let Some(v) = sandbox {
        p["sandbox"] = json!(v);
    }
    if let Some(v) = reviewer {
        p["approvalsReviewer"] = json!(v);
    }
    if let Some(m) = &s.model {
        p["model"] = json!(m);
        if let Some(pr) = &s.model_provider {
            p["modelProvider"] = json!(pr);
        }
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

pub(super) fn sandbox_policy(mode: &str) -> Option<Value> {
    match mode {
        "read-only" => Some(json!({ "type": "readOnly" })),
        "danger-full-access" => Some(json!({ "type": "dangerFullAccess" })),
        "workspace-write" => Some(json!({ "type": "workspaceWrite" })),
        _ => None,
    }
}

/// Per-turn fields. Model, effort and speed go with every turn (as in the desktop
/// client); permissions only when thread/settings/update is unavailable.
pub(super) fn turn_overrides(s: &Settings, with_permissions: bool, params: &mut Value) {
    if with_permissions {
        let (policy, sandbox, reviewer) = s.permission(true);
        if let Some(v) = policy {
            params["approvalPolicy"] = json!(v);
        }
        if let Some(v) = sandbox.as_deref().and_then(sandbox_policy) {
            params["sandboxPolicy"] = v;
        }
        if let Some(v) = reviewer {
            params["approvalsReviewer"] = json!(v);
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
                    let provider = m.get("modelProvider").and_then(Value::as_str).filter(|p| !p.is_empty()).unwrap_or("openai").to_string();
                    Some(ModelInfo {
                        id,
                        provider,
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

pub fn window_label(mins: Option<i64>) -> String {
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
    if method == "mcpServer/elicitation/request" {
        let action = match decision {
            Decision::Accept | Decision::AcceptForSession => "accept",
            Decision::Decline => "decline",
            Decision::Cancel => "cancel",
        };
        return json!({ "action": action, "content": null, "_meta": null });
    }
    json!({ "decision": decision.wire() })
}

