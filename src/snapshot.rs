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
    let weak = app.as_weak();
    let shot = slint::Timer::default();
    shot.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 3), move || {
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
}
