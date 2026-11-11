//! The account page of the settings: the accounts of the pool grouped by provider, adding an
//! account (ChatGPT, Claude, API key) and the selection policy.
//!
//! Contract: ref/docs/16-account-pool-contract.md (inocode/accounts/list, use, setEnabled, rename,
//! setPriority, remove, login/start, login/complete, login/cancel, addApiKey, policy/read|write,
//! inocode/providers/list; notifications updated and loginCompleted).
//! Layout and flow follow OpenCodex's account screens (ref/docs/21-opencodex-account-ui.md).
//! There is no re-authentication method in the contract, so there is no such button.
//! Keys, codes and tokens leave their fields at once and are never shown or logged.

use crate::state::*;
use crate::*;
use serde_json::{json, Value};

#[derive(Default)]
struct Data {
    accounts: Vec<Value>,
    /// (id, name, auth kinds) from inocode/providers/list.
    providers: Vec<(String, String, Vec<String>)>,
    policy: Value,
    login_id: Option<String>,
    auth_url: String,
    /// The API key kept while the pool answers (fallback to the plain login).
    pending_key: Option<String>,
    /// modelProvider/list: (id, name, base URL) and the default provider's id.
    endpoints: Vec<(String, String, String)>,
    default_provider: String,
    /// The provider whose policy (switching threshold) is loaded: each provider has its own.
    policy_provider: String,
    /// The account being signed in again (re-login), when the dialog is for that.
    reauth_id: Option<String>,
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static DATA: RefCell<Data> = RefCell::new(Data::default());
}

fn rpc(tag: &str, method: &str, params: Value) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: tag.to_string(), method: method.to_string(), params });
    }
}

fn note(app: &AppWindow, text: &str) {
    app.global::<AccountsState>().set_note(text.into());
}

fn login_note(app: &AppWindow, text: &str) {
    app.global::<AccountsState>().set_login_note(text.into());
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The page was opened: load the accounts, the providers and the policy.
pub(crate) fn open(app: &AppWindow) {
    {
        let st = app.global::<AccountsState>();
        st.set_provider_id("".into());
        st.set_account_id("".into());
        st.set_reorder(false);
        st.set_loading(true);
    }
    note(app, "");
    refresh(false);
    rpc("acct.providers", "inocode/providers/list", json!({}));
    reload_endpoints();
}

/// The connection settings of the providers (config's model_providers and the default one).
pub(crate) fn reload_endpoints() {
    rpc("acct.mp", "modelProvider/list", json!({}));
}

fn refresh(force: bool) {
    rpc("acct.list", "inocode/accounts/list", json!({ "refresh": force }));
}

/// A sign-in entry (ChatGPT, Claude, API key): the same call from the empty page, the add dialog,
/// the home screen and the sidebar.
pub(crate) fn begin(app: &AppWindow, kind: &str) {
    app.global::<SettingsPageState>().set_open(true);
    let (pid, aid) = {
        let st = app.global::<AccountsState>();
        (st.get_provider_id().to_string(), st.get_account_id().to_string())
    };
    crate::settings_page::show_accounts(app);
    // Showing the page starts at the top; an entry used inside a provider stays there.
    if !pid.is_empty() {
        enter_provider(app, &pid, false);
        if !aid.is_empty() {
            enter_account(app, &aid, false);
        }
    }
    let st = app.global::<AccountsState>();
    DATA.with(|d| d.borrow_mut().reauth_id = None);
    st.set_add_alias("".into());
    st.set_add_key("".into());
    st.set_code_value("".into());
    st.set_code_open(false);
    st.set_waiting(false);
    st.set_login_note("".into());
    st.set_device_code("".into());
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        d.login_id = None;
        d.auth_url.clear();
    });
    // A sign-in entry of a known provider opens that provider's screen.
    let known = match kind {
        "chatgpt" => Some("openai"),
        "claude" => Some("claude"),
        k if k.starts_with("key:") => Some(&k[4..]),
        _ => None,
    };
    if let Some(id) = known {
        if st.get_provider_id().as_str() != id {
            enter_provider(app, id, false);
        }
    }
    match kind {
        "chatgpt" => {
            st.set_add_kind("chatgpt".into());
            st.set_add_title("ChatGPT でログイン".into());
            st.set_add_step(2);
        }
        "claude" => {
            st.set_add_kind("claude".into());
            st.set_add_title("Claude でログイン".into());
            st.set_add_step(2);
        }
        k if k.starts_with("key:") => {
            st.set_add_kind(k.into());
            st.set_add_title(format!("{} API キー", crate::settings_page::provider_name(&k[4..])).into());
            st.set_add_step(2);
        }
        other => {
            st.set_search(if other == "key" { "API キー".into() } else { "".into() });
            st.set_add_step(1);
            set_rows(app);
        }
    }
}

fn visit(app: &AppWindow) {
    let st = app.global::<AccountsState>();
    crate::nav::visit(app, crate::nav::Entry::Account(st.get_provider_id().to_string(), st.get_account_id().to_string()));
}

/// Open a provider's screen.
fn enter_provider(app: &AppWindow, id: &str, record: bool) {
    let st = app.global::<AccountsState>();
    st.set_provider_id(id.into());
    st.set_provider_name(crate::settings_page::provider_name(id).into());
    st.set_account_id("".into());
    st.set_confirm_id("".into());
    st.set_reorder(false);
    st.set_threshold("".into());
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        d.policy_provider = id.to_string();
        d.policy = Value::Null;
    });
    rpc("acct.policy", "inocode/accounts/policy/read", json!({ "provider": id }));
    st.set_settings(ModelRc::from(Rc::new(VecModel::from(Vec::<ProvSetting>::new()))));
    if id == "openai" {
        rpc("acct.cfg", "config/read", json!({ "includeLayers": false }));
    }
    render(app);
    set_rows(app);
    if record {
        visit(app);
    }
}

fn enter_account(app: &AppWindow, id: &str, record: bool) {
    let st = app.global::<AccountsState>();
    st.set_account_id(id.into());
    st.set_confirm_id("".into());
    render(app);
    st.set_rename_text(st.get_cur().alias);
    if record {
        visit(app);
    }
}

/// The history stepped to a screen of the account section.
pub(crate) fn show_at(app: &AppWindow, provider: &str, account: &str) {
    crate::settings_page::show_accounts(app);
    if provider.is_empty() {
        return;
    }
    enter_provider(app, provider, false);
    if !account.is_empty() {
        enter_account(app, account, false);
    }
}

/// The dialog's list: one row per sign-in method of each provider, filtered by the search text.
fn set_rows(app: &AppWindow) {
    let pid = app.global::<AccountsState>().get_provider_id().to_string();
    let q = if pid.is_empty() { app.global::<AccountsState>().get_search().to_lowercase() } else { String::new() };
    let mut providers = DATA.with(|d| d.borrow().providers.clone());
    // The ways to sign in that each known provider always has, whatever the server lists.
    for (id, name, kinds) in [
        ("openai", "OpenAI", ["chatgpt", "apiKey"]),
        ("claude", "Claude", ["claudeOauth", "apiKey"]),
    ] {
        match providers.iter_mut().find(|p| p.0 == id) {
            Some(p) => {
                for k in kinds {
                    if !p.2.iter().any(|x| x == k) {
                        p.2.push(k.to_string());
                    }
                }
            }
            None => providers.push((id.to_string(), name.to_string(), kinds.iter().map(|k| k.to_string()).collect())),
        }
    }
    let mut rows = Vec::new();
    for (id, name, kinds) in providers.iter().filter(|p| pid.is_empty() || p.0 == pid) {
        for k in kinds {
            let (rid, title, kind) = match k.as_str() {
                "chatgpt" => ("chatgpt".to_string(), "ChatGPT".to_string(), "ログイン"),
                "claudeOauth" => ("claude".to_string(), "Claude(サブスク)".to_string(), "ログイン"),
                "apiKey" => (format!("key:{id}"), format!("{name} API キー"), "API キー"),
                _ => continue,
            };
            if q.is_empty() || title.to_lowercase().contains(&q) || kind.to_lowercase().contains(&q) || name.to_lowercase().contains(&q) {
                rows.push(AddRow { id: rid.into(), title: title.into(), kind: kind.into(), provider: id.as_str().into() });
            }
        }
    }
    app.global::<AccountsState>().set_add_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn bar(label: &str, w: &Value) -> Option<AcctBar> {
    let pct = w.get("usedPercent").and_then(Value::as_f64)?;
    Some(AcctBar { label: label.into(), pct: pct as f32, reset: reset_text(w.get("resetsAt").and_then(Value::as_i64)).into() })
}

/// "1時間20分", "4日8時間", "5分".
fn span_jp(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}日{h}時間")
    } else if h > 0 {
        format!("{h}時間{m}分")
    } else {
        format!("{}分", m.max(1))
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn reset_text(at: Option<i64>) -> String {
    at.filter(|t| *t > now_secs()).map(|t| format!("あと{}で使用量が戻ります", span_jp(t - now_secs()))).unwrap_or_default()
}

/// The accounts in the order of use: the highest priority first, the server's order inside a tie.
fn ordered(accounts: &[Value]) -> Vec<Value> {
    let mut v = accounts.to_vec();
    v.sort_by_key(|a| std::cmp::Reverse(a.get("priority").and_then(Value::as_i64).unwrap_or(0)));
    v
}

/// The mean of the used percentages of one window over the accounts that report it.
fn mean(accounts: &[Value], key: &str) -> Option<i64> {
    let vals: Vec<f64> = accounts
        .iter()
        .filter_map(|a| a.get("usage").filter(|u| u.is_object()).and_then(|u| u.get(key)).and_then(|w| w.get("usedPercent")).and_then(Value::as_f64))
        .collect();
    if vals.is_empty() {
        None
    } else {
        Some((vals.iter().sum::<f64>() / vals.len() as f64).round() as i64)
    }
}

fn provider_of(a: &Value) -> &str {
    if s(a, "provider").is_empty() {
        "openai"
    } else {
        s(a, "provider")
    }
}

/// The mean usage of the accounts as two bars (5 hours, week); none when they report none.
fn mean_bars(accounts: &[Value]) -> Vec<RowBar> {
    let mut bars = Vec::new();
    for (key, label) in [("fiveHour", "5時間"), ("weekly", "週")] {
        if let Some(n) = mean(accounts, key) {
            bars.push(RowBar { label: label.into(), pct: n as f32, text: format!("{n}%").into() });
        }
    }
    bars
}

/// The rows of the top level: the providers of the server plus any provider an account names.
fn render_providers(app: &AppWindow, all: &[Value]) {
    let mut list: Vec<(String, String)> = DATA.with(|d| d.borrow().providers.iter().map(|p| (p.0.clone(), p.1.clone())).collect());
    // OpenAI and Claude are always there, even when the server does not list them.
    for (id, name) in [("openai", "OpenAI"), ("claude", "Claude")] {
        if !list.iter().any(|p| p.0 == id) {
            list.push((id.into(), name.into()));
        }
    }
    for a in all {
        let id = provider_of(a);
        if !list.iter().any(|p| p.0 == id) {
            list.push((id.to_string(), crate::settings_page::provider_name(id)));
        }
    }
    let rows: Vec<ProvRow> = list
        .iter()
        .map(|(id, name)| {
            let mine: Vec<Value> = all.iter().filter(|a| provider_of(a) == id).cloned().collect();
            let tone = if mine.is_empty() {
                "off"
            } else if mine.iter().any(|a| s(a, "state") == "active") {
                "ok"
            } else {
                "warn"
            };
            ProvRow {
                id: id.as_str().into(),
                name: name.as_str().into(),
                count: mine.len() as i32,
                bars: ModelRc::from(Rc::new(VecModel::from(mean_bars(&mine)))),
                summary: "".into(),
                tone: tone.into(),
            }
        })
        .collect();
    let mut rows = rows;
    // Providers added through the config (OpenAI-compatible) have no accounts.
    let ends = DATA.with(|d| d.borrow().endpoints.clone());
    for (id, name, url) in ends {
        if !rows.iter().any(|r| r.id.as_str() == id) {
            rows.push(ProvRow { id: id.as_str().into(), name: name.as_str().into(), count: -1, bars: ModelRc::default(), summary: url.as_str().into(), tone: "ok".into() });
        }
    }
    app.global::<AccountsState>().set_provs(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn card(a: &Value, now: i64) -> AcctCard {
    let kind = s(a, "kind");
    let provider = provider_of(a);
    let mut bars = Vec::new();
    if let Some(u) = a.get("usage").filter(|u| u.is_object()) {
        for (key, name) in [("fiveHour", "5時間"), ("weekly", "週")] {
            if let Some(b) = u.get(key).filter(|w| !w.is_null()).and_then(|w| bar(name, w)) {
                bars.push(b);
            }
        }
    }
    let full = bars.iter().any(|b| b.pct >= 99.5);
    let recover = a
        .get("cooldownUntil")
        .and_then(Value::as_i64)
        .filter(|t| *t > now)
        .map(|t| format!("あと{}で回復", span_jp(t - now)))
        .unwrap_or_default();
    let in_use = a.get("inUse").and_then(Value::as_bool).unwrap_or(false);
    let (badge, tone) = match s(a, "state") {
        "cooldown" if full => ("上限に達しています", "warn"),
        "cooldown" => ("今は使えません", "warn"),
        "disabled" => ("停止中", "off"),
        _ if in_use => ("使用中", "ok"),
        _ => ("待機中", "ok"),
    };
    AcctCard {
        id: s(a, "id").into(),
        alias: s(a, "alias").into(),
        email: s(a, "email").into(),
        owner: s(a, "name").into(),
        provider: crate::settings_page::provider_name(provider).into(),
        kind_label: match kind {
            "apiKey" => "API キー",
            "claudeOauth" => "サブスク",
            _ => "ChatGPT",
        }
        .into(),
        plan: s(a, "plan").into(),
        badge: badge.into(),
        recover: recover.into(),
        tone: tone.into(),
        in_use,
        enabled: s(a, "state") != "disabled",
        can_reauth: kind != "apiKey" && !a.get("isDefault").and_then(Value::as_bool).unwrap_or(false),
        bars: ModelRc::from(Rc::new(VecModel::from(bars))),
    }
}

fn render(app: &AppWindow) {
    let everything = DATA.with(|d| d.borrow().accounts.clone());
    render_providers(app, &everything);
    let st = app.global::<AccountsState>();
    let pid = st.get_provider_id().to_string();
    {
        let (url, found, def) = DATA.with(|d| {
            let d = d.borrow();
            match d.endpoints.iter().find(|e| e.0 == pid) {
                Some(e) => (e.2.clone(), true, d.default_provider == pid),
                None => (String::new(), false, false),
            }
        });
        st.set_endpoint_url(url.into());
        st.set_has_endpoint(found);
        st.set_is_default(def);
        st.set_pooled(pid.is_empty() || matches!(pid.as_str(), "openai" | "claude") || DATA.with(|d| d.borrow().providers.iter().any(|p| p.0 == pid)));
    }
    let mine: Vec<Value> = everything.into_iter().filter(|a| pid.is_empty() || provider_of(a) == pid).collect();
    let now = now_secs();
    let cards: Vec<AcctCard> = ordered(&mine).iter().map(|a| card(a, now)).collect();
    let aid = st.get_account_id().to_string();
    if !aid.is_empty() {
        match cards.iter().find(|c| c.id.as_str() == aid) {
            Some(c) => st.set_cur(c.clone()),
            // Deleted or gone: back to the provider's screen.
            None => st.set_account_id("".into()),
        }
    }
    st.set_cards(ModelRc::from(Rc::new(VecModel::from(cards))));
    st.set_loading(false);
    st.set_total(ModelRc::from(Rc::new(VecModel::from(mean_bars(&mine)))));
}

fn show_policy(app: &AppWindow) {
    let p = DATA.with(|d| d.borrow().policy.clone());
    app.global::<AccountsState>()
        .set_threshold(p.get("thresholdPercent").and_then(Value::as_f64).map(|n| (n.round() as i64).to_string()).unwrap_or_default().into());
}

fn write_policy(change: impl FnOnce(&mut Value)) {
    let mut p = DATA.with(|d| d.borrow().policy.clone());
    if !p.is_object() {
        p = json!({});
    }
    change(&mut p);
    let provider = DATA.with(|d| d.borrow().policy_provider.clone());
    rpc("acct.policy.write", "inocode/accounts/policy/write", json!({ "provider": provider, "policy": p }));
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let st = app.global::<AccountsState>();
    let weak = app.as_weak();
    st.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<AccountsState>().set_loading(true);
            refresh(true);
        }
    });
    let weak = app.as_weak();
    st.on_begin(move |kind| {
        if let Some(app) = weak.upgrade() {
            begin(&app, &kind);
        }
    });
    let weak = app.as_weak();
    st.on_close_add(move || {
        let Some(app) = weak.upgrade() else { return };
        if let Some(id) = DATA.with(|d| d.borrow_mut().login_id.take()) {
            rpc("acct.login.cancel", "inocode/accounts/login/cancel", json!({ "loginId": id }));
        }
        let st = app.global::<AccountsState>();
        st.set_add_step(0);
        st.set_waiting(false);
        st.set_code_open(false);
    });
    let weak = app.as_weak();
    st.on_search_changed(move |_| {
        if let Some(app) = weak.upgrade() {
            set_rows(&app);
        }
    });
    let weak = app.as_weak();
    st.on_pick_row(move |id| {
        if let Some(app) = weak.upgrade() {
            begin(&app, &id);
        }
    });
    let weak = app.as_weak();
    st.on_start_login(move |method| {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<AccountsState>();
        let kind = st.get_add_kind().to_string();
        let provider = if kind == "claude" { "claude" } else { "openai" };
        let alias = st.get_add_alias().trim().to_string();
        let alias = if alias.is_empty() { crate::settings_page::provider_name(provider) } else { alias };
        if let Some(id) = DATA.with(|d| d.borrow().reauth_id.clone()) {
            rpc("acct.login.start", "inocode/accounts/reauth", json!({ "id": id, "method": method.as_str() }));
        } else {
            rpc(
                "acct.login.start",
                "inocode/accounts/login/start",
                json!({ "alias": alias, "provider": provider, "method": method.as_str() }),
            );
        }
        login_note(&app, "サインインを開始しています…");
    });
    let weak = app.as_weak();
    st.on_cancel_login(move || {
        let Some(app) = weak.upgrade() else { return };
        if let Some(id) = DATA.with(|d| d.borrow_mut().login_id.take()) {
            rpc("acct.login.cancel", "inocode/accounts/login/cancel", json!({ "loginId": id }));
        }
        let st = app.global::<AccountsState>();
        st.set_waiting(false);
        st.set_code_open(false);
        st.set_device_code("".into());
        login_note(&app, "");
    });
    let weak = app.as_weak();
    st.on_copy_link(move || {
        let Some(app) = weak.upgrade() else { return };
        let url = DATA.with(|d| d.borrow().auth_url.clone());
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(url)) {
            Ok(()) => login_note(&app, "リンクをコピーしました"),
            Err(_) => login_note(&app, "コピーできませんでした"),
        }
    });
    let weak = app.as_weak();
    st.on_submit_code(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<AccountsState>();
        let code = st.get_code_value().trim().to_string();
        let Some(login_id) = DATA.with(|d| d.borrow().login_id.clone()) else { return };
        if code.is_empty() {
            return;
        }
        st.set_code_value("".into());
        rpc("acct.login.complete", "inocode/accounts/login/complete", json!({ "loginId": login_id, "code": code }));
        login_note(&app, "確認しています…");
    });
    let weak = app.as_weak();
    st.on_add_key_now(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<AccountsState>();
        let key = st.get_add_key().trim().to_string();
        if key.is_empty() {
            login_note(&app, "API キーを入力してください");
            return;
        }
        let kind = st.get_add_kind().to_string();
        let provider = kind.strip_prefix("key:").unwrap_or("openai").to_string();
        let alias = st.get_add_alias().trim().to_string();
        let alias = if alias.is_empty() { crate::settings_page::provider_name(&provider) } else { alias };
        // The key leaves the field at once.
        st.set_add_key("".into());
        let mut params = json!({ "alias": alias, "apiKey": key });
        if provider != "openai" {
            params["provider"] = json!(provider);
        }
        if provider == "openai" {
            DATA.with(|d| d.borrow_mut().pending_key = Some(key));
        }
        rpc("acct.addkey", "inocode/accounts/addApiKey", params);
        login_note(&app, "追加しています…");
    });
    st.on_use_next(|id| {
        let thread = STATE.with(|s| s.borrow().active.clone());
        let mut p = json!({ "id": id.to_string() });
        if !thread.is_empty() {
            p["threadId"] = json!(thread);
        }
        rpc("acct.act", "inocode/accounts/use", p);
    });
    st.on_toggle(|id, enabled| rpc("acct.act", "inocode/accounts/setEnabled", json!({ "id": id.to_string(), "enabled": enabled })));
    st.on_rename(|id, alias| {
        let alias = alias.trim().to_string();
        if !alias.is_empty() {
            rpc("acct.act", "inocode/accounts/rename", json!({ "id": id.to_string(), "alias": alias }));
        }
    });
    let weak = app.as_weak();
    let weak_pid = move || weak.upgrade().map(|a| a.global::<AccountsState>().get_provider_id().to_string()).unwrap_or_default();
    st.on_move_account(move |id, delta| {
        let pid = weak_pid();
        let list = DATA.with(|d| ordered(&d.borrow().accounts.iter().filter(|a| pid.is_empty() || provider_of(a) == pid).cloned().collect::<Vec<_>>()));
        let ids: Vec<String> = list.iter().map(|a| s(a, "id").to_string()).collect();
        let Some(i) = ids.iter().position(|x| x == id.as_str()) else { return };
        let j = i as i64 + delta as i64;
        if j < 0 || j as usize >= ids.len() {
            return;
        }
        let mut ids = ids;
        ids.swap(i, j as usize);
        // The list order becomes the priority: the first has the highest.
        let n = ids.len() as i64;
        for (k, aid) in ids.iter().enumerate() {
            let prio = (n - k as i64) * 10;
            let old = list.iter().find(|a| s(a, "id") == aid).and_then(|a| a.get("priority").and_then(Value::as_i64)).unwrap_or(0);
            if old != prio {
                rpc("acct.act", "inocode/accounts/setPriority", json!({ "id": aid, "priority": prio }));
            }
        }
    });
    let weak = app.as_weak();
    st.on_reauth(move |id| {
        let Some(app) = weak.upgrade() else { return };
        let kind = DATA.with(|d| d.borrow().accounts.iter().find(|a| s(a, "id") == id.as_str()).map(|a| s(a, "kind").to_string())).unwrap_or_default();
        begin(&app, if kind == "claudeOauth" { "claude" } else { "chatgpt" });
        let st = app.global::<AccountsState>();
        st.set_add_title("ログインし直す".into());
        DATA.with(|d| d.borrow_mut().reauth_id = Some(id.to_string()));
    });
    let weak = app.as_weak();
    st.on_remove(move |id| {
        if let Some(app) = weak.upgrade() {
            let st = app.global::<AccountsState>();
            st.set_confirm_id("".into());
            st.set_account_id("".into());
        }
        rpc("acct.act", "inocode/accounts/remove", json!({ "id": id.to_string() }));
    });
    let weak = app.as_weak();
    st.on_commit_threshold(move |t| match t.trim().parse::<i64>() {
        Ok(n) if (1..=100).contains(&n) => write_policy(|p| p["thresholdPercent"] = json!(n)),
        _ => {
            if let Some(app) = weak.upgrade() {
                note(&app, "しきい値は 1 から 100 の数で入力してください");
            }
        }
    });
    let weak = app.as_weak();
    st.on_choose_setting(move |key, index| {
        let Some(app) = weak.upgrade() else { return };
        let Some((_, label, opts)) = crate::settings_page::OPENAI_SETTINGS.iter().find(|(k, _, _)| *k == key.as_str()) else { return };
        let Some((shown, wire)) = opts.get(index as usize) else { return };
        crate::settings_page::write_config(&key, json!(wire));
        let st = app.global::<AccountsState>();
        let rows: Vec<ProvSetting> = st
            .get_settings()
            .iter()
            .map(|mut r| {
                if r.key == key {
                    r.value = (*shown).into();
                }
                r
            })
            .collect();
        let _ = label;
        st.set_settings(ModelRc::from(Rc::new(VecModel::from(rows))));
    });
    let weak = app.as_weak();
    st.on_open_provider(move |id| {
        if let Some(app) = weak.upgrade() {
            enter_provider(&app, &id, true);
        }
    });
    let weak = app.as_weak();
    st.on_open_account(move |id| {
        if let Some(app) = weak.upgrade() {
            enter_account(&app, &id, true);
        }
    });
    let weak = app.as_weak();
    st.on_back(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<AccountsState>();
        if !st.get_account_id().is_empty() {
            st.set_account_id("".into());
            st.set_confirm_id("".into());
        } else {
            st.set_provider_id("".into());
            st.set_reorder(false);
        }
        render(&app);
        visit(&app);
    });
    let weak = app.as_weak();
    st.on_make_default(move || {
        if let Some(app) = weak.upgrade() {
            let pid = app.global::<AccountsState>().get_provider_id().to_string();
            if !pid.is_empty() {
                crate::settings_page::write_config("model_provider", json!(pid));
            }
        }
    });
    let weak = app.as_weak();
    st.on_add_compatible(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<AccountsState>();
        let (name, url) = (st.get_np_name().trim().to_string(), st.get_np_url().trim().to_string());
        let (env, key) = (st.get_np_env().trim().to_string(), st.get_np_key().trim().to_string());
        if name.is_empty() || url.is_empty() || (env.is_empty() && key.is_empty()) {
            login_note(&app, "名前、base URL、API キー(環境変数名か値)を入力してください");
            return;
        }
        let mut cfg = json!({ "name": name, "base_url": url, "wire_api": "responses", "requires_openai_auth": false });
        if !env.is_empty() {
            cfg["env_key"] = json!(env);
        }
        if !key.is_empty() {
            cfg["experimental_bearer_token"] = json!(key);
        }
        // The key leaves the field at once.
        st.set_np_key("".into());
        crate::settings_page::upsert_config(&format!("model_providers.{}", crate::settings_page::slug(&name)), cfg);
        st.set_np_name("".into());
        st.set_np_url("".into());
        st.set_np_env("".into());
        st.set_add_step(0);
        note(&app, "プロバイダを追加しました");
    });
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if !tag.starts_with("acct.") {
        return;
    }
    let st = app.global::<AccountsState>();
    let v = match result {
        Ok(v) => v,
        Err(e) => {
            match tag {
                "acct.list" => {
                    st.set_loading(false);
                    note(app, &format!("アカウントを読み込めませんでした: {e}"));
                }
                "acct.addkey" => {
                    // The pool is not available: the plain API-key login of the main account.
                    if let Some(key) = DATA.with(|d| d.borrow_mut().pending_key.take()) {
                        rpc("acct.login.key", "account/login/start", json!({ "type": "apiKey", "apiKey": key }));
                    } else {
                        login_note(app, &format!("追加できませんでした: {e}"));
                    }
                }
                "acct.login.start" | "acct.login.complete" | "acct.login.key" => login_note(app, &format!("できませんでした: {e}")),
                "acct.providers" | "acct.policy" => {}
                _ => note(app, &format!("できませんでした: {e}")),
            }
            return;
        }
    };
    match tag {
        "acct.list" => {
            DATA.with(|d| d.borrow_mut().accounts = v.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default());
            render(app);
        }
        "acct.providers" => {
            let list = v
                .get("providers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|p| {
                    (
                        s(p, "id").to_string(),
                        s(p, "name").to_string(),
                        p.get("authKinds").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),
                    )
                })
                .collect();
            DATA.with(|d| d.borrow_mut().providers = list);
            render(app);
            if st.get_add_step() == 1 || !st.get_provider_id().is_empty() {
                set_rows(app);
            }
        }
        "acct.policy" | "acct.policy.write" => {
            let p = v.get("policy").cloned().unwrap_or(Value::Null);
            if !p.is_null() {
                DATA.with(|d| d.borrow_mut().policy = p);
                show_policy(app);
            }
        }
        "acct.cfg" => {
            let config = v.get("config").cloned().unwrap_or(Value::Null);
            let rows: Vec<ProvSetting> = crate::settings_page::OPENAI_SETTINGS
                .iter()
                .map(|(key, label, opts)| {
                    let wire = crate::settings_page::lookup(&config, key).and_then(Value::as_str).unwrap_or("");
                    let cur = opts.iter().position(|(_, w)| *w == wire);
                    let desc = match *key {
                        "model_verbosity" => "応答の長さの目安(Responses API の text.verbosity)。",
                        _ => "推論の要約の詳細度(Responses API の reasoning.summary)。",
                    };
                    ProvSetting {
                        key: (*key).into(),
                        title: (*label).into(),
                        desc: desc.into(),
                        value: cur.map(|i| opts[i].0).unwrap_or("").into(),
                        opts: ModelRc::from(Rc::new(VecModel::from(opts.iter().map(|o| SharedString::from(o.0)).collect::<Vec<_>>()))),
                    }
                })
                .collect();
            if st.get_provider_id().as_str() == "openai" {
                st.set_settings(ModelRc::from(Rc::new(VecModel::from(rows))));
            }
        }
        "acct.mp" => {
            let default = v.get("defaultProviderId").and_then(Value::as_str).unwrap_or("").to_string();
            let list: Vec<(String, String, String)> = v
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|p| {
                    let id = s(p, "id");
                    (id.to_string(), if s(p, "name").is_empty() { id.to_string() } else { s(p, "name").to_string() }, s(p, "baseUrl").to_string())
                })
                .collect();
            DATA.with(|d| {
                let mut d = d.borrow_mut();
                d.endpoints = list;
                d.default_provider = default;
            });
            render(app);
        }
        "acct.act" => refresh(false),
        "acct.login.start" => {
            DATA.with(|d| {
                let mut d = d.borrow_mut();
                d.login_id = v.get("loginId").and_then(Value::as_str).map(str::to_string);
                d.auth_url = s(v, "authUrl").to_string();
            });
            let url = if s(v, "authUrl").is_empty() { s(v, "verificationUrl") } else { s(v, "authUrl") };
            if !st.get_no_browser() && !url.is_empty() {
                crate::settings_page::open_url(url);
            }
            if s(v, "authUrl").is_empty() {
                DATA.with(|d| d.borrow_mut().auth_url = url.to_string());
            }
            st.set_device_code(s(v, "userCode").into());
            st.set_waiting(true);
            let entry = v.get("codeEntry").and_then(Value::as_bool).unwrap_or(false);
            st.set_code_open(entry);
            login_note(app, if entry { "ブラウザでログインし、表示されたコードを貼り付けてください" } else { "" });
        }
        "acct.login.complete" => login_note(app, "確認しました。完了を待っています…"),
        "acct.login.key" | "acct.addkey" => {
            DATA.with(|d| d.borrow_mut().pending_key = None);
            st.set_add_step(0);
            note(app, "API キーを追加しました");
            refresh(false);
        }
        _ => {}
    }
}

/// `inocode/accounts/updated` and `loginCompleted`.
pub(crate) fn on_notify(app: &AppWindow, method: &str, params: &Value) {
    match method {
        "inocode/accounts/updated" => {
            if let Some(list) = params.get("accounts").and_then(Value::as_array) {
                DATA.with(|d| d.borrow_mut().accounts = list.clone());
                render(app);
            }
        }
        "inocode/accounts/loginCompleted" => {
            let st = app.global::<AccountsState>();
            DATA.with(|d| d.borrow_mut().login_id = None);
            st.set_waiting(false);
            st.set_code_open(false);
            st.set_device_code("".into());
            if params.get("success").and_then(Value::as_bool).unwrap_or(false) {
                st.set_add_step(0);
                note(app, "ログインしました");
                refresh(false);
            } else {
                let e = s(params, "error");
                login_note(app, &format!("ログインできませんでした{}", if e.is_empty() { String::new() } else { format!(": {e}") }));
            }
        }
        _ => {}
    }
}
