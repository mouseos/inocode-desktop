#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod client;

use client::Event;
use slint::{Model, ModelRc, VecModel};
use std::rc::Rc;

slint::include_modules!();

fn push(model: &VecModel<Message>, role: &str, text: &str) {
    model.push(Message { role: role.into(), text: text.into() });
}

fn apply(app: &AppWindow, event: Event) {
    let model = app.get_messages();
    let Some(model) = model.as_any().downcast_ref::<VecModel<Message>>() else { return };
    match event {
        Event::Status(s) => app.set_status(s.into()),
        Event::Delta(delta) => {
            let n = model.row_count();
            match model.row_data(n.wrapping_sub(1)) {
                Some(mut last) if last.role == "assistant" => {
                    last.text.push_str(&delta);
                    model.set_row_data(n - 1, last);
                }
                _ => push(model, "assistant", &delta),
            }
            app.invoke_scroll_bottom();
        }
        Event::TurnDone => {}
        Event::Error(e) => {
            push(model, "assistant", &format!("エラー: {e}"));
            app.invoke_scroll_bottom();
        }
    }
}

fn main() {
    let app = AppWindow::new().unwrap();
    app.set_messages(ModelRc::from(Rc::new(VecModel::<Message>::default())));

    let url = std::env::var("INOCODE_WS_URL").unwrap_or_else(|_| "ws://127.0.0.1:8765".into());
    let weak = app.as_weak();
    let handle = Rc::new(client::spawn(url, move |event| {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                apply(&app, event);
            }
        });
    }));

    let weak = app.as_weak();
    app.on_submit(move |text| {
        let Some(app) = weak.upgrade() else { return };
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let model = app.get_messages();
        if let Some(model) = model.as_any().downcast_ref::<VecModel<Message>>() {
            push(model, "user", &text);
        }
        handle.send_prompt(text);
        app.set_prompt("".into());
        app.invoke_scroll_bottom();
    });
    app.run().unwrap();
}
