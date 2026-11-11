//! Settings page (opened from the account menu): configuration, MCP servers, skills,
//! archived sessions, usage, account, keyboard shortcuts.
//!
//! Official: config/read, mcpServerStatus/list, config/mcpServer/reload, skills/list,
//! skills/config/write, thread/list (archived), thread/unarchive, account/rateLimits/read,
//! account/read, account/logout. Claude Desktop: full-page settings with a list on the left
//! (General, Claude Code, MCP servers, Skills, Usage, Account; cd19fedd2, cf400e6a4).
//! The page reads and changes what the server holds; the InoCode "設定" tab is separate.

use crate::state::*;
use crate::*;
use serde_json::{json, Value};

const SECTIONS: [&str; 7] = ["一般", "MCP サーバー", "スキル", "アーカイブ", "使用量", "アカウント", "ショートカット"];

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
}

fn rpc(tag: &str, method: &str, params: Value) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: tag.to_string(), method: method.to_string(), params });
    }
}

fn row(id: &str, title: &str, desc: &str, value: &str, kind: i32, on: bool) -> SRow {
    SRow { id: id.into(), title: title.into(), desc: desc.into(), value: value.into(), kind, on }
}

fn set_rows(app: &AppWindow, rows: Vec<SRow>) {
    let s = app.global::<SettingsPageState>();
    s.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
    s.set_loading(false);
}

pub(crate) fn open(app: &AppWindow) {
    app.global::<SettingsPageState>().set_open(true);
    load(app, app.global::<SettingsPageState>().get_section());
}

fn load(app: &AppWindow, section: i32) {
    let s = app.global::<SettingsPageState>();
    s.set_section(section);
    s.set_title(SECTIONS.get(section as usize).copied().unwrap_or("").into());
    set_rows(app, Vec::new());
    s.set_loading(true);
    match section {
        0 => rpc("spage.config", "config/read", json!({ "includeLayers": false })),
        1 => rpc("spage.mcp", "mcpServerStatus/list", json!({})),
        2 => {
            let cwd = STATE.with(|st| st.borrow().active_cwd.clone());
            rpc("spage.skills", "skills/list", json!({ "cwds": [cwd] }));
        }
        3 => rpc("spage.archive", "thread/list", json!({ "limit": 50, "archived": true })),
        4 => rpc("spage.usage", "account/rateLimits/read", Value::Null),
        5 => rpc("spage.account", "account/read", json!({ "refreshToken": false })),
        _ => {
            let keys = [
                ("Ctrl+K", "コマンドパレットを開く"),
                ("Enter", "送信"),
                ("Shift+Enter", "改行"),
                ("Ctrl+Shift+M", "モデルメニューを開く"),
                ("Esc", "パレットを閉じる / 名前変更を取り消す"),
                ("↑ ↓ Enter", "パレットの候補を選ぶ"),
            ];
            set_rows(app, keys.iter().map(|(k, d)| row("", d, "", k, 0, false)).collect());
        }
    }
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    app.global::<SettingsPageState>().set_sections(strings(SECTIONS.iter().map(|s| s.to_string())));
    let weak = app.as_weak();
    app.global::<SettingsPageState>().on_select(move |i| {
        if let Some(app) = weak.upgrade() {
            load(&app, i);
        }
    });
    let weak = app.as_weak();
    app.global::<SettingsPageState>().on_row_action(move |id| {
        let Some(app) = weak.upgrade() else { return };
        match id.as_str() {
            "mcp-reload" => {
                rpc("spage.mcp.reload", "config/mcpServer/reload", json!({}));
            }
            "logout" => {
                rpc("chat.logout", "account/logout", Value::Null);
                app.global::<SettingsPageState>().set_open(false);
            }
            a if a.starts_with("unarchive:") => {
                rpc("spage.unarchive", "thread/unarchive", json!({ "threadId": &a[10..] }));
            }
            _ => {}
        }
    });
    app.global::<SettingsPageState>().on_row_toggle(|id, on| {
        if let Some(path) = id.strip_prefix("skill:") {
            rpc("spage.skill.write", "skills/config/write", json!({ "path": path, "enabled": on }));
        }
    });
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if !tag.starts_with("spage.") {
        return;
    }
    let v = match result {
        Ok(v) => v,
        Err(e) => {
            if !tag.ends_with(".write") {
                set_rows(app, vec![row("", "読み込めませんでした", "", e, 0, false)]);
            }
            return;
        }
    };
    match tag {
        "spage.config" => {
            let mut rows = Vec::new();
            if let Some(map) = v.get("config").and_then(Value::as_object) {
                for (k, val) in map {
                    if let Some(text) = scalar(val) {
                        rows.push(row("", k, "", &text, 0, false));
                    }
                }
            }
            set_rows(app, rows);
        }
        "spage.mcp" => {
            let mut rows: Vec<SRow> = v
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|m| {
                    row(
                        "",
                        m.get("name").and_then(Value::as_str).unwrap_or(""),
                        "",
                        m.get("runtimeStatus").and_then(Value::as_str).unwrap_or("接続済み"),
                        0,
                        false,
                    )
                })
                .collect();
            rows.push(row("mcp-reload", "設定を再読み込み", "", "再読み込み", 2, false));
            set_rows(app, rows);
        }
        "spage.mcp.reload" => load(app, 1),
        "spage.skills" => {
            let mut rows = Vec::new();
            for entry in v.get("data").and_then(Value::as_array).into_iter().flatten() {
                for s in entry.get("skills").and_then(Value::as_array).into_iter().flatten() {
                    let path = s.get("path").and_then(Value::as_str).unwrap_or("");
                    rows.push(row(
                        &format!("skill:{path}"),
                        s.get("name").and_then(Value::as_str).unwrap_or(""),
                        s.get("description").and_then(Value::as_str).unwrap_or(""),
                        "",
                        1,
                        s.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                    ));
                }
            }
            set_rows(app, rows);
        }
        "spage.archive" => {
            let list = client::parse_threads(v);
            let rows: Vec<SRow> = list
                .iter()
                .map(|t| row(&format!("unarchive:{}", t.id), &t.title, &t.cwd, "アーカイブを解除", 2, false))
                .collect();
            set_rows(app, if rows.is_empty() { vec![row("", "アーカイブ済みのセッションはありません", "", "", 0, false)] } else { rows });
        }
        "spage.unarchive" => {
            load(app, 3);
            crate::sidebar::refresh();
        }
        "spage.usage" => {
            let mut rows = Vec::new();
            let snap = v.get("rateLimits").unwrap_or(&Value::Null);
            for key in ["primary", "secondary"] {
                if let Some(w) = snap.get(key).filter(|w| !w.is_null()) {
                    let used = w.get("usedPercent").and_then(Value::as_i64).unwrap_or(0);
                    let mins = w.get("windowDurationMins").and_then(Value::as_i64);
                    let label = format!("{} の枠", client::rate_window_label(mins));
                    rows.push(row("", &label, "", &format!("{used}% 使用"), 0, false));
                }
            }
            if let Some(c) = snap.get("credits").filter(|c| !c.is_null()) {
                let bal = c.get("balance").and_then(Value::as_str).unwrap_or("");
                rows.push(row("", "クレジット残高", "", bal, 0, false));
            }
            if rows.is_empty() {
                rows.push(row("", "使用量の情報はありません", "", "", 0, false));
            }
            set_rows(app, rows);
        }
        "spage.account" => {
            let (logged_in, label) = client::parse_account(v);
            let mut rows = vec![row("", "プラン", "", &label, 0, false)];
            if logged_in {
                rows.push(row("logout", "ログアウト", "", "ログアウト", 2, false));
            }
            set_rows(app, rows);
        }
        _ => {}
    }
}
