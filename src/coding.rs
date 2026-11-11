//! Basic coding: the approvals that wait per thread, a folder that cannot be written (the
//! sandbox runs read-only), the trust setting of a folder, the folder that was used last, and
//! reconnecting by hand.

use crate::state::*;
use crate::*;
use serde_json::{json, Value};
use std::cell::RefCell;

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
}

// ------------------------------------------------------------------ pure helpers (tested)

/// The approval of the screen from what the client collected.
pub(crate) fn to_approval(info: &client::ApprovalInfo) -> Approval {
    let lines: Vec<ALine> = info.lines.iter().map(|(t, k)| ALine { text: t.as_str().into(), kind: *k }).collect();
    Approval {
        key: info.key.as_str().into(),
        title: info.title.as_str().into(),
        detail: info.detail.as_str().into(),
        summary: info.summary.as_str().into(),
        lines: ModelRc::from(Rc::new(VecModel::from(lines))),
        long: info.long,
        simple: info.simple,
    }
}

/// Add a pending approval (a request that is sent again after a resume replaces the old card).
pub(crate) fn push_pending(list: &mut Vec<(String, Approval)>, thread: &str, a: Approval) {
    list.retain(|(_, x)| x.key != a.key);
    list.push((thread.to_string(), a));
}

/// The approvals shown for `active`: those of that thread (and those that name no thread).
pub(crate) fn visible(list: &[(String, Approval)], active: &str) -> Vec<Approval> {
    list.iter().filter(|(t, _)| t.is_empty() || t == active).map(|(_, a)| a.clone()).collect()
}

/// config/value/write key of a folder's trust: projects."<path>".trust_level. The path is a
/// quoted segment, so dots stay in it, and backslashes and quotes are escaped.
pub(crate) fn trust_key_path(path: &str) -> String {
    format!("projects.\"{}\".trust_level", path.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The text over the composer when the folder cannot be written.
pub(crate) fn sandbox_notice(can_setup: bool) -> String {
    if can_setup {
        "このフォルダは書き込めません。サンドボックスが読み取り専用で動いています。Windows のサンドボックスをセットアップしてください。".into()
    } else {
        "このフォルダは書き込めません。サンドボックスが読み取り専用で動いています。".into()
    }
}

// ------------------------------------------------------------------ screen

pub(crate) fn render_approvals(app: &AppWindow, st: &State) {
    let rows = visible(&st.pending, &st.active);
    app.global::<AppState>().set_approvals(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// The server told what the thread runs with.
pub(crate) fn on_applied(app: &AppWindow, st: &mut State, sandbox: &str, cwd: &str) {
    // Writing was meant (workspace-write or full access) but the server runs it read-only.
    let downgraded = !sandbox.is_empty() && sandbox == "readOnly" && st.settings.wants_write();
    let s = app.global::<AppState>();
    if downgraded {
        let can_setup = cfg!(windows);
        s.set_sandbox_can_setup(can_setup);
        s.set_sandbox_notice(sandbox_notice(can_setup).into());
    } else {
        s.set_sandbox_notice("".into());
    }
    // The folder used last is where the next new chat starts, also after a restart.
    if !cwd.is_empty() && st.settings.cwd.as_deref() != Some(cwd) {
        st.settings.cwd = Some(cwd.to_string());
        gui_prefs::save_last_cwd(cwd);
        if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
            h.send(Command::SetSettings(st.settings.clone()));
        }
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    let s = app.global::<AppState>();
    match (tag, result) {
        ("coding.sandbox.readiness", Ok(v)) => {
            if v.get("status").and_then(Value::as_str) == Some("ready") {
                s.set_sandbox_can_setup(false);
                s.set_sandbox_notice("サンドボックスはセットアップ済みです。新しいチャットから使えます。".into());
            } else if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
                let cwd = STATE.with(|st| st.borrow().active_cwd.clone());
                let mut p = json!({ "mode": "elevated" });
                if !cwd.is_empty() {
                    p["cwd"] = json!(cwd);
                }
                h.send(Command::Rpc { tag: "coding.sandbox.setup".into(), method: "windowsSandbox/setupStart".into(), params: p });
                s.set_sandbox_can_setup(false);
                s.set_sandbox_notice("サンドボックスをセットアップしています…".into());
            }
        }
        ("coding.sandbox.readiness", Err(e)) | ("coding.sandbox.setup", Err(e)) => {
            s.set_sandbox_notice(format!("サンドボックスのセットアップができません: {e}").into());
        }
        _ => {}
    }
}

pub(crate) fn on_notify(app: &AppWindow, method: &str, params: &Value) {
    if method == "windowsSandbox/setupCompleted" {
        let s = app.global::<AppState>();
        if params.get("success").and_then(Value::as_bool).unwrap_or(false) {
            s.set_sandbox_notice("サンドボックスのセットアップが終わりました。新しいチャットから書き込めます。".into());
        } else {
            let e = params.get("error").and_then(Value::as_str).unwrap_or("失敗しました");
            s.set_sandbox_notice(format!("サンドボックスのセットアップができません: {e}").into());
        }
    }
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let st = app.global::<AppState>();
    let h = handle.clone();
    st.on_reconnect(move || h.send(Command::Reconnect));
    let h = handle.clone();
    st.on_sandbox_setup(move || {
        h.send(Command::Rpc { tag: "coding.sandbox.readiness".into(), method: "windowsSandbox/readiness".into(), params: Value::Null });
    });
    if let Ok(which) = std::env::var("INOCODE_GUI_DEMO") {
        // After the connection came up (it clears the requests of the old one).
        let weak = app.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(6), move || {
            if let Some(app) = weak.upgrade() {
                demo(&app, which == "2");
            }
        });
    }
    let weak = app.as_weak();
    st.on_sandbox_dismiss(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<AppState>().set_sandbox_notice("".into());
        }
    });
}

/// Mock only (INOCODE_GUI_DEMO=1: the cards, 2: the notices): the cards of the three kinds of requests, a read-only folder
/// and a lost connection, so that the screen can be looked at without a server.
fn demo(app: &AppWindow, notices: bool) {
    if notices {
        let s = app.global::<AppState>();
        s.set_sandbox_can_setup(true);
        s.set_sandbox_notice(sandbox_notice(true).into());
        s.set_disconnected(true);
        return;
    }
    let changes = json!([
        { "path": "src/main.rs", "kind": { "type": "update" }, "diff": "@@ -1,4 +1,5 @@\n fn main() {\n-    println!(\"hello\");\n+    let name = \"world\";\n+    println!(\"hello {name}\");\n }" },
        { "path": "docs/new.md", "kind": { "type": "add" }, "diff": "# 新しい文書\n\n本文" },
    ]);
    let cmd = format!("cargo build --release --target x86_64-pc-windows-msvc && copy target\\release\\app.exe D:\\deploy\\app.exe\ncd D:\\deploy\nstart app.exe --config {}", "x".repeat(260));
    let reqs = [
        ("item/commandExecution/requestApproval", json!({ "threadId": "", "command": cmd, "cwd": "D:/work/demo", "reason": "リリースビルドを配置します" }), None),
        ("item/fileChange/requestApproval", json!({ "threadId": "", "reason": "main.rs を直し、文書を足します", "grantRoot": "D:/work/demo" }), Some(changes)),
        ("item/permissions/requestApproval", json!({ "threadId": "", "cwd": "D:/work/demo", "reason": "ビルドの出力先に書き込みます", "permissions": { "network": { "enabled": true }, "fileSystem": { "write": ["D:/work/demo/out"] } } }), None),
    ];
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        for (i, (method, params, changes)) in reqs.iter().enumerate() {
            if let Some(mut info) = client::approval_info(method, params, changes.as_ref()) {
                info.key = format!("demo{i}");
                push_pending(&mut st.pending, "", to_approval(&info));
            }
        }
        render_approvals(app, &st);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(key: &str) -> Approval {
        Approval { key: key.into(), ..Approval::default() }
    }

    #[test]
    fn approvals_wait_per_thread() {
        let mut list = Vec::new();
        push_pending(&mut list, "a", approval("1"));
        push_pending(&mut list, "b", approval("2"));
        assert_eq!(visible(&list, "a").len(), 1);
        assert_eq!(visible(&list, "a")[0].key, "1");
        // Switching away and back: the card of the first thread is still there.
        assert_eq!(visible(&list, "b")[0].key, "2");
        assert_eq!(visible(&list, "a")[0].key, "1");
        // The same request sent again after a resume is one card.
        push_pending(&mut list, "a", approval("1"));
        assert_eq!(list.len(), 2);
        // A request that names no thread is shown on every thread.
        push_pending(&mut list, "", approval("3"));
        assert_eq!(visible(&list, "zzz").len(), 1);
    }

    #[test]
    fn trust_key_path_quotes_the_folder() {
        assert_eq!(trust_key_path("D:\\work\\my.app"), "projects.\"D:\\\\work\\\\my.app\".trust_level");
        assert_eq!(trust_key_path("/home/a"), "projects.\"/home/a\".trust_level");
    }

    #[test]
    fn notice_mentions_setup_only_when_there_is_one() {
        assert!(sandbox_notice(true).contains("セットアップ"));
        assert!(!sandbox_notice(false).contains("セットアップ"));
        assert!(sandbox_notice(false).contains("書き込めません"));
    }
}
