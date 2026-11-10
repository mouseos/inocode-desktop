//! WebSocket client for the InoCode app-server.
//!
//! The wire format is JSON-RPC without the "jsonrpc" field. The client runs on
//! one background thread: it performs initialize / initialized / thread/start,
//! then sends turn/start for each prompt and forwards agent message deltas.
//! Deltas are coalesced (about one flush per frame) so a fast stream does not
//! flood the UI thread.

use serde_json::{json, Value};
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::{client, Message as WsMessage, WebSocket};

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Status(String),
    Delta(String),
    TurnDone,
    Error(String),
}

pub struct Handle {
    tx: Sender<String>,
}

impl Handle {
    pub fn send_prompt(&self, text: String) {
        let _ = self.tx.send(text);
    }
}

const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

pub fn spawn(url: String, on_event: impl Fn(Event) + Send + 'static) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        on_event(Event::Status("接続中…".into()));
        if let Err(e) = run(&url, rx, &on_event) {
            on_event(Event::Error(e));
            on_event(Event::Status("未接続".into()));
        }
    });
    Handle { tx }
}

type Socket = WebSocket<TcpStream>;

fn send(ws: &mut Socket, value: Value) -> Result<(), String> {
    ws.send(WsMessage::text(value.to_string())).map_err(|e| e.to_string())
}

fn connect(url: &str) -> Result<Socket, String> {
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
    let (ws, _) = client(request, stream).map_err(|e| format!("{e}"))?;
    ws.get_ref()
        .set_read_timeout(Some(Duration::from_millis(8)))
        .map_err(|e| e.to_string())?;
    Ok(ws)
}

fn run(url: &str, rx: Receiver<String>, on_event: &dyn Fn(Event)) -> Result<(), String> {
    let mut ws = connect(url)?;
    send(
        &mut ws,
        json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "inocode_desktop",
                    "title": "InoCode Desktop",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }
        }),
    )?;

    let mut next_id: u64 = 3;
    let mut thread_id: Option<String> = None;
    let mut pending = String::new();
    let mut last_flush = Instant::now();

    loop {
        if let Some(tid) = &thread_id {
            loop {
                match rx.try_recv() {
                    Ok(text) => {
                        let id = next_id;
                        next_id += 1;
                        send(
                            &mut ws,
                            json!({
                                "id": id,
                                "method": "turn/start",
                                "params": {
                                    "threadId": tid,
                                    "input": [{ "type": "text", "text": text }]
                                }
                            }),
                        )?;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }
        }

        match ws.read() {
            Ok(WsMessage::Text(text)) => {
                let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                handle_message(&mut ws, &value, &mut thread_id, &mut pending, on_event)?;
            }
            Ok(WsMessage::Close(_)) => return Err("接続が閉じられました".into()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.to_string()),
        }

        if !pending.is_empty() && last_flush.elapsed() >= FLUSH_INTERVAL {
            on_event(Event::Delta(std::mem::take(&mut pending)));
            last_flush = Instant::now();
        }
    }
}

fn handle_message(
    ws: &mut Socket,
    value: &Value,
    thread_id: &mut Option<String>,
    pending: &mut String,
    on_event: &dyn Fn(Event),
) -> Result<(), String> {
    let id = value.get("id");
    let method = value.get("method").and_then(Value::as_str);

    // A request from the server (for example an approval): this client does not
    // handle them yet, so answer with an error instead of leaving it hanging.
    if let (Some(id), Some(_)) = (id, method) {
        return send(
            ws,
            json!({ "id": id, "error": { "code": -32601, "message": "unsupported by InoCode Desktop" } }),
        );
    }

    if let Some(id) = id {
        if let Some(err) = value.get("error") {
            let message = err.get("message").and_then(Value::as_str).unwrap_or("error");
            on_event(Event::Error(message.to_string()));
            return Ok(());
        }
        match id.as_u64() {
            Some(1) => {
                send(ws, json!({ "method": "initialized" }))?;
                send(ws, json!({ "id": 2, "method": "thread/start", "params": {} }))?;
            }
            Some(2) => {
                let tid = value
                    .pointer("/result/thread/id")
                    .and_then(Value::as_str)
                    .ok_or("thread/start の応答に thread.id がありません")?;
                *thread_id = Some(tid.to_string());
                on_event(Event::Status("接続済み".into()));
            }
            _ => {}
        }
        return Ok(());
    }

    match method {
        Some("item/agentMessage/delta") => {
            if let Some(delta) = value.pointer("/params/delta").and_then(Value::as_str) {
                pending.push_str(delta);
            }
        }
        Some("turn/completed") => {
            if !pending.is_empty() {
                on_event(Event::Delta(std::mem::take(pending)));
            }
            on_event(Event::TurnDone);
            on_event(Event::Status("接続済み".into()));
        }
        Some("error") => {
            let message = value
                .pointer("/params/error/message")
                .and_then(Value::as_str)
                .unwrap_or("error");
            let will_retry = value
                .pointer("/params/willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // The server retries on its own; show progress in the status area
            // and keep the conversation free of transient errors.
            if will_retry {
                on_event(Event::Status(message.to_string()));
            } else {
                on_event(Event::Error(message.to_string()));
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Minimal app-server stand-in that follows the documented handshake and
    /// streams a reply in several deltas.
    fn mock_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut seen_initialized = false;
            loop {
                let Ok(WsMessage::Text(text)) = ws.read() else { return };
                let v: Value = serde_json::from_str(text.as_str()).unwrap();
                assert!(v.get("jsonrpc").is_none(), "jsonrpc field must not be sent");
                match v["method"].as_str().unwrap() {
                    "initialize" => {
                        assert_ne!(v["params"]["clientInfo"]["name"], "Codex Desktop");
                        ws.send(WsMessage::text(json!({"id": v["id"], "result": {"userAgent": "mock", "codexHome": "/x", "platformFamily": "unix", "platformOs": "linux"}}).to_string())).unwrap();
                    }
                    "initialized" => seen_initialized = true,
                    "thread/start" => {
                        assert!(seen_initialized);
                        ws.send(WsMessage::text(json!({"id": v["id"], "result": {"thread": {"id": "t1"}}}).to_string())).unwrap();
                    }
                    "turn/start" => {
                        assert_eq!(v["params"]["threadId"], "t1");
                        assert_eq!(v["params"]["input"][0]["text"], "こんにちは");
                        ws.send(WsMessage::text(json!({"id": v["id"], "result": {"turn": {"id": "u1"}}}).to_string())).unwrap();
                        for d in ["こん", "にちは", "、世界"] {
                            ws.send(WsMessage::text(json!({"method": "item/agentMessage/delta", "params": {"threadId": "t1", "turnId": "u1", "itemId": "i1", "delta": d}}).to_string())).unwrap();
                        }
                        ws.send(WsMessage::text(json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "u1"}}}).to_string())).unwrap();
                    }
                    other => panic!("unexpected method {other}"),
                }
            }
        });
        url
    }

    #[test]
    fn handshake_and_streaming() {
        let url = mock_server();
        let (tx, rx) = mpsc::channel();
        let handle = spawn(url, move |e| {
            let _ = tx.send(e);
        });
        handle.send_prompt("こんにちは".into());

        let mut text = String::new();
        let mut connected = false;
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("event") {
                Event::Status(s) if s == "接続済み" => connected = true,
                Event::Status(_) => {}
                Event::Delta(d) => text.push_str(&d),
                Event::TurnDone => break,
                Event::Error(e) => panic!("{e}"),
            }
        }
        assert!(connected);
        assert_eq!(text, "こんにちは、世界");
    }
}
