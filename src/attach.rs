//! Composer extras: the "+" menu, attachments, @ mentions and slash commands.
//!
//! Official: UserInput localImage / mention / skill, fuzzyFileSearch, skills/list, and the
//! composer's `/` commands (/compact, /rename, /archive). Claude Desktop: "+" menu (Add files or
//! photos, Slash commands, plugins and skills), attachment chips, @ suggestions, "/" menu
//! (c227f4d5e, cc183b3ab).

use crate::sidebar as menus;
use crate::state::*;
use crate::*;
use serde_json::{json, Value};

#[derive(Clone)]
struct Attach {
    id: String,
    label: String,
    kind: &'static str,
    /// "mention", "localImage" or "skill" (the UserInput type) and its fields.
    input: Value,
}

#[derive(Default)]
struct Data {
    items: Vec<Attach>,
    /// (name, path, description) from skills/list.
    skills: Vec<(String, String, String)>,
    next: u32,
    /// Mention suggestions: (file name, absolute path).
    hits: Vec<(String, String)>,
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static DATA: RefCell<Data> = RefCell::new(Data::default());
}

const SLASH: [(&str, &str); 4] = [
    ("/compact", "このセッションのコンテキストを圧縮"),
    ("/rename", "このセッションの名前を変更"),
    ("/archive", "このセッションをアーカイブ"),
    ("/new", "新規セッションを開始"),
];

const IMAGE_EXT: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];

fn refresh_chips(app: &AppWindow) {
    let rows: Vec<AttachItem> = DATA.with(|d| {
        d.borrow()
            .items
            .iter()
            .map(|a| AttachItem { id: a.id.as_str().into(), label: a.label.as_str().into(), kind: a.kind.into() })
            .collect()
    });
    app.global::<AttachState>().set_items(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn set_suggestions(app: &AppWindow, kind: &str, rows: Vec<SuggestRow>) {
    let s = app.global::<AttachState>();
    s.set_suggest_kind(kind.into());
    s.set_suggestions(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn add(app: &AppWindow, label: String, kind: &'static str, input: Value) {
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        d.next += 1;
        let id = format!("a{}", d.next);
        d.items.push(Attach { id, label, kind, input });
    });
    refresh_chips(app);
}

fn active_cwd() -> String {
    STATE.with(|s| s.borrow().active_cwd.clone())
}

fn pick_files(app: &AppWindow) {
    let Some(files) = rfd::FileDialog::new().set_title("ファイルまたは写真を追加").pick_files() else { return };
    for f in files {
        let path = f.to_string_lossy().to_string();
        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
        let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if IMAGE_EXT.contains(&ext.as_str()) {
            add(app, name, "image", json!({ "type": "localImage", "path": path }));
        } else {
            add(app, name.clone(), "file", json!({ "type": "mention", "name": name, "path": path }));
        }
    }
}

/// A "+" menu row was picked. Returns true when the action was ours.
pub(crate) fn menu_pick(app: &AppWindow, action: &str) -> bool {
    match action {
        "plus:file" => pick_files(app),
        "plus:slash" => {
            app.set_prompt("/".into());
            on_text(app, "/");
        }
        a if a.starts_with("skill:") => {
            let idx: usize = a[6..].parse().unwrap_or(usize::MAX);
            if let Some((name, path, _)) = DATA.with(|d| d.borrow().skills.get(idx).cloned()) {
                add(app, name.clone(), "skill", json!({ "type": "skill", "name": name, "path": path }));
            }
        }
        _ => return false,
    }
    true
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let weak = app.as_weak();
    app.global::<AttachState>().on_open_plus(move |x, y| {
        let Some(app) = weak.upgrade() else { return };
        let mut items = vec![
            menus::simple_item("plus:file", "ファイルまたは写真を追加", "\u{e145}"),
            menus::simple_item("plus:slash", "スラッシュコマンド", "\u{e86f}"),
        ];
        let skills = DATA.with(|d| d.borrow().skills.clone());
        if !skills.is_empty() {
            items.push(menus::separator());
            for (i, (name, _, _)) in skills.iter().enumerate().take(12) {
                items.push(menus::simple_item(&format!("skill:{i}"), name, "\u{e87b}"));
            }
        }
        menus::open_menu(&app, items, x, y);
        // Fetch the skill list for next time.
        if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
            let cwd = active_cwd();
            if !cwd.is_empty() {
                h.send(Command::Rpc { tag: "attach.skills".into(), method: "skills/list".into(), params: json!({ "cwds": [cwd] }) });
            }
        }
    });
    let weak = app.as_weak();
    app.global::<AttachState>().on_choose_folder(move || {
        let Some(app) = weak.upgrade() else { return };
        if let Some(dir) = rfd::FileDialog::new().set_title("作業フォルダを選択").pick_folder() {
            // A new session in that folder, like picking a project.
            app.global::<AppState>().invoke_new_chat_in(dir.to_string_lossy().to_string().into());
            // A folder that was just added: ask whether to trust it.
            crate::sidebar::ask_trust(&app, &dir.to_string_lossy());
        }
    });
    let weak = app.as_weak();
    app.global::<AttachState>().on_remove(move |id| {
        DATA.with(|d| d.borrow_mut().items.retain(|a| a.id != id.as_str()));
        if let Some(app) = weak.upgrade() {
            refresh_chips(&app);
        }
    });
    let weak = app.as_weak();
    app.global::<AttachState>().on_text_changed(move |t| {
        if let Some(app) = weak.upgrade() {
            on_text(&app, &t);
        }
    });
    let weak = app.as_weak();
    app.global::<AttachState>().on_pick_suggestion(move |id| {
        let Some(app) = weak.upgrade() else { return };
        let text = app.get_prompt().to_string();
        if let Some(rest) = id.strip_prefix("slash:") {
            // A command that needs an argument waits for it; the others are sent at once.
            let cmd = rest.to_string();
            if cmd == "/rename" {
                app.set_prompt(format!("{cmd} ").into());
            } else {
                app.set_prompt("".into());
                if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
                    run_slash(&app, &h, &cmd, "");
                }
            }
            set_suggestions(&app, "", Vec::new());
        } else if let Some(i) = id.strip_prefix("mention:").and_then(|i| i.parse::<usize>().ok()) {
            if let Some((name, path)) = DATA.with(|d| d.borrow().hits.get(i).cloned()) {
                // Replace the "@query" at the end with "@name ".
                let cut = text.rfind('@').unwrap_or(text.len());
                app.set_prompt(format!("{}@{} ", &text[..cut], name).into());
                add(&app, name.clone(), "file", json!({ "type": "mention", "name": name, "path": path }));
            }
            set_suggestions(&app, "", Vec::new());
        }
    });
}

/// The composer text changed: show the @ file suggestions or the slash commands.
fn on_text(app: &AppWindow, text: &str) {
    // "/" at the start, no space yet: command names.
    if text.starts_with('/') && !text.contains(' ') {
        let rows: Vec<SuggestRow> = SLASH
            .iter()
            .filter(|(c, _)| c.starts_with(text))
            .map(|(c, d)| SuggestRow { id: format!("slash:{c}").into(), label: (*c).into(), sub: (*d).into() })
            .collect();
        set_suggestions(app, "slash", rows);
        return;
    }
    // The last word starts with "@": ask the server for files under the working folder.
    let word = text.rsplit(char::is_whitespace).next().unwrap_or("");
    if let Some(q) = word.strip_prefix('@') {
        let cwd = active_cwd();
        if !cwd.is_empty() && !q.is_empty() {
            if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
                h.send(Command::Rpc {
                    tag: format!("attach.mention|{q}"),
                    method: "fuzzyFileSearch".into(),
                    params: json!({ "query": q, "roots": [cwd], "cancellationToken": null }),
                });
            }
            return;
        }
    }
    set_suggestions(app, "", Vec::new());
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if tag == "attach.skills" {
        if let Ok(v) = result {
            let mut skills = Vec::new();
            for entry in v.get("data").and_then(Value::as_array).into_iter().flatten() {
                for s in entry.get("skills").and_then(Value::as_array).into_iter().flatten() {
                    if s.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
                        skills.push((
                            s.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                            s.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
                            s.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        ));
                    }
                }
            }
            DATA.with(|d| d.borrow_mut().skills = skills);
        }
        return;
    }
    let Some(query) = tag.strip_prefix("attach.mention|") else { return };
    let text = app.get_prompt().to_string();
    let word = text.rsplit(char::is_whitespace).next().unwrap_or("");
    if word.strip_prefix('@') != Some(query) {
        return;
    }
    let Ok(v) = result else { return };
    let mut hits = Vec::new();
    for f in v.get("files").and_then(Value::as_array).into_iter().flatten().take(30) {
        if f.get("match_type").and_then(Value::as_str) != Some("file") {
            continue;
        }
        let root = f.get("root").and_then(Value::as_str).unwrap_or("");
        let rel = f.get("path").and_then(Value::as_str).unwrap_or("");
        if rel.starts_with(".git") {
            continue;
        }
        let abs = std::path::Path::new(root).join(rel).to_string_lossy().to_string();
        hits.push((f.get("file_name").and_then(Value::as_str).unwrap_or(rel).to_string(), abs, rel.to_string()));
        if hits.len() == 8 {
            break;
        }
    }
    let rows: Vec<SuggestRow> = hits
        .iter()
        .enumerate()
        .map(|(i, (name, _, rel))| SuggestRow { id: format!("mention:{i}").into(), label: name.as_str().into(), sub: rel.as_str().into() })
        .collect();
    DATA.with(|d| d.borrow_mut().hits = hits.into_iter().map(|(n, p, _)| (n, p)).collect());
    set_suggestions(app, "mention", rows);
}

/// Attachments to send with the next message (clears the chips).
pub(crate) fn take_inputs(app: &AppWindow) -> Vec<Value> {
    let items = DATA.with(|d| std::mem::take(&mut d.borrow_mut().items));
    refresh_chips(app);
    set_suggestions(app, "", Vec::new());
    items.into_iter().map(|a| a.input).collect()
}

fn run_slash(app: &AppWindow, h: &client::Handle, cmd: &str, arg: &str) -> bool {
    let tid = STATE.with(|s| Some(s.borrow().active.clone()).filter(|a| !a.is_empty()));
    let rpc = |tag: String, method: &str, params: Value| h.send(Command::Rpc { tag, method: method.to_string(), params });
    match (cmd, tid) {
        ("/compact", Some(t)) => rpc("prompt.compact".into(), "thread/compact/start", json!({ "threadId": t })),
        ("/rename", Some(t)) if !arg.is_empty() => {
            rpc(format!("chat.rename|{t}|{arg}"), "thread/name/set", json!({ "threadId": t, "name": arg }))
        }
        ("/archive", Some(t)) => rpc(format!("chat.archive|{t}"), "thread/archive", json!({ "threadId": t })),
        ("/new", _) => h.send(Command::NewThread),
        _ => return false,
    }
    let _ = app;
    true
}

/// A message that is a slash command runs it instead of being sent. True when it was one.
pub(crate) fn try_slash(app: &AppWindow, h: &client::Handle, text: &str) -> bool {
    if !text.starts_with('/') {
        return false;
    }
    let (cmd, arg) = text.split_once(' ').unwrap_or((text, ""));
    if !SLASH.iter().any(|(c, _)| *c == cmd) {
        return false;
    }
    run_slash(app, h, cmd, arg.trim())
}
