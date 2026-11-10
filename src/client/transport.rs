//! Transports (WebSocket and stdio child process) and the search for the inocode binary.

use serde_json::Value;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Child, ChildStdin, Command as Proc, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::{client, Message as WsMessage, WebSocket};
use super::*;

// ---------------------------------------------------------------- transport

pub(super) enum Conn {
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
    pub(super) fn open(cfg: &Config) -> Result<Conn, String> {
        if cfg.use_stdio {
            open_stdio(&cfg.stdio_program, None)
        } else {
            open_ws(&cfg.ws_url)
        }
    }

    pub(super) fn send(&mut self, value: Value) -> Result<(), String> {
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
    pub(super) fn recv(&mut self) -> Result<Option<String>, String> {
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

pub(super) fn open_ws(url: &str) -> Result<Conn, String> {
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

pub(super) fn toml_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub(super) fn open_stdio(program: &str, version: Option<&str>) -> Result<Conn, String> {
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

