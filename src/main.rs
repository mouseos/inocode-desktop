#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod client;

use client::{Command, Config, Decision, Event, Level, ModelInfo, Msg, Settings, ThreadInfo};
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
    models: Vec<ModelInfo>,
    settings: Settings,
    /// What the server reported for the active thread.
    applied_model: String,
    applied_effort: String,
}

/// Permission presets: (label, approval policy, sandbox mode).
const PERMS: [(&str, &str, &str); 4] = [
    ("確認して実行", "on-request", "workspace-write"),
    ("信頼済みのみ自動", "untrusted", "workspace-write"),
    ("読み取り専用", "on-request", "read-only"),
    ("フルアクセス", "never", "danger-full-access"),
];

fn strings(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    let v: Vec<SharedString> = items.into_iter().map(SharedString::from).collect();
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn effort_label(e: &str) -> String {
    match e {
        "none" => "なし",
        "minimal" => "最小",
        "low" => "低",
        "medium" => "中",
        "high" => "高",
        "xhigh" => "超高",
        "max" => "最大",
        "ultra" => "ウルトラ",
        "persistent" => "持続",
        other => other,
    }
    .to_string()
}

/// Models offered in the picker, by the desktop client's rules: not hidden, and usable
/// with the current Daybreak choice (a model that advertises access programs must offer
/// "standard" when the choice is off, or a daybreak program when it is on).
fn visible_models(st: &State) -> Vec<&ModelInfo> {
    st.models
        .iter()
        .filter(|m| !m.hidden && m.usable(st.settings.daybreak))
        .collect()
}

/// Selected model: the user's choice, else what the server uses, else the catalog default,
/// else the first visible model.
fn selected_model<'a>(st: &'a State, visible: &[&'a ModelInfo]) -> Option<&'a ModelInfo> {
    let by_id = |id: &str| visible.iter().copied().find(|m| m.id == id);
    st.settings
        .model
        .as_deref()
        .and_then(by_id)
        .or_else(|| by_id(&st.applied_model))
        .or_else(|| visible.iter().copied().find(|m| m.is_default))
        .or_else(|| visible.first().copied())
}

/// Push the model / effort / permission choices into the window.
fn refresh_selectors(app: &AppWindow, st: &State) {
    let visible = visible_models(st);
    let sel = selected_model(st, &visible);
    let mi = sel.and_then(|m| visible.iter().position(|v| v.id == m.id));
    app.set_model_names(strings(visible.iter().map(|m| m.name.clone())));
    app.set_model_index(mi.map(|i| i as i32).unwrap_or(-1));
    let efforts: Vec<String> = sel.map(|m| m.efforts.clone()).unwrap_or_default();
    let eff = st
        .settings
        .effort
        .clone()
        .filter(|e| efforts.contains(e))
        .or_else(|| Some(st.applied_effort.clone()).filter(|e| efforts.contains(e)))
        .or_else(|| sel.map(|m| m.default_effort.clone()));
    let ei = eff.and_then(|e| efforts.iter().position(|x| *x == e)).unwrap_or(0);
    app.set_effort_names(strings(efforts.iter().map(|e| format!("推論 {}", effort_label(e)))));
    app.set_effort_index(if efforts.is_empty() { -1 } else { ei as i32 });
    let pi = PERMS
        .iter()
        .position(|(_, a, sb)| *a == st.settings.approval && *sb == st.settings.sandbox)
        .unwrap_or(0);
    app.set_perm_index(pi as i32);
    // The switch is offered when any listed model advertises a Daybreak program.
    app.set_daybreak_available(st.models.iter().any(|m| !m.hidden && m.daybreak()));
    app.set_daybreak_on(st.settings.daybreak);
}

fn sandbox_mode(policy_type: &str) -> &'static str {
    match policy_type {
        "readOnly" => "read-only",
        "dangerFullAccess" => "danger-full-access",
        _ => "workspace-write",
    }
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
    let mut groups: BTreeMap<String, (String, Vec<ChatItem>)> = BTreeMap::new();
    for t in &state.threads {
        let name = std::path::Path::new(&t.cwd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "(フォルダなし)".to_string());
        let selected = t.id == state.active;
        groups.entry(name).or_insert_with(|| (t.cwd.clone(), Vec::new())).1.push(ChatItem {
            id: t.id.as_str().into(),
            title: t.title.as_str().into(),
            status: if selected && state.busy { 1 } else { 0 },
            selected,
            depth: 0,
        });
    }
    let projects: Vec<ProjectItem> = groups
        .into_iter()
        .map(|(name, (cwd, chats))| ProjectItem {
            name: name.into(),
            cwd: cwd.into(),
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
        Event::Models(list) => {
            let mut st = state.borrow_mut();
            st.settings.programs = list.clone();
            st.models = list;
            refresh_selectors(app, &st);
        }
        Event::Applied { model, effort, approval, sandbox, cwd } => {
            let mut st = state.borrow_mut();
            st.applied_model = model;
            st.applied_effort = effort;
            if !approval.is_empty() && approval != "granular" {
                st.settings.approval = approval;
            }
            if !sandbox.is_empty() {
                st.settings.sandbox = sandbox_mode(&sandbox).to_string();
            }
            app.set_cwd_text(cwd.into());
            refresh_selectors(app, &st);
        }
        Event::Account { logged_in, label } => {
            app.set_logged_in(logged_in);
            app.set_account_label(label.into());
            if !logged_in {
                app.set_status("未ログイン".into());
                app.set_status_level(2);
            }
        }
        Event::Usage { percent, text } => {
            app.set_context_percent(percent);
            app.set_usage_text(text.into());
        }
        Event::RateLimits(text) => app.set_rate_text(text.into()),
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
    app.set_perm_names(strings(PERMS.iter().map(|p| p.0.to_string())));
    app.set_perm_index(0);

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
    // Model / effort / permission choices are kept in STATE and sent as thread settings;
    // the client applies them to thread/start and as overrides on every turn/start.
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_pick_model(move |i| {
        let Some(app) = weak.upgrade() else { return };
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            let Some(m) = visible_models(&st).get(i as usize).map(|m| (*m).clone()) else { return };
            st.settings.model = Some(m.id.clone());
            if !st.settings.effort.as_ref().map(|e| m.efforts.contains(e)).unwrap_or(false) {
                st.settings.effort = Some(m.default_effort.clone()).filter(|e| !e.is_empty());
            }
            h.send(Command::SetSettings(st.settings.clone()));
            refresh_selectors(&app, &st);
        });
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_pick_effort(move |i| {
        let Some(app) = weak.upgrade() else { return };
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            let visible = visible_models(&st);
            let efforts = selected_model(&st, &visible).map(|m| m.efforts.clone()).unwrap_or_default();
            if let Some(e) = efforts.get(i as usize) {
                st.settings.effort = Some(e.clone());
                h.send(Command::SetSettings(st.settings.clone()));
                refresh_selectors(&app, &st);
            }
        });
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_pick_perm(move |i| {
        let Some(app) = weak.upgrade() else { return };
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            let Some((_, approval, sandbox)) = PERMS.get(i as usize) else { return };
            st.settings.approval = approval.to_string();
            st.settings.sandbox = sandbox.to_string();
            st.settings.perm_changed = true;
            h.send(Command::SetSettings(st.settings.clone()));
            refresh_selectors(&app, &st);
        });
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_toggle_daybreak(move || {
        let Some(app) = weak.upgrade() else { return };
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.settings.daybreak = !st.settings.daybreak;
            let usable = st
                .settings
                .model
                .as_deref()
                .and_then(|id| st.models.iter().find(|m| m.id == id))
                .map(|m| m.usable(st.settings.daybreak))
                .unwrap_or(true);
            if !usable {
                st.settings.model = None;
            }
            h.send(Command::SetSettings(st.settings.clone()));
            // The choice is made when a thread is created, so start a fresh one.
            h.send(Command::NewThread);
            refresh_selectors(&app, &st);
        });
    });
    let h = handle.clone();
    app.on_login(move || h.send(Command::Login));
    let h = handle.clone();
    app.on_new_chat_in(move |cwd| h.send(Command::NewThreadIn(cwd.to_string())));
    let h = handle.clone();
    app.on_switch_transport(move || h.send(Command::SwitchTransport));
    app.run().unwrap();
    // Closing the window: stop the client so the app-server child it started exits too.
    handle.shutdown();
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}
