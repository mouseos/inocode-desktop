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
    // INOCODE_GUI_OPEN opens a screen of the settings first: settings:<section> | provider:<id> |
    // account:<provider>:<account id> | history:<provider>:<account id> | add
    let mut extra: Vec<slint::Timer> = Vec::new();
    if let Ok(what) = std::env::var("INOCODE_GUI_OPEN") {
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
    shot.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 5), move || {
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
