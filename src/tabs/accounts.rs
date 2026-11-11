//! 設定タブの「アカウント」ページ: アカウントプール(inocode/accounts/*)。
//! 一覧、ChatGPT の追加(ブラウザ)、API キーの追加、削除、別名、優先度、有効/無効、今すぐ使う、方針の編集。
//! 通知 inocode/accounts/updated、switched、loginCompleted を処理する。メールアドレスは扱わない。

use super::Host;
use crate::client::Event;
use crate::{AccountRow, AppWindow, SettingsTabState};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

const TAG_LIST: &str = "accounts.list";
const TAG_ACTION: &str = "accounts.action";
const TAG_LOGIN: &str = "accounts.login";
const TAG_POLICY_READ: &str = "accounts.policy.read";
const TAG_POLICY_WRITE: &str = "accounts.policy.write";

#[derive(Default)]
struct A {
    accounts: Vec<Value>,
    policy: Value,
    login_id: Option<String>,
}

thread_local! {
    static A: RefCell<A> = RefCell::new(A::default());
}

// ------------------------------------------------------------------ pure helpers (tested)

fn payload(v: &Value) -> &Value {
    v.get("response").unwrap_or(v)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Unix 秒 -> "11-11 05:00"(UTC)
fn time_label(secs: i64) -> String {
    super::team_log::time_label(secs * 1000)
}

fn window_label(w: Option<&Value>) -> (f32, String) {
    let Some(w) = w.filter(|w| w.is_object()) else { return (0.0, "—".into()) };
    let pct = w.get("usedPercent").and_then(Value::as_f64).unwrap_or(0.0);
    let reset = w.get("resetsAt").and_then(Value::as_i64).map(|t| format!(" · リセット {}", time_label(t))).unwrap_or_default();
    (pct as f32, format!("{}%{reset}", pct.round() as i64))
}

fn cooldown_label(until: Option<i64>, now: i64) -> String {
    match until {
        Some(t) if t > now => {
            let left = t - now;
            if left >= 3600 {
                format!("あと {} 時間 {} 分", left / 3600, left % 3600 / 60)
            } else if left >= 60 {
                format!("あと {} 分", left / 60)
            } else {
                format!("あと {left} 秒")
            }
        }
        _ => String::new(),
    }
}

fn state_label(st: &str) -> &str {
    match st {
        "active" => "利用可",
        "cooldown" => "冷却中",
        "disabled" => "無効",
        other => other,
    }
}

fn account_row(a: &Value, now: i64) -> AccountRow {
    let usage = a.get("usage").filter(|u| u.is_object());
    let (five_pct, five_label) = window_label(usage.and_then(|u| u.get("fiveHour")));
    let (weekly_pct, weekly_label) = window_label(usage.and_then(|u| u.get("weekly")));
    let state = s(a, "state");
    AccountRow {
        id: s(a, "id").into(),
        alias: s(a, "alias").into(),
        kind_label: match (s(a, "provider"), s(a, "kind")) {
            ("claude", "apiKey") => "Claude · API キー",
            ("claude", _) => "Claude · サブスク",
            (_, "apiKey") => "API キー",
            _ => "ChatGPT",
        }
        .into(),
        plan: s(a, "plan").into(),
        has_usage: usage.is_some(),
        five_pct,
        five_label: five_label.into(),
        weekly_pct,
        weekly_label: weekly_label.into(),
        state: state.into(),
        state_label: state_label(state).into(),
        cooldown_label: cooldown_label(a.get("cooldownUntil").and_then(Value::as_i64), now).into(),
        priority: a.get("priority").and_then(Value::as_i64).unwrap_or(0) as i32,
        in_use: a.get("inUse").and_then(Value::as_bool).unwrap_or(false),
        enabled: state != "disabled",
    }
}

const WINDOWS: [&str; 3] = ["fiveHour", "weekly", "both"];
const SELECTIONS: [&str; 3] = ["quota", "roundRobin", "fillFirst"];
const ORDERS: [&str; 3] = ["afterChatgpt", "beforeChatgpt", "mixed"];

fn index_of(table: &[&str], v: &str, default: i32) -> i32 {
    table.iter().position(|x| *x == v).map(|i| i as i32).unwrap_or(default)
}

fn num_text(v: Option<&Value>) -> String {
    match v.and_then(Value::as_f64) {
        Some(f) if f.fract() == 0.0 => format!("{}", f as i64),
        Some(f) => f.to_string(),
        None => String::new(),
    }
}

/// フォームの値で方針を作る。読み込んだ方針の未知の項目は残す。
fn build_policy(base: &Value, threshold: &str, cooldown: &str, window: i32, selection: i32, order: i32, auto: bool) -> Result<Value, String> {
    let t: f64 = threshold.trim().parse().map_err(|_| "しきい値には数値を入力してください")?;
    if !(0.0..=100.0).contains(&t) {
        return Err("しきい値は 0〜100 の範囲で入力してください".into());
    }
    let c: i64 = cooldown.trim().parse().map_err(|_| "冷却の既定には整数(秒)を入力してください")?;
    if c < 0 {
        return Err("冷却の既定は 0 以上で入力してください".into());
    }
    let mut p = if base.is_object() { base.clone() } else { json!({}) };
    p["thresholdPercent"] = json!(t);
    p["defaultCooldownSeconds"] = json!(c);
    p["window"] = json!(WINDOWS[window.clamp(0, 2) as usize]);
    p["selection"] = json!(SELECTIONS[selection.clamp(0, 2) as usize]);
    p["apiKeyOrder"] = json!(ORDERS[order.clamp(0, 2) as usize]);
    p["autoSwitch"] = json!(auto);
    Ok(p)
}

fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).creation_flags(0x0800_0000).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

// ------------------------------------------------------------------ rendering

fn render(app: &AppWindow) {
    let now = now_secs();
    // Grouped by provider (the order inside a provider is kept).
    let rows: Vec<AccountRow> = A.with(|a| {
        let mut list: Vec<Value> = a.borrow().accounts.clone();
        list.sort_by_key(|x| s(x, "provider") == "claude");
        list.iter().map(|x| account_row(x, now)).collect()
    });
    app.global::<SettingsTabState>().set_accounts(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn render_policy(app: &AppWindow) {
    let st = app.global::<SettingsTabState>();
    A.with(|a| {
        let p = &a.borrow().policy;
        if !p.is_object() {
            return;
        }
        st.set_pol_threshold(num_text(p.get("thresholdPercent")).into());
        st.set_pol_cooldown(num_text(p.get("defaultCooldownSeconds")).into());
        st.set_pol_window(index_of(&WINDOWS, s(p, "window"), 2));
        st.set_pol_selection(index_of(&SELECTIONS, s(p, "selection"), 0));
        st.set_pol_order(index_of(&ORDERS, s(p, "apiKeyOrder"), 0));
        st.set_pol_auto(p.get("autoSwitch").and_then(Value::as_bool).unwrap_or(true));
    });
}

fn request_all(host: &Host) {
    host.rpc(TAG_LIST, "inocode/accounts/list", json!({}));
    host.rpc(TAG_POLICY_READ, "inocode/accounts/policy/read", json!({}));
}

fn alias_of(id: &str) -> String {
    A.with(|a| a.borrow().accounts.iter().find(|x| s(x, "id") == id).map(|x| s(x, "alias").to_string())).unwrap_or_default()
}

// ------------------------------------------------------------------ hooks (called from settings.rs)

pub fn install(app: &AppWindow, host: &Host) {
    let st = app.global::<SettingsTabState>();

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_page_changed(move |i| {
        if i == 1 && weak.upgrade().is_some() {
            request_all(&h);
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_acc_add_chatgpt(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<SettingsTabState>();
        let alias = st.get_new_alias().trim().to_string();
        if alias.is_empty() {
            st.set_acc_note("別名を入力してください".into());
            return;
        }
        st.set_acc_note("".into());
        st.set_login_pending(true);
        st.set_login_note("".into());
        h.rpc(TAG_LOGIN, "inocode/accounts/login/start", json!({ "alias": alias, "method": "browser" }));
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_acc_cancel_login(move || {
        if let Some(id) = A.with(|a| a.borrow_mut().login_id.take()) {
            h.rpc(TAG_ACTION, "inocode/accounts/login/cancel", json!({ "loginId": id }));
        }
        if let Some(app) = weak.upgrade() {
            let st = app.global::<SettingsTabState>();
            st.set_login_pending(false);
            st.set_login_note("".into());
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_acc_add_key(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<SettingsTabState>();
        let (alias, key) = (st.get_new_alias().trim().to_string(), st.get_new_key().trim().to_string());
        if alias.is_empty() || key.is_empty() {
            st.set_acc_note("別名と API キーを入力してください".into());
            return;
        }
        st.set_acc_note("".into());
        // 保存後は表示しない: 送った時点で入力欄を空にする
        st.set_new_key("".into());
        h.rpc(TAG_ACTION, "inocode/accounts/addApiKey", json!({ "alias": alias, "apiKey": key }));
    });

    let weak = app.as_weak();
    st.on_acc_remove_ask(move |id| {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<SettingsTabState>();
        st.set_removing_alias(alias_of(&id).into());
        st.set_removing_id(id);
    });
    let weak = app.as_weak();
    st.on_acc_remove_cancel(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<SettingsTabState>().set_removing_id("".into());
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_acc_remove_confirm(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<SettingsTabState>();
        let id = st.get_removing_id().to_string();
        st.set_removing_id("".into());
        if !id.is_empty() {
            h.rpc(TAG_ACTION, "inocode/accounts/remove", json!({ "id": id }));
        }
    });

    let weak = app.as_weak();
    st.on_acc_rename_start(move |id| {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<SettingsTabState>();
        st.set_rename_text(alias_of(&id).into());
        st.set_renaming_id(id);
    });
    let weak = app.as_weak();
    st.on_acc_rename_cancel(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<SettingsTabState>().set_renaming_id("".into());
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_acc_rename_save(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<SettingsTabState>();
        let (id, alias) = (st.get_renaming_id().to_string(), st.get_rename_text().trim().to_string());
        if alias.is_empty() {
            st.set_acc_note("別名を入力してください".into());
            return;
        }
        st.set_renaming_id("".into());
        h.rpc(TAG_ACTION, "inocode/accounts/rename", json!({ "id": id, "alias": alias }));
    });

    let h = host.clone();
    st.on_acc_priority(move |id, delta| {
        let cur = A.with(|a| a.borrow().accounts.iter().find(|x| s(x, "id") == id.as_str()).and_then(|x| x.get("priority").and_then(Value::as_i64))).unwrap_or(0);
        h.rpc(TAG_ACTION, "inocode/accounts/setPriority", json!({ "id": id.as_str(), "priority": cur + delta as i64 }));
    });
    let h = host.clone();
    st.on_acc_toggle(move |id, enabled| h.rpc(TAG_ACTION, "inocode/accounts/setEnabled", json!({ "id": id.as_str(), "enabled": enabled })));
    let h = host.clone();
    st.on_acc_use(move |id| h.rpc(TAG_ACTION, "inocode/accounts/use", json!({ "id": id.as_str() })));

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_pol_save(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<SettingsTabState>();
        let base = A.with(|a| a.borrow().policy.clone());
        match build_policy(&base, &st.get_pol_threshold(), &st.get_pol_cooldown(), st.get_pol_window(), st.get_pol_selection(), st.get_pol_order(), st.get_pol_auto()) {
            Ok(p) => {
                st.set_acc_note("".into());
                h.rpc(TAG_POLICY_WRITE, "inocode/accounts/policy/write", json!({ "policy": p }));
            }
            Err(e) => st.set_acc_note(e.into()),
        }
    });
}

pub fn opened(app: &AppWindow, host: &Host) {
    if app.global::<SettingsTabState>().get_page() == 1 {
        request_all(host);
    }
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    let st = app.global::<SettingsTabState>();
    match event {
        Event::Rpc { tag, result } => match (tag.as_str(), result) {
            (TAG_LIST, Ok(v)) => {
                A.with(|a| a.borrow_mut().accounts = payload(v).get("accounts").and_then(Value::as_array).cloned().unwrap_or_default());
                render(app);
            }
            (TAG_POLICY_READ, Ok(v)) | (TAG_POLICY_WRITE, Ok(v)) => {
                if let Some(p) = payload(v).get("policy") {
                    A.with(|a| a.borrow_mut().policy = p.clone());
                    render_policy(app);
                }
            }
            (TAG_LOGIN, Ok(v)) => {
                let p = payload(v);
                A.with(|a| a.borrow_mut().login_id = Some(s(p, "loginId").to_string()));
                if !s(p, "authUrl").is_empty() {
                    open_url(s(p, "authUrl"));
                    st.set_login_note("ブラウザでログインしてください".into());
                } else if !s(p, "verificationUrl").is_empty() {
                    st.set_login_note(format!("{} を開いてコード {} を入力してください", s(p, "verificationUrl"), s(p, "userCode")).into());
                }
            }
            (TAG_LOGIN, Err(e)) => {
                st.set_login_pending(false);
                st.set_acc_note(e.as_str().into());
            }
            (TAG_ACTION, Ok(_)) => {
                st.set_acc_note("".into());
                host.rpc(TAG_LIST, "inocode/accounts/list", json!({}));
            }
            (TAG_ACTION, Err(e)) | (TAG_POLICY_READ, Err(e)) | (TAG_POLICY_WRITE, Err(e)) | (TAG_LIST, Err(e)) => {
                st.set_acc_note(e.as_str().into());
            }
            _ => {}
        },
        Event::Notify { method, params } => match method.as_str() {
            "inocode/accounts/updated" => {
                if let Some(list) = params.get("accounts").and_then(Value::as_array) {
                    A.with(|a| a.borrow_mut().accounts = list.clone());
                    render(app);
                }
            }
            "inocode/accounts/switched" => {
                // 使用中の印を、切り替え先の別名に移す
                let to = s(params, "toAlias").to_string();
                A.with(|a| {
                    for x in a.borrow_mut().accounts.iter_mut() {
                        let hit = s(x, "alias") == to;
                        x["inUse"] = json!(hit);
                    }
                });
                render(app);
            }
            "inocode/accounts/loginCompleted" => {
                A.with(|a| a.borrow_mut().login_id = None);
                st.set_login_pending(false);
                st.set_login_note("".into());
                if params.get("success").and_then(Value::as_bool).unwrap_or(false) {
                    st.set_new_alias("".into());
                    st.set_acc_note("".into());
                    host.rpc(TAG_LIST, "inocode/accounts/list", json!({}));
                } else {
                    st.set_acc_note(format!("ログインに失敗しました: {}", s(params, "error")).into());
                }
            }
            _ => {}
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract").join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn rows_show_usage_cooldown_and_never_an_email() {
        let list = payload(&fixture("accounts-list.json")).get("accounts").and_then(Value::as_array).cloned().unwrap();
        let main = account_row(&list[0], 1794390000);
        assert_eq!((main.alias.as_str(), main.kind_label.as_str(), main.plan.as_str()), ("メイン", "ChatGPT", "pro"));
        assert!(main.in_use && main.has_usage && main.enabled);
        assert!(main.five_label.starts_with("42%"));
        let sub = account_row(&list[1], 1794390000);
        assert_eq!(sub.state_label, "冷却中");
        assert_eq!(sub.cooldown_label, "あと 2 時間 20 分");
        let key = account_row(&list[2], 0);
        assert!(!key.has_usage);
        assert_eq!(key.kind_label, "API キー");
        let off = account_row(&list[3], 0);
        assert!(!off.enabled);
        assert!(!serde_json::to_string(&list).unwrap().contains('@'));
    }

    #[test]
    fn policy_round_trip_keeps_unknown_fields() {
        let base = payload(&fixture("accounts-policy-read.json")).get("policy").cloned().unwrap();
        let mut with_extra = base.clone();
        with_extra["future"] = json!(1);
        let p = build_policy(&with_extra, "85", "120", 0, 1, 2, false).unwrap();
        assert_eq!(p["thresholdPercent"], json!(85.0));
        assert_eq!(p["defaultCooldownSeconds"], json!(120));
        assert_eq!((p["window"].as_str(), p["selection"].as_str(), p["apiKeyOrder"].as_str()), (Some("fiveHour"), Some("roundRobin"), Some("mixed")));
        assert_eq!(p["autoSwitch"], json!(false));
        assert_eq!(p["future"], json!(1));
        assert!(build_policy(&base, "x", "60", 0, 0, 0, true).is_err());
        assert!(build_policy(&base, "120", "60", 0, 0, 0, true).is_err());
        assert!(build_policy(&base, "90", "-1", 0, 0, 0, true).is_err());
        assert_eq!(num_text(base.get("thresholdPercent")), "90");
    }
}
