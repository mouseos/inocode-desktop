//! 設定タブ: 機能フラグ(inocode/flags/read, write)。バージョン探索の設定は AppState の
//! discovery とそのコールバック(src/callbacks.rs)をそのまま使う。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, FlagRow, ParamRow, SettingsTabState};
use serde_json::{json, Value};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const TAG_READ: &str = "settings.flags.read";
const TAG_WRITE: &str = "settings.flags.write";
const TAG_PARAMS_READ: &str = "settings.params.read";
const TAG_PARAMS_WRITE: &str = "settings.params.write";

/// パラメータの表示名(inocode/params/read のキー)。無いキーはキーのまま「その他」に出す。
const PARAM_LABELS: [(&str, &str); 14] = [
    ("advisor.model", "相談役のモデル"),
    ("advisor.same_failure_k", "同じ失敗の回数"),
    ("advisor.repeat_n", "同じ操作の回数"),
    ("advisor.no_progress_turns", "進捗なしのターン数"),
    ("advisor.alternation_len", "往復の長さ"),
    ("compaction.reattach_files", "再添付のファイル数"),
    ("compaction.reattach_tokens", "再添付のトークン上限"),
    ("compaction.max_consecutive_failures", "自動圧縮を止める連続失敗の回数"),
    ("compaction.preview_bytes", "退避した出力のプレビュー(バイト)"),
    ("team.speedup_threshold", "複数にする速度向上の閾値"),
    ("team.split_overhead_secs", "分割の所要(秒)"),
    ("team.integrate_overhead_secs", "統合の所要(秒)"),
    ("team.task_secs", "1 件の既定所要(秒)"),
    ("team.fork_turns_default", "fork_turns の既定"),
];

/// 選択肢の表示名
fn option_label(o: &str) -> String {
    match o {
        "none" => "なし",
        "all" => "すべて",
        other => other,
    }
    .to_string()
}

#[derive(Debug, Clone, PartialEq)]
struct Param {
    key: String,
    /// "int" / "float" / "string" / "enum"
    kind: String,
    /// サーバーが持っている値(文字列)
    value: String,
    default: String,
    /// 入力欄の値
    edit: String,
    options: Vec<String>,
}

fn num_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if let Some(f) = n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 1e15) {
                format!("{}", f as i64)
            } else {
                n.to_string()
            }
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn parse_params(result: &Value) -> Vec<Param> {
    let p = result.get("response").unwrap_or(result);
    p.get("params")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let key = x.get("key").and_then(Value::as_str)?.to_string();
                    let value = x.get("value").map(num_text).unwrap_or_default();
                    Some(Param {
                        key,
                        kind: x.get("kind").and_then(Value::as_str).unwrap_or("string").to_string(),
                        default: x.get("default").map(num_text).unwrap_or_default(),
                        edit: value.clone(),
                        value,
                        options: x
                            .get("options")
                            .and_then(Value::as_array)
                            .map(|o| o.iter().filter_map(Value::as_str).map(str::to_string).collect())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 入力欄の文字列 -> 書き込む値。数値として読めなければ None。
fn write_value(kind: &str, text: &str) -> Option<Value> {
    let t = text.trim();
    match kind {
        "int" => t.parse::<i64>().ok().map(Value::from),
        "float" => t.parse::<f64>().ok().filter(|f| f.is_finite()).map(Value::from),
        _ => Some(Value::from(t)),
    }
}

fn param_row(p: &Param) -> ParamRow {
    let label = PARAM_LABELS.iter().find(|(k, _)| *k == p.key).map(|(_, l)| l.to_string()).unwrap_or_else(|| p.key.clone());
    ParamRow {
        key: p.key.as_str().into(),
        label: label.into(),
        kind: match p.kind.as_str() {
            "int" => 0,
            "float" => 1,
            "enum" => 3,
            _ => 2,
        },
        text: p.edit.as_str().into(),
        dirty: p.edit != p.value,
        at_default: p.value == p.default,
        options: ModelRc::from(Rc::new(VecModel::from(p.options.iter().map(|o| o.as_str().into()).collect::<Vec<slint::SharedString>>()))),
        option_labels: ModelRc::from(Rc::new(VecModel::from(p.options.iter().map(|o| option_label(o).into()).collect::<Vec<slint::SharedString>>()))),
        current: p.value.as_str().into(),
    }
}

fn group_of(key: &str) -> usize {
    match key.split('.').next().unwrap_or("") {
        "advisor" => 0,
        "compaction" => 1,
        "team" => 2,
        _ => 3,
    }
}

fn render_params(app: &AppWindow) {
    let mut groups: [Vec<ParamRow>; 4] = Default::default();
    PARAMS.with(|p| {
        let guard = p.borrow();
        let mut all: Vec<&Param> = guard.iter().collect();
        all.sort_by_key(|x| PARAM_LABELS.iter().position(|(k, _)| *k == x.key).unwrap_or(usize::MAX));
        for x in all {
            groups[group_of(&x.key)].push(param_row(x));
        }
    });
    let st = app.global::<SettingsTabState>();
    let [a, c, t, o] = groups;
    st.set_params_advisor(ModelRc::from(Rc::new(VecModel::from(a))));
    st.set_params_compaction(ModelRc::from(Rc::new(VecModel::from(c))));
    st.set_params_team(ModelRc::from(Rc::new(VecModel::from(t))));
    st.set_params_other(ModelRc::from(Rc::new(VecModel::from(o))));
}

/// 入力中は行の dirty だけ直す(入力欄を作り直さない)。
fn refresh_dirty(app: &AppWindow, key: &str) {
    let Some(param) = PARAMS.with(|p| p.borrow().iter().find(|x| x.key == key).cloned()) else { return };
    let st = app.global::<SettingsTabState>();
    for model in [st.get_params_advisor(), st.get_params_compaction(), st.get_params_team(), st.get_params_other()] {
        for i in 0..model.row_count() {
            if let Some(mut row) = model.row_data(i) {
                if row.key == key {
                    row.dirty = param.edit != param.value;
                    model.set_row_data(i, row);
                    return;
                }
            }
        }
    }
}

fn write_param(host: &Host, key: &str, value: Value) {
    host.rpc(TAG_PARAMS_WRITE, "inocode/params/write", json!({ "key": key, "value": value }));
}

fn install_params(app: &AppWindow, host: &Host) {
    let st = app.global::<SettingsTabState>();
    let weak = app.as_weak();
    st.on_param_edited(move |key, text| {
        let Some(app) = weak.upgrade() else { return };
        PARAMS.with(|p| {
            if let Some(x) = p.borrow_mut().iter_mut().find(|x| x.key == key.as_str()) {
                x.edit = text.to_string();
            }
        });
        refresh_dirty(&app, key.as_str());
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_param_save(move |key| {
        let Some(app) = weak.upgrade() else { return };
        let Some(p) = PARAMS.with(|p| p.borrow().iter().find(|x| x.key == key.as_str()).cloned()) else { return };
        match write_value(&p.kind, &p.edit) {
            Some(v) => {
                app.global::<SettingsTabState>().set_error("".into());
                write_param(&h, &p.key, v);
            }
            None => app.global::<SettingsTabState>().set_error(format!("{} には数値を入力してください", p.key).into()),
        }
    });
    let h = host.clone();
    st.on_param_reset(move |key| write_param(&h, key.as_str(), Value::Null));
    let h = host.clone();
    st.on_param_choose(move |key, value| write_param(&h, key.as_str(), Value::from(value.as_str())));
}

thread_local! {
    static PARAMS: RefCell<Vec<Param>> = RefCell::new(Vec::new());
}

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
    install_params(app, host);
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
    host.rpc(TAG_PARAMS_READ, "inocode/params/read", json!({}));
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    let Event::Rpc { tag, result } = event else { return };
    let state = app.global::<SettingsTabState>();
    match (tag.as_str(), result) {
        (TAG_PARAMS_READ, Ok(v)) => {
            PARAMS.with(|p| *p.borrow_mut() = parse_params(v));
            render_params(app);
        }
        (TAG_PARAMS_READ, Err(e)) => state.set_error(e.as_str().into()),
        (TAG_PARAMS_WRITE, Ok(_)) => {
            state.set_error("".into());
            host.rpc(TAG_PARAMS_READ, "inocode/params/read", json!({}));
        }
        (TAG_PARAMS_WRITE, Err(e)) => {
            state.set_error(e.as_str().into());
            host.rpc(TAG_PARAMS_READ, "inocode/params/read", json!({}));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ref/contract-fixtures").join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn params_fixture_parses_with_kinds_and_defaults() {
        let list = parse_params(&fixture("params-read.json"));
        assert_eq!(list.len(), 14);
        let k = list.iter().find(|p| p.key == "advisor.same_failure_k").unwrap();
        assert_eq!((k.kind.as_str(), k.value.as_str(), k.default.as_str()), ("int", "3", "3"));
        let t = list.iter().find(|p| p.key == "team.speedup_threshold").unwrap();
        assert_eq!(t.value, "1.3");
        let s = list.iter().find(|p| p.key == "team.split_overhead_secs").unwrap();
        assert_eq!(s.value, "60", "60.0 is shown without a fraction");
        let f = list.iter().find(|p| p.key == "team.fork_turns_default").unwrap();
        assert_eq!(f.options, vec!["none", "all"]);
        assert_eq!(param_row(f).kind, 3);
    }

    #[test]
    fn write_values_follow_the_kind() {
        assert_eq!(write_value("int", " 4 "), Some(json!(4)));
        assert_eq!(write_value("int", "4.5"), None);
        assert_eq!(write_value("float", "1.5"), Some(json!(1.5)));
        assert_eq!(write_value("float", "x"), None);
        assert_eq!(write_value("string", " gpt-5.6-luna "), Some(json!("gpt-5.6-luna")));
    }

    #[test]
    fn unknown_keys_go_to_other_group() {
        assert_eq!(group_of("advisor.model"), 0);
        assert_eq!(group_of("compaction.x"), 1);
        assert_eq!(group_of("team.y"), 2);
        assert_eq!(group_of("zzz"), 3);
    }
}
