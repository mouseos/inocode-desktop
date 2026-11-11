//! The one settings page (gear and account menu).
//!
//! Sections driven by the Codex config table (src/settings_items.rs: general, approval and
//! sandbox, context and history, notifications and privacy, display, agents and memory), the
//! feature flags, model providers, profiles and project trust, MCP servers, skills, shortcuts,
//! memory, InoCode, account (sign-in with ChatGPT or an API key, account pool), archived
//! sessions, usage, and the raw config.toml.
//!
//! Official: config/read, config/value/write, experimentalFeature/list, modelProvider/list,
//! account/login/start (apiKey), memory/status|reset, mcpServerStatus/list, skills/*,
//! thread/list archived, account/rateLimits/read. Claude Desktop: full-page settings with the
//! list on the left and a search field (cd19fedd2, cf400e6a4).
//! Coverage of the Codex keys: ref/docs/17-settings-coverage.md.

use crate::gui_prefs::{self, Prefs, Shortcut};
use crate::settings_items::{self as items, Item, K};
use crate::state::*;
use crate::*;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
enum S {
    Cat(usize),
    Features,
    Providers,
    Profiles,
    Mcp,
    Skills,
    Shortcuts,
    Memory,
    Inocode,
    Account,
    Archive,
    Usage,
    Raw,
}

fn sections() -> Vec<(S, String)> {
    let mut v: Vec<(S, String)> = items::CATEGORIES.iter().enumerate().map(|(i, n)| (S::Cat(i), n.to_string())).collect();
    v.extend([
        (S::Features, "機能フラグ".to_string()),
        (S::Providers, "モデルプロバイダ".to_string()),
        (S::Profiles, "プロファイルと信頼".to_string()),
        (S::Mcp, "MCP サーバー".to_string()),
        (S::Skills, "スキル".to_string()),
        (S::Shortcuts, "ショートカット".to_string()),
        (S::Memory, "メモリ".to_string()),
        (S::Inocode, "InoCode".to_string()),
        (S::Account, "アカウント".to_string()),
        (S::Archive, "アーカイブ".to_string()),
        (S::Usage, "使用量".to_string()),
        (S::Raw, "詳細(config.toml)".to_string()),
    ]);
    v
}

const SCHEMES: [&str; 3] = ["システム", "ライト", "ダーク"];

#[derive(Default)]
struct Data {
    config: Value,
    section: usize,
    features: Vec<(String, String, bool)>,
    /// An API key typed for the pool, kept only until the answer arrives (fallback to the plain login).
    pending_key: Option<String>,
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static PREFS: RefCell<Prefs> = RefCell::new(Prefs::default());
    static DATA: RefCell<Data> = RefCell::new(Data::default());
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

fn note(app: &AppWindow, text: &str) {
    app.global::<SettingsPageState>().set_login_note(text.into());
}

/// Called once at start: restore the saved scheme and shortcuts.
pub(crate) fn init(app: &AppWindow) {
    let prefs = gui_prefs::load();
    gui_prefs::apply(app, &prefs);
    PREFS.with(|p| *p.borrow_mut() = prefs);
}

/// The "カスタマイズ" row: the skills page of the settings.
pub(crate) fn open_customize(app: &AppWindow) {
    let idx = sections().iter().position(|(s, _)| *s == S::Skills).unwrap_or(0);
    app.global::<SettingsPageState>().set_open(true);
    load(app, idx as i32);
}

pub(crate) fn open(app: &AppWindow) {
    app.global::<SettingsPageState>().set_open(true);
    load(app, app.global::<SettingsPageState>().get_section());
}

fn config_path() -> std::path::PathBuf {
    let home = std::env::var_os("INOCODE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| std::path::PathBuf::from(h).join(".inocode")))
        .unwrap_or_default();
    home.join("config.toml")
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

/// The row of one config item, with its current value.
fn item_row(it: &Item, config: &Value) -> SRow {
    let id = format!("cfg:{}", it.key);
    let cur = items::lookup(config, it.key);
    match &it.kind {
        K::Bool => row(&id, it.label, "", "", 1, cur.and_then(Value::as_bool).unwrap_or(false)),
        K::Choice(opts) => {
            let wire = cur.and_then(Value::as_str).unwrap_or("");
            let shown = opts.iter().find(|(_, w)| *w == wire).map(|(s, _)| *s).unwrap_or("");
            choice(&id, it.label, shown, opts.iter().map(|(s, _)| s.to_string()).collect())
        }
        K::Int => row(&id, it.label, "", &cur.and_then(Value::as_i64).map(|n| n.to_string()).unwrap_or_default(), 5, false),
        K::Text => row(&id, it.label, "", cur.and_then(Value::as_str).unwrap_or(""), 5, false),
        K::List => {
            let line = cur
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            row(&id, it.label, "", &line, 5, false)
        }
    }
}

fn scheme_row() -> SRow {
    let scheme = PREFS.with(|p| p.borrow().scheme) as usize;
    choice("scheme", "テーマ", SCHEMES[scheme.min(2)], SCHEMES.iter().map(|s| s.to_string()).collect())
}

fn show_layout(app: &AppWindow, layout: i32) {
    app.global::<SettingsPageState>().set_layout(layout);
}

/// Show a section without recording a history step (used by back / forward).
pub(crate) fn show_section(app: &AppWindow, section: i32) {
    load(app, section);
}

pub(crate) fn set_sidebar_open(open: bool) {
    remember(|p| p.sidebar_open = open);
}

fn load(app: &AppWindow, section: i32) {
    let list = sections();
    let s = app.global::<SettingsPageState>();
    let idx = (section.max(0) as usize).min(list.len() - 1);
    DATA.with(|d| d.borrow_mut().section = idx);
    crate::nav::visit(app, crate::nav::Entry::Settings(idx as i32));
    s.set_section(idx as i32);
    s.set_search("".into());
    s.set_title(list[idx].1.as_str().into());
    s.set_recording_id("".into());
    s.set_login_note("".into());
    show_layout(app, 0);
    set_rows(app, Vec::new());
    s.set_loading(true);
    match list[idx].0 {
        S::Cat(_) | S::Profiles => rpc("spage.config", "config/read", json!({ "includeLayers": false })),
        S::Features => {
            DATA.with(|d| d.borrow_mut().features.clear());
            rpc("spage.feat", "experimentalFeature/list", json!({ "limit": 100 }));
        }
        S::Providers => {
            show_layout(app, 3);
            rpc("spage.prov", "modelProvider/list", json!({}));
        }
        S::Mcp => rpc("spage.mcp", "mcpServerStatus/list", json!({})),
        S::Skills => {
            let cwd = STATE.with(|st| st.borrow().active_cwd.clone());
            rpc("spage.skills", "skills/list", json!({ "cwds": [cwd] }));
        }
        S::Shortcuts => set_rows(app, shortcut_rows()),
        S::Memory => rpc("spage.memory", "memory/status", json!({})),
        S::Inocode => {
            show_layout(app, 1);
            s.set_loading(false);
            app.global::<Tabs>().invoke_opened(4);
        }
        S::Account => {
            show_layout(app, 2);
            s.set_loading(false);
            app.global::<Tabs>().invoke_opened(4);
            rpc("spage.account", "account/read", json!({ "refreshToken": false }));
        }
        S::Archive => rpc("spage.archive", "thread/list", json!({ "limit": 50, "archived": true })),
        S::Usage => rpc("spage.usage", "account/rateLimits/read", Value::Null),
        S::Raw => {
            show_layout(app, 4);
            s.set_loading(false);
            reload_raw(app);
        }
    }
}

fn reload_raw(app: &AppWindow) {
    let s = app.global::<SettingsPageState>();
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            s.set_raw_text(text.into());
            s.set_raw_note(path.display().to_string().into());
        }
        Err(_) => {
            s.set_raw_text("".into());
            s.set_raw_note(format!("{}(まだありません。保存すると作成します)", path.display()).into());
        }
    }
}

fn search(app: &AppWindow, query: &str) {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        load(app, app.global::<SettingsPageState>().get_section());
        return;
    }
    let config = DATA.with(|d| d.borrow().config.clone());
    if config.is_null() {
        rpc("spage.config", "config/read", json!({ "includeLayers": false }));
        return;
    }
    show_layout(app, 0);
    let rows: Vec<SRow> = items::ITEMS
        .iter()
        .filter(|it| it.label.to_lowercase().contains(&q) || it.key.to_lowercase().contains(&q))
        .map(|it| item_row(it, &config))
        .collect();
    set_rows(app, rows);
}

fn remember(update: impl FnOnce(&mut Prefs)) {
    PREFS.with(|p| {
        let mut p = p.borrow_mut();
        update(&mut p);
        gui_prefs::save(&p);
    });
}

fn write_config(key: &str, value: Value) {
    rpc("spage.cfg.write", "config/value/write", json!({ "keyPath": key, "value": value, "mergeStrategy": "replace" }));
}

fn upsert_config(key: &str, value: Value) {
    rpc("spage.cfg.write", "config/value/write", json!({ "keyPath": key, "value": value, "mergeStrategy": "upsert" }));
}

fn slug(name: &str) -> String {
    let s: String = name.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "custom".to_string() } else { s }
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let st = app.global::<SettingsPageState>();
    st.set_sections(strings(sections().into_iter().map(|(_, n)| n)));

    let weak = app.as_weak();
    st.on_open_page(move || {
        if let Some(app) = weak.upgrade() {
            open(&app);
        }
    });
    let weak = app.as_weak();
    st.on_open_section(move |i| {
        if let Some(app) = weak.upgrade() {
            app.global::<SettingsPageState>().set_open(true);
            load(&app, i);
        }
    });
    let weak = app.as_weak();
    st.on_select(move |i| {
        if let Some(app) = weak.upgrade() {
            load(&app, i);
        }
    });
    let weak = app.as_weak();
    st.on_do_search(move |q| {
        if let Some(app) = weak.upgrade() {
            search(&app, &q);
        }
    });
    let weak = app.as_weak();
    st.on_row_action(move |id| {
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
            "proj-add" => {
                if let Some(dir) = rfd::FileDialog::new().set_title("信頼するフォルダを選択").pick_folder() {
                    upsert_config("projects", json!({ dir.to_string_lossy(): { "trust_level": "trusted" } }));
                }
            }
            a if a.starts_with("unarchive:") => rpc("spage.unarchive", "thread/unarchive", json!({ "threadId": &a[10..] })),
            a if a.starts_with("prov-default:") => write_config("model_provider", json!(&a[13..])),
            a if a.starts_with("profile-use:") => write_config("profile", json!(&a[12..])),
            _ => {}
        }
    });
    st.on_row_toggle(|id, on| {
        if let Some(path) = id.strip_prefix("skill:") {
            rpc("spage.skill.write", "skills/config/write", json!({ "path": path, "enabled": on }));
        } else if let Some(name) = id.strip_prefix("feat:") {
            write_config(&format!("features.{name}"), json!(on));
        } else if let Some(key) = id.strip_prefix("cfg:") {
            write_config(key, json!(on));
        }
    });
    let weak = app.as_weak();
    st.on_row_choose(move |id, index| {
        let Some(app) = weak.upgrade() else { return };
        let index = index as usize;
        if id == "scheme" {
            remember(|p| p.scheme = index.min(2) as i32);
            let prefs = PREFS.with(|p| p.borrow().clone());
            gui_prefs::apply(&app, &prefs);
            load(&app, 0);
        } else if let Some(path) = id.strip_prefix("proj:") {
            let level = if index == 0 { "trusted" } else { "untrusted" };
            upsert_config("projects", json!({ path: { "trust_level": level } }));
        } else if let Some(key) = id.strip_prefix("cfg:") {
            if let Some(Item { kind: K::Choice(opts), .. }) = items::ITEMS.iter().find(|i| i.key == key) {
                if let Some((_, wire)) = opts.get(index) {
                    write_config(key, json!(wire));
                }
            }
        }
    });
    let weak = app.as_weak();
    st.on_row_text(move |id, text| {
        let Some(app) = weak.upgrade() else { return };
        let Some(key) = id.strip_prefix("cfg:") else { return };
        let Some(it) = items::ITEMS.iter().find(|i| i.key == key) else { return };
        let text = text.trim();
        let value = match &it.kind {
            _ if text.is_empty() => Value::Null,
            K::Int => match text.parse::<i64>() {
                Ok(n) => json!(n),
                Err(_) => {
                    app.global::<AppState>().set_status(format!("{} には数値を入力してください", it.label).into());
                    app.global::<AppState>().set_status_level(2);
                    return;
                }
            },
            K::List => json!(text.split_whitespace().collect::<Vec<_>>()),
            _ => json!(text),
        };
        write_config(key, value);
    });
    let weak = app.as_weak();
    st.on_record(move |id, key, ctrl, shift, alt| {
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
    let weak = app.as_weak();
    st.on_login_chatgpt(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<AppState>().invoke_login();
            note(&app, "ブラウザでログインしてください");
        }
    });
    let weak = app.as_weak();
    st.on_login_key(move || {
        let Some(app) = weak.upgrade() else { return };
        let s = app.global::<SettingsPageState>();
        let key = s.get_key_value().trim().to_string();
        let alias = s.get_key_alias().trim().to_string();
        if key.is_empty() {
            note(&app, "API キーを入力してください");
            return;
        }
        // The key leaves the field at once; it is never shown again.
        s.set_key_value("".into());
        if alias.is_empty() {
            rpc("spage.login.key", "account/login/start", json!({ "type": "apiKey", "apiKey": key }));
        } else {
            DATA.with(|d| d.borrow_mut().pending_key = Some(key.clone()));
            rpc("spage.addkey", "inocode/accounts/addApiKey", json!({ "alias": alias, "apiKey": key }));
        }
        note(&app, "送信しています…");
    });
    let weak = app.as_weak();
    st.on_add_provider(move || {
        let Some(app) = weak.upgrade() else { return };
        let s = app.global::<SettingsPageState>();
        let (name, url) = (s.get_prov_name().trim().to_string(), s.get_prov_url().trim().to_string());
        let (env, key) = (s.get_prov_env().trim().to_string(), s.get_prov_key().trim().to_string());
        if name.is_empty() || url.is_empty() || (env.is_empty() && key.is_empty()) {
            note(&app, "名前、base URL、API キー(環境変数名か値)を入力してください");
            return;
        }
        let mut cfg = json!({ "name": name, "base_url": url, "wire_api": "responses", "requires_openai_auth": false });
        if !env.is_empty() {
            cfg["env_key"] = json!(env);
        }
        if !key.is_empty() {
            cfg["experimental_bearer_token"] = json!(key);
        }
        s.set_prov_key("".into());
        upsert_config(&format!("model_providers.{}", slug(&name)), cfg);
        note(&app, "追加しています…");
    });
    let weak = app.as_weak();
    st.on_save_raw(move || {
        let Some(app) = weak.upgrade() else { return };
        let s = app.global::<SettingsPageState>();
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if path.exists() {
            let _ = std::fs::copy(&path, path.with_extension("toml.bak"));
        }
        match std::fs::write(&path, s.get_raw_text().as_str()) {
            Ok(()) => {
                s.set_raw_note(format!("{} に保存しました(元のファイルは .bak に残しています)", path.display()).into());
                rpc("spage.config.touch", "config/read", json!({ "includeLayers": false }));
            }
            Err(e) => s.set_raw_note(format!("保存できませんでした: {e}").into()),
        }
    });
    let weak = app.as_weak();
    st.on_reload_raw(move || {
        if let Some(app) = weak.upgrade() {
            reload_raw(&app);
        }
    });
}

fn config_rows(section: S, config: &Value) -> Vec<SRow> {
    match section {
        S::Cat(c) => {
            let mut rows = Vec::new();
            if c == 0 {
                rows.push(scheme_row());
            }
            rows.extend(items::ITEMS.iter().filter(|i| i.cat == c).map(|i| item_row(i, config)));
            rows
        }
        _ => {
            // Profiles and project trust.
            let mut rows = Vec::new();
            let active = config.get("profile").and_then(Value::as_str).unwrap_or("");
            if let Some(map) = config.get("profiles").and_then(Value::as_object) {
                for name in map.keys() {
                    if name == active {
                        rows.push(row("", name, "", "使用中", 0, false));
                    } else {
                        rows.push(row(&format!("profile-use:{name}"), name, "", "使う", 2, false));
                    }
                }
            }
            if rows.is_empty() {
                rows.push(row("", "プロファイルはありません", "", "", 0, false));
            }
            if let Some(map) = config.get("projects").and_then(Value::as_object) {
                for (path, v) in map {
                    let level = v.get("trust_level").and_then(Value::as_str).unwrap_or("");
                    let shown = if level == "trusted" { "信頼する" } else if level == "untrusted" { "信頼しない" } else { "" };
                    rows.push(choice(&format!("proj:{path}"), path, shown, vec!["信頼する".into(), "信頼しない".into()]));
                }
            }
            rows.push(row("proj-add", "信頼するフォルダを追加", "", "フォルダを選択", 2, false));
            rows
        }
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if !tag.starts_with("spage.") {
        return;
    }
    let v = match result {
        Ok(v) => v,
        Err(e) => {
            match tag {
                "spage.addkey" => {
                    // The pool is not available: use the plain API-key login.
                    if let Some(key) = DATA.with(|d| d.borrow_mut().pending_key.take()) {
                        rpc("spage.login.key", "account/login/start", json!({ "type": "apiKey", "apiKey": key }));
                    }
                }
                "spage.login.key" => note(app, &format!("ログインできませんでした: {e}")),
                t if t.ends_with(".write") || t == "spage.memory.reset" => {
                    app.global::<AppState>().set_status(format!("設定を変更できませんでした: {e}").into());
                    app.global::<AppState>().set_status_level(2);
                    note(app, &format!("変更できませんでした: {e}"));
                }
                "spage.account" | "spage.config.touch" => {}
                _ => set_rows(app, vec![row("", "読み込めませんでした", "", e, 0, false)]),
            }
            return;
        }
    };
    let list = sections();
    let current = DATA.with(|d| d.borrow().section);
    match tag {
        "spage.config" => {
            let config = v.get("config").cloned().unwrap_or(Value::Null);
            DATA.with(|d| d.borrow_mut().config = config.clone());
            let query = app.global::<SettingsPageState>().get_search().to_string();
            if !query.trim().is_empty() {
                search(app, &query);
            } else if matches!(list.get(current).map(|s| s.0), Some(S::Cat(_) | S::Profiles)) {
                set_rows(app, config_rows(list[current].0, &config));
            }
        }
        "spage.config.touch" => {
            DATA.with(|d| d.borrow_mut().config = v.get("config").cloned().unwrap_or(Value::Null));
        }
        "spage.cfg.write" => {
            note(app, "");
            match list.get(current).map(|s| s.0) {
                Some(S::Providers) => load(app, current as i32),
                Some(S::Features) => {}
                _ => rpc("spage.config", "config/read", json!({ "includeLayers": false })),
            }
        }
        "spage.feat" => {
            let mut next = None;
            DATA.with(|d| {
                let mut d = d.borrow_mut();
                for f in v.get("data").and_then(Value::as_array).into_iter().flatten() {
                    let name = f.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    let title = f.get("displayName").and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or(&name).to_string();
                    d.features.push((name, title, f.get("enabled").and_then(Value::as_bool).unwrap_or(false)));
                }
                next = v.get("nextCursor").and_then(Value::as_str).map(str::to_string);
            });
            if let Some(c) = next {
                rpc("spage.feat", "experimentalFeature/list", json!({ "limit": 100, "cursor": c }));
            } else if matches!(list.get(current).map(|s| s.0), Some(S::Features)) {
                let rows: Vec<SRow> = DATA.with(|d| {
                    d.borrow().features.iter().map(|(n, t, on)| row(&format!("feat:{n}"), t, "", "", 1, *on)).collect()
                });
                set_rows(app, rows);
            }
        }
        "spage.prov" => {
            let default = v.get("defaultProviderId").and_then(Value::as_str).unwrap_or("");
            let rows: Vec<SRow> = v
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|p| {
                    let id = p.get("id").and_then(Value::as_str).unwrap_or("");
                    let name = p.get("name").and_then(Value::as_str).unwrap_or(id);
                    let url = p.get("baseUrl").and_then(Value::as_str).unwrap_or("");
                    if id == default {
                        row("", name, url, "既定", 0, false)
                    } else {
                        row(&format!("prov-default:{id}"), name, url, "既定にする", 2, false)
                    }
                })
                .collect();
            set_rows(app, rows);
        }
        "spage.login.key" | "spage.addkey" => {
            DATA.with(|d| d.borrow_mut().pending_key = None);
            note(app, "API キーを登録しました");
        }
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
        "spage.memory.reset" => load(app, current as i32),
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
        "spage.mcp.reload" => load(app, current as i32),
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
            load(app, current as i32);
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
