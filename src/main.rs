#![windows_subsystem = "windows"]

mod chat_actions;
mod chat_md;
mod coding;
mod client;
mod accounts_page;
mod attach;
mod callbacks;
mod discovery;
mod errmsg;
mod git_panel;
mod gui_prefs;
mod highlight;
mod markdown;
mod events;
mod metrics;
mod nav;
mod palette;
mod panel_data;
mod prompts;
mod reply;
mod settings_page;
mod sidebar;
mod state;
mod thread_stats;
mod tabs;
mod view;

use state::*;
use view::*;
use client::{Command, Config, Decision, Event, Level, ModelInfo, Msg, Perm, Settings, ThreadInfo};
use discovery::{Outcome, Prefs};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use slint::ComponentHandle;
use std::rc::Rc;

slint::include_modules!();

fn main() {
    let app = AppWindow::new().unwrap();
    app.global::<AppState>().set_messages(ModelRc::from(Rc::new(VecModel::<Message>::default())));
    app.global::<AppState>().set_approvals(ModelRc::from(Rc::new(VecModel::<Approval>::default())));
    app.global::<AppState>().set_projects(ModelRc::from(Rc::new(VecModel::<ProjectItem>::default())));
    STATE.with(|s| {
        s.borrow_mut().prefs = discovery::load_prefs();
        refresh_pickers(&app, &s.borrow());
        refresh_discovery(&app, &s.borrow());
    });

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
                TAB_HOST.with(|h| {
                    if let Some(host) = h.borrow().as_ref() {
                        tabs::event(&app, host, &event);
                    }
                });
                STATE.with(|s| events::apply(&app, s, event));
            }
        });
    }));
    // The first thread starts in the folder that was used last, not where the app was launched.
    if let Some(cwd) = gui_prefs::load_last_cwd() {
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.settings.cwd = Some(cwd);
            handle.send(Command::SetSettings(st.settings.clone()));
        });
    }
    let host = tabs::Host::new(handle.clone());
    tabs::install(&app, &host);
    TAB_HOST.with(|h| *h.borrow_mut() = Some(host));

    callbacks::register(&app, &handle);
    settings_page::init(&app);
    app.run().unwrap();
    // Closing the window: stop the client so the app-server child it started exits too.
    handle.shutdown();
}
