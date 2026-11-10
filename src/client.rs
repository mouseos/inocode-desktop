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
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
    pub hidden: bool,
    pub is_default: bool,
    /// `availableAccessPrograms.cyber` as advertised by model/list for this caller
    /// ("standard", "daybreakBlue", "daybreakRed").
    pub programs: Vec<String>,
    /// Service tiers (id, name, description); the first speed option is always "standard".
    pub service_tiers: Vec<(String, String, String)>,
    pub default_service_tier: Option<String>,
    /// Set for models only found by probing a newer client version: the lowest version
    /// that listed them. Such models are "unverified" in InoCode.
    pub min_version: Option<String>,
}

impl ModelInfo {
    /// The model advertises a Daybreak access program for this caller.
    pub fn daybreak(&self) -> bool {
        self.programs.iter().any(|p| p.starts_with("daybreak"))
    }

    /// Program to request for a turn: the Daybreak program when the choice is on,
    /// "standard" when off, and nothing when the model advertises no programs.
    pub fn cyber_program(&self, daybreak: bool) -> Option<&'static str> {
        let has = |p: &str| self.programs.iter().any(|x| x == p);
        if daybreak {
            if has("daybreakBlue") {
                Some("daybreakBlue")
            } else if has("daybreakRed") {
                Some("daybreakRed")
            } else {
                None
            }
        } else if has("standard") {
            Some("standard")
        } else {
            None
        }
    }

    /// The service tier that the "Fast" row selects (the model's single faster tier).
    pub fn fast_tier(&self) -> Option<&str> {
        match self.service_tiers.as_slice() {
            [(id, _, _)] => Some(id.as_str()),
            _ => None,
        }
    }

    /// The model has a "Daybreak" variant row: a standard mode plus a Daybreak program.
    /// Daybreak-only models (no "standard") are listed as plain rows.
    pub fn has_daybreak_variant(&self) -> bool {
        self.programs.iter().any(|p| p == "standard") && self.daybreak()
    }

    /// Program for a turn: the Daybreak program when the choice is on, "standard" when it is
    /// off, and for a model that only has a Daybreak program, that program.
    pub fn program_for(&self, daybreak: bool) -> Option<&'static str> {
        self.cyber_program(daybreak).or_else(|| {
            if self.programs.iter().any(|p| p == "standard") {
                None
            } else {
                self.cyber_program(true)
            }
        })
    }
}

/// Permission modes of the composer's permission dropdown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Perm {
    /// "Custom (config.toml)": no override, the server's own configuration applies.
    Server,
    /// "Ask for approval"
    Ask,
    /// "Approve for me": an automatic reviewer answers approval requests.
    Guardian,
    /// "Full access"
    Full,
}

impl Perm {
    fn policy(self) -> Option<&'static str> {
        match self {
            Perm::Server => None,
            Perm::Ask | Perm::Guardian => Some("on-request"),
            Perm::Full => Some("never"),
        }
    }
    fn sandbox_mode(self) -> Option<&'static str> {
        match self {
            Perm::Server => None,
            Perm::Ask | Perm::Guardian => Some("workspace-write"),
            Perm::Full => Some("danger-full-access"),
        }
    }
    /// Reviewer as spelled in thread/settings/update and in turn/start.
    fn reviewer(self, turn: bool) -> Option<&'static str> {
        match self {
            Perm::Server => None,
            Perm::Guardian => Some(if turn { "auto_review" } else { "guardian_subagent" }),
            Perm::Ask | Perm::Full => Some("user"),
        }
    }
    /// What the server reports (policy, sandbox type, reviewer) mapped back to a mode.
    pub fn from_server(approval: &str, sandbox: &str, reviewer: &str) -> Perm {
        match (approval, sandbox, reviewer) {
            ("never", "dangerFullAccess", _) => Perm::Full,
            ("on-request", "workspaceWrite", "auto_review") => Perm::Guardian,
            ("on-request", "workspaceWrite", _) => Perm::Ask,
            _ => Perm::Server,
        }
    }
}

/// Thread settings chosen in the UI (the desktop client keeps these as the thread's
/// "latest thread settings" and pushes changes with thread/settings/update).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// None: not chosen. Some(None): Standard speed. Some(Some(id)): that service tier.
    pub service_tier: Option<Option<String>>,
    pub perm: Option<Perm>,
    pub daybreak: bool,
    /// Model catalog from model/list, used to resolve the program sent with each turn.
    pub programs: Vec<ModelInfo>,
    pub cwd: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            model: None,
            effort: None,
            service_tier: None,
            perm: None,
            daybreak: false,
            programs: Vec::new(),
            cwd: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Models(Vec<ModelInfo>),
    Account { logged_in: bool, label: String },
    /// Context usage of the active thread (percent of the window) and a text summary.
    Usage { percent: i32, text: String },
    RateLimits(String),
    /// configRequirements/read: allowed approval policies / sandbox modes (None = unrestricted).
    Requirements { policies: Option<Vec<String>>, sandboxes: Option<Vec<String>> },
    /// thread.daybreakEnabled as stored by the server.
    Daybreak(bool),
    /// What the server reports for the thread that was just started or resumed.
    Applied { model: String, effort: String, approval: String, sandbox: String, reviewer: String, cwd: String },
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
    NewThreadIn(String),
    SetSettings(Settings),
    SetDaybreak(bool),
    Login,
    OpenThread(String),
    Interrupt,
    Approve { key: String, decision: Decision },
    SwitchTransport,
    Quit,
}

pub struct Handle {
    tx: Sender<Command>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Stop the client and wait until the app-server child it started is gone.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(t) = self.thread.lock().ok().and_then(|mut g| g.take()) {
            let _ = t.join();
        }
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
const MODELS_STALE: Duration = Duration::from_secs(300);

pub fn spawn(cfg: Config, on_event: impl Fn(Event) + Send + 'static) -> Handle {
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || supervise(cfg, rx, on_event));
    Handle { tx, thread: std::sync::Mutex::new(Some(thread)) }
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
            open_stdio(&cfg.stdio_program, None)
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

/// Where `inocode.exe` is looked up, in order: `INOCODE_BIN`, PATH, next to this
/// executable, then development build output (`cli/inocode/codex-rs/target-win/dev-small`)
/// relative to the executable and to the working directory.
pub fn find_inocode(configured: &str) -> Result<std::path::PathBuf, String> {
    use std::path::{Path, PathBuf};
    let name = if cfg!(windows) { "inocode.exe" } else { "inocode" };
    let explicit = !configured.is_empty() && configured != "inocode";
    if explicit {
        let p = PathBuf::from(configured);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("INOCODE_BIN が指すファイルが見つかりません: {configured}"))
        };
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(name);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    if let Some(dir) = &exe_dir {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    let dev = Path::new("cli").join("inocode").join("codex-rs").join("target-win").join("dev-small").join(name);
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(dir) = &exe_dir {
        bases.extend(dir.ancestors().map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        bases.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    for base in bases {
        let p = base.join(&dev);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err("inocode.exe が見つかりません。INOCODE_BIN を指定してください".to_string())
}

/// Version (x.y.z) reported by the installed official Codex CLI (`codex --version`), if any.
pub fn official_codex_version() -> Option<String> {
    let mut cmd;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // The npm shim is a .cmd file, so go through cmd.exe.
        cmd = Proc::new("cmd");
        cmd.args(["/c", "codex", "--version"]).creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    {
        cmd = Proc::new("codex");
        cmd.arg("--version");
    }
    let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    parse_codex_version(&String::from_utf8_lossy(&out.stdout))
}

/// "codex-cli 0.155.0-alpha.16.4" -> "0.155.0"
pub fn parse_codex_version(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|word| {
        let whole = word.split(['-', '+']).next()?;
        let parts: Vec<&str> = whole.split('.').collect();
        (parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())))
            .then(|| whole.to_string())
    })
}

/// The version the main app-server presents: INOCODE_CLIENT_VERSION if set, else the
/// installed official Codex's.
pub fn base_client_version() -> Option<String> {
    std::env::var("INOCODE_CLIENT_VERSION").ok().and_then(|v| parse_codex_version(&v)).or_else(official_codex_version)
}

/// Start a short-lived app-server that presents `version`, read model/list
/// (`includeHidden: true`) and stop it. Returns the raw model objects.
/// The program writes `~/.inocode/models_cache.json` with this version in it; the main
/// app-server ignores a cache stamped with another version and fetches its own list.
pub fn probe_models(program: &str, version: &str) -> Result<Vec<Value>, String> {
    let mut conn = open_stdio(program, Some(version))?;
    let deadline = Instant::now() + Duration::from_secs(40);
    conn.send(json!({ "id": 1, "method": "initialize", "params": {
        "clientInfo": { "name": "inocode_desktop", "title": "InoCode Desktop", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "experimentalApi": true } } }))?;
    let mut sent_list = false;
    loop {
        if Instant::now() > deadline {
            return Err("モデル一覧の取得がタイムアウトしました".into());
        }
        let Some(line) = conn.recv()? else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        match v.get("id").and_then(Value::as_u64) {
            Some(1) if !sent_list => {
                if v.get("error").is_some() {
                    return Err("initialize が失敗しました".into());
                }
                conn.send(json!({ "method": "initialized" }))?;
                conn.send(json!({ "id": 2, "method": "model/list",
                    "params": { "includeHidden": true, "cursor": null, "limit": 100 } }))?;
                sent_list = true;
            }
            Some(2) => {
                if let Some(e) = v.get("error") {
                    return Err(e.get("message").and_then(Value::as_str).unwrap_or("model/list failed").to_string());
                }
                let list = v.pointer("/result/data").and_then(Value::as_array).cloned().unwrap_or_default();
                return Ok(list);
            }
            _ => {}
        }
    }
}

fn toml_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn open_stdio(program: &str, version: Option<&str>) -> Result<Conn, String> {
    let program = find_inocode(program)?;
    let mut cmd = Proc::new(&program);
    cmd.args(["app-server", "--listen", "stdio://"]);
    // Optional model catalog file (for example OpenCodex's), passed as a config override.
    if let Ok(path) = std::env::var("INOCODE_MODEL_CATALOG") {
        if !path.is_empty() {
            cmd.arg("-c").arg(format!("model_catalog_json={}", toml_string(&path)));
        }
    }
    // Present the version of the official Codex that is installed, so the backend offers the
    // same models; INOCODE_CLIENT_VERSION in our own environment wins.
    if let Some(v) = version {
        cmd.env("INOCODE_CLIENT_VERSION", v);
    } else if std::env::var_os("INOCODE_CLIENT_VERSION").is_none() {
        if let Some(v) = official_codex_version() {
            cmd.env("INOCODE_CLIENT_VERSION", v);
        }
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("{} を起動できません: {e}", program.display()))?;
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
    Models,
    Requirements,
    SettingsUpdate,
    Daybreak,
    Account,
    RateLimits,
    Login,
    Other,
}

struct Session<'a, F: Fn(Event)> {
    conn: Conn,
    on_event: &'a F,
    active_thread: &'a mut Option<String>,
    settings: &'a mut Settings,
    /// Settings already pushed to the thread (only changes are sent).
    sent: Settings,
    settings_update_unsupported: bool,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    turn_id: Option<String>,
    delta: String,
    last_flush: Instant,
    approvals: HashMap<String, (Value, String, Value)>,
    msg_streamed: bool,
    login_pending: bool,
    /// When model/list was last requested (the desktop client treats it as stale after 5 minutes).
    models_asked: Instant,
    ready: bool,
}

impl<'a, F: Fn(Event)> Session<'a, F> {
    fn new(
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

    fn request(&mut self, method: &str, params: Value, kind: Pending) -> Result<(), String> {
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

    fn command(&mut self, cmd: Command, cfg: &mut Config) -> Result<Option<End>, String> {
        match cmd {
            Command::SwitchTransport => {
                cfg.use_stdio = !cfg.use_stdio;
                return Ok(Some(End::Switch));
            }
            Command::Quit => return Ok(Some(End::Quit)),
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
    fn push_settings(&mut self) -> Result<(), String> {
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

    fn refresh_models(&mut self) -> Result<(), String> {
        self.models_asked = Instant::now();
        self.request(
            "model/list",
            json!({ "includeHidden": true, "cursor": null, "limit": 100 }),
            Pending::Models,
        )
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

fn thread_start_params(s: &Settings) -> Value {
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

fn sandbox_policy(mode: &str) -> Value {
    match mode {
        "read-only" => json!({ "type": "readOnly" }),
        "danger-full-access" => json!({ "type": "dangerFullAccess" }),
        _ => json!({ "type": "workspaceWrite" }),
    }
}

/// Per-turn fields. Model, effort and speed go with every turn (as in the desktop
/// client); permissions only when thread/settings/update is unavailable.
fn turn_overrides(s: &Settings, with_permissions: bool, params: &mut Value) {
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

fn applied(result: &Value) -> Event {
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

pub fn parse_account(result: &Value) -> (bool, String) {
    match result.pointer("/account/type").and_then(Value::as_str) {
        Some("chatgpt") => {
            let email = result.pointer("/account/email").and_then(Value::as_str).unwrap_or("ChatGPT");
            let plan = result.pointer("/account/planType").and_then(Value::as_str).unwrap_or("");
            (true, if plan.is_empty() { email.to_string() } else { format!("{email} · {plan}") })
        }
        Some("apiKey") => (true, "API キー".to_string()),
        Some(other) => (true, other.to_string()),
        None => {
            let need = result.get("requiresOpenaiAuth").and_then(Value::as_bool).unwrap_or(true);
            (!need, if need { "未ログイン".to_string() } else { "ローカル".to_string() })
        }
    }
}

fn window_label(mins: Option<i64>) -> String {
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

fn open_url(url: &str) {
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
    fn missing_binary_message() {
        let e = find_inocode("no-such-dir/inocode-missing.exe").unwrap_err();
        assert!(e.contains("INOCODE_BIN"));
    }

    #[test]
    fn cyber_programs() {
        let m = |programs: &[&str]| ModelInfo {
            id: "m".into(),
            name: "m".into(),
            description: String::new(),
            efforts: vec![],
            default_effort: String::new(),
            hidden: false,
            is_default: false,
            programs: programs.iter().map(|s| s.to_string()).collect(),
            service_tiers: vec![],
            default_service_tier: None,
            min_version: None,
        };
        let sol = m(&["daybreakBlue"]);
        assert_eq!(sol.cyber_program(true), Some("daybreakBlue"));
        assert_eq!(sol.cyber_program(false), None);
        assert!(!sol.has_daybreak_variant());
        assert_eq!(sol.program_for(false), Some("daybreakBlue"));
        let std = m(&["standard"]);
        assert_eq!(std.cyber_program(false), Some("standard"));
        assert!(!std.has_daybreak_variant());
        assert!(m(&["standard", "daybreakBlue"]).has_daybreak_variant());
        assert_eq!(m(&[]).program_for(true), None);
        let mut p = json!({});
        let mut s = Settings { model: Some("m".into()), programs: vec![sol], ..Settings::default() };
        s.daybreak = true;
        turn_overrides(&s, false, &mut p);
        assert_eq!(p["cyberAccessProgram"], "daybreakBlue");
        assert!(p.get("approvalPolicy").is_none());
        s.perm = Some(Perm::Guardian);
        turn_overrides(&s, true, &mut p);
        assert_eq!(p["approvalsReviewer"], "auto_review");
        assert_eq!(Perm::from_server("never", "dangerFullAccess", "user"), Perm::Full);
        assert_eq!(Perm::from_server("on-request", "workspaceWrite", "auto_review"), Perm::Guardian);
    }

    #[test]
    fn codex_version_parse() {
        assert_eq!(parse_codex_version("codex-cli 0.155.0-alpha.16.4
").as_deref(), Some("0.155.0"));
        assert_eq!(parse_codex_version("nothing here"), None);
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
            stdio_program: std::env::var("INOCODE_BIN").unwrap_or_default(),
            use_stdio: std::env::var("INOCODE_WS_URL").is_err(),
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
                    if std::env::var("LIVE_SETTINGS").is_ok() {
                        // thread/settings/update and thread/metadata/update, without a model call.
                        let st = Settings {
                            model: Some("gpt-5.6-luna".into()),
                            effort: Some("low".into()),
                            service_tier: Some(None),
                            perm: Some(Perm::Guardian),
                            ..Settings::default()
                        };
                        h.send(Command::SetSettings(st));
                        h.send(Command::SetDaybreak(false));
                        std::thread::sleep(Duration::from_secs(3));
                        while let Ok(e) = rx.try_recv() {
                            println!("{e:?}");
                        }
                        break;
                    }
                    if std::env::var("LIVE_PROMPT").is_ok() {
                        h.send(Command::Prompt("hello".into()));
                    } else {
                        // Give the model / account replies time to arrive, then stop.
                        std::thread::sleep(Duration::from_secs(4));
                        while let Ok(e) = rx.try_recv() {
                            println!("{e:?}");
                        }
                        break;
                    }
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
