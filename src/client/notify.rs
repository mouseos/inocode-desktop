//! Server requests (approvals) and notification handling.

use serde_json::{json, Value};
use super::*;

impl<F: Fn(Event)> Session<'_, F> {
    pub(super) fn server_request(&mut self, id: Value, method: &str, params: Value) -> Result<(), String> {
        let (title, detail) = match method {
            "item/commandExecution/requestApproval" => {
                let mut detail = String::new();
                if let Some(c) = params.get("command").and_then(Value::as_str) {
                    detail.push_str(c);
                }
                if let Some(d) = params.get("cwd").and_then(Value::as_str) {
                    detail.push_str(&format!("\n作業フォルダ: {d}"));
                }
                if let Some(r) = params.get("reason").and_then(Value::as_str) {
                    detail.push_str(&format!("\n理由: {r}"));
                }
                ("コマンドの実行を許可しますか？".to_string(), detail)
            }
            "item/fileChange/requestApproval" => {
                let mut detail = String::new();
                if let Some(r) = params.get("reason").and_then(Value::as_str) {
                    detail.push_str(r);
                }
                if let Some(g) = params.get("grantRoot").and_then(Value::as_str) {
                    detail.push_str(&format!("\n対象フォルダ: {g}"));
                }
                ("ファイルの変更を許可しますか？".to_string(), detail)
            }
            "item/permissions/requestApproval" => {
                let detail = params.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
                ("追加の権限を許可しますか？".to_string(), detail)
            }
            "mcpServer/elicitation/request" => {
                let server = params.get("serverName").and_then(Value::as_str).unwrap_or("MCP").to_string();
                let mut detail = params.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                if let Some(u) = params.get("url").and_then(Value::as_str) {
                    detail.push_str(&format!("\n{u}"));
                }
                let key = id.to_string();
                self.approvals.insert(key.clone(), (id, method.to_string(), params));
                (self.on_event)(Event::Elicit { key, title: format!("{server} が入力を求めています"), detail: detail.trim().to_string() });
                return Ok(());
            }
            "item/tool/requestUserInput" => {
                let items: Vec<QuestionInfo> = params
                    .get("questions")
                    .and_then(Value::as_array)
                    .map(|qs| {
                        qs.iter()
                            .map(|q| QuestionInfo {
                                id: q.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                                header: q.get("header").and_then(Value::as_str).unwrap_or("").to_string(),
                                question: q.get("question").and_then(Value::as_str).unwrap_or("").to_string(),
                                options: q
                                    .get("options")
                                    .and_then(Value::as_array)
                                    .map(|o| o.iter().filter_map(|x| x.get("label").and_then(Value::as_str).map(str::to_string)).collect())
                                    .unwrap_or_default(),
                                secret: q.get("isSecret").and_then(Value::as_bool).unwrap_or(false),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let key = id.to_string();
                self.approvals.insert(key.clone(), (id, method.to_string(), params));
                (self.on_event)(Event::Questions { key, items });
                return Ok(());
            }
            _ => {
                return self.conn.send(json!({
                    "id": id,
                    "error": { "code": -32601, "message": "unsupported by InoCode Desktop" }
                }));
            }
        };
        let key = id.to_string();
        self.approvals.insert(key.clone(), (id, method.to_string(), params));
        (self.on_event)(Event::Approval { key, title, detail: detail.trim().to_string() });
        Ok(())
    }

    pub(super) fn notification(&mut self, method: &str, params: &Value) {
        match method {
            "turn/started" => {
                self.turn_id = params.pointer("/turn/id").and_then(Value::as_str).map(str::to_string);
                (self.on_event)(Event::TurnStarted);
            }
            "item/started" => {
                let item = params.get("item").unwrap_or(&Value::Null);
                match item.get("type").and_then(Value::as_str) {
                    Some("agentMessage") => {
                        self.flush();
                        self.msg_streamed = false;
                        (self.on_event)(Event::AssistantStart);
                    }
                    _ => {
                        if let (Some(id), Some(text)) = (item_id(item), tool_text(item)) {
                            (self.on_event)(Event::Tool { id, text, detail: tool_detail(item) });
                        }
                    }
                }
            }
            "item/commandExecution/outputDelta" | "item/reasoning/summaryTextDelta" | "item/plan/delta" => {
                let (id, delta) = (
                    params.get("itemId").and_then(Value::as_str),
                    params.get("delta").and_then(Value::as_str),
                );
                if let (Some(id), Some(delta)) = (id, delta) {
                    (self.on_event)(Event::ToolDelta { id: id.to_string(), delta: delta.to_string() });
                }
            }
            "turn/diff/updated" => {
                let diff = params.get("diff").and_then(Value::as_str).unwrap_or("");
                let files = diff.lines().filter(|l| l.starts_with("diff --git ")).count();
                if files > 0 {
                    (self.on_event)(Event::Note {
                        id: "turn-diff".into(),
                        text: format!("{files} 件のファイルを編集しました"),
                        detail: diff.to_string(),
                    });
                }
            }
            "turn/plan/updated" => {
                let steps = params.get("plan").and_then(Value::as_array).cloned().unwrap_or_default();
                let done = steps.iter().filter(|s| s.get("status").and_then(Value::as_str) == Some("completed")).count();
                let mut detail = params.get("explanation").and_then(Value::as_str).unwrap_or("").to_string();
                for s in &steps {
                    let mark = match s.get("status").and_then(Value::as_str) {
                        Some("completed") => "完了",
                        Some("inProgress") => "進行中",
                        _ => "未着手",
                    };
                    detail.push_str(&format!("\n[{mark}] {}", s.get("step").and_then(Value::as_str).unwrap_or("")));
                }
                (self.on_event)(Event::Note {
                    id: "turn-plan".into(),
                    text: format!("{done}/{} 件のタスク", steps.len()),
                    detail: detail.trim().to_string(),
                });
            }
            "warning" => {
                let message = params.get("message").and_then(Value::as_str).unwrap_or("");
                if !message.is_empty() {
                    (self.on_event)(Event::Note { id: format!("warning:{message}"), text: format!("警告: {message}"), detail: String::new() });
                }
            }
            "item/agentMessage/delta" => {
                if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                    self.msg_streamed = true;
                    self.delta.push_str(delta);
                }
            }
            "item/completed" => {
                let item = params.get("item").unwrap_or(&Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                    // A message that never streamed (no deltas) still has to show up.
                    if !self.msg_streamed {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            self.delta.push_str(text);
                            self.flush();
                        }
                    }
                } else if let (Some(id), Some(text)) = (item_id(item), tool_text(item)) {
                    self.flush();
                    (self.on_event)(Event::Tool { id, text, detail: tool_detail(item) });
                }
            }
            "turn/completed" => {
                self.flush();
                let interrupted = params.pointer("/turn/status").and_then(Value::as_str) == Some("interrupted");
                self.turn_id = None;
                (self.on_event)(Event::TurnDone { interrupted });
                (self.on_event)(Event::Status("接続済み".into(), Level::Ok));
                let _ = self.list_threads();
            }
            "thread/tokenUsage/updated" => {
                let total = params.pointer("/tokenUsage/total/totalTokens").and_then(Value::as_i64).unwrap_or(0);
                let last = params.pointer("/tokenUsage/last/totalTokens").and_then(Value::as_i64).unwrap_or(0);
                let window = params.pointer("/tokenUsage/modelContextWindow").and_then(Value::as_i64).unwrap_or(0);
                let percent = if window > 0 { ((last * 100) / window).clamp(0, 100) as i32 } else { 0 };
                let text = if window > 0 {
                    format!("{last} / {window} トークン (累計 {total})")
                } else {
                    format!("累計 {total} トークン")
                };
                (self.on_event)(Event::Usage { percent, text });
            }
            "account/rateLimits/updated" => {
                if let Some(text) = rate_limit_text(params.get("rateLimits").unwrap_or(&Value::Null)) {
                    (self.on_event)(Event::RateLimits(text));
                }
            }
            "account/updated" | "account/login/completed" => {
                self.login_pending = false;
                let _ = self.refresh_models();
                let _ = self.request("account/read", json!({ "refreshToken": false }), Pending::Account);
                let _ = self.request("account/rateLimits/read", Value::Null, Pending::RateLimits);
            }
            "serverRequest/resolved" => {
                if let Some(rid) = params.get("requestId") {
                    let key = rid.to_string();
                    if self.approvals.remove(&key).is_some() {
                        (self.on_event)(Event::ApprovalResolved(key));
                    }
                }
            }
            "error" => {
                let message = params.pointer("/error/message").and_then(Value::as_str).unwrap_or("error");
                let will_retry = params.get("willRetry").and_then(Value::as_bool).unwrap_or(false);
                if will_retry {
                    (self.on_event)(Event::Status(message.to_string(), Level::Warn));
                } else {
                    self.report_error(message);
                    (self.on_event)(Event::TurnDone { interrupted: false });
                }
            }
            // Session list changes (name, status, read state, archive, section) reach the sidebar.
            m if m.starts_with("inocode/") || m.starts_with("thread/") => {
                (self.on_event)(Event::Notify { method: m.to_string(), params: params.clone() });
            }
            _ => {}
        }
    }
}

