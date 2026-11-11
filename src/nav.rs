//! Window navigation: the history behind the back / forward arrows, the sidebar toggle and the
//! application menu (the ≡ button).
//!
//! Claude Desktop: Back / Forward (arbRxbtBkP, PH29MShDiy), Toggle sidebar (The8hwLUFg), and
//! the Electron menu File / View / Window / Help (13-ux-mapping 1.14). Only entries that work
//! here are in the menu.

use crate::sidebar as menus;
use crate::*;

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Entry {
    Thread(String),
    Tab(i32),
    Settings(i32),
}

#[derive(Default)]
pub(crate) struct History {
    entries: Vec<Entry>,
    idx: usize,
    /// Set while a back / forward step is being applied.
    navigating: bool,
    /// A thread the step asked the server to open; its ThreadActive must not be recorded again.
    expect: Option<String>,
}

impl History {
    pub(crate) fn visit(&mut self, e: Entry) -> bool {
        if self.navigating {
            return false;
        }
        if let Entry::Thread(id) = &e {
            if self.expect.as_deref() == Some(id) {
                self.expect = None;
                return false;
            }
        }
        if self.entries.get(self.idx) == Some(&e) {
            return false;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.idx + 1);
        }
        self.entries.push(e);
        self.idx = self.entries.len() - 1;
        true
    }

    pub(crate) fn can_back(&self) -> bool {
        self.idx > 0
    }

    pub(crate) fn can_forward(&self) -> bool {
        self.idx + 1 < self.entries.len()
    }

    /// Move one step; the entry to show.
    pub(crate) fn step(&mut self, back: bool) -> Option<Entry> {
        if back && self.can_back() {
            self.idx -= 1;
        } else if !back && self.can_forward() {
            self.idx += 1;
        } else {
            return None;
        }
        self.entries.get(self.idx).cloned()
    }
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static HISTORY: RefCell<History> = RefCell::new(History::default());
}

fn flags(app: &AppWindow) {
    let (b, f) = HISTORY.with(|h| {
        let h = h.borrow();
        (h.can_back(), h.can_forward())
    });
    app.global::<NavState>().set_can_back(b);
    app.global::<NavState>().set_can_forward(f);
}

pub(crate) fn visit(app: &AppWindow, e: Entry) {
    if HISTORY.with(|h| h.borrow_mut().visit(e)) {
        flags(app);
    }
}

fn go(app: &AppWindow, back: bool) {
    let Some(e) = HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        let e = h.step(back);
        if e.is_some() {
            h.navigating = true;
        }
        e
    }) else {
        return;
    };
    let s = app.global::<SettingsPageState>();
    match &e {
        Entry::Thread(id) => {
            s.set_open(false);
            app.global::<Tabs>().set_current(0);
            HISTORY.with(|h| h.borrow_mut().expect = Some(id.clone()));
            if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
                h.send(Command::OpenThread(id.clone()));
            }
        }
        Entry::Tab(i) => {
            s.set_open(false);
            app.global::<Tabs>().invoke_select(*i);
        }
        Entry::Settings(section) => {
            app.global::<Tabs>().set_current(0);
            s.set_open(true);
            crate::settings_page::show_section(app, *section);
        }
    }
    HISTORY.with(|h| h.borrow_mut().navigating = false);
    flags(app);
}

fn toggle_sidebar(app: &AppWindow) {
    let open = !app.get_sidebar_open();
    app.set_sidebar_open(open);
    crate::settings_page::set_sidebar_open(open);
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let weak = app.as_weak();
    app.global::<NavState>().on_back(move || {
        if let Some(app) = weak.upgrade() {
            go(&app, true);
        }
    });
    let weak = app.as_weak();
    app.global::<NavState>().on_forward(move || {
        if let Some(app) = weak.upgrade() {
            go(&app, false);
        }
    });
    let weak = app.as_weak();
    app.global::<NavState>().on_toggle_sidebar(move || {
        if let Some(app) = weak.upgrade() {
            toggle_sidebar(&app);
        }
    });
    let weak = app.as_weak();
    app.global::<NavState>().on_visited_tab(move |i| {
        if let Some(app) = weak.upgrade() {
            visit(&app, Entry::Tab(i));
        }
    });
}

/// The ≡ menu. Group names are disabled rows.
pub(crate) fn app_menu(app: &AppWindow) -> Vec<MenuItem> {
    let it = menus::simple_item;
    let head = |label: &str| {
        let mut m = it("", label, "");
        m.disabled = true;
        m
    };
    let fullscreen = app.window().is_fullscreen();
    let maximized = app.window().is_maximized();
    vec![
        head("ファイル"),
        it("app:new", "新規セッション", ""),
        it("app:palette", "コマンドパレット", ""),
        it("app:settings", "設定", ""),
        it("app:quit", "終了", ""),
        menus::separator(),
        head("表示"),
        it("app:sidebar", "サイドバーを開閉", ""),
        it("app:panel", "右パネルを開閉", ""),
        it("app:fullscreen", if fullscreen { "全画面を終了" } else { "全画面" }, ""),
        menus::separator(),
        head("ウィンドウ"),
        it("app:min", "最小化", ""),
        it("app:max", if maximized { "元のサイズに戻す" } else { "最大化" }, ""),
        menus::separator(),
        head("ヘルプ"),
        it("app:about", "バージョン情報", ""),
    ]
}

/// Menu actions of the ≡ menu. Returns true when the action was ours.
pub(crate) fn menu_pick(app: &AppWindow, action: &str) -> bool {
    let Some(a) = action.strip_prefix("app:") else { return false };
    match a {
        "new" => app.global::<AppState>().invoke_new_chat(),
        "palette" => app.global::<PaletteState>().set_open(true),
        "settings" => crate::settings_page::open(app),
        "quit" => {
            let _ = slint::quit_event_loop();
        }
        "sidebar" => toggle_sidebar(app),
        "panel" => app.set_panel_open(!app.get_panel_open()),
        "fullscreen" => app.window().set_fullscreen(!app.window().is_fullscreen()),
        "min" => app.window().set_minimized(true),
        "max" => app.window().set_maximized(!app.window().is_maximized()),
        "about" => crate::sidebar::show_info(app, "InoCode Desktop", &format!("バージョン {}", env!("CARGO_PKG_VERSION"))),
        t if t.starts_with("tab:") => {
            let i: i32 = t[4..].parse().unwrap_or(0);
            app.global::<SettingsPageState>().set_open(false);
            app.global::<Tabs>().invoke_select(i);
        }
        _ => return false,
    }
    true
}
