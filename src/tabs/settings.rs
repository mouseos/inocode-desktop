//! 設定タブ: 機能フラグ(inocode/flags/read, write)。バージョン探索の設定は AppState の
//! discovery とそのコールバック(src/callbacks.rs)をそのまま使う。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, FlagRow, SettingsTabState};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const TAG_READ: &str = "settings.flags.read";
const TAG_WRITE: &str = "settings.flags.write";

/// 契約書 2 章のキー(表示順)と日本語ラベル。
const FLAGS: [(&str, &str); 11] = [
    ("metrics.enabled", "測定の記録"),
    ("wait.event_driven", "完了のイベント通知"),
    ("knowledge.enabled", "知識データベース"),
    ("knowledge.pinned_rules", "常時表示の禁止リスト"),
    ("knowledge.guard", "操作のガード"),
    ("protocol.enabled", "内部通信の電報文"),
    ("peer_messaging.enabled", "兄弟同士の直接メッセージ"),
    ("advisor.enabled", "停滞の検出と相談役"),
    ("compaction.persist_large_output", "大きな出力のディスク退避"),
    ("team.auto_select", "1 人か複数かの自動選択"),
    ("evidence.enabled", "根拠データベース"),
];

thread_local! {
    /// Current value per flag (config.toml keys that are absent count as on).
    static VALUES: RefCell<Vec<bool>> = RefCell::new(vec![true; FLAGS.len()]);
}

pub fn install(app: &AppWindow, host: &Host) {
    render(app);
    let write_host = host.clone();
    let weak = app.as_weak();
    app.global::<SettingsTabState>().on_set_flag(move |key, on| {
        let Some(app) = weak.upgrade() else { return };
        if let Some(i) = FLAGS.iter().position(|(k, _)| *k == key.as_str()) {
            VALUES.with(|v| v.borrow_mut()[i] = on);
            render(&app);
        }
        write_host.rpc(TAG_WRITE, "inocode/flags/write", json!({ "key": key.as_str(), "value": on }));
    });
    let host = host.clone();
    app.global::<SettingsTabState>().on_refresh(move || read(&host));
}

pub fn opened(_app: &AppWindow, host: &Host) {
    read(host);
}

fn read(host: &Host) {
    host.rpc(TAG_READ, "inocode/flags/read", json!({}));
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    let Event::Rpc { tag, result } = event else { return };
    let state = app.global::<SettingsTabState>();
    match (tag.as_str(), result) {
        (TAG_READ, Ok(v)) => {
            let flags = v.get("flags").unwrap_or(&Value::Null);
            VALUES.with(|vals| {
                let mut vals = vals.borrow_mut();
                for (i, (key, _)) in FLAGS.iter().enumerate() {
                    vals[i] = flags.get(*key).and_then(Value::as_bool).unwrap_or(true);
                }
            });
            state.set_error("".into());
            render(app);
        }
        (TAG_WRITE, Ok(_)) => state.set_error("".into()),
        (TAG_READ, Err(e)) => state.set_error(e.as_str().into()),
        (TAG_WRITE, Err(e)) => {
            // The write was refused: show what the server really has.
            state.set_error(e.as_str().into());
            read(host);
        }
        _ => {}
    }
}

fn render(app: &AppWindow) {
    let rows: Vec<FlagRow> = VALUES.with(|vals| {
        let vals = vals.borrow();
        FLAGS
            .iter()
            .enumerate()
            .map(|(i, (key, label))| FlagRow { key: (*key).into(), label: (*label).into(), on: vals[i] })
            .collect()
    });
    app.global::<SettingsTabState>().set_flags(ModelRc::from(Rc::new(VecModel::from(rows))));
}
