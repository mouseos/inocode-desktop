//! Client for the InoCode app-server over WebSocket or stdio.
//!
//! The wire format is JSON-RPC without the "jsonrpc" field. One background thread
//! owns the connection: it performs initialize / initialized, lists threads, starts
//! or resumes one, and then serves commands from the UI (prompt, interrupt,
//! approvals, thread switching). Streaming deltas are coalesced to about one
//! flush per frame. If the connection drops it reconnects with a growing delay and
//! resumes the thread that was open.


mod mock;
mod notify;
mod parse;
mod session;
mod transport;
mod types;

use mock::mock_rpc;
use parse::*;
use session::*;
use transport::*;
use serde_json::json;
use session::supervise;
use std::sync::mpsc;
use std::time::Duration;

pub use parse::{history_from_thread, parse_account, parse_models, parse_threads, rate_limit_text, tool_text};
pub use transport::{base_client_version, find_inocode, probe_models};
pub use types::*;

const FLUSH_INTERVAL: Duration = Duration::from_millis(16);
const POLL: Duration = Duration::from_millis(8);
const MODELS_STALE: Duration = Duration::from_secs(300);

pub fn spawn(cfg: Config, on_event: impl Fn(Event) + Send + 'static) -> Handle {
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || supervise(cfg, rx, on_event));
    Handle { tx, thread: std::sync::Mutex::new(Some(thread)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::time::Instant;

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
    fn account_label_has_no_email() {
        let reply = json!({
            "account": { "type": "chatgpt", "email": "someone@example.com", "planType": "pro", "name": "A B" },
            "requiresOpenaiAuth": true
        });
        let (ok, label) = parse_account(&reply);
        assert!(ok);
        assert!(!label.contains('@') && !label.contains("example") && !label.contains("someone"));
        assert_eq!(label, "pro");
        let no_plan = json!({ "account": { "type": "chatgpt", "email": "x@y.zz" } });
        assert_eq!(parse_account(&no_plan).1, "アカウント");
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

    #[test]
    fn thread_list_flags() {
        let list = json!({"data":[
            {"id":"a","preview":"x","cwd":"C:/p","section":{"id":"s","name":"Pinned"},"status":{"type":"active","activeFlags":["waitingOnApproval"]}},
            {"id":"b","preview":"y","cwd":"C:/p","section":null,"status":{"type":"idle"}}
        ],"readStates":{"b":{"firstUnread":{"type":"threadStart"},"revision":"r1"}}});
        let t = parse_threads(&list);
        assert!(t[0].pinned && t[0].status == 4 && !t[0].unread);
        assert!(!t[1].pinned && t[1].unread && t[1].revision == "r1" && t[1].status == 0);
    }
}
