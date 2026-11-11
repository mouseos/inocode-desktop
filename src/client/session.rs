//! The supervisor (reconnect loop) and the session state machine.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use super::*;

// --------------------------------------------------------------- supervisor

pub(super) enum End {
    Quit,
    Switch,
    Lost(String),
}

pub(super) fn transport_name(cfg: &Config) -> String {
    if cfg.use_stdio { "stdio" } else { "WS" }.to_string()
}

pub(super) fn supervise(mut cfg: Config, rx: Receiver<Command>, on_event: impl Fn(Event)) {
    let mut active_thread: Option<String> = None;
    let mut settings = Settings::default();
    let mut attempt: u32 = 0;
    loop {
        on_event(Event::Transport(transport_name(&cfg)));
        let status = if attempt == 0 { "接続中…".to_string() } else { format!("再接続中 ({attempt})…") };
        on_event(Event::Status(status, Level::Info));

        let end = match Conn::open(&cfg) {
            Ok(conn) => {
                let mut session = Session::new(conn, &mut active_thread, &mut settings, &on_event);
                let end = session.run(&rx, &mut cfg);
                if session.ready {
                    attempt = 0;
                }
                end
            }
            Err(e) => End::Lost(e),
        };
        match end {
            End::Quit => return,
            End::Switch => {
                attempt = 0;
                continue;
            }
            End::Lost(reason) => {
                attempt += 1;
                let delay = Duration::from_secs((1u64 << attempt.min(4)).min(10));
                on_event(Event::Status(
                    format!("未接続: {reason} ({}秒後に再接続)", delay.as_secs()),
                    Level::Error,
                ));
                on_event(Event::TurnDone { interrupted: false });
                let deadline = Instant::now() + delay;
                while Instant::now() < deadline {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(Command::SwitchTransport) => {
                            cfg.use_stdio = !cfg.use_stdio;
                            attempt = 0;
                            break;
                        }
                        Ok(Command::Quit) => return,
                        Ok(Command::Prompt(_)) => on_event(Event::Error("未接続のため送信できません".into())),
                        Ok(Command::Rpc { tag, method, .. }) => {
                            if !mock_rpc(&on_event, &tag, &method) {
                                on_event(Event::Rpc { tag, result: Err("未接続です".into()) });
                            }
                        }
                        Ok(_) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ session

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Pending {
    Init,
    ThreadList,
    ThreadStart,
    ThreadResume,
    TurnStart,
    Models,
    Requirements,
    SettingsUpdate,
    Daybreak,
    Account,
    RateLimits,
    Login,
    Other,
}

pub(super) struct Session<'a, F: Fn(Event)> {
    pub(super) conn: Conn,
    pub(super) on_event: &'a F,
    pub(super) active_thread: &'a mut Option<String>,
    pub(super) settings: &'a mut Settings,
    /// Settings already pushed to the thread (only changes are sent).
    pub(super) sent: Settings,
    pub(super) settings_update_unsupported: bool,
    pub(super) next_id: u64,
    pub(super) pending: HashMap<u64, Pending>,
    /// Tags of Command::Rpc requests in flight, by request id.
    pub(super) rpc_tags: HashMap<u64, String>,
    pub(super) turn_id: Option<String>,
    pub(super) delta: String,
    pub(super) last_flush: Instant,
    pub(super) approvals: HashMap<String, (Value, String, Value)>,
    pub(super) msg_streamed: bool,
    pub(super) login_pending: bool,
    /// When model/list was last requested (the desktop client treats it as stale after 5 minutes).
    pub(super) models_asked: Instant,
    pub(super) ready: bool,
}

impl<'a, F: Fn(Event)> Session<'a, F> {
    pub(super) fn new(
        conn: Conn,
        active_thread: &'a mut Option<String>,
        settings: &'a mut Settings,
        on_event: &'a F,
    ) -> Self {
        Session {
            conn,
            on_event,
            active_thread,
            sent: settings.clone(),
            settings,
            settings_update_unsupported: false,
            next_id: 1,
            pending: HashMap::new(),
            rpc_tags: HashMap::new(),
            turn_id: None,
            delta: String::new(),
            last_flush: Instant::now(),
            approvals: HashMap::new(),
            msg_streamed: false,
            login_pending: false,
            models_asked: Instant::now(),
            ready: false,
        }
    }

    pub(super) fn request(&mut self, method: &str, params: Value, kind: Pending) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, kind);
        // Like the desktop client, leave "params" out when there are none.
        if params.is_null() {
            self.conn.send(json!({ "id": id, "method": method }))
        } else {
            self.conn.send(json!({ "id": id, "method": method, "params": params }))
        }
    }

    pub(super) fn flush(&mut self) {
        if !self.delta.is_empty() {
            (self.on_event)(Event::Delta(std::mem::take(&mut self.delta)));
        }
        self.last_flush = Instant::now();
    }

    pub(super) fn run(&mut self, rx: &Receiver<Command>, cfg: &mut Config) -> End {
        match self.run_inner(rx, cfg) {
            Ok(end) => end,
            Err(e) => End::Lost(e),
        }
    }

    pub(super) fn run_inner(&mut self, rx: &Receiver<Command>, cfg: &mut Config) -> Result<End, String> {
        self.request(
            "initialize",
            json!({ "clientInfo": {
                "name": "inocode_desktop",
                "title": "InoCode Desktop",
                "version": env!("CARGO_PKG_VERSION"),
            }, "capabilities": { "experimentalApi": true } }),
            Pending::Init,
        )?;

        loop {
            loop {
                match rx.try_recv() {
                    Ok(cmd) => {
                        if let Some(end) = self.command(cmd, cfg)? {
                            return Ok(end);
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(End::Quit),
                }
            }

            if let Some(text) = self.conn.recv()? {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    self.handle(&value)?;
                }
            }

            if !self.delta.is_empty() && self.last_flush.elapsed() >= FLUSH_INTERVAL {
                self.flush();
            }

            // Newly released models show up without a restart: ask again when stale.
            if self.ready && self.models_asked.elapsed() >= MODELS_STALE {
                self.refresh_models()?;
            }
        }
    }

    pub(super) fn command(&mut self, cmd: Command, cfg: &mut Config) -> Result<Option<End>, String> {
        match cmd {
            Command::SwitchTransport => {
                cfg.use_stdio = !cfg.use_stdio;
                return Ok(Some(End::Switch));
            }
            Command::Quit => return Ok(Some(End::Quit)),
            Command::Rpc { tag, method, params } => {
                if mock_rpc(self.on_event, &tag, &method) {
                    return Ok(None);
                }
                let id = self.next_id;
                self.rpc_tags.insert(id, tag);
                self.request(&method, params, Pending::Other)?;
            }
            Command::Prompt(text) => {
                let Some(tid) = self.active_thread.clone() else {
                    (self.on_event)(Event::Error("スレッドがまだ開始されていません".into()));
                    return Ok(None);
                };
                let mut params = json!({ "threadId": tid, "input": [{ "type": "text", "text": text, "text_elements": [] }] });
                turn_overrides(self.settings, self.settings_update_unsupported, &mut params);
                self.request("turn/start", params, Pending::TurnStart)?;
            }
            Command::NewThread => {
                let params = thread_start_params(self.settings);
                self.request("thread/start", params, Pending::ThreadStart)?;
            }
            Command::NewThreadIn(cwd) => {
                self.settings.cwd = Some(cwd);
                let params = thread_start_params(self.settings);
                self.request("thread/start", params, Pending::ThreadStart)?;
            }
            Command::SetSettings(s) => {
                *self.settings = s;
                self.push_settings()?;
            }
            Command::SetDaybreak(on) => {
                self.settings.daybreak = on;
                if let Some(tid) = self.active_thread.clone() {
                    self.request(
                        "thread/metadata/update",
                        json!({ "threadId": tid, "daybreakEnabled": on }),
                        Pending::Daybreak,
                    )?;
                }
            }
            Command::Login => {
                // One ChatGPT login at a time, as in the desktop client.
                if !self.login_pending {
                    self.login_pending = true;
                    self.request("account/login/start", json!({ "type": "chatgpt" }), Pending::Login)?;
                }
            }
            Command::OpenThread(id) => {
                self.request("thread/resume", json!({ "threadId": id }), Pending::ThreadResume)?;
            }
            Command::Interrupt => {
                if let (Some(tid), Some(turn)) = (self.active_thread.clone(), self.turn_id.clone()) {
                    self.request("turn/interrupt", json!({ "threadId": tid, "turnId": turn }), Pending::Other)?;
                }
            }
            Command::Steer(text) => match (self.active_thread.clone(), self.turn_id.clone()) {
                (Some(tid), Some(turn)) => {
                    let input = json!([{ "type": "text", "text": text, "text_elements": [] }]);
                    self.request(
                        "turn/steer",
                        json!({ "threadId": tid, "expectedTurnId": turn, "input": input }),
                        Pending::Other,
                    )?;
                }
                _ => (self.on_event)(Event::Error("実行中のターンがありません".into())),
            },
            Command::Answer { key, answers } => {
                if let Some((id, _, _)) = self.approvals.remove(&key) {
                    let mut map = serde_json::Map::new();
                    for (qid, list) in answers {
                        map.insert(qid, json!({ "answers": list }));
                    }
                    self.conn.send(json!({ "id": id, "result": { "answers": map } }))?;
                    (self.on_event)(Event::ApprovalResolved(key));
                }
            }
            Command::Approve { key, decision } => {
                if let Some((id, method, params)) = self.approvals.remove(&key) {
                    let result = approval_result(&method, &params, decision);
                    self.conn.send(json!({ "id": id, "result": result }))?;
                    (self.on_event)(Event::ApprovalResolved(key));
                }
            }
        }
        Ok(None)
    }

    /// Push changed choices to the thread (thread/settings/update), as the desktop client does.
    pub(super) fn push_settings(&mut self) -> Result<(), String> {
        let Some(tid) = self.active_thread.clone() else { return Ok(()) };
        if self.settings_update_unsupported {
            return Ok(());
        }
        let new = self.settings.clone();
        let old = self.sent.clone();
        let mut p = json!({ "threadId": tid });
        let mut any = false;
        if new.model != old.model {
            if let Some(m) = &new.model {
                p["model"] = json!(m);
                any = true;
            }
        }
        if new.effort != old.effort {
            if let Some(e) = &new.effort {
                p["effort"] = json!(e);
                any = true;
            }
        }
        if new.service_tier != old.service_tier {
            if let Some(t) = &new.service_tier {
                p["serviceTier"] = match t {
                    Some(id) => json!(id),
                    None => Value::Null,
                };
                any = true;
            }
        }
        if new.perm != old.perm {
            if let Some(perm) = new.perm {
                if let Some(v) = perm.policy() {
                    p["approvalPolicy"] = json!(v);
                    any = true;
                }
                if let Some(v) = perm.sandbox_mode() {
                    p["sandboxPolicy"] = sandbox_policy(v);
                    any = true;
                }
                if let Some(v) = perm.reviewer(false) {
                    p["approvalsReviewer"] = json!(v);
                    any = true;
                }
            }
        }
        self.sent = new;
        if any {
            self.request("thread/settings/update", p, Pending::SettingsUpdate)?;
        }
        Ok(())
    }

    pub(super) fn refresh_models(&mut self) -> Result<(), String> {
        self.models_asked = Instant::now();
        self.request(
            "model/list",
            json!({ "includeHidden": true, "cursor": null, "limit": 100 }),
            Pending::Models,
        )
    }

    pub(super) fn list_threads(&mut self) -> Result<(), String> {
        self.request("thread/list", json!({ "limit": 50 }), Pending::ThreadList)
    }

    pub(super) fn handle(&mut self, value: &Value) -> Result<(), String> {
        let id = value.get("id");
        let method = value.get("method").and_then(Value::as_str);

        // Request from the server.
        if let (Some(id), Some(method)) = (id, method) {
            return self.server_request(id.clone(), method, value.get("params").cloned().unwrap_or(Value::Null));
        }
        // Response to one of ours.
        if let Some(id) = id.and_then(Value::as_u64) {
            if let Some(tag) = self.rpc_tags.remove(&id) {
                self.pending.remove(&id);
                let result = match value.get("error") {
                    Some(err) => Err(err.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                    None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                };
                (self.on_event)(Event::Rpc { tag, result });
                return Ok(());
            }
            let kind = self.pending.remove(&id).unwrap_or(Pending::Other);
            return self.response(kind, value);
        }
        if let Some(method) = method {
            self.notification(method, value.get("params").unwrap_or(&Value::Null));
        }
        Ok(())
    }

    pub(super) fn response(&mut self, kind: Pending, value: &Value) -> Result<(), String> {
        if let Some(err) = value.get("error") {
            let message = err.get("message").and_then(Value::as_str).unwrap_or("error").to_string();
            match kind {
                // Listing is optional; an older server may not know the method.
                Pending::ThreadList | Pending::Other | Pending::Models | Pending::RateLimits | Pending::Account => {}
                Pending::Init => return Err(format!("initialize が失敗しました: {message}")),
                Pending::Login => {
                    self.login_pending = false;
                    self.report_error(&message);
                }
                Pending::SettingsUpdate => {
                    // Like the desktop client: remember that the server lacks the method and
                    // send the choices with every turn instead.
                    let m = message.to_ascii_lowercase();
                    if m.contains("method not found") || m.contains("unknown variant") || m.contains("unsupported") {
                        self.settings_update_unsupported = true;
                    } else {
                        self.report_error(&message);
                    }
                }
                Pending::Requirements | Pending::Daybreak => {}
                _ => self.report_error(&message),
            }
            return Ok(());
        }
        let result = value.get("result").unwrap_or(&Value::Null);
        match kind {
            Pending::Init => {
                self.conn.send(json!({ "method": "initialized" }))?;
                self.ready = true;
                self.refresh_models()?;
                self.request("account/read", json!({ "refreshToken": false }), Pending::Account)?;
                self.request("account/rateLimits/read", Value::Null, Pending::RateLimits)?;
                self.request("configRequirements/read", Value::Null, Pending::Requirements)?;
                self.list_threads()?;
                match self.active_thread.clone() {
                    Some(tid) => {
                        self.request("thread/resume", json!({ "threadId": tid }), Pending::ThreadResume)?
                    }
                    None => {
                        let params = thread_start_params(self.settings);
                        self.request("thread/start", params, Pending::ThreadStart)?
                    }
                }
            }
            Pending::ThreadList => (self.on_event)(Event::Threads(parse_threads(result))),
            Pending::ThreadStart => {
                let tid = result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .ok_or("thread/start の応答に thread.id がありません")?;
                *self.active_thread = Some(tid.to_string());
                self.turn_id = None;
                (self.on_event)(Event::ThreadActive { id: tid.to_string(), title: String::new() });
                (self.on_event)(applied(result));
                (self.on_event)(Event::History(Vec::new()));
                (self.on_event)(Event::Status("接続済み".into(), Level::Ok));
                self.list_threads()?;
            }
            Pending::ThreadResume => {
                let thread = result.get("thread").unwrap_or(&Value::Null);
                if let Some(tid) = thread.get("id").and_then(Value::as_str) {
                    *self.active_thread = Some(tid.to_string());
                    self.turn_id = None;
                    (self.on_event)(Event::ThreadActive { id: tid.to_string(), title: thread_title(thread) });
                }
                (self.on_event)(applied(result));
                (self.on_event)(Event::History(history_from_thread(thread)));
                (self.on_event)(Event::Status("接続済み".into(), Level::Ok));
            }
            Pending::Models => {
                let list = parse_models(result);
                self.settings.programs = list.clone();
                (self.on_event)(Event::Models(list));
            }
            Pending::Account => {
                let (logged_in, label) = parse_account(result);
                (self.on_event)(Event::Account { logged_in, label });
            }
            Pending::RateLimits => {
                if let Some(text) = rate_limit_text(result.get("rateLimits").unwrap_or(&Value::Null)) {
                    (self.on_event)(Event::RateLimits(text));
                }
            }
            Pending::Requirements => {
                let req = result.get("requirements").unwrap_or(&Value::Null);
                let list = |key: &str| {
                    req.get(key).and_then(Value::as_array).map(|a| {
                        a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()
                    })
                };
                (self.on_event)(Event::Requirements {
                    policies: list("allowedApprovalPolicies"),
                    sandboxes: list("allowedSandboxModes"),
                });
            }
            Pending::SettingsUpdate => {}
            Pending::Daybreak => {
                if let Some(on) = result.pointer("/thread/daybreakEnabled").and_then(Value::as_bool) {
                    self.settings.daybreak = on;
                    (self.on_event)(Event::Daybreak(on));
                }
            }
            Pending::Login => {
                if let Some(url) = result.get("authUrl").and_then(Value::as_str) {
                    open_url(url);
                    (self.on_event)(Event::Status("ブラウザでログインしてください".into(), Level::Warn));
                }
            }
            Pending::TurnStart | Pending::Other => {}
        }
        Ok(())
    }

    pub(super) fn report_error(&mut self, message: &str) {
        if is_auth_error(message) {
            (self.on_event)(Event::Status("認証が必要です (401)".into(), Level::Error));
        }
        (self.on_event)(Event::Error(message.to_string()));
    }

}

