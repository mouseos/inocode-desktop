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

struct Data {
    confirm: Option<Confirm>,
    pinned_section: Option<String>,
    /// thread/list sortKey: "created_at", "updated_at" or "section_position" (manual).
    sort: &'static str,
    /// One flat list instead of one group per folder.
    flat: bool,
    /// 0 all, 1 unread, 2 needs you, 3 archived.
    filter: i32,
    cursor: Option<String>,
}

impl Default for Data {
    fn default() -> Self {
        Data { confirm: None, pinned_section: None, sort: "updated_at", flat: false, filter: 0, cursor: None }
    }
}

/// (flat list, filter) as the sidebar shows them.
pub(crate) fn mode() -> (bool, i32) {
    DATA.with(|d| {
        let d = d.borrow();
        (d.flat, d.filter)
    })
}

fn checked(mut m: MenuItem, on: bool) -> MenuItem {
    m.checked = on;
    m
}

/// Reload the session list with the sidebar's own sort and filter (thread/list).
pub(crate) fn refresh() {
    let Some(h) = HANDLE.with(|c| c.borrow().clone()) else { return };
    let (sort, filter) = DATA.with(|d| {
        let d = d.borrow();
        (d.sort, d.filter)
    });
    let mut params = json!({ "limit": 50, "sortKey": sort });
    if filter == 3 {
        params["archived"] = json!(true);
    }
    rpc(&h, "chat.list".into(), "thread/list", params);
}

fn load_more() {
    let Some(h) = HANDLE.with(|c| c.borrow().clone()) else { return };
    let (sort, filter, cursor) = DATA.with(|d| {
        let d = d.borrow();
        (d.sort, d.filter, d.cursor.clone())
    });
    let Some(cursor) = cursor else { return };
    let mut params = json!({ "limit": 50, "sortKey": sort, "cursor": cursor });
    if filter == 3 {
        params["archived"] = json!(true);
    }
    rpc(&h, "chat.list.more".into(), "thread/list", params);
}

fn sort_menu() -> Vec<MenuItem> {
    let (sort, flat) = DATA.with(|d| {
        let d = d.borrow();
        (d.sort, d.flat)
    });
    vec![
        checked(item("sort:created_at", "作成順", "", false, false), sort == "created_at"),
        checked(item("sort:updated_at", "更新順", "", false, false), sort == "updated_at"),
        checked(item("sort:section_position", "手動順", "", false, false), sort == "section_position"),
        separator(),
        checked(item("group:project", "プロジェクト別", "", false, false), !flat),
        checked(item("group:flat", "フラット", "", false, false), flat),
    ]
}

fn filter_menu() -> Vec<MenuItem> {
    let f = DATA.with(|d| d.borrow().filter);
    vec![
        checked(item("filter:0", "すべて", "", false, false), f == 0),
        checked(item("filter:1", "未読", "", false, false), f == 1),
        checked(item("filter:2", "対応待ち", "", false, false), f == 2),
        separator(),
        checked(item("filter:3", "アーカイブ済み", "", false, false), f == 3),
    ]
}

thread_local! {
    static DATA: RefCell<Data> = RefCell::new(Data::default());
}

fn item(id: &str, label: &str, glyph: &str, danger: bool, disabled: bool) -> MenuItem {
    MenuItem { id: id.into(), label: label.into(), glyph: glyph.into(), danger, disabled, separator: false, checked: false }
}

pub(crate) fn simple_item(id: &str, label: &str, glyph: &str) -> MenuItem {
    item(id, label, glyph, false, false)
}

pub(crate) fn open_menu(app: &AppWindow, items: Vec<MenuItem>, x: f32, y: f32) {
    set_menu(app, items, x, y);
}

pub(crate) fn separator() -> MenuItem {
    MenuItem { id: "".into(), label: "".into(), glyph: "".into(), danger: false, disabled: false, separator: true, checked: false }
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
        if mode().1 == 3 {
            item("unarchive", "アーカイブを解除", Glyph::folder(), false, false)
        } else {
            item("archive", "アーカイブ", Glyph::folder(), false, false)
        },
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
    crate::palette::register(app, handle);
    crate::chat_actions::register(app, handle);
    crate::prompts::register(app, handle);
    crate::attach::register(app, handle);
    crate::panel_data::register(app, handle);
    let weak = app.as_weak();
    app.global::<SidebarState>().on_open_menu(move |kind, id, x, y| {
        let Some(app) = weak.upgrade() else { return };
        match kind.as_str() {
            "head" | "head-more" => {
                // The header's title and "…" use the same menu as a session row.
                let active = STATE.with(|s| s.borrow().active.clone());
                let mut items = STATE.with(|s| thread_menu(&s.borrow(), &active));
                items.extend(crate::panel_data::thread_menu_extras(kind == "head-more"));
                set_menu(&app, items, x, y);
                MENU_FOR.with(|m| *m.borrow_mut() = active);
            }
            "sort" => set_menu(&app, sort_menu(), x, y),
            "more" => set_menu(&app, filter_menu(), x, y),
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
        if crate::attach::menu_pick(&app, &action) || crate::panel_data::menu_pick(&app, &action) {
            return;
        }
        let id = MENU_FOR.with(|m| m.borrow().clone());
        if let Some((kind, v)) = action.split_once(':') {
            DATA.with(|d| {
                let mut d = d.borrow_mut();
                match kind {
                    "sort" => d.sort = match v { "created_at" => "created_at", "section_position" => "section_position", _ => "updated_at" },
                    "group" => d.flat = v == "flat",
                    "filter" => d.filter = v.parse().unwrap_or(0),
                    _ => {}
                }
            });
            refresh();
            return;
        }
        match action.as_str() {
            "unarchive" => rpc(&h, format!("chat.unarchive|{id}"), "thread/unarchive", json!({ "threadId": id })),
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
    app.global::<SidebarState>().on_load_more(load_more);
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
    if matches!(kind, "chat.turns" | "chat.fork" | "chat.revert") {
        return;
    }
    if kind == "chat.palette" {
        if let Ok(v) = result {
            crate::palette::on_search(app, &id, v);
        }
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
            refresh();
            return;
        }
    };
    match kind {
        "chat.list" | "chat.list.more" => {
            let mut list = client::parse_threads(result);
            DATA.with(|d| d.borrow_mut().cursor = result.get("nextCursor").and_then(Value::as_str).map(str::to_string));
            app.global::<SidebarState>().set_has_more(DATA.with(|d| d.borrow().cursor.is_some()));
            STATE.with(|s| {
                let mut st = s.borrow_mut();
                if kind == "chat.list.more" {
                    let mut all = std::mem::take(&mut st.threads);
                    all.append(&mut list);
                    list = all;
                }
                st.threads = list;
                sync_project_cwds(&mut st);
                rebuild_projects(app, &st);
                refresh_pickers(app, &st);
            });
            if app.global::<PaletteState>().get_query().trim().is_empty() {
                crate::palette::show_recents(app);
            }
        }
        "chat.rename" => {
            let active = STATE.with(|s| s.borrow().active == id);
            if active {
                app.global::<AppState>().set_session_title(extra.as_str().into());
            }
            refresh();
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
            refresh();
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
        // chat.pin, chat.unpin, chat.read, chat.unarchive
        _ => refresh(),
    }
}

/// Session-list notifications from the server.
pub(crate) fn on_notify(method: &str) {
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
        refresh();
    }
}
