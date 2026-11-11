//! The one settings page (gear and account menu): general (colour scheme, approval policy,
//! sandbox, reasoning), shortcuts (recorded here), memory, InoCode (feature flags, version
//! discovery, connection), account (pool), MCP servers, skills, archived sessions, usage.
//!
//! Official: config/read, config/value/write, memory/status|reset, mcpServerStatus/list,
//! config/mcpServer/reload, skills/list, skills/config/write, thread/list archived,
//! thread/unarchive, account/rateLimits/read, account/read, account/logout.
//! Claude Desktop: full-page settings, list on the left, content on the right (cd19fedd2).
//! The InoCode and account sections embed the pages that used to be the "設定" tab.

use crate::gui_prefs::{self, Prefs, Shortcut};
use crate::state::*;
use crate::*;
use serde_json::{json, Value};

const SECTIONS: [&str; 9] =
    ["一般", "ショートカット", "メモリ", "InoCode", "アカウント", "MCP サーバー", "スキル", "アーカイブ", "使用量"];
const SECTION_INOCODE: i32 = 3;
const SECTION_ACCOUNT: i32 = 4;

/// (config key, label, [(shown, written)]) of the editable choices in "一般".
const CHOICES: [(&str, &str, &[(&str, &str)]); 4] = [
    (
        "approval_policy",
        "承認ポリシー",
        &[("確認する", "untrusted"), ("要求時のみ", "on-request"), ("確認しない", "never")],
    ),
    (
        "sandbox_mode",
        "サンドボックス",
        &[("読み取り専用", "read-only"), ("ワークスペース書き込み", "workspace-write"), ("フルアクセス", "danger-full-access")],
    ),
    (
        "model_reasoning_effort",
        "推論の強さ",
        &[("最小", "minimal"), ("軽", "low"), ("中", "medium"), ("高", "high"), ("極高", "xhigh")],
    ),
    ("model_verbosity", "応答の詳しさ", &[("簡潔", "low"), ("標準", "medium"), ("詳細", "high")]),
];

const SCHEMES: [&str; 3] = ["システム", "ライト", "ダーク"];

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static PREFS: RefCell<Prefs> = RefCell::new(Prefs::default());
    static CONFIG: RefCell<Value> = RefCell::new(Value::Null);
}

fn rpc(tag: &str, method: &str, params: Value) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: tag.to_string(), method: method.to_string(), params });
    }
}

fn row(id: &str, title: &str, desc: &str, value: &str, kind: i32, on: bool) -> SRow {
    SRow {
        id: id.into(),
        title: title.into(),
        desc: desc.into(),
        value: value.into(),
        kind,
        on,
        opts: ModelRc::from(Rc::new(VecModel::<SharedString>::default())),
    }
}

fn choice(id: &str, title: &str, value: &str, opts: Vec<String>) -> SRow {
    let mut r = row(id, title, "", value, 3, false);
    r.opts = strings(opts);
    r
}

fn set_rows(app: &AppWindow, rows: Vec<SRow>) {
    let s = app.global::<SettingsPageState>();
    s.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
    s.set_loading(false);
}

/// Called once at start: restore the saved scheme and shortcuts.
pub(crate) fn init(app: &AppWindow) {
    let prefs = gui_prefs::load();
    gui_prefs::apply(app, &prefs);
    PREFS.with(|p| *p.borrow_mut() = prefs);
}

pub(crate) fn open(app: &AppWindow) {
    app.global::<SettingsPageState>().set_open(true);
    load(app, app.global::<SettingsPageState>().get_section());
}

fn shortcut_rows() -> Vec<SRow> {
    let (pal, model) = PREFS.with(|p| {
        let p = p.borrow();
        (p.palette.label(), p.model.label())
    });
    vec![
        row("key:palette", "コマンドパレットを開く", "", &pal, 4, false),
        row("key:model", "モデルメニューを開く", "", &model, 4, false),
        row("", "送信", "", "Enter", 0, false),
        row("", "改行", "", "Shift+Enter", 0, false),
    ]
}

fn general_rows(config: &Value) -> Vec<SRow> {
    let scheme = PREFS.with(|p| p.borrow().scheme) as usize;
    let mut rows = vec![choice("scheme", "テーマ", SCHEMES[scheme.min(2)], SCHEMES.iter().map(|s| s.to_string()).collect())];
    for (key, label, opts) in CHOICES {
        let cur = config.get(key).and_then(Value::as_str).unwrap_or("");
        let shown = opts.iter().find(|(_, w)| *w == cur).map(|(s, _)| *s).unwrap_or("");
        rows.push(choice(&format!("cfg:{key}"), label, shown, opts.iter().map(|(s, _)| s.to_string()).collect()));
    }
    rows
}

fn load(app: &AppWindow, section: i32) {
    let s = app.global::<SettingsPageState>();
    s.set_section(section);
    s.set_title(SECTIONS.get(section as usize).copied().unwrap_or("").into());
    s.set_recording_id("".into());
    set_rows(app, Vec::new());
    s.set_loading(true);
    match section {
        0 => rpc("spage.config", "config/read", json!({ "includeLayers": false })),
        1 => set_rows(app, shortcut_rows()),
        2 => rpc("spage.memory", "memory/status", json!({})),
        SECTION_INOCODE => {
            s.set_loading(false);
            app.global::<Tabs>().invoke_opened(4);
        }
        SECTION_ACCOUNT => {
            s.set_loading(false);
            app.global::<Tabs>().invoke_opened(4);
            rpc("spage.account", "account/read", json!({ "refreshToken": false }));
        }
        5 => rpc("spage.mcp", "mcpServerStatus/list", json!({})),
        6 => {
            let cwd = STATE.with(|st| st.borrow().active_cwd.clone());
            rpc("spage.skills", "skills/list", json!({ "cwds": [cwd] }));
        }
        7 => rpc("spage.archive", "thread/list", json!({ "limit": 50, "archived": true })),
        _ => rpc("spage.usage", "account/rateLimits/read", Value::Null),
    }
}

fn remember(update: impl FnOnce(&mut Prefs)) {
    PREFS.with(|p| {
        let mut p = p.borrow_mut();
        update(&mut p);
        gui_prefs::save(&p);
    });
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    app.global::<SettingsPageState>().set_sections(strings(SECTIONS.iter().map(|s| s.to_string())));
    let weak = app.as_weak();
    app.global::<SettingsPageState>().on_open_page(move || {
        if let Some(app) = weak.upgrade() {
            open(&app);
        }
    });
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
            "mcp-reload" => rpc("spage.mcp.reload", "config/mcpServer/reload", json!({})),
            "memory-reset" => crate::sidebar::ask_request(
                &app,
                "spage.memory.reset",
                "memory/reset",
                "メモリをリセットしますか？",
                "保存されているメモリを消去します。この操作は元に戻せません。",
                "リセット",
            ),
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
    let weak = app.as_weak();
    app.global::<SettingsPageState>().on_row_choose(move |id, index| {
        let Some(app) = weak.upgrade() else { return };
        let index = index as usize;
        if id == "scheme" {
            remember(|p| p.scheme = index.min(2) as i32);
            let prefs = PREFS.with(|p| p.borrow().clone());
            gui_prefs::apply(&app, &prefs);
            load(&app, 0);
        } else if let Some(key) = id.strip_prefix("cfg:") {
            if let Some((_, _, opts)) = CHOICES.iter().find(|(k, _, _)| *k == key) {
                if let Some((_, wire)) = opts.get(index) {
                    rpc(
                        "spage.cfg.write",
                        "config/value/write",
                        json!({ "keyPath": key, "value": wire, "mergeStrategy": "replace" }),
                    );
                }
            }
        }
    });
    let weak = app.as_weak();
    app.global::<SettingsPageState>().on_record(move |id, key, ctrl, shift, alt| {
        let Some(app) = weak.upgrade() else { return };
        let sc = Shortcut { key: key.to_string(), ctrl, shift, alt };
        match id.as_str() {
            "key:palette" => remember(|p| p.palette = sc),
            "key:model" => remember(|p| p.model = sc),
            _ => return,
        }
        let prefs = PREFS.with(|p| p.borrow().clone());
        gui_prefs::apply(&app, &prefs);
        set_rows(&app, shortcut_rows());
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
            if tag.ends_with(".write") || tag == "spage.memory.reset" {
                app.global::<AppState>().set_status(format!("設定を変更できませんでした: {e}").into());
                app.global::<AppState>().set_status_level(2);
                return;
            }
            if tag != "spage.account" {
                set_rows(app, vec![row("", "読み込めませんでした", "", e, 0, false)]);
            }
            return;
        }
    };
    match tag {
        "spage.config" => {
            let config = v.get("config").cloned().unwrap_or(Value::Null);
            let mut rows = general_rows(&config);
            let model = config.get("model").and_then(scalar).unwrap_or_default();
            if !model.is_empty() {
                rows.push(row("", "モデル", "", &model, 0, false));
            }
            CONFIG.with(|c| *c.borrow_mut() = config);
            set_rows(app, rows);
        }
        "spage.cfg.write" => load(app, 0),
        "spage.memory" => {
            let n = v.get("v2ConsolidatedThreads").and_then(Value::as_i64).unwrap_or(0);
            let ready = v.get("v2Ready").and_then(Value::as_bool).unwrap_or(false);
            set_rows(
                app,
                vec![
                    row("", "状態", "", if ready { "有効" } else { "準備中" }, 0, false),
                    row("", "統合済みのスレッド", "", &n.to_string(), 0, false),
                    row("memory-reset", "メモリをリセット", "", "リセット", 2, false),
                ],
            );
        }
        "spage.memory.reset" => load(app, 2),
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
        "spage.mcp.reload" => load(app, 5),
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
            load(app, 7);
            crate::sidebar::refresh();
        }
        "spage.usage" => {
            let mut rows = Vec::new();
            let snap = v.get("rateLimits").unwrap_or(&Value::Null);
            for key in ["primary", "secondary"] {
                if let Some(w) = snap.get(key).filter(|w| !w.is_null()) {
                    let used = w.get("usedPercent").and_then(Value::as_i64).unwrap_or(0);
                    let mins = w.get("windowDurationMins").and_then(Value::as_i64);
                    let label = format!("{}の枠", client::rate_window_label(mins));
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
            let (_, label) = client::parse_account(v);
            app.global::<SettingsPageState>().set_plan(label.into());
        }
        _ => {}
    }
}
