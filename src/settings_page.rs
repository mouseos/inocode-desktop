//! The one settings page (gear, account menu, ≡ menu).
//!
//! Layout follows Claude Desktop (left list in groups, content on the right); the items and
//! what they do follow Codex Desktop (ref/docs/18-settings-official.md). Only items that work
//! here are shown. InoCode's own features are in their own group at the end.
//!
//! Official: config/read, config/value/write (approval_policy, sandbox_mode, web_search,
//! model_verbosity, model_reasoning_summary, memories.*, desktop.followUpQueueMode, features.*),
//! experimentalFeature/list, memory/reset, modelProvider/list, hooks/list, skills/*,
//! mcpServerStatus/list, thread/list (archived), account/rateLimits/read, account/read,
//! account/login/start (apiKey), account/logout.

use crate::gui_prefs::{self, Prefs, Shortcut};
use crate::state::*;
use crate::*;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
enum S {
    Head,
    General,
    Config,
    Personal,
    Shortcuts,
    Archive,
    Usage,
    Account,
    Providers,
    Hooks,
    Skills,
    Mcp,
    Inocode,
    Pool,
    SignIn,
}

fn nav() -> Vec<(S, &'static str)> {
    vec![
        (S::Head, "環境設定"),
        (S::General, "一般"),
        (S::Config, "構成"),
        (S::Personal, "パーソナライズ"),
        (S::Shortcuts, "キーボードショートカット"),
        (S::Archive, "アーカイブ済みチャット"),
        (S::Usage, "使用量"),
        (S::Account, "アカウント"),
        (S::Head, "このコンピュータ"),
        (S::Providers, "モデルプロバイダー"),
        (S::Hooks, "フック"),
        (S::Head, "連携"),
        (S::Skills, "スキル"),
        (S::Mcp, "MCP サーバー"),
        (S::Head, "InoCode"),
        (S::Inocode, "InoCode"),
        (S::Pool, "アカウントプール"),
        (S::SignIn, "サインイン"),
    ]
}

fn index_of(s: S) -> i32 {
    nav().iter().position(|(x, _)| *x == s).unwrap_or(1) as i32
}

const SCHEMES: [&str; 3] = ["システム", "ライト", "ダーク"];

/// (config key, label, [(shown, written)]) of the "構成" page (Codex: Agent defaults).
const AGENT_DEFAULTS: [(&str, &str, &[(&str, &str)]); 5] = [
    ("approval_policy", "承認ポリシー", &[("未信頼", "untrusted"), ("リクエスト時", "on-request"), ("承認を求めない", "never")]),
    ("sandbox_mode", "サンドボックス設定", &[("読み取り専用", "read-only"), ("ワークスペース内での書き込み", "workspace-write"), ("フルアクセス", "danger-full-access")]),
    ("web_search", "ウェブ検索", &[("無効", "disabled"), ("キャッシュ済み", "cached"), ("インデックス済み", "indexed"), ("ライブ", "live")]),
    ("model_verbosity", "出力の詳細度", &[("低", "low"), ("中", "medium"), ("高", "high")]),
    ("model_reasoning_summary", "推論の概要", &[("自動", "auto"), ("簡潔", "concise"), ("詳細", "detailed"), ("なし", "none")]),
];

/// Codex: follow-up behavior (desktop.followUpQueueMode).
const FOLLOW_UP: [(&str, &str); 2] = [("キューに追加", "queue"), ("今すぐ反映", "steer")];

/// Replies of the usage page's three requests.
#[derive(Default)]
struct Usage {
    rate: Option<Value>,
    pool: Option<Value>,
    account: Option<Value>,
    got: u8,
}

#[derive(Default)]
struct Data {
    config: Value,
    section: usize,
    usage: Usage,
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

/// Called once at start: restore the saved scheme, sidebar state and shortcuts.
pub(crate) fn init(app: &AppWindow) {
    let prefs = gui_prefs::load();
    gui_prefs::apply(app, &prefs);
    PREFS.with(|p| *p.borrow_mut() = prefs);
}

/// Once the server is ready: read the settings the GUI itself honours.
pub(crate) fn boot() {
    rpc("spage.boot", "config/read", json!({ "includeLayers": false }));
}

fn lookup<'a>(config: &'a Value, key: &str) -> Option<&'a Value> {
    let mut v = config;
    for part in key.split('.') {
        v = v.get(part)?;
    }
    Some(v).filter(|v| !v.is_null())
}

fn remember(update: impl FnOnce(&mut Prefs)) {
    PREFS.with(|p| {
        let mut p = p.borrow_mut();
        update(&mut p);
        gui_prefs::save(&p);
    });
}

pub(crate) fn set_sidebar_open(open: bool) {
    remember(|p| p.sidebar_open = open);
}

pub(crate) fn open(app: &AppWindow) {
    app.global::<SettingsPageState>().set_open(true);
    let cur = app.global::<SettingsPageState>().get_section();
    load(app, if cur <= 0 { index_of(S::General) } else { cur });
}

pub(crate) fn open_usage(app: &AppWindow) {
    app.global::<SettingsPageState>().set_open(true);
    load(app, index_of(S::Usage));
}

/// Show a section without recording a history step (used by back / forward).
pub(crate) fn show_section(app: &AppWindow, section: i32) {
    load(app, section);
}

fn config_dir() -> std::path::PathBuf {
    std::env::var_os("INOCODE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| std::path::PathBuf::from(h).join(".inocode")))
        .unwrap_or_default()
}

fn agents_path() -> std::path::PathBuf {
    config_dir().join("AGENTS.md")
}

fn open_file(path: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd").args(["/C", "start", ""]).arg(path).creation_flags(0x0800_0000).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
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
    let follow = lookup(config, "desktop.followUpQueueMode").and_then(Value::as_str).unwrap_or("queue");
    let shown = FOLLOW_UP.iter().find(|(_, w)| *w == follow).map(|(s, _)| *s).unwrap_or(FOLLOW_UP[0].0);
    vec![
        choice("scheme", "テーマ", SCHEMES[scheme.min(2)], SCHEMES.iter().map(|s| s.to_string()).collect()),
        choice("follow", "フォローアップの動作", shown, FOLLOW_UP.iter().map(|(s, _)| s.to_string()).collect()),
    ]
}

fn config_rows(config: &Value) -> Vec<SRow> {
    let mut rows = vec![row("open-config", "config.toml", "", "ファイルを開く", 2, false)];
    for (key, label, opts) in AGENT_DEFAULTS {
        let wire = lookup(config, key).and_then(Value::as_str).unwrap_or("");
        let shown = opts.iter().find(|(_, w)| *w == wire).map(|(s, _)| *s).unwrap_or("");
        rows.push(choice(&format!("cfg:{key}"), label, shown, opts.iter().map(|(s, _)| s.to_string()).collect()));
    }
    let net = lookup(config, "sandbox_workspace_write.network_access").and_then(Value::as_bool).unwrap_or(false);
    rows.push(row("cfgb:sandbox_workspace_write.network_access", "ネットワークアクセスを許可", "", "", 1, net));
    rows
}

fn personal_rows(config: &Value) -> Vec<SRow> {
    let b = |k: &str, d: bool| lookup(config, k).and_then(Value::as_bool).unwrap_or(d);
    vec![
        row("cfgb:memories.use_memories", "Codex のメモリを有効にする", "", "", 1, b("memories.use_memories", false)),
        row("cfgb:memories.generate_memories", "メモリを生成する", "", "", 1, b("memories.generate_memories", false)),
        // The config key says the opposite: disable_on_external_context.
        row("cfgi:memories.disable_on_external_context", "ツールを使ったチャットからのメモリ生成を許可", "", "", 1, !b("memories.disable_on_external_context", false)),
        row("memory-reset", "Codex のメモリをすべて削除", "", "削除", 2, false),
    ]
}

fn load(app: &AppWindow, section: i32) {
    let list = nav();
    let s = app.global::<SettingsPageState>();
    let mut idx = (section.max(0) as usize).min(list.len() - 1);
    if list[idx].0 == S::Head {
        idx += 1;
    }
    DATA.with(|d| d.borrow_mut().section = idx);
    crate::nav::visit(app, crate::nav::Entry::Settings(idx as i32));
    s.set_section(idx as i32);
    s.set_title(list[idx].1.into());
    s.set_recording_id("".into());
    s.set_login_note("".into());
    s.set_layout(0);
    set_rows(app, Vec::new());
    s.set_loading(true);
    match list[idx].0 {
        S::Head => {}
        S::General | S::Config => rpc("spage.config", "config/read", json!({ "includeLayers": false })),
        S::Personal => {
            s.set_layout(4);
            reload_agents(app);
            rpc("spage.config", "config/read", json!({ "includeLayers": false }));
        }
        S::Shortcuts => set_rows(app, shortcut_rows()),
        S::Archive => rpc("spage.archive", "thread/list", json!({ "limit": 50, "archived": true })),
        S::Usage => {
            s.set_layout(5);
            DATA.with(|d| d.borrow_mut().usage = Usage::default());
            s.set_usage(ModelRc::from(Rc::new(VecModel::<UsageAccount>::default())));
            rpc("spage.usage", "account/rateLimits/read", Value::Null);
            rpc("spage.usage.pool", "inocode/accounts/list", json!({ "refresh": true }));
            rpc("spage.usage.account", "account/read", json!({ "refreshToken": false }));
        }
        S::Account => rpc("spage.account", "account/read", json!({ "refreshToken": false })),
        S::Providers => {
            s.set_layout(3);
            rpc("spage.prov", "modelProvider/list", json!({}));
        }
        S::Hooks => {
            let cwd = STATE.with(|st| st.borrow().active_cwd.clone());
            rpc("spage.hooks", "hooks/list", json!({ "cwds": [cwd] }));
        }
        S::Skills => {
            let cwd = STATE.with(|st| st.borrow().active_cwd.clone());
            rpc("spage.skills", "skills/list", json!({ "cwds": [cwd] }));
        }
        S::Mcp => rpc("spage.mcp", "mcpServerStatus/list", json!({})),
        S::Inocode => {
            s.set_layout(1);
            s.set_loading(false);
            app.global::<Tabs>().invoke_opened(4);
        }
        S::Pool => {
            s.set_layout(2);
            s.set_loading(false);
            app.global::<Tabs>().invoke_opened(4);
        }
        S::SignIn => {
            s.set_layout(6);
            s.set_loading(false);
        }
    }
}

fn reload_agents(app: &AppWindow) {
    let s = app.global::<SettingsPageState>();
    let path = agents_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            s.set_raw_text(text.into());
            s.set_raw_note(path.display().to_string().into());
        }
        Err(_) => {
            s.set_raw_text("".into());
            s.set_raw_note(format!("{}(保存すると作成します)", path.display()).into());
        }
    }
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
    let items: Vec<NavItem> = nav().into_iter().map(|(s, l)| NavItem { label: l.into(), heading: s == S::Head }).collect();
    st.set_sections(ModelRc::from(Rc::new(VecModel::from(items))));

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
    st.on_open_providers(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<SettingsPageState>().set_open(true);
            load(&app, index_of(S::Providers));
        }
    });
    let weak = app.as_weak();
    st.on_open_signin(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<SettingsPageState>().set_open(true);
            load(&app, index_of(S::SignIn));
        }
    });
    let weak = app.as_weak();
    st.on_select(move |i| {
        if let Some(app) = weak.upgrade() {
            load(&app, i);
        }
    });
    let weak = app.as_weak();
    st.on_row_action(move |id| {
        let Some(app) = weak.upgrade() else { return };
        match id.as_str() {
            "open-config" => open_file(&config_dir().join("config.toml")),
            "mcp-reload" => rpc("spage.mcp.reload", "config/mcpServer/reload", json!({})),
            "memory-reset" => crate::sidebar::ask_request(
                &app,
                "spage.memory.reset",
                "memory/reset",
                "Codex のメモリをすべて削除しますか？",
                "保存されているメモリを消去します。この操作は元に戻せません。",
                "削除",
            ),
            "logout" => {
                rpc("chat.logout", "account/logout", Value::Null);
                app.global::<SettingsPageState>().set_open(false);
            }
            a if a.starts_with("unarchive:") => rpc("spage.unarchive", "thread/unarchive", json!({ "threadId": &a[10..] })),
            a if a.starts_with("prov-default:") => write_config("model_provider", json!(&a[13..])),
            _ => {}
        }
    });
    st.on_row_toggle(|id, on| {
        if let Some(path) = id.strip_prefix("skill:") {
            rpc("spage.skill.write", "skills/config/write", json!({ "path": path, "enabled": on }));
        } else if let Some(name) = id.strip_prefix("feat:") {
            write_config(&format!("features.{name}"), json!(on));
        } else if let Some(key) = id.strip_prefix("cfgb:") {
            write_config(key, json!(on));
        } else if let Some(key) = id.strip_prefix("cfgi:") {
            write_config(key, json!(!on));
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
            load(&app, index_of(S::General));
        } else if id == "follow" {
            if let Some((_, wire)) = FOLLOW_UP.get(index) {
                crate::prompts::set_follow_steer(*wire == "steer");
                write_config("desktop.followUpQueueMode", json!(wire));
            }
        } else if let Some(key) = id.strip_prefix("cfg:") {
            if let Some((_, _, opts)) = AGENT_DEFAULTS.iter().find(|(k, _, _)| *k == key) {
                if let Some((_, wire)) = opts.get(index) {
                    write_config(key, json!(wire));
                }
            }
        }
    });
    st.on_row_text(|_, _| {});
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
        let path = agents_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&path, s.get_raw_text().as_str()) {
            Ok(()) => s.set_raw_note(format!("{} に保存しました", path.display()).into()),
            Err(e) => s.set_raw_note(format!("保存できませんでした: {e}").into()),
        }
    });
    let weak = app.as_weak();
    st.on_reload_raw(move || {
        if let Some(app) = weak.upgrade() {
            reload_agents(&app);
        }
    });
    st.on_open_config_file(|| open_file(&config_dir().join("config.toml")));
    let weak = app.as_weak();
    st.on_switch_transport(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<AppState>().invoke_switch_transport();
        }
    });
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
                "spage.account" | "spage.boot" | "spage.feat" => {}
                "spage.usage" | "spage.usage.pool" | "spage.usage.account" => usage_reply(app, tag, None),
                _ => set_rows(app, vec![row("", "読み込めませんでした", "", e, 0, false)]),
            }
            return;
        }
    };
    let list = nav();
    let current = DATA.with(|d| d.borrow().section);
    let here = list.get(current).map(|s| s.0);
    match tag {
        "spage.boot" => {
            let config = v.get("config").cloned().unwrap_or(Value::Null);
            let steer = lookup(&config, "desktop.followUpQueueMode").and_then(Value::as_str) == Some("steer");
            crate::prompts::set_follow_steer(steer);
            DATA.with(|d| d.borrow_mut().config = config);
        }
        "spage.config" => {
            let config = v.get("config").cloned().unwrap_or(Value::Null);
            DATA.with(|d| d.borrow_mut().config = config.clone());
            match here {
                Some(S::General) => {
                    set_rows(app, general_rows(&config));
                    // The beta features follow once the list arrives.
                    rpc("spage.feat", "experimentalFeature/list", json!({ "limit": 100 }));
                }
                Some(S::Config) => set_rows(app, config_rows(&config)),
                Some(S::Personal) => set_rows(app, personal_rows(&config)),
                _ => {}
            }
        }
        "spage.feat" => {
            if !matches!(here, Some(S::General)) {
                return;
            }
            let config = DATA.with(|d| d.borrow().config.clone());
            let mut rows = general_rows(&config);
            let mut any = false;
            for f in v.get("data").and_then(Value::as_array).into_iter().flatten() {
                if f.get("stage").and_then(Value::as_str) != Some("beta") {
                    continue;
                }
                let name = f.get("name").and_then(Value::as_str).unwrap_or("");
                let title = f.get("displayName").and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or(name);
                let desc = f.get("description").and_then(Value::as_str).unwrap_or("");
                rows.push(row(&format!("feat:{name}"), title, desc, "", 1, f.get("enabled").and_then(Value::as_bool).unwrap_or(false)));
                any = true;
            }
            if !any {
                rows.push(row("", "実験機能(ベータ版)", "", "ベータ版の実験機能はありません", 0, false));
            }
            set_rows(app, rows);
        }
        "spage.cfg.write" => {
            note(app, "");
            match here {
                Some(S::Providers) => load(app, current as i32),
                _ => rpc("spage.config", "config/read", json!({ "includeLayers": false })),
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
        "spage.memory.reset" => load(app, current as i32),
        "spage.hooks" => {
            let mut rows = Vec::new();
            for entry in v.get("data").and_then(Value::as_array).into_iter().flatten() {
                for h in entry.get("hooks").and_then(Value::as_array).into_iter().flatten() {
                    let ev = h.get("eventName").and_then(Value::as_str).unwrap_or("");
                    let what = h
                        .get("command")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| h.get("handlerType").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_default();
                    let trust = h.get("trustStatus").and_then(Value::as_str).unwrap_or("");
                    rows.push(row("", ev, &what, trust, 0, false));
                }
            }
            if rows.is_empty() {
                rows.push(row("", "フックはありません", "", "", 0, false));
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
            rows.push(row("open-config", "config.toml", "", "ファイルを開く", 2, false));
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
        "spage.usage" | "spage.usage.pool" | "spage.usage.account" => usage_reply(app, tag, Some(v.clone())),
        "spage.account" => {
            let (logged_in, label) = client::parse_account(v);
            if matches!(here, Some(S::Account)) {
                let mut rows = vec![row("", "プラン", "", &label, 0, false)];
                if logged_in {
                    rows.push(row("logout", "ログアウト", "", "ログアウト", 2, false));
                }
                set_rows(app, rows);
            }
        }
        _ => {}
    }
}

fn window_name(mins: Option<i64>) -> String {
    match mins {
        Some(m) if m >= 10_000 => "週".to_string(),
        Some(m) if m >= 60 && m % 60 == 0 => format!("{} 時間", m / 60),
        Some(m) => format!("{m} 分"),
        None => "枠".to_string(),
    }
}

/// "あと 3 時間 10 分でリセット" until `resets_at` (Unix seconds).
fn until(resets_at: Option<i64>) -> String {
    let Some(at) = resets_at else { return String::new() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let secs = (at - now).max(0);
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    let text = if d > 0 { format!("{d} 日 {h} 時間") } else if h > 0 { format!("{h} 時間 {m} 分") } else { format!("{m} 分") };
    format!("あと {text} でリセット")
}

fn bar(label: &str, w: &Value) -> Option<UsageBar> {
    let pct = w.get("usedPercent").and_then(Value::as_f64)?;
    Some(UsageBar { label: label.into(), pct: pct as f32, reset: until(w.get("resetsAt").and_then(Value::as_i64)).into() })
}

/// Bars of an `account/rateLimits/read` snapshot.
fn snapshot_bars(snap: &Value) -> Vec<UsageBar> {
    ["primary", "secondary"]
        .iter()
        .filter_map(|k| snap.get(*k).filter(|w| !w.is_null()))
        .filter_map(|w| bar(&window_name(w.get("windowDurationMins").and_then(Value::as_i64)), w))
        .collect()
}

fn usage_reply(app: &AppWindow, tag: &str, v: Option<Value>) {
    let done = DATA.with(|d| {
        let mut d = d.borrow_mut();
        match tag {
            "spage.usage" => d.usage.rate = v,
            "spage.usage.pool" => d.usage.pool = v,
            _ => d.usage.account = v,
        }
        d.usage.got += 1;
        d.usage.got >= 3
    });
    if !done {
        return;
    }
    let (rate, pool, account) = DATA.with(|d| {
        let d = d.borrow();
        (d.usage.rate.clone(), d.usage.pool.clone(), d.usage.account.clone())
    });
    let snap = rate.as_ref().and_then(|r| r.get("rateLimits")).cloned().unwrap_or(Value::Null);
    let email = account.as_ref().and_then(|a| a.pointer("/account/email")).and_then(Value::as_str).unwrap_or("").to_string();
    let plan_now = account.as_ref().and_then(|a| a.pointer("/account/planType")).and_then(Value::as_str).unwrap_or("").to_string();
    let credits = |snap: &Value| -> String {
        match snap.get("credits").filter(|c| !c.is_null()) {
            Some(c) if c.get("unlimited").and_then(Value::as_bool).unwrap_or(false) => "無制限".to_string(),
            Some(c) if c.get("hasCredits").and_then(Value::as_bool).unwrap_or(false) => c.get("balance").and_then(Value::as_str).unwrap_or("").to_string(),
            _ => String::new(),
        }
    };
    let banked = rate
        .as_ref()
        .and_then(|r| r.pointer("/rateLimitResetCredits/availableCount"))
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .map(|n| format!("{n} 件"))
        .unwrap_or_default();

    let mut cards: Vec<UsageAccount> = Vec::new();
    let accounts = pool.as_ref().and_then(|p| p.get("accounts")).and_then(Value::as_array).cloned().unwrap_or_default();
    for a in &accounts {
        let in_use = a.get("inUse").and_then(Value::as_bool).unwrap_or(false);
        let mut bars = Vec::new();
        if let Some(u) = a.get("usage").filter(|u| !u.is_null()) {
            for (key, name) in [("fiveHour", "5 時間"), ("weekly", "週")] {
                if let Some(b) = u.get(key).filter(|w| !w.is_null()).and_then(|w| bar(name, w)) {
                    bars.push(b);
                }
            }
        }
        if bars.is_empty() && in_use {
            bars = snapshot_bars(&snap);
        }
        // The server tells the e-mail address only for the signed-in account.
        let shows_email = a.get("isDefault").and_then(Value::as_bool).unwrap_or(false) || (in_use && accounts.len() == 1);
        cards.push(UsageAccount {
            alias: a.get("alias").and_then(Value::as_str).unwrap_or("").into(),
            email: if shows_email { email.as_str().into() } else { "".into() },
            plan: a.get("plan").and_then(Value::as_str).unwrap_or("").into(),
            in_use,
            bars: ModelRc::from(Rc::new(VecModel::from(bars))),
            credits: if in_use { credits(&snap).into() } else { "".into() },
            banked: if in_use { banked.as_str().into() } else { "".into() },
        });
    }
    if cards.is_empty() {
        // No pool: the signed-in account only.
        let (_, label) = account.as_ref().map(client::parse_account).unwrap_or((false, "アカウント".to_string()));
        cards.push(UsageAccount {
            alias: label.as_str().into(),
            email: email.as_str().into(),
            plan: plan_now.as_str().into(),
            in_use: true,
            bars: ModelRc::from(Rc::new(VecModel::from(snapshot_bars(&snap)))),
            credits: credits(&snap).into(),
            banked: banked.as_str().into(),
        });
    }
    let s = app.global::<SettingsPageState>();
    s.set_usage(ModelRc::from(Rc::new(VecModel::from(cards))));
    s.set_loading(false);
}
