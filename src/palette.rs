//! Command palette (Ctrl+K): recent sessions, actions and thread search.
//!
//! Official: thread/search (experimental). Claude Desktop: "Search chats and sessions…"
//! with Recents / Actions / Search results (cbd6c85c8, c518a31cc).

use crate::state::*;
use crate::*;
use serde_json::{json, Value};

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
}

fn row(id: &str, title: &str, sub: &str, kind: i32) -> PaletteRow {
    PaletteRow { id: id.into(), title: title.into(), sub: sub.into(), kind }
}

fn folder_name(cwd: &str) -> String {
    std::path::Path::new(cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn set_rows(app: &AppWindow, rows: Vec<PaletteRow>) {
    let s = app.global::<PaletteState>();
    s.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
    s.set_selected(0);
}

/// The rows shown for an empty query: the new-session action and the latest sessions.
pub(crate) fn show_recents(app: &AppWindow) {
    let mut rows = vec![row("new", "新規セッション", "", 0)];
    STATE.with(|s| {
        for t in s.borrow().threads.iter().take(8) {
            rows.push(row(&t.id, &t.title, &folder_name(&t.cwd), 1));
        }
    });
    set_rows(app, rows);
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    show_recents(app);
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<PaletteState>().on_query_changed(move |q| {
        let Some(app) = weak.upgrade() else { return };
        let q = q.trim().to_string();
        if q.is_empty() {
            show_recents(&app);
            return;
        }
        // Titles of the loaded sessions answer at once; the server search follows.
        let needle = q.to_lowercase();
        let mut rows = Vec::new();
        STATE.with(|s| {
            for t in s.borrow().threads.iter().filter(|t| t.title.to_lowercase().contains(&needle)).take(10) {
                rows.push(row(&t.id, &t.title, &folder_name(&t.cwd), 2));
            }
        });
        set_rows(&app, rows);
        h.send(Command::Rpc {
            tag: format!("chat.palette|{q}"),
            method: "thread/search".into(),
            params: json!({ "searchTerm": q, "limit": 10 }),
        });
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<PaletteState>().on_pick(move |id| {
        let Some(app) = weak.upgrade() else { return };
        app.global::<PaletteState>().set_open(false);
        app.global::<PaletteState>().set_query("".into());
        show_recents(&app);
        if id == "new" {
            h.send(Command::NewThread);
        } else if !id.is_empty() {
            h.send(Command::OpenThread(id.to_string()));
        }
    });
}

/// Search results for the query in the tag.
pub(crate) fn on_search(app: &AppWindow, query: &str, result: &Value) {
    if app.global::<PaletteState>().get_query().trim() != query {
        return;
    }
    let mut rows = Vec::new();
    for hit in result.get("data").and_then(Value::as_array).into_iter().flatten() {
        let Some(thread) = hit.get("thread") else { continue };
        let Some(info) = client::parse_threads(&json!({ "data": [thread] })).into_iter().next() else { continue };
        let snippet = hit.get("snippet").and_then(Value::as_str).unwrap_or("").replace('\n', " ");
        let sub = if snippet.is_empty() { folder_name(&info.cwd) } else { snippet.chars().take(40).collect() };
        rows.push(row(&info.id, &info.title, &sub, 2));
    }
    if !rows.is_empty() {
        set_rows(app, rows);
    }
}
