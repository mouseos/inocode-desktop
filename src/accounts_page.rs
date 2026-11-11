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
    app.global::<AccountsState>().set_loading(true);
    note(app, "");
    refresh(false);
    rpc("acct.providers", "inocode/providers/list", json!({}));
    rpc("acct.policy", "inocode/accounts/policy/read", json!({}));
}

fn refresh(force: bool) {
    rpc("acct.list", "inocode/accounts/list", json!({ "refresh": force }));
}

/// A sign-in entry (ChatGPT, Claude, API key): the same call from the empty page, the add dialog,
/// the home screen and the sidebar.
pub(crate) fn begin(app: &AppWindow, kind: &str) {
    app.global::<SettingsPageState>().set_open(true);
    crate::settings_page::show_accounts(app);
    let st = app.global::<AccountsState>();
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

/// The dialog's list: one row per sign-in method of each provider, filtered by the search text.
fn set_rows(app: &AppWindow) {
    let q = app.global::<AccountsState>().get_search().to_lowercase();
    let providers = DATA.with(|d| d.borrow().providers.clone());
    let providers = if providers.is_empty() {
        vec![
            ("openai".to_string(), "OpenAI".to_string(), vec!["chatgpt".to_string(), "apiKey".to_string()]),
            ("claude".to_string(), "Claude".to_string(), vec!["claudeOauth".to_string(), "apiKey".to_string()]),
        ]
    } else {
        providers
    };
    let mut rows = Vec::new();
    for (id, name, kinds) in &providers {
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

const ORDER: [i64; 5] = [10, 5, 0, -5, -10];

fn order_index(p: i64) -> i32 {
    ORDER.iter().enumerate().min_by_key(|(_, v)| (p - **v).abs()).map(|(i, _)| i as i32).unwrap_or(2)
}

fn bar(label: &str, w: &Value) -> Option<AcctBar> {
    let pct = w.get("usedPercent").and_then(Value::as_f64)?;
    Some(AcctBar { label: label.into(), pct: pct as f32, reset: crate::settings_page::until(w.get("resetsAt").and_then(Value::as_i64)).into() })
}

fn render(app: &AppWindow) {
    let mut accounts = DATA.with(|d| d.borrow().accounts.clone());
    // Grouped by provider: OpenAI first, the order inside a provider is kept.
    accounts.sort_by_key(|a| s(a, "provider") != "openai" && !s(a, "provider").is_empty());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let cards: Vec<AcctCard> = accounts
        .iter()
        .map(|a| {
            let kind = s(a, "kind");
            let provider = if s(a, "provider").is_empty() { "openai" } else { s(a, "provider") };
            let mut bars = Vec::new();
            if let Some(u) = a.get("usage").filter(|u| u.is_object()) {
                for (key, name) in [("fiveHour", "5時間"), ("weekly", "週")] {
                    if let Some(b) = u.get(key).filter(|w| !w.is_null()).and_then(|w| bar(name, w)) {
                        bars.push(b);
                    }
                }
            }
            let cooldown = a
                .get("cooldownUntil")
                .and_then(Value::as_i64)
                .filter(|t| *t > now)
                .map(|t| format!("あと {}", crate::settings_page::span(t - now)))
                .unwrap_or_default();
            AcctCard {
                id: s(a, "id").into(),
                provider: crate::settings_page::provider_name(provider).into(),
                alias: s(a, "alias").into(),
                plan: s(a, "plan").into(),
                kind_label: match kind {
                    "apiKey" => "API キー",
                    "claudeOauth" => "サブスク",
                    _ => "ChatGPT",
                }
                .into(),
                state: s(a, "state").into(),
                cooldown: cooldown.into(),
                in_use: a.get("inUse").and_then(Value::as_bool).unwrap_or(false),
                enabled: s(a, "state") != "disabled",
                order: order_index(a.get("priority").and_then(Value::as_i64).unwrap_or(0)),
                bars: ModelRc::from(Rc::new(VecModel::from(bars))),
            }
        })
        .collect();
    let st = app.global::<AccountsState>();
    st.set_cards(ModelRc::from(Rc::new(VecModel::from(cards))));
    st.set_loading(false);
}

fn show_policy(app: &AppWindow) {
    let p = DATA.with(|d| d.borrow().policy.clone());
    let st = app.global::<AccountsState>();
    st.set_auto_switch(p.get("autoSwitch").and_then(Value::as_bool).unwrap_or(true));
    st.set_strategy(match s(&p, "selection") {
        "roundRobin" => 1,
        "fillFirst" => 2,
        _ => 0,
    });
    st.set_threshold(p.get("thresholdPercent").and_then(Value::as_i64).map(|n| n.to_string()).unwrap_or_default().into());
}

fn write_policy(change: impl FnOnce(&mut Value)) {
    let mut p = DATA.with(|d| d.borrow().policy.clone());
    if !p.is_object() {
        p = json!({});
    }
    change(&mut p);
    rpc("acct.policy.write", "inocode/accounts/policy/write", json!({ "policy": p }));
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
        rpc(
            "acct.login.start",
            "inocode/accounts/login/start",
            json!({ "alias": alias, "provider": provider, "method": method.as_str() }),
        );
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
    st.on_set_order(|id, i| {
        if let Some(p) = ORDER.get(i as usize) {
            rpc("acct.act", "inocode/accounts/setPriority", json!({ "id": id.to_string(), "priority": p }));
        }
    });
    let weak = app.as_weak();
    st.on_remove(move |id| {
        if let Some(app) = weak.upgrade() {
            let st = app.global::<AccountsState>();
            st.set_confirm_id("".into());
            st.set_open_id("".into());
        }
        rpc("acct.act", "inocode/accounts/remove", json!({ "id": id.to_string() }));
    });
    st.on_set_auto(|on| write_policy(|p| p["autoSwitch"] = json!(on)));
    st.on_set_strategy(|i| {
        let v = ["quota", "roundRobin", "fillFirst"].get(i as usize).copied().unwrap_or("quota");
        write_policy(|p| p["selection"] = json!(v));
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
    st.on_open_providers(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<AccountsState>().set_add_step(0);
            crate::settings_page::show_providers(&app);
        }
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
            if st.get_add_step() == 1 {
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
