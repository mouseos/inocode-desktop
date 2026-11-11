//! A way to look at the window without the mouse, the keyboard or the front: with
//! INOCODE_GUI_SNAPSHOT=<file.png> the app opens the tab INOCODE_GUI_TAB (0 = chat ... 3 = team),
//! waits, writes what the window shows (rendered by the app itself) to the file and quits.

use crate::*;
use std::cell::RefCell;
use std::time::Duration;

thread_local! {
    static TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(Vec::new()) };
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub(crate) fn install(app: &AppWindow) {
    let Ok(path) = std::env::var("INOCODE_GUI_SNAPSHOT") else { return };
    let tab = env_num("INOCODE_GUI_TAB", 0) as i32;
    let delay = env_num("INOCODE_GUI_SNAPSHOT_DELAY", 8);
    let weak = app.as_weak();
    let open = slint::Timer::default();
    open.start(slint::TimerMode::SingleShot, Duration::from_secs(delay), move || {
        if let Some(app) = weak.upgrade() {
            app.global::<Tabs>().invoke_select(tab);
        }
    });
    // INOCODE_GUI_ATTACH=<picture>: a sent message with the picture, and chips in the composer.
    let mut extra: Vec<slint::Timer> = Vec::new();
    if let Ok(file) = std::env::var("INOCODE_GUI_ATTACH") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            if let Some(app) = weak.upgrade() {
                crate::attach::demo(&app, std::path::Path::new(&file));
            }
        });
        extra.push(t);
    }
    // INOCODE_GUI_MODELS=<file>: a model/list reply (the `result`) that is shown as the list of
    // models; INOCODE_GUI_OPEN=picker opens the model popup.
    if let Ok(file) = std::env::var("INOCODE_GUI_MODELS") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        // After the real server's own list, so that this one stays.
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 2), move || {
            let Some(app) = weak.upgrade() else { return };
            let Ok(text) = std::fs::read_to_string(&file) else { return };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
            let list = crate::client::parse_models(v.get("response").unwrap_or(&v));
            STATE.with(|s| {
                let mut st = s.borrow_mut();
                st.settings.programs = list.clone();
                st.models = list;
                st.models_loaded = true;
            });
            crate::model_init::on_models(&app);
            STATE.with(|s| crate::view::refresh_pickers(&app, &s.borrow()));
        });
        extra.push(t);
    }
    if std::env::var("INOCODE_GUI_OPEN").as_deref() == Ok("picker") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 3), move || {
            if let Some(app) = weak.upgrade() {
                app.global::<PickerDemo>().set_open(true);
            }
        });
        extra.push(t);
    }
    // INOCODE_GUI_OPEN opens a screen of the settings first: settings:<section> | provider:<id> |
    // account:<provider>:<account id> | history:<provider>:<account id> | add
    if let Ok(what) = std::env::var("INOCODE_GUI_OPEN").map(|w| if w == "picker" { String::new() } else { w }).and_then(|w| if w.is_empty() { Err(std::env::VarError::NotPresent) } else { Ok(w) }) {
        let weak = app.as_weak();
        let first = slint::Timer::default();
        let w1 = what.clone();
        first.start(slint::TimerMode::SingleShot, Duration::from_secs(delay), move || {
            let Some(app) = weak.upgrade() else { return };
            match w1.split(':').next().unwrap_or("") {
                "settings" => {
                    let n = w1.split(':').nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                    app.global::<SettingsPageState>().invoke_open_section(n);
                }
                _ => crate::settings_page::open_usage(&app),
            }
        });
        let weak = app.as_weak();
        let second = slint::Timer::default();
        second.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 2), move || {
            let Some(app) = weak.upgrade() else { return };
            let parts: Vec<&str> = what.split(':').collect();
            match parts.as_slice() {
                ["provider", p] => crate::accounts_page::show_at(&app, p, ""),
                ["account", p, a] => crate::accounts_page::show_at(&app, p, a),
                ["history", p, a] => {
                    crate::accounts_page::show_at(&app, p, a);
                    crate::accounts_page::open_history_modal(&app, a);
                }
                ["add"] => app.global::<AccountsState>().invoke_begin("".into()),
                _ => {}
            }
        });
        extra.extend([first, second]);
    }
    let weak = app.as_weak();
    let shot = slint::Timer::default();
    shot.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 6), move || {
        if let Some(app) = weak.upgrade() {
            if let Ok(buf) = app.window().take_snapshot() {
                if let Ok(file) = std::fs::File::create(&path) {
                    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), buf.width(), buf.height());
                    enc.set_color(png::ColorType::Rgba);
                    enc.set_depth(png::BitDepth::Eight);
                    if let Ok(mut w) = enc.write_header() {
                        let _ = w.write_image_data(buf.as_bytes());
                    }
                }
            }
        }
        let _ = slint::quit_event_loop();
    });
    TIMERS.with(|t| t.borrow_mut().extend([open, shot]));
    TIMERS.with(|t| t.borrow_mut().extend(extra));
}
