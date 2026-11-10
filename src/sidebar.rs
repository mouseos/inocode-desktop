//! Sidebar: the session row menu (rename, pin, read/unread, archive, delete), the confirmation
//! dialog and the account menu.
//!
//! Based on the official desktop client's `sidebarElectron.*` actions (thread/name/set,
//! thread/archive, thread/delete, thread/readState/update, threadSection/* + thread/section/move)
//! and on Claude Desktop's session row menu (right click or "…", in-place rename).

use crate::state::*;
use crate::*;
use serde_json::{json, Value};

/// The server keeps pins in a section with this name.
const PINNED: &str = "Pinned";

/// What the confirmation dialog will do when accepted.
#[derive(Clone)]
enum Confirm {
    Archive(String),
    Delete(String),
    Logout,
}

#[derive(Default)]
struct Data {
    confirm: Option<Confirm>,
    pinned_section: Option<String>,
}

thread_local! {
    static DATA: RefCell<Data> = RefCell::new(Data::default());
}

fn item(id: &str, label: &str, glyph: &str, danger: bool, disabled: bool) -> MenuItem {
    MenuItem { id: id.into(), label: label.into(), glyph: glyph.into(), danger, disabled, separator: false }
}

fn separator() -> MenuItem {
    MenuItem { id: "".into(), label: "".into(), glyph: "".into(), danger: false, disabled: false, separator: true }
}

fn set_menu(app: &AppWindow, items: Vec<MenuItem>, x: f32, y: f32) {
    let s = app.global::<SidebarState>();
    s.set_menu_items(ModelRc::from(Rc::new(VecModel::from(items))));
    s.set_menu_x(x);
    s.set_menu_y(y);
    s.set_menu_open(true);
}

fn thread_menu(st: &State, id: &str) -> Vec<MenuItem> {
    let t = st.threads.iter().find(|t| t.id == id);
    let pinned = t.map(|t| t.pinned).unwrap_or(false);
    let unread = t.map(|t| t.unread).unwrap_or(false);
    let running = t.map(|t| t.status != 0).unwrap_or(false) || (st.active == id && st.busy);
    vec![
        item("rename", "名前を変更", Glyph::edit_note(), false, false),
        item("pin", if pinned { "ピン留めを解除" } else { "ピン留め" }, Glyph::push_pin(), false, false),
        // Like Claude: a session that is still working or needs you cannot be marked.
        item("read", if unread { "既読にする" } else { "未読にする" }, Glyph::done_all(), false, running),
        separator(),
        item("archive", "アーカイブ", Glyph::folder(), false, false),
        item("delete", "削除", Glyph::close(), true, false),
    ]
}

/// The icon glyph constants live in Slint; mirror the ones this file needs.
#[allow(non_snake_case)]
mod Glyph {
    pub fn edit_note() -> &'static str { "\u{e745}" }
    pub fn push_pin() -> &'static str { "\u{f10d}" }
    pub fn done_all() -> &'static str { "\u{e877}" }
    pub fn folder() -> &'static str { "\u{e2c7}" }
    pub fn close() -> &'static str { "\u{e5cd}" }
    pub fn settings() -> &'static str { "\u{e8b8}" }
    pub fn account() -> &'static str { "\u{e853}" }
}

fn rpc(h: &client::Handle, tag: String, method: &str, params: Value) {
    h.send(Command::Rpc { tag, method: method.to_string(), params });
}

fn confirm(app: &AppWindow, what: Confirm, title: &str, body: &str, label: &str, danger: bool) {
    DATA.with(|d| d.borrow_mut().confirm = Some(what));
    let s = app.global::<SidebarState>();
    s.set_confirm_title(title.into());
    s.set_confirm_body(body.into());
    s.set_confirm_label(label.into());
    s.set_confirm_danger(danger);
    s.set_confirm_open(true);
}

fn title_of(st: &State, id: &str) -> String {
    st.threads.iter().find(|t| t.id == id).map(|t| t.title.clone()).unwrap_or_default()
}

/// Called once from callbacks::register.
pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let weak = app.as_weak();
    app.global::<SidebarState>().on_open_menu(move |kind, id, x, y| {
        let Some(app) = weak.upgrade() else { return };
        match kind.as_str() {
            "thread" => {
                let items = STATE.with(|s| thread_menu(&s.borrow(), &id));
                set_menu(&app, items, x, y);
                MENU_FOR.with(|m| *m.borrow_mut() = id.to_string());
            }
            _ => {
                let logged_in = app.global::<AppState>().get_logged_in();
                let items = vec![
                    item("settings", "設定", Glyph::settings(), false, false),
                    item("switch", "アカウントを切り替え", Glyph::account(), false, false),
                    separator(),
                    if logged_in {
                        item("logout", "ログアウト", Glyph::close(), false, false)
                    } else {
                        item("login", "ログイン", Glyph::account(), false, false)
                    },
                ];
                set_menu(&app, items, x, y);
                // The footer is at the bottom: open the menu upwards.
                app.global::<SidebarState>().set_menu_y(y - (4.0 * 28.0 + 9.0 + 8.0 + 44.0));
            }
        }
    });

    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<SidebarState>().on_menu_pick(move |action| {
        let Some(app) = weak.upgrade() else { return };
        let id = MENU_FOR.with(|m| m.borrow().clone());
        match action.as_str() {
            "rename" => app.global::<SidebarState>().set_rename_id(id.as_str().into()),
            "pin" => {
                let pinned = STATE.with(|s| s.borrow().threads.iter().any(|t| t.id == id && t.pinned));
                if pinned {
                    rpc(&h, format!("chat.unpin|{id}"), "thread/section/move", json!({ "threadId": id, "sectionId": null }));
                } else if let Some(sec) = DATA.with(|d| d.borrow().pinned_section.clone()) {
                    rpc(&h, format!("chat.pin|{id}"), "thread/section/move", json!({ "threadId": id, "sectionId": sec }));
                } else {
                    rpc(&h, format!("chat.pin.list|{id}"), "threadSection/list", json!({}));
                }
            }
            "read" => set_read(&h, &id, None),
            "archive" => {
                let title = STATE.with(|s| title_of(&s.borrow(), &id));
                confirm(
                    &app,
                    Confirm::Archive(id),
                    "セッションをアーカイブしますか？",
                    &format!("「{title}」を停止し、アーカイブ済みに移動します。いつでもアーカイブを解除できます。"),
                    "アーカイブ",
                    false,
                );
            }
            "delete" => {
                let title = STATE.with(|s| title_of(&s.borrow(), &id));
                confirm(
                    &app,
                    Confirm::Delete(id),
                    "セッションを削除しますか？",
                    &format!("「{title}」は完全に削除されます。この操作は元に戻せません。"),
                    "削除",
                    true,
                );
            }
            "settings" => app.global::<AppState>().set_settings_open(true),
            "login" => app.global::<AppState>().invoke_login(),
            "switch" => rpc(&h, "chat.account.sessions".into(), "accountSession/list", json!({})),
            "logout" => confirm(
                &app,
                Confirm::Logout,
                "ログアウトしますか？",
                "このアカウントからログアウトします。",
                "ログアウト",
                false,
            ),
            _ => {}
        }
    });

    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<SidebarState>().on_commit_rename(move |id, text| {
        let Some(app) = weak.upgrade() else { return };
        // Enter and the focus loss both land here; the first one wins.
        if app.global::<SidebarState>().get_rename_id() != id {
            return;
        }
        app.global::<SidebarState>().set_rename_id("".into());
        let id = id.to_string();
        let name = text.trim().to_string();
        let old = STATE.with(|s| title_of(&s.borrow(), &id));
        if name.is_empty() || name == old {
            return;
        }
        rpc(&h, format!("chat.rename|{id}|{name}"), "thread/name/set", json!({ "threadId": id, "name": name }));
    });

    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<SidebarState>().on_confirm(move || {
        let Some(app) = weak.upgrade() else { return };
        app.global::<SidebarState>().set_confirm_open(false);
        match DATA.with(|d| d.borrow_mut().confirm.take()) {
            Some(Confirm::Archive(id)) => {
                rpc(&h, format!("chat.archive|{id}"), "thread/archive", json!({ "threadId": id }))
            }
            Some(Confirm::Delete(id)) => rpc(&h, format!("chat.delete|{id}"), "thread/delete", json!({ "threadId": id })),
            Some(Confirm::Logout) => rpc(&h, "chat.logout".into(), "account/logout", Value::Null),
            None => {}
        }
    });
    let weak = app.as_weak();
    app.global::<SidebarState>().on_cancel_confirm(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<SidebarState>().set_confirm_open(false);
        }
        DATA.with(|d| d.borrow_mut().confirm = None);
    });
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static MENU_FOR: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Mark a thread read (None: toggle from its current state).
fn set_read(h: &client::Handle, id: &str, want_read: Option<bool>) {
    let (unread, revision) = STATE.with(|s| {
        s.borrow().threads.iter().find(|t| t.id == id).map(|t| (t.unread, t.revision.clone())).unwrap_or((false, String::new()))
    });
    let read = want_read.unwrap_or(unread);
    if revision.is_empty() || (want_read.is_some() && read != unread) {
        return;
    }
    let op = if read { "read" } else { "unread" };
    rpc(
        h,
        format!("chat.read|{id}"),
        "thread/readState/update",
        json!({ "threadId": id, "operation": { "type": op }, "expectedRevision": revision }),
    );
}

/// An open session is read: clear its unread mark.
pub(crate) fn on_thread_active(id: &str) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        set_read(&h, id, Some(true));
    }
}

/// Results of the requests above (tags start with "chat.").
pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    let Some(h) = HANDLE.with(|c| c.borrow().clone()) else { return };
    let h = &*h;
    let mut parts = tag.splitn(3, '|');
    let kind = parts.next().unwrap_or("");
    let id = parts.next().unwrap_or("").to_string();
    let extra = parts.next().unwrap_or("").to_string();
    if !kind.starts_with("chat.") {
        return;
    }
    let result = match result {
        Ok(v) => v,
        Err(e) => {
            // The unread revision may be stale; the list refresh below fetches a new one.
            if kind != "chat.read" {
                app.global::<AppState>().set_status(format!("操作に失敗しました: {e}").into());
                app.global::<AppState>().set_status_level(2);
            }
            h.send(Command::ListThreads);
            return;
        }
    };
    match kind {
        "chat.rename" => {
            let active = STATE.with(|s| s.borrow().active == id);
            if active {
                app.global::<AppState>().set_session_title(extra.as_str().into());
            }
            h.send(Command::ListThreads);
        }
        "chat.pin.list" => {
            let found = result
                .get("data")
                .and_then(Value::as_array)
                .and_then(|a| a.iter().find(|s| s.get("name").and_then(Value::as_str) == Some(PINNED)))
                .and_then(|s| s.get("id").and_then(Value::as_str))
                .map(str::to_string);
            match found {
                Some(sec) => {
                    DATA.with(|d| d.borrow_mut().pinned_section = Some(sec.clone()));
                    rpc(h, format!("chat.pin|{id}"), "thread/section/move", json!({ "threadId": id, "sectionId": sec }));
                }
                None => rpc(h, format!("chat.pin.create|{id}"), "threadSection/create", json!({ "name": PINNED })),
            }
        }
        "chat.pin.create" => {
            if let Some(sec) = result.pointer("/section/id").and_then(Value::as_str) {
                DATA.with(|d| d.borrow_mut().pinned_section = Some(sec.to_string()));
                rpc(h, format!("chat.pin|{id}"), "thread/section/move", json!({ "threadId": id, "sectionId": sec }));
            }
        }
        "chat.archive" | "chat.delete" => {
            let active = STATE.with(|s| s.borrow().active == id);
            if active {
                h.send(Command::NewThread);
            }
            h.send(Command::ListThreads);
        }
        "chat.logout" => {
            app.global::<AppState>().set_logged_in(false);
            app.global::<AppState>().set_account_label("未ログイン".into());
        }
        "chat.account.sessions" => {
            // Account switching shows the sessions the server reports; the choice is made in the settings page.
            let n = result.get("data").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            app.global::<AppState>().set_status(format!("アカウントセッション {n} 件").into());
        }
        // chat.pin, chat.unpin, chat.read
        _ => h.send(Command::ListThreads),
    }
}

/// Session-list notifications from the server.
pub(crate) fn on_notify(method: &str) {
    let Some(h) = HANDLE.with(|c| c.borrow().clone()) else { return };
    if matches!(
        method,
        "thread/status/changed"
            | "thread/readState/changed"
            | "thread/name/updated"
            | "thread/archived"
            | "thread/unarchived"
            | "thread/deleted"
            | "thread/section/changed"
    ) {
        h.send(Command::ListThreads);
    }
}
