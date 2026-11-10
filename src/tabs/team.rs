//! チームタブ。このファイルと ui/tabs/team.slint だけを編集して実装する。
//! 呼び出し元は src/tabs/mod.rs (登録は 1 か所)。

use super::Host;
use crate::client::Event;
use crate::AppWindow;

/// 起動時に 1 回。コールバックの登録 (app.global::<TeamState>().on_xxx(...)) をここで行う。
pub fn install(_app: &AppWindow, _host: &Host) {}

/// このタブが開かれるたびに呼ばれる。一覧の再取得などをここで始める。
pub fn opened(_app: &AppWindow, _host: &Host) {}

/// クライアントのイベント (Event::Rpc / Event::Notify) が届くたびに呼ばれる。
/// 自分の tag (例: "team.list") か method だけを見て、それ以外は無視する。
pub fn event(_app: &AppWindow, _host: &Host, _event: &Event) {}
