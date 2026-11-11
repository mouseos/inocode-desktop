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
    /// The picture of an image attachment, when it could be read.
    thumb: Option<slint::Image>,
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

/// The commands of the composer. The names are Codex's own slash commands; only the ones that the
/// app-server (or this window) can carry out are here.
const SLASH: [(&str, &str); 16] = [
    ("/review", "変更をレビューして問題を見つける(続けて指示も書ける)"),
    ("/diff", "git の差分と、ステージ・コミット・ブランチ"),
    ("/init", "AGENTS.md を作る"),
    ("/compact", "このセッションのコンテキストを圧縮"),
    ("/status", "このセッションの設定とトークンの使用量"),
    ("/new", "新規セッションを開始"),
    ("/clear", "新規セッションを開始"),
    ("/fork", "このセッションを分岐"),
    ("/rename", "このセッションの名前を変更"),
    ("/archive", "このセッションをアーカイブ"),
    ("/copy", "最後の回答をコピー"),
    ("/export", "会話を Markdown のファイルに書き出す"),
    ("/mention", "ファイルを @ で指定する"),
    ("/cd", "作業フォルダを変える"),
    ("/pwd", "作業フォルダを表示"),
    ("/stop", "バックグラウンドのターミナルをすべて止める"),
];

/// What /init sends (the same task as Codex's /init).
const INIT_PROMPT: &str = "Generate a file named AGENTS.md that serves as a contributor guide for this repository. Your goal is to produce a clear, concise, and well-structured document with descriptive headings and actionable explanations for each section. Follow the outline below, but adapt as needed: add sections if relevant, and omit those that do not apply. Target 200-400 words; use Markdown headings. Cover: project structure and module organization; build, test and development commands; coding style and naming conventions; testing guidelines; commit and pull request guidelines. Keep the document specific to this repository.";

const IMAGE_EXT: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];

fn refresh_chips(app: &AppWindow) {
    let rows: Vec<AttachItem> = DATA.with(|d| {
        d.borrow()
            .items
            .iter()
            .map(|a| AttachItem {
                id: a.id.as_str().into(),
                label: a.label.as_str().into(),
                kind: a.kind.into(),
                thumb: a.thumb.clone().unwrap_or_default(),
                has_thumb: a.thumb.is_some(),
            })
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
    // A picture is shown as itself; one that cannot be read stays a pill with its name.
    let thumb = if kind == "image" {
        input.get("path").and_then(Value::as_str).and_then(|p| slint::Image::load_from_path(std::path::Path::new(p)).ok())
    } else {
        None
    };
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        d.next += 1;
        let id = format!("a{}", d.next);
        d.items.push(Attach { id, label, kind, thumb, input });
    });
    refresh_chips(app);
}

fn active_cwd() -> String {
    STATE.with(|s| s.borrow().active_cwd.clone())
}

/// A file to send along: an image goes as a picture, anything else as a mention.
fn attach_file(app: &AppWindow, f: &std::path::Path) {
    let path = f.to_string_lossy().to_string();
    let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
    let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if IMAGE_EXT.contains(&ext.as_str()) {
        add(app, name, "image", json!({ "type": "localImage", "path": path }));
    } else {
        add(app, name.clone(), "file", json!({ "type": "mention", "name": name, "path": path }));
    }
}

fn pick_files(app: &AppWindow) {
    let Some(files) = rfd::FileDialog::new().set_title("ファイルまたは写真を追加").pick_files() else { return };
    for f in files {
        attach_file(app, &f);
    }
}

/// An RGBA picture (what the clipboard gives) as PNG bytes.
fn rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if rgba.len() as u64 != width as u64 * height as u64 * 4 || width == 0 || height == 0 {
        return Err("画像の大きさが合いません".into());
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// The picture on the clipboard becomes an attachment (a PNG file in the temp folder).
/// False when the clipboard holds no picture: the paste is then left to the text field.
fn paste_image(app: &AppWindow) -> bool {
    let Ok(mut clip) = arboard::Clipboard::new() else { return false };
    let Ok(img) = clip.get_image() else { return false };
    match rgba_to_png(img.width as u32, img.height as u32, &img.bytes) {
        Ok(png) => {
            let dir = std::env::temp_dir().join("inocode-desktop-paste");
            let _ = std::fs::create_dir_all(&dir);
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
            let path = dir.join(format!("paste-{stamp}.png"));
            match std::fs::write(&path, png) {
                Ok(()) => add(app, "貼り付けた画像".to_string(), "image", json!({ "type": "localImage", "path": path.to_string_lossy() })),
                Err(e) => app.global::<AppState>().set_status(format!("画像を保存できません: {e}").into()),
            }
            true
        }
        Err(e) => {
            app.global::<AppState>().set_status(format!("画像を貼り付けられません: {e}").into());
            true
        }
    }
}

/// A "+" menu row was picked. Returns true when the action was ours.
pub(crate) fn menu_pick(app: &AppWindow, action: &str) -> bool {
    match action {
        "plus:file" => pick_files(app),
        "plus:paste" => {
            if !paste_image(app) {
                app.global::<AppState>().set_status("クリップボードに画像がありません".into());
            }
        }
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
            menus::simple_item("plus:paste", "クリップボードの画像を貼り付け", "\u{e14f}"),
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
    app.global::<AttachState>().on_paste_image(move || weak.upgrade().map(|app| paste_image(&app)).unwrap_or(false));
    let weak = app.as_weak();
    app.global::<AttachState>().on_files_dropped(move |data| {
        let Some(app) = weak.upgrade() else { return };
        if let Ok(paths) = data.file_paths() {
            for p in paths {
                attach_file(&app, p);
            }
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

/// For the snapshot entry (INOCODE_GUI_ATTACH=<picture file>): a message that was sent with the
/// picture, then the picture and a file attached in the composer.
pub(crate) fn demo(app: &AppWindow, picture: &std::path::Path) {
    attach_file(app, picture);
    let images = peek_images();
    crate::view::messages_with(app, |m| {
        crate::view::push(m, "", "user", "この画像を見てください");
        let n = m.row_count() - 1;
        if let Some(mut row) = m.row_data(n) {
            row.images = ModelRc::from(Rc::new(VecModel::from(images)));
            m.set_row_data(n, row);
        }
    });
    take_inputs(app);
    attach_file(app, picture);
    attach_file(app, std::path::Path::new("Cargo.toml"));
}

/// The pictures that go with the next message (what the chips show), for the chat to show too.
pub(crate) fn peek_images() -> Vec<slint::Image> {
    DATA.with(|d| d.borrow().items.iter().filter_map(|a| a.thumb.clone()).collect())
}

/// Attachments to send with the next message (clears the chips).
pub(crate) fn take_inputs(app: &AppWindow) -> Vec<Value> {
    let items = DATA.with(|d| std::mem::take(&mut d.borrow_mut().items));
    refresh_chips(app);
    set_suggestions(app, "", Vec::new());
    items.into_iter().map(|a| a.input).collect()
}

/// A row in the chat that tells something (no server involved).
fn say(app: &AppWindow, id: &str, text: &str, detail: &str) {
    crate::view::messages_with(app, |m| crate::view::upsert_tool(m, id, text, Some(detail)));
    app.invoke_scroll_bottom();
}

/// The conversation as a Markdown file: who spoke, and what was run.
pub(crate) fn export_markdown(rows: &[(String, String, String)]) -> String {
    let mut out = String::from("# 会話\n");
    for (role, text, detail) in rows {
        match role.as_str() {
            "user" => out.push_str(&format!("\n## ユーザー\n\n{text}\n")),
            "assistant" => out.push_str(&format!("\n## アシスタント\n\n{text}\n")),
            _ => {
                out.push_str(&format!("\n> {text}\n"));
                if !detail.trim().is_empty() {
                    out.push_str(&format!("\n```\n{}\n```\n", detail.trim_end()));
                }
            }
        }
    }
    out
}

/// "/status": what this session runs with.
fn status_text(st: &State) -> String {
    let perm = match crate::state::current_perm(st) {
        Perm::Ask => "承認を依頼",
        Perm::Guardian => "代わりに承認",
        Perm::Full => "フルアクセス",
        Perm::Server => "カスタム",
    };
    let model = if st.settings.model.is_some() { st.settings.model.clone().unwrap_or_default() } else { st.applied_model.clone() };
    let effort = st.settings.effort.clone().unwrap_or_else(|| st.applied_effort.clone());
    let mut lines = vec![format!("モデル: {model}"), format!("思考量: {effort}"), format!("権限: {perm}"), format!("作業フォルダ: {}", st.active_cwd)];
    if !st.branch.is_empty() {
        lines.push(format!("ブランチ: {}", st.branch));
    }
    lines.join("\n")
}

fn run_slash(app: &AppWindow, h: &client::Handle, cmd: &str, arg: &str) -> bool {
    let tid = STATE.with(|s| Some(s.borrow().active.clone()).filter(|a| !a.is_empty()));
    let rpc = |tag: String, method: &str, params: Value| h.send(Command::Rpc { tag, method: method.to_string(), params });
    match (cmd, tid) {
        ("/review", Some(t)) => {
            let target = if arg.is_empty() { json!({ "type": "uncommittedChanges" }) } else { json!({ "type": "custom", "instructions": arg }) };
            rpc("slash.review".into(), "review/start", json!({ "threadId": t, "target": target }));
        }
        ("/diff", _) => crate::git_panel::open(app),
        ("/init", Some(_)) => h.send(Command::Prompt(INIT_PROMPT.to_string())),
        ("/status", _) => {
            let mut text = STATE.with(|s| status_text(&s.borrow()));
            let usage = app.global::<AppState>().get_usage_text().to_string();
            if !usage.is_empty() {
                text.push_str(&format!("
コンテキスト: {usage}"));
            }
            say(app, "slash:status", "セッションの状態", &text);
        }
        ("/clear", _) => h.send(Command::NewThread),
        ("/fork", Some(_)) => {
            let n = app.global::<AppState>().get_messages().row_count();
            if n > 0 {
                app.global::<MessageActions>().invoke_act("fork".into(), (n - 1) as i32);
            }
        }
        ("/copy", _) => {
            let model = app.global::<AppState>().get_messages();
            if let Some(i) = (0..model.row_count()).rev().find(|i| model.row_data(*i).map(|m| m.role == "assistant").unwrap_or(false)) {
                app.global::<MessageActions>().invoke_act("copy".into(), i as i32);
            }
        }
        ("/export", _) => {
            let model = app.global::<AppState>().get_messages();
            let rows: Vec<(String, String, String)> = (0..model.row_count())
                .filter_map(|i| model.row_data(i))
                .map(|m| (m.role.to_string(), m.text.to_string(), m.detail.to_string()))
                .collect();
            if rows.is_empty() {
                return false;
            }
            if let Some(path) = rfd::FileDialog::new().set_title("会話を書き出す").set_file_name("conversation.md").add_filter("Markdown", &["md"]).save_file() {
                if let Err(e) = std::fs::write(&path, export_markdown(&rows)) {
                    app.global::<AppState>().set_status(format!("書き出せません: {e}").into());
                }
            }
        }
        ("/mention", _) => {
            let t = app.get_prompt().to_string();
            let t = t.trim_end_matches(|c: char| c == '/' || c.is_alphabetic()).to_string();
            app.set_prompt(format!("{t}@").into());
        }
        ("/cd", _) => app.global::<AttachState>().invoke_choose_folder(),
        ("/pwd", _) => {
            let cwd = active_cwd();
            say(app, "slash:pwd", "作業フォルダ", &cwd);
        }
        ("/stop", Some(t)) => rpc("slash.stop".into(), "thread/backgroundTerminals/clean", json!({ "threadId": t })),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clipboard_picture_becomes_a_png() {
        let rgba: Vec<u8> = (0..2 * 3 * 4).map(|i| i as u8).collect();
        let png = rgba_to_png(2, 3, &rgba).unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        // and back: the same pixels
        let mut r = png::Decoder::new(std::io::Cursor::new(&png)).read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size().unwrap()];
        let info = r.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 3));
        assert_eq!(&buf[..rgba.len()], &rgba[..]);
        // a size that does not fit the bytes is refused
        assert!(rgba_to_png(2, 2, &rgba).is_err());
        assert!(rgba_to_png(0, 0, &[]).is_err());
    }
}
