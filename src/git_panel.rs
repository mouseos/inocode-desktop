//! Git of the working folder (the panel of ui/git_panel.slint): the changed files and their diff,
//! stage and unstage, commit, branches (list, switch, create), and the review of the changes.
//! The git commands run in the working folder of the active thread.

use crate::state::STATE;
use crate::*;
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::process::Command as Proc;

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static ENTRIES: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

/// A diff or a file is read up to this size.
const MAX_BYTES: usize = 400_000;
/// And shown up to this many lines.
const MAX_LINES: usize = 3000;

// ------------------------------------------------------------------ pure helpers (tested)

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub path: String,
    /// Index (staged) status and working tree status, as in `git status --porcelain`.
    pub x: char,
    pub y: char,
}

/// `git status --porcelain=v1 -z`: entries "XY path\0"; a rename or copy has the old path after.
pub(crate) fn parse_status(out: &str) -> Vec<Entry> {
    let mut parts = out.split('\0').filter(|p| !p.is_empty());
    let mut entries = Vec::new();
    while let Some(p) = parts.next() {
        let mut chars = p.chars();
        let (Some(x), Some(y), Some(' ')) = (chars.next(), chars.next(), chars.next()) else { continue };
        let path = chars.as_str().to_string();
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            let _old = parts.next();
        }
        entries.push(Entry { path, x, y });
    }
    entries
}

/// `git diff --numstat`: "added<TAB>removed<TAB>path" (a binary file has "-").
pub(crate) fn parse_numstat(out: &str) -> HashMap<String, (i64, i64)> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let (a, d, path) = (f.next()?, f.next()?, f.next()?);
            Some((path.to_string(), (a.parse().unwrap_or(0), d.parse().unwrap_or(0))))
        })
        .collect()
}

/// The letter of a file in the list.
pub(crate) fn status_letter(e: &Entry) -> &'static str {
    if e.x == '?' {
        return "?";
    }
    let c = if e.y != ' ' { e.y } else { e.x };
    match c {
        'A' => "A",
        'D' => "D",
        'R' | 'C' => "R",
        _ => "M",
    }
}

/// The file has changes that are not staged (or is new): the button stages them.
pub(crate) fn can_stage(e: &Entry) -> bool {
    e.y != ' ' || e.x == '?'
}

pub(crate) fn is_staged(e: &Entry) -> bool {
    e.x != ' ' && e.x != '?'
}

/// A diff as lines with their kind (0 plain, 1 added, 2 removed, 4 header or hunk).
pub(crate) fn diff_lines(text: &str) -> Vec<(String, i32)> {
    text.lines()
        .take(MAX_LINES)
        .map(|l| {
            let kind = if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff --git") || l.starts_with("index ") || l.starts_with("@@") || l.starts_with("new file") || l.starts_with("deleted file") {
                4
            } else if l.starts_with('+') {
                1
            } else if l.starts_with('-') {
                2
            } else {
                0
            };
            (l.to_string(), kind)
        })
        .collect()
}

/// A new file shown as added lines.
pub(crate) fn as_added(text: &str) -> Vec<(String, i32)> {
    text.lines().take(MAX_LINES).map(|l| (format!("+{l}"), 1)).collect()
}

pub(crate) fn branch_names(out: &str) -> Vec<String> {
    out.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('(')).map(str::to_string).collect()
}

/// A name that git accepts for a branch and that cannot be taken for an option.
pub(crate) fn valid_branch_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with('-')
        && !n.starts_with('/')
        && !n.ends_with('/')
        && !n.ends_with('.')
        && !n.ends_with(".lock")
        && !n.contains("..")
        && !n.contains("//")
        && !n.contains("@{")
        && !n.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
}

fn stat_label(a: i64, d: i64) -> String {
    if a == 0 && d == 0 {
        String::new()
    } else {
        format!("+{a} −{d}")
    }
}

// ------------------------------------------------------------------ git

fn git(cwd: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Proc::new("git");
    cmd.arg("-C").arg(cwd).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|e| format!("git を起動できません: {e}"))?;
    if out.status.success() {
        let mut bytes = out.stdout;
        bytes.truncate(MAX_BYTES);
        Ok(String::from_utf8_lossy(&bytes).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err };
        Err(msg.lines().next().unwrap_or("git が失敗しました").to_string())
    }
}

fn cwd() -> String {
    STATE.with(|s| s.borrow().active_cwd.clone())
}

// ------------------------------------------------------------------ screen

fn set_note(app: &AppWindow, text: &str) {
    app.global::<GitState>().set_note(text.into());
}

fn set_diff(app: &AppWindow, title: &str, lines: Vec<(String, i32)>) {
    let g = app.global::<GitState>();
    g.set_diff_title(title.into());
    let rows: Vec<GitLine> = lines.into_iter().map(|(t, k)| GitLine { text: t.into(), kind: k }).collect();
    g.set_diff(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// The branch under the composer (empty: not a git folder).
pub(crate) fn set_branch(app: &AppWindow, branch: &str) {
    app.global::<GitState>().set_branch(branch.into());
}

fn render_files(app: &AppWindow, selected: Option<usize>, stats: &HashMap<String, (i64, i64)>) {
    let rows: Vec<GitFile> = ENTRIES.with(|e| {
        e.borrow()
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let (a, d) = stats.get(&f.path).copied().unwrap_or((0, 0));
                GitFile {
                    path: f.path.as_str().into(),
                    status: status_letter(f).into(),
                    can_stage: can_stage(f),
                    staged: is_staged(f),
                    stat: stat_label(a, d).into(),
                    selected: Some(i) == selected,
                }
            })
            .collect()
    });
    app.global::<GitState>().set_files(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// Read the state of the folder again.
fn refresh(app: &AppWindow) {
    let cwd = cwd();
    let g = app.global::<GitState>();
    if cwd.is_empty() {
        set_note(app, "作業フォルダがありません");
        return;
    }
    match git(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(b) => {
            let b = b.trim().to_string();
            set_branch(app, &b);
            STATE.with(|s| s.borrow_mut().branch = b);
        }
        Err(e) => {
            set_branch(app, "");
            ENTRIES.with(|x| x.borrow_mut().clear());
            render_files(app, None, &HashMap::new());
            set_diff(app, "", Vec::new());
            set_note(app, &format!("git のフォルダではありません: {e}"));
            return;
        }
    }
    let entries = git(&cwd, &["status", "--porcelain=v1", "-z"]).map(|o| parse_status(&o)).unwrap_or_default();
    let stats = git(&cwd, &["diff", "HEAD", "--numstat"]).or_else(|_| git(&cwd, &["diff", "--numstat"])).map(|o| parse_numstat(&o)).unwrap_or_default();
    ENTRIES.with(|x| *x.borrow_mut() = entries);
    render_files(app, None, &stats);
    set_diff(app, "", Vec::new());
    let branches = git(&cwd, &["branch", "--format=%(refname:short)"]).map(|o| branch_names(&o)).unwrap_or_default();
    g.set_branches(ModelRc::from(Rc::new(VecModel::from(branches.into_iter().map(SharedString::from).collect::<Vec<_>>()))));
}

fn select(app: &AppWindow, i: usize) {
    let Some(entry) = ENTRIES.with(|e| e.borrow().get(i).cloned()) else { return };
    let cwd = cwd();
    let lines = if entry.x == '?' {
        let path = std::path::Path::new(&cwd).join(&entry.path);
        match std::fs::read(&path) {
            Ok(mut bytes) => {
                bytes.truncate(MAX_BYTES);
                as_added(&String::from_utf8_lossy(&bytes))
            }
            Err(e) => vec![(format!("読めません: {e}"), 0)],
        }
    } else {
        let text = git(&cwd, &["diff", "HEAD", "--", &entry.path]).or_else(|_| git(&cwd, &["diff", "--", &entry.path])).unwrap_or_else(|e| e);
        diff_lines(&text)
    };
    // the numbers are read again with the list
    let numstat = git(&cwd, &["diff", "HEAD", "--numstat"]).map(|o| parse_numstat(&o)).unwrap_or_default();
    render_files(app, Some(i), &numstat);
    set_diff(app, &entry.path, lines);
}

/// Run a git command that changes something, then show the new state.
fn change(app: &AppWindow, args: &[&str], done: &str) -> bool {
    let cwd = cwd();
    match git(&cwd, args) {
        Ok(out) => {
            refresh(app);
            let first = out.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            set_note(app, &if first.is_empty() { done.to_string() } else { format!("{done}: {first}") });
            true
        }
        Err(e) => {
            set_note(app, &e);
            false
        }
    }
}

pub(crate) fn open(app: &AppWindow) {
    let g = app.global::<GitState>();
    g.set_open(true);
    g.set_show_branches(false);
    set_note(app, "");
    refresh(app);
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let g = app.global::<GitState>();
    let weak = app.as_weak();
    g.on_show(move || {
        if let Some(app) = weak.upgrade() {
            open(&app);
        }
    });
    let weak = app.as_weak();
    g.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            set_note(&app, "");
            refresh(&app);
        }
    });
    let weak = app.as_weak();
    g.on_select(move |i| {
        if let Some(app) = weak.upgrade() {
            select(&app, i.max(0) as usize);
        }
    });
    let weak = app.as_weak();
    g.on_toggle_stage(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some(e) = ENTRIES.with(|x| x.borrow().get(i.max(0) as usize).cloned()) else { return };
        if can_stage(&e) {
            change(&app, &["add", "--", &e.path], "ステージしました");
        } else {
            change(&app, &["restore", "--staged", "--", &e.path], "ステージを解除しました");
        }
    });
    let weak = app.as_weak();
    g.on_stage_all(move || {
        if let Some(app) = weak.upgrade() {
            change(&app, &["add", "-A"], "すべてステージしました");
        }
    });
    let weak = app.as_weak();
    g.on_commit(move || {
        let Some(app) = weak.upgrade() else { return };
        let g = app.global::<GitState>();
        let message = g.get_commit_message().trim().to_string();
        if message.is_empty() {
            set_note(&app, "コミットのメッセージを入力してください");
            return;
        }
        if !ENTRIES.with(|x| x.borrow().iter().any(is_staged)) {
            set_note(&app, "ステージした変更がありません");
            return;
        }
        if change(&app, &["commit", "-m", &message], "コミットしました") {
            g.set_commit_message("".into());
        }
    });
    let weak = app.as_weak();
    g.on_switch_branch(move |name| {
        let Some(app) = weak.upgrade() else { return };
        if name.as_str() == app.global::<GitState>().get_branch().as_str() {
            return;
        }
        if change(&app, &["switch", &name], "ブランチを切り替えました") {
            app.global::<GitState>().set_show_branches(false);
        }
    });
    let weak = app.as_weak();
    g.on_create_branch(move || {
        let Some(app) = weak.upgrade() else { return };
        let g = app.global::<GitState>();
        let name = g.get_new_branch().trim().to_string();
        if !valid_branch_name(&name) {
            set_note(&app, "ブランチの名前に使えない文字があります");
            return;
        }
        if change(&app, &["switch", "-c", &name], "ブランチを作りました") {
            g.set_new_branch("".into());
            g.set_show_branches(false);
        }
    });
    let weak = app.as_weak();
    g.on_review(move || {
        let Some(app) = weak.upgrade() else { return };
        let thread = STATE.with(|s| Some(s.borrow().active.clone()).filter(|a| !a.is_empty()));
        let Some(thread) = thread else { return };
        if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
            h.send(Command::Rpc {
                tag: "slash.review".into(),
                method: "review/start".into(),
                params: json!({ "threadId": thread, "target": { "type": "uncommittedChanges" } }),
            });
        }
        app.global::<GitState>().set_open(false);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_entries_with_renames_and_new_files() {
        let out = " M src/a.rs\0A  new.rs\0?? notes.txt\0R  moved.rs\0old.rs\0MM both.rs\0 D gone.rs\0";
        let e = parse_status(out);
        let paths: Vec<&str> = e.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "new.rs", "notes.txt", "moved.rs", "both.rs", "gone.rs"]);
        let letters: Vec<&str> = e.iter().map(status_letter).collect();
        assert_eq!(letters, vec!["M", "A", "?", "R", "M", "D"]);
        // a change that is not staged can be staged; a staged one only unstaged
        assert!(can_stage(&e[0]) && !is_staged(&e[0]));
        assert!(!can_stage(&e[1]) && is_staged(&e[1]));
        assert!(can_stage(&e[2]) && !is_staged(&e[2]));
        assert!(can_stage(&e[4]) && is_staged(&e[4]));
        assert!(parse_status("").is_empty());
    }

    #[test]
    fn numstat_and_diff_lines() {
        let n = parse_numstat("3\t1\tsrc/a.rs\n-\t-\timg.png\n");
        assert_eq!(n["src/a.rs"], (3, 1));
        assert_eq!(n["img.png"], (0, 0));
        assert_eq!(stat_label(3, 1), "+3 −1");
        assert_eq!(stat_label(0, 0), "");
        let lines = diff_lines("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same");
        let kinds: Vec<i32> = lines.iter().map(|l| l.1).collect();
        assert_eq!(kinds, vec![4, 4, 4, 4, 2, 1, 0]);
        assert_eq!(as_added("a\nb"), vec![("+a".to_string(), 1), ("+b".to_string(), 1)]);
    }

    #[test]
    fn branch_names_and_validation() {
        assert_eq!(branch_names("main\nfeat/x\n(HEAD detached at abc)\n"), vec!["main", "feat/x"]);
        assert!(valid_branch_name("feat/new-thing"));
        for bad in ["", "-x", "a b", "a..b", "a~1", "x.lock", "/a", "a/", "a//b", "a:b", "a@{1}", "a\\b"] {
            assert!(!valid_branch_name(bad), "{bad}");
        }
    }
}
