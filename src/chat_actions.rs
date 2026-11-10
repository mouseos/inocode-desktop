//! Hover actions under a message: copy, edit, rewind, fork.
//!
//! Official: thread/fork (lastTurnId), thread/revert (beforeTurnId), turn/start for the edited
//! message (codex.userMessage.editAriaLabel, assistantMessageContent.copyResponseTooltip,
//! forkAriaLabel). Claude Desktop: Copy message, Rewind to here, Fork from here (c96d752d0).
//!
//! The message list does not carry turn ids: the n-th user message is the n-th turn, so the
//! turn is found by counting user messages and reading the turn list from the server.

use crate::state::*;
use crate::*;
use serde_json::{json, Value};

#[derive(Clone)]
struct Pending {
    kind: String,
    ordinal: usize,
    text: String,
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

fn status(app: &AppWindow, text: &str, level: i32) {
    app.global::<AppState>().set_status(text.into());
    app.global::<AppState>().set_status_level(level);
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<MessageActions>().on_act(move |kind, index| {
        let Some(app) = weak.upgrade() else { return };
        let model = app.global::<AppState>().get_messages();
        let Some(msg) = model.row_data(index as usize) else { return };
        if kind == "copy" {
            match arboard::Clipboard::new().and_then(|mut c| c.set_text(msg.text.to_string())) {
                Ok(()) => {}
                Err(e) => status(&app, &format!("コピーできませんでした: {e}"), 2),
            }
            return;
        }
        if app.global::<AppState>().get_busy() {
            status(&app, "実行中は使えません", 2);
            return;
        }
        // Turns before this message: user messages above it (for a user message, it counts itself).
        let mut users = 0usize;
        for i in 0..=(index as usize) {
            if model.row_data(i).map(|m| m.role == "user").unwrap_or(false) {
                users += 1;
            }
        }
        if users == 0 {
            return;
        }
        let Some(thread) = STATE.with(|s| Some(s.borrow().active.clone()).filter(|a| !a.is_empty())) else { return };
        PENDING.with(|p| {
            *p.borrow_mut() = Some(Pending { kind: kind.to_string(), ordinal: users - 1, text: msg.text.to_string() })
        });
        h.send(Command::Rpc {
            tag: "chat.turns".into(),
            method: "thread/read".into(),
            params: json!({ "threadId": thread, "includeTurns": true }),
        });
    });
}

/// The n-th turn id out of a thread/read result.
fn turn_id(result: &Value, ordinal: usize) -> Option<String> {
    result.pointer("/thread/turns")?.as_array()?.get(ordinal)?.get("id")?.as_str().map(str::to_string)
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if !matches!(tag, "chat.turns" | "chat.fork" | "chat.revert") {
        return;
    }
    let Some(h) = HANDLE.with(|c| c.borrow().clone()) else { return };
    let value = match result {
        Ok(v) => v,
        Err(e) => {
            PENDING.with(|p| *p.borrow_mut() = None);
            status(app, &format!("操作に失敗しました: {e}"), 2);
            return;
        }
    };
    let active = STATE.with(|s| s.borrow().active.clone());
    match tag {
        "chat.turns" => {
            let Some(p) = PENDING.with(|p| p.borrow().clone()) else { return };
            let Some(turn) = turn_id(value, p.ordinal) else {
                status(app, "対象のターンが見つかりません", 2);
                return;
            };
            if p.kind == "fork" {
                h.send(Command::Rpc {
                    tag: "chat.fork".into(),
                    method: "thread/fork".into(),
                    params: json!({ "threadId": active, "lastTurnId": turn }),
                });
            } else {
                // Rewind and edit both take this message and everything after it out.
                h.send(Command::Rpc {
                    tag: "chat.revert".into(),
                    method: "thread/revert".into(),
                    params: json!({ "threadId": active, "beforeTurnId": turn }),
                });
            }
        }
        "chat.fork" => {
            PENDING.with(|p| *p.borrow_mut() = None);
            if let Some(id) = value.pointer("/thread/id").and_then(Value::as_str) {
                h.send(Command::OpenThread(id.to_string()));
                crate::sidebar::refresh();
            }
        }
        _ => {
            // chat.revert: reload the shortened history; an edit puts the text back in the composer.
            let p = PENDING.with(|p| p.borrow_mut().take());
            h.send(Command::OpenThread(active));
            if let Some(p) = p.filter(|p| p.kind == "edit") {
                app.set_prompt(p.text.into());
            }
        }
    }
}
