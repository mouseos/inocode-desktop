//! Right-hand panel data and the header menus: MCP connectors, background terminals,
//! Remote Control, folder name, copy as Markdown / working folder.
//!
//! Official: mcpServerStatus/list, mcpServer/startupStatus/updated, config/mcpServer/reload,
//! thread/backgroundTerminals/list|terminate|clean, remoteControl/status/read|enable|disable,
//! threadHeader.moreActions. Claude Desktop: Session info (MCP servers {n} connected,
//! Environment), "Show this session's background tasks", Stop this task (c96d752d0, cc183b3ab).

use crate::state::*;
use crate::*;
use serde_json::{json, Value};

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
}

fn rpc(tag: &str, method: &str, params: Value) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: tag.to_string(), method: method.to_string(), params });
    }
}

fn active() -> Option<String> {
    STATE.with(|s| Some(s.borrow().active.clone()).filter(|a| !a.is_empty()))
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    app.global::<PanelState>().on_toggle_remote(|on| {
        rpc("panel.remote", if on { "remoteControl/enable" } else { "remoteControl/disable" }, json!({}));
    });
    app.global::<PanelState>().on_reload_mcp(|| rpc("panel.mcp.reload", "config/mcpServer/reload", json!({})));
    app.global::<PanelState>().on_stop_terminal(|id| {
        if let Some(t) = active() {
            rpc("panel.term.stop", "thread/backgroundTerminals/terminate", json!({ "threadId": t, "processId": id.to_string() }));
        }
    });
    app.global::<PanelState>().on_stop_all_terminals(|| {
        if let Some(t) = active() {
            rpc("panel.term.stop", "thread/backgroundTerminals/clean", json!({ "threadId": t }));
        }
    });
}

/// A thread became active: show its folder and reload the panel's lists.
pub(crate) fn on_thread_active(app: &AppWindow, id: &str, cwd: &str) {
    let name = std::path::Path::new(cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    app.global::<PanelState>().set_thread_id(id.into());
    app.global::<PanelState>().set_folder_name(name.into());
    refresh(id);
}

pub(crate) fn refresh(thread: &str) {
    rpc("panel.mcp", "mcpServerStatus/list", json!({}));
    rpc("panel.remote.read", "remoteControl/status/read", Value::Null);
    refresh_terminals(thread);
}

/// Takes the thread id: the caller may hold the shared state.
pub(crate) fn refresh_terminals(thread: &str) {
    if !thread.is_empty() {
        rpc("panel.terms", "thread/backgroundTerminals/list", json!({ "threadId": thread }));
    }
}

fn set_remote(app: &AppWindow, status: &str) {
    let s = app.global::<PanelState>();
    s.set_remote_status(status.into());
    s.set_remote_on(matches!(status, "connecting" | "connected"));
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if !tag.starts_with("panel.") {
        return;
    }
    let s = app.global::<PanelState>();
    match (tag, result) {
        ("panel.mcp", Ok(v)) => {
            let mut rows = Vec::new();
            for m in v.get("data").and_then(Value::as_array).into_iter().flatten() {
                let name = m.get("name").and_then(Value::as_str).unwrap_or("");
                let status = m.get("runtimeStatus").and_then(Value::as_str).unwrap_or("接続済み");
                rows.push(McpRow { name: name.into(), status: status.into() });
            }
            s.set_mcp_summary(if rows.is_empty() { "".into() } else { format!("{} 件接続", rows.len()).into() });
            s.set_mcp(ModelRc::from(Rc::new(VecModel::from(rows))));
        }
        ("panel.mcp.reload", _) => rpc("panel.mcp", "mcpServerStatus/list", json!({})),
        ("panel.terms", Ok(v)) => {
            let rows: Vec<TermRow> = v
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|t| TermRow {
                    id: t.get("processId").and_then(Value::as_str).unwrap_or("").into(),
                    text: t.get("command").and_then(Value::as_str).unwrap_or("").into(),
                })
                .collect();
            s.set_terminals(ModelRc::from(Rc::new(VecModel::from(rows))));
        }
        // The thread may not be loaded yet: no terminals.
        ("panel.terms", Err(_)) => s.set_terminals(ModelRc::from(Rc::new(VecModel::<TermRow>::default()))),
        ("panel.term.stop", _) => refresh_terminals(&active().unwrap_or_default()),
        ("panel.remote.read", Ok(v)) => set_remote(app, v.get("status").and_then(Value::as_str).unwrap_or("disabled")),
        ("panel.remote", Ok(_)) => rpc("panel.remote.read", "remoteControl/status/read", Value::Null),
        ("panel.remote", Err(e)) => {
            // Put the switch back to what the server reports.
            app.global::<AppState>().set_status(format!("Remote Control: {e}").into());
            app.global::<AppState>().set_status_level(2);
            rpc("panel.remote.read", "remoteControl/status/read", Value::Null);
        }
        _ => {}
    }
}

pub(crate) fn on_notify(app: &AppWindow, method: &str, params: &Value) {
    match method {
        "remoteControl/status/changed" => {
            if let Some(st) = params.get("status").and_then(Value::as_str) {
                set_remote(app, st);
            }
        }
        "mcpServer/startupStatus/updated" => rpc("panel.mcp", "mcpServerStatus/list", json!({})),
        _ => {}
    }
}

pub(crate) fn thread_menu_extras(more: bool) -> Vec<MenuItem> {
    if !more {
        return Vec::new();
    }
    vec![
        crate::sidebar::separator(),
        crate::sidebar::simple_item("copymd", "Markdown でコピー", "\u{e14d}"),
        crate::sidebar::simple_item("copycwd", "作業フォルダをコピー", "\u{e2c7}"),
    ]
}

fn markdown(app: &AppWindow) -> String {
    let model = app.global::<AppState>().get_messages();
    let mut out = String::new();
    for i in 0..model.row_count() {
        let Some(m) = model.row_data(i) else { continue };
        match m.role.as_str() {
            "user" => out.push_str(&format!("## ユーザー\n\n{}\n\n", m.text)),
            "assistant" => out.push_str(&format!("## アシスタント\n\n{}\n\n", m.text)),
            _ => {}
        }
    }
    out.trim_end().to_string()
}

/// Header menu actions. Returns true when the action was ours.
pub(crate) fn menu_pick(app: &AppWindow, action: &str) -> bool {
    let text = match action {
        "copymd" => markdown(app),
        "copycwd" => STATE.with(|s| s.borrow().active_cwd.clone()),
        _ => return false,
    };
    if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        app.global::<AppState>().set_status(format!("コピーできませんでした: {e}").into());
        app.global::<AppState>().set_status_level(2);
    }
    true
}
