#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod client;

use client::{Command, Config, Decision, Event, Level, Msg, ThreadInfo};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

slint::include_modules!();

/// UI-side state that is not part of the Slint models.
#[derive(Default)]
struct State {
    threads: Vec<ThreadInfo>,
    active: String,
    busy: bool,
}

fn with_messages<R>(app: &AppWindow, f: impl FnOnce(&VecModel<Message>) -> R) -> Option<R> {
    let model = app.get_messages();
    let vec = model.as_any().downcast_ref::<VecModel<Message>>()?;
    Some(f(vec))
}

fn push(model: &VecModel<Message>, id: &str, role: &str, text: &str) {
    model.push(Message { id: id.into(), role: role.into(), text: text.into() });
}

fn rebuild_projects(app: &AppWindow, state: &State) {
    // Group threads by the last component of their working directory.
    let mut groups: BTreeMap<String, Vec<ChatItem>> = BTreeMap::new();
    for t in &state.threads {
        let name = std::path::Path::new(&t.cwd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "(フォルダなし)".to_string());
        let selected = t.id == state.active;
        groups.entry(name).or_default().push(ChatItem {
            id: t.id.as_str().into(),
            title: t.title.as_str().into(),
            status: if selected && state.busy { 1 } else { 0 },
            selected,
            depth: 0,
        });
    }
    let projects: Vec<ProjectItem> = groups
        .into_iter()
        .map(|(name, chats)| ProjectItem {
            name: name.into(),
            chats: ModelRc::from(Rc::new(VecModel::from(chats))),
        })
        .collect();
    app.set_projects(ModelRc::from(Rc::new(VecModel::from(projects))));
}

fn apply(app: &AppWindow, state: &RefCell<State>, event: Event) {
    match event {
        Event::Status(text, level) => {
            app.set_status(text.into());
            app.set_status_level(match level {
                Level::Info => 0,
                Level::Ok => 1,
                Level::Warn => 2,
                Level::Error => 3,
            });
        }
        Event::Transport(name) => app.set_transport(name.into()),
        Event::Threads(list) => {
            let mut st = state.borrow_mut();
            st.threads = list;
            rebuild_projects(app, &st);
        }
        Event::ThreadActive { id, title } => {
            let mut st = state.borrow_mut();
            st.active = id;
            st.busy = false;
            app.set_busy(false);
            app.set_session_title(if title.is_empty() { "新しいセッション".into() } else { SharedString::from(title) });
            rebuild_projects(app, &st);
        }
        Event::History(list) => {
            let rows: Vec<Message> = list
                .into_iter()
                .map(|Msg { id, role, text }| Message { id: id.into(), role: role.into(), text: text.into() })
                .collect();
            app.set_messages(ModelRc::from(Rc::new(VecModel::from(rows))));
            app.set_approvals(ModelRc::from(Rc::new(VecModel::<Approval>::default())));
            app.invoke_scroll_bottom();
        }
        Event::AssistantStart => {
            with_messages(app, |m| push(m, "", "assistant", ""));
        }
        Event::Delta(delta) => {
            with_messages(app, |model| {
                let n = model.row_count();
                match model.row_data(n.wrapping_sub(1)) {
                    Some(mut last) if last.role == "assistant" => {
                        last.text.push_str(&delta);
                        model.set_row_data(n - 1, last);
                    }
                    _ => push(model, "", "assistant", &delta),
                }
            });
            app.invoke_scroll_bottom();
        }
        Event::Tool { id, text } => {
            with_messages(app, |model| {
                for i in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(i) {
                        if row.role == "tool" && row.id == id.as_str() {
                            row.text = text.as_str().into();
                            model.set_row_data(i, row);
                            return;
                        }
                    }
                }
                push(model, &id, "tool", &text);
            });
            app.invoke_scroll_bottom();
        }
        Event::TurnStarted => {
            let mut st = state.borrow_mut();
            st.busy = true;
            app.set_busy(true);
            rebuild_projects(app, &st);
        }
        Event::TurnDone { interrupted } => {
            let mut st = state.borrow_mut();
            st.busy = false;
            app.set_busy(false);
            rebuild_projects(app, &st);
            if interrupted {
                with_messages(app, |m| push(m, "", "tool", "中断しました"));
                app.invoke_scroll_bottom();
            }
        }
        Event::Approval { key, title, detail } => {
            let model = app.get_approvals();
            if let Some(vec) = model.as_any().downcast_ref::<VecModel<Approval>>() {
                vec.push(Approval { key: key.into(), title: title.into(), detail: detail.into() });
            }
        }
        Event::ApprovalResolved(key) => {
            let model = app.get_approvals();
            if let Some(vec) = model.as_any().downcast_ref::<VecModel<Approval>>() {
                for i in (0..vec.row_count()).rev() {
                    if vec.row_data(i).map(|a| a.key == key.as_str()).unwrap_or(false) {
                        vec.remove(i);
                    }
                }
            }
        }
        Event::Error(e) => {
            with_messages(app, |m| push(m, "", "assistant", &format!("エラー: {e}")));
            app.invoke_scroll_bottom();
        }
    }
}

fn main() {
    let app = AppWindow::new().unwrap();
    app.set_messages(ModelRc::from(Rc::new(VecModel::<Message>::default())));
    app.set_approvals(ModelRc::from(Rc::new(VecModel::<Approval>::default())));
    app.set_projects(ModelRc::from(Rc::new(VecModel::<ProjectItem>::default())));

    // Default: start `inocode app-server --listen stdio://` as a child process.
    // INOCODE_WS_URL connects to an already running app-server over WebSocket instead;
    // clicking the connection status in the top bar switches between the two.
    let ws_url = std::env::var("INOCODE_WS_URL").ok();
    let cfg = Config {
        use_stdio: ws_url.is_none() && std::env::var("INOCODE_STDIO").map(|v| v != "0").unwrap_or(true),
        ws_url: ws_url.unwrap_or_else(|| "ws://127.0.0.1:8765".into()),
        stdio_program: std::env::var("INOCODE_BIN").unwrap_or_default(),
    };

    // The state is only touched on the UI thread; events cross over via invoke_from_event_loop.
    let weak = app.as_weak();
    let handle = Rc::new(client::spawn(cfg, move |event| {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                STATE.with(|s| apply(&app, s, event));
            }
        });
    }));

    let weak = app.as_weak();
    let h = handle.clone();
    app.on_submit(move |text| {
        let Some(app) = weak.upgrade() else { return };
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        with_messages(&app, |m| push(m, "", "user", &text));
        h.send(Command::Prompt(text));
        app.set_prompt("".into());
        app.invoke_scroll_bottom();
    });
    let h = handle.clone();
    app.on_new_chat(move || h.send(Command::NewThread));
    let h = handle.clone();
    app.on_open_thread(move |id| h.send(Command::OpenThread(id.to_string())));
    let h = handle.clone();
    app.on_interrupt(move || h.send(Command::Interrupt));
    let h = handle.clone();
    app.on_decide(move |key, decision| {
        h.send(Command::Approve { key: key.to_string(), decision: Decision::parse(&decision) })
    });
    let h = handle.clone();
    app.on_switch_transport(move || h.send(Command::SwitchTransport));
    app.run().unwrap();
    // Closing the window: stop the client so the app-server child it started exits too.
    handle.shutdown();
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}
