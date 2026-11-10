//! Client for the InoCode app-server over WebSocket or stdio.
//!
//! The wire format is JSON-RPC without the "jsonrpc" field. One background thread
//! owns the connection: it performs initialize / initialized, lists threads, starts
//! or resumes one, and then serves commands from the UI (prompt, interrupt,
//! approvals, thread switching). Streaming deltas are coalesced to about one
//! flush per frame. If the connection drops it reconnects with a growing delay and
//! resumes the thread that was open.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Child, ChildStdin, Command as Proc, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::{client, Message as WsMessage, WebSocket};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Info,
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub id: String,
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadInfo {
    pub id: String,
    pub title: String,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Status(String, Level),
    Threads(Vec<ThreadInfo>),
    ThreadActive { id: String, title: String },
    /// Replace the conversation (new or resumed thread).
    History(Vec<Msg>),
    AssistantStart,
    Delta(String),
    /// Insert or update a command / tool row.
    Tool { id: String, text: String },
    TurnStarted,
    TurnDone { interrupted: bool },
    Approval { key: String, title: String, detail: String },
    ApprovalResolved(String),
    Error(String),
    /// Which transport is in use ("WS" or "stdio").
    Transport(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

impl Decision {
    pub fn parse(s: &str) -> Decision {
        match s {
            "accept" => Decision::Accept,
            "session" => Decision::AcceptForSession,
            "cancel" => Decision::Cancel,
            _ => Decision::Decline,
        }
    }
    fn wire(self) -> &'static str {
        match self {
            Decision::Accept => "accept",
            Decision::AcceptForSession => "acceptForSession",
            Decision::Decline => "decline",
            Decision::Cancel => "cancel",
        }
    }
}

pub enum Command {
    Prompt(String),
    NewThread,
    OpenThread(String),
    Interrupt,
    Approve { key: String, decision: Decision },
    SwitchTransport,
}

pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

#[derive(Clone)]
pub struct Config {
    pub ws_url: String,
    pub stdio_program: String,
    pub use_stdio: bool,
}

const FLUSH_INTERVAL: Duration = Duration::from_millis(16);
const POLL: Duration = Duration::from_millis(8);

pub fn spawn(cfg: Config, on_event: impl Fn(Event) + Send + 'static) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || supervise(cfg, rx, on_event));
    Handle { tx }
}

// ---------------------------------------------------------------- transport

enum Conn {
    Ws(Box<WebSocket<TcpStream>>),
    Stdio {
        child: Child,
        stdin: ChildStdin,
        lines: Receiver<Result<String, String>>,
    },
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Conn::Stdio { child, .. } = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Conn {
    fn open(cfg: &Config) -> Result<Conn, String> {
        if cfg.use_stdio {
            open_stdio(&cfg.stdio_program)
        } else {
            open_ws(&cfg.ws_url)
        }
    }

    fn send(&mut self, value: Value) -> Result<(), String> {
        match self {
            Conn::Ws(ws) => ws.send(WsMessage::text(value.to_string())).map_err(|e| e.to_string()),
            Conn::Stdio { stdin, .. } => {
                let mut line = value.to_string();
                line.push('\n');
                stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).map_err(|e| e.to_string())
            }
        }
    }

    /// Next text frame / line, or None when nothing arrived within the poll interval.
    fn recv(&mut self) -> Result<Option<String>, String> {
        match self {
            Conn::Ws(ws) => match ws.read() {
                Ok(WsMessage::Text(t)) => Ok(Some(t.as_str().to_string())),
                Ok(WsMessage::Close(_)) => Err("接続が閉じられました".into()),
                Ok(_) => Ok(None),
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    Ok(None)
                }
                Err(e) => Err(e.to_string()),
            },
            Conn::Stdio { lines, .. } => match lines.recv_timeout(POLL) {
                Ok(Ok(line)) => Ok(Some(line)),
                Ok(Err(e)) => Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err("app-server が終了しました".into()),
            },
        }
    }
}

fn open_ws(url: &str) -> Result<Conn, String> {
    let request = url.into_client_request().map_err(|e| e.to_string())?;
    let uri = request.uri().clone();
    let host = uri.host().ok_or("URL にホストがありません")?.to_string();
    let port = uri.port_u16().unwrap_or(80);
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("アドレスを解決できません")?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    let _ = stream.set_nodelay(true);
    let (ws, _) = client(request, stream).map_err(|e| e.to_string())?;
    ws.get_ref().set_read_timeout(Some(POLL)).map_err(|e| e.to_string())?;
    Ok(Conn::Ws(Box::new(ws)))
}

fn open_stdio(program: &str) -> Result<Conn, String> {
    let mut cmd = Proc::new(program);
    cmd.args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("{program} を起動できません: {e}"))?;
    let stdin = child.stdin.take().ok_or("stdin を取得できません")?;
    let stdout = child.stdout.take().ok_or("stdout を取得できません")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) if l.trim().is_empty() => {}
                Ok(l) => {
                    if tx.send(Ok(l)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
            }
        }
        let _ = tx.send(Err("app-server が終了しました".into()));
    });
    Ok(Conn::Stdio { child, stdin, lines: rx })
}

// --------------------------------------------------------------- supervisor

enum End {
    Quit,
    Switch,
    Lost(String),
}

fn transport_name(cfg: &Config) -> String {
    if cfg.use_stdio { "stdio" } else { "WS" }.to_string()
}

fn supervise(mut cfg: Config, rx: Receiver<Command>, on_event: impl Fn(Event)) {
    let mut active_thread: Option<String> = None;
    let mut attempt: u32 = 0;
    loop {
        on_event(Event::Transport(transport_name(&cfg)));
        let status = if attempt == 0 { "接続中…".to_string() } else { format!("再接続中 ({attempt})…") };
        on_event(Event::Status(status, Level::Info));

        let end = match Conn::open(&cfg) {
            Ok(conn) => {
                let mut session = Session::new(conn, &mut active_thread, &on_event);
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
                        Ok(Command::Prompt(_)) => on_event(Event::Error("未接続のため送信できません".into())),
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
enum Pending {
    Init,
    ThreadList,
    ThreadStart,
    ThreadResume,
    TurnStart,
    Other,
}

struct Session<'a, F: Fn(Event)> {
    conn: Conn,
    on_event: &'a F,
    active_thread: &'a mut Option<String>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    turn_id: Option<String>,
    delta: String,
    last_flush: Instant,
    approvals: HashMap<String, (Value, String, Value)>,
    msg_streamed: bool,
    ready: bool,
}

impl<'a, F: Fn(Event)> Session<'a, F> {
    fn new(conn: Conn, active_thread: &'a mut Option<String>, on_event: &'a F) -> Self {
        Session {
            conn,
            on_event,
            active_thread,
            next_id: 1,
            pending: HashMap::new(),
            turn_id: None,
            delta: String::new(),
            last_flush: Instant::now(),
            approvals: HashMap::new(),
            msg_streamed: false,
            ready: false,
        }
    }

    fn request(&mut self, method: &str, params: Value, kind: Pending) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, kind);
        self.conn.send(json!({ "id": id, "method": method, "params": params }))
    }

    fn flush(&mut self) {
        if !self.delta.is_empty() {
            (self.on_event)(Event::Delta(std::mem::take(&mut self.delta)));
        }
        self.last_flush = Instant::now();
    }

    fn run(&mut self, rx: &Receiver<Command>, cfg: &mut Config) -> End {
        match self.run_inner(rx, cfg) {
            Ok(end) => end,
            Err(e) => End::Lost(e),
        }
    }

    fn run_inner(&mut self, rx: &Receiver<Command>, cfg: &mut Config) -> Result<End, String> {
        self.request(
            "initialize",
            json!({ "clientInfo": {
                "name": "inocode_desktop",
                "title": "InoCode Desktop",
                "version": env!("CARGO_PKG_VERSION"),
            } }),
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
        }
    }

    fn command(&mut self, cmd: Command, cfg: &mut Config) -> Result<Option<End>, String> {
        match cmd {
            Command::SwitchTransport => {
                cfg.use_stdio = !cfg.use_stdio;
                return Ok(Some(End::Switch));
            }
            Command::Prompt(text) => {
                let Some(tid) = self.active_thread.clone() else {
                    (self.on_event)(Event::Error("スレッドがまだ開始されていません".into()));
                    return Ok(None);
                };
                self.request(
                    "turn/start",
                    json!({ "threadId": tid, "input": [{ "type": "text", "text": text }] }),
                    Pending::TurnStart,
                )?;
            }
            Command::NewThread => {
                self.request("thread/start", json!({}), Pending::ThreadStart)?;
            }
            Command::OpenThread(id) => {
                self.request("thread/resume", json!({ "threadId": id }), Pending::ThreadResume)?;
            }
            Command::Interrupt => {
                if let (Some(tid), Some(turn)) = (self.active_thread.clone(), self.turn_id.clone()) {
                    self.request("turn/interrupt", json!({ "threadId": tid, "turnId": turn }), Pending::Other)?;
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

    fn list_threads(&mut self) -> Result<(), String> {
        self.request("thread/list", json!({ "limit": 50 }), Pending::ThreadList)
    }

    fn handle(&mut self, value: &Value) -> Result<(), String> {
        let id = value.get("id");
        let method = value.get("method").and_then(Value::as_str);

        // Request from the server.
        if let (Some(id), Some(method)) = (id, method) {
            return self.server_request(id.clone(), method, value.get("params").cloned().unwrap_or(Value::Null));
        }
        // Response to one of ours.
        if let Some(id) = id.and_then(Value::as_u64) {
            let kind = self.pending.remove(&id).unwrap_or(Pending::Other);
            return self.response(kind, value);
        }
        if let Some(method) = method {
            self.notification(method, value.get("params").unwrap_or(&Value::Null));
        }
        Ok(())
    }

    fn response(&mut self, kind: Pending, value: &Value) -> Result<(), String> {
        if let Some(err) = value.get("error") {
            let message = err.get("message").and_then(Value::as_str).unwrap_or("error").to_string();
            match kind {
                // Listing is optional; an older server may not know the method.
                Pending::ThreadList | Pending::Other => {}
                Pending::Init => return Err(format!("initialize が失敗しました: {message}")),
                _ => self.report_error(&message),
            }
            return Ok(());
        }
        let result = value.get("result").unwrap_or(&Value::Null);
        match kind {
            Pending::Init => {
                self.conn.send(json!({ "method": "initialized" }))?;
                self.ready = true;
                self.list_threads()?;
                match self.active_thread.clone() {
                    Some(tid) => {
                        self.request("thread/resume", json!({ "threadId": tid }), Pending::ThreadResume)?
                    }
                    None => self.request("thread/start", json!({}), Pending::ThreadStart)?,
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
                (self.on_event)(Event::History(history_from_thread(thread)));
                (self.on_event)(Event::Status("接続済み".into(), Level::Ok));
            }
            Pending::TurnStart | Pending::Other => {}
        }
        Ok(())
    }

    fn report_error(&mut self, message: &str) {
        if is_auth_error(message) {
            (self.on_event)(Event::Status("認証が必要です (401)".into(), Level::Error));
        }
        (self.on_event)(Event::Error(message.to_string()));
    }

    fn server_request(&mut self, id: Value, method: &str, params: Value) -> Result<(), String> {
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

    fn notification(&mut self, method: &str, params: &Value) {
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
                            (self.on_event)(Event::Tool { id, text });
                        }
                    }
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
                    (self.on_event)(Event::Tool { id, text });
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
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ helpers

fn is_auth_error(message: &str) -> bool {
    message.contains("401") || message.to_ascii_lowercase().contains("unauthorized")
}

fn item_id(item: &Value) -> Option<String> {
    item.get("id").and_then(Value::as_str).map(str::to_string)
}

fn status_label(status: &str) -> &'static str {
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

fn thread_title(thread: &Value) -> String {
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

fn approval_result(method: &str, params: &Value, decision: Decision) -> Value {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_item_text() {
        let item = json!({"type":"commandExecution","id":"a","command":"dir","status":"completed","exitCode":2});
        assert_eq!(tool_text(&item).unwrap(), "実行: dir — 完了 (終了コード 2)");
        assert!(tool_text(&json!({"type":"reasoning"})).is_none());
    }

    #[test]
    fn threads_and_history() {
        let list = json!({"data":[{"id":"t1","preview":"hello\nworld","cwd":"D:\\x\\proj"}]});
        let t = parse_threads(&list);
        assert_eq!(t[0].title, "hello");
        let thread = json!({"turns":[{"items":[
            {"type":"userMessage","id":"u","content":[{"type":"text","text":"hi"}]},
            {"type":"fileChange","id":"f","changes":[{},{}],"status":"completed"},
            {"type":"agentMessage","id":"m","text":"ok"}
        ]}]});
        let h = history_from_thread(&thread);
        assert_eq!(h.len(), 3);
        assert_eq!(h[1].text, "ファイル変更 2 件 — 完了");
    }

    #[test]
    fn approval_wire() {
        let r = approval_result("item/commandExecution/requestApproval", &Value::Null, Decision::AcceptForSession);
        assert_eq!(r["decision"], "acceptForSession");
        assert!(is_auth_error("unexpected status 401 Unauthorized"));
    }

    /// Live check against a running app-server (WS_URL, or INOCODE_BIN for stdio).
    /// Run with: cargo test live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live() {
        let cfg = Config {
            ws_url: std::env::var("INOCODE_WS_URL").unwrap_or_else(|_| "ws://127.0.0.1:18802".into()),
            stdio_program: std::env::var("INOCODE_BIN").unwrap_or_else(|_| "inocode".into()),
            use_stdio: std::env::var("INOCODE_STDIO").map(|v| v == "1").unwrap_or(false),
        };
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let h = spawn(cfg, move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        let mut sent = false;
        let mut text = String::new();
        let deadline = Instant::now() + Duration::from_secs(40);
        while Instant::now() < deadline {
            let Ok(e) = rx.recv_timeout(Duration::from_millis(200)) else { continue };
            println!("{e:?}");
            match e {
                Event::Status(s, Level::Ok) if !sent && s == "接続済み" => {
                    sent = true;
                    h.send(Command::Prompt("hello".into()));
                }
                Event::Delta(d) => text.push_str(&d),
                Event::TurnDone { .. } => break,
                _ => {}
            }
        }
        println!("TEXT={text}");
        assert!(sent);
    }
}
