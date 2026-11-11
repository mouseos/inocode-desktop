//! 画面タブ (知識 / 根拠 / チーム / 設定) の登録。
//!
//! 各タブは自分のファイル (src/tabs/<name>.rs と ui/tabs/<name>.slint) だけを編集して実装する。
//! 新しいタブを足すときは、ここ (mod 宣言と TABS) と ui/tabs.slint の 2 か所を直す。

use crate::client::{Command, Event, Handle};
use crate::AppWindow;
use serde_json::Value;
use slint::ComponentHandle;
use std::rc::Rc;

mod accounts;
mod evidence;
mod knowledge;
mod settings;
mod stats;
mod team;
mod team_log;

/// タブから app-server へ要求を出すための窓口。
#[derive(Clone)]
#[allow(dead_code)] // tabs use it as they are implemented
pub struct Host {
    handle: Rc<Handle>,
}

impl Host {
    pub fn new(handle: Rc<Handle>) -> Host {
        Host { handle }
    }

    /// JSON-RPC を 1 件送る。応答は Event::Rpc { tag, result } として event() に届く。
    /// tag は呼び出し側が決める識別子 ("knowledge.list" など)。
    /// INOCODE_GUI_MOCK=1 のとき、`inocode/*` は ref/contract-fixtures の JSON が返る。
    #[allow(dead_code)]
    pub fn rpc(&self, tag: &str, method: &str, params: Value) {
        self.handle.send(Command::Rpc { tag: tag.to_string(), method: method.to_string(), params });
    }
}

struct Tab {
    install: fn(&AppWindow, &Host),
    opened: fn(&AppWindow, &Host),
    event: fn(&AppWindow, &Host, &Event),
}

/// ui/tabs.slint の Tabs.titles と同じ順 (チャットを除く: 添字 i が Tabs.current == i + 1)。
const TABS: [Tab; 4] = [
    Tab { install: knowledge::install, opened: knowledge::opened, event: knowledge::event },
    Tab { install: evidence::install, opened: evidence::opened, event: evidence::event },
    Tab { install: team::install, opened: team::opened, event: team::event },
    Tab { install: settings::install, opened: settings::opened, event: settings::event },
];

pub fn install(app: &AppWindow, host: &Host) {
    for tab in &TABS {
        (tab.install)(app, host);
    }
    stats::install(app, host);
    let weak = app.as_weak();
    let host = host.clone();
    app.global::<crate::Tabs>().on_opened(move |i| {
        if i < 1 {
            return;
        }
        if let (Some(app), Some(tab)) = (weak.upgrade(), TABS.get((i - 1) as usize)) {
            (tab.opened)(&app, &host);
        }
    });
}

/// クライアントのイベントを全タブへ配る。
pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    for tab in &TABS {
        (tab.event)(app, host, event);
    }
    stats::event(app, host, event);
}
