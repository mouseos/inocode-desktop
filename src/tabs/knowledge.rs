//! 知識タブ。このファイルと ui/tabs/knowledge.slint だけを編集して実装する。
//! 呼び出し元は src/tabs/mod.rs (登録は 1 か所)。
//!
//! app-server の `inocode/knowledge/{list,search,read,write,setStatus}` を使う。
//! 追加: ガードの発動(inocode/guard/list、通知 guard/fired)、常に有効な禁止リスト(inocode/knowledge/pinned)、
//! 引き継ぎパックのプレビュー(inocode/knowledge/handoffPreview)、失敗例の自動記録の通知(inocode/knowledge/recorded)。
//! 応答は contract (ref/docs/09-overnight-contract.md 3, 4 章) の形。モックの応答は
//! `{method, request, response}` の形なので、`response` があればそれを使う。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, KField, KRecord, KRow, KnowledgeState, LogRow};
use serde_json::{json, Map, Value};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

/// (wire 名, 画面の名前)
const KINDS: [(&str, &str); 7] = [
    ("rule", "禁止事項"),
    ("failure_case", "失敗例"),
    ("issue", "課題"),
    ("decision", "決定"),
    ("fact", "事実"),
    ("contract", "契約"),
    ("procedure", "手順"),
];
const STATUSES: [(&str, &str); 4] =
    [("unverified", "未検証"), ("verified", "検証済み"), ("superseded", "置換済み"), ("retracted", "撤回")];
const CONFIDENCES: [(&str, &str); 3] = [("high", "高"), ("medium", "中"), ("low", "低")];

/// 種類の選択の末尾(kinds の並び)にある「常に有効な禁止リスト」の添字
const PINNED_INDEX: i32 = 8;

fn label_of(table: &[(&str, &str)], wire: &str) -> String {
    table.iter().find(|(w, _)| *w == wire).map(|(_, l)| l.to_string()).unwrap_or_else(|| wire.to_string())
}

// ------------------------------------------------------------------ pure helpers (tested)

/// モックは `{method, request, response}`、実サーバーは result そのもの。
fn payload(result: &Value) -> &Value {
    result.get("response").unwrap_or(result)
}

/// "2026-11-10T09:12:00Z" -> "2026-11-10 09:12"
fn short_time(s: &str) -> String {
    let t: String = s.replace('T', " ").replace('Z', "");
    t.chars().take(16).collect()
}

#[derive(Debug, Clone, PartialEq)]
struct RowData {
    id: String,
    kind_label: String,
    title: String,
    summary: String,
    status: String,
    status_label: String,
    confidence_label: String,
    updated: String,
}

fn parse_rows(result: &Value) -> Vec<RowData> {
    let list = payload(result).get("records").and_then(Value::as_array).cloned().unwrap_or_default();
    list.iter()
        .filter_map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let id = s("id");
            if id.is_empty() {
                return None;
            }
            Some(RowData {
                id,
                kind_label: label_of(&KINDS, &s("kind")),
                title: s("title"),
                summary: s("summary"),
                status: s("status"),
                status_label: label_of(&STATUSES, &s("status")),
                confidence_label: label_of(&CONFIDENCES, &s("confidence")),
                updated: short_time(&s("updatedAt")),
            })
        })
        .collect()
}

/// 常に有効な禁止リスト -> (行, id -> 範囲, 再注入される文面)
fn parse_pinned(result: &Value) -> (Vec<RowData>, Vec<(String, String)>, String) {
    let p = payload(result);
    let list = p.get("rules").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut rows = Vec::new();
    let mut scopes = Vec::new();
    for r in &list {
        let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let id = s("id");
        if id.is_empty() {
            continue;
        }
        scopes.push((id.clone(), s("scope")));
        rows.push(RowData {
            id,
            kind_label: label_of(&KINDS, "rule"),
            title: s("title"),
            summary: s("reason"),
            status: s("status"),
            status_label: label_of(&STATUSES, &s("status")),
            confidence_label: "".into(),
            updated: if s("scope") == "global" { "共通" } else { "プロジェクト" }.into(),
        });
    }
    (rows, scopes, p.get("text").and_then(Value::as_str).unwrap_or("").to_string())
}

fn guard_key(e: &Value) -> String {
    let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    format!("{}{}{}", e.get("tsMs").and_then(Value::as_i64).unwrap_or(0), s("toolUseId"), s("ruleId"))
}

/// ガードの発動 1 件 -> (見出し, 補足, 詳細, tone)。遮断は 2、警告は 1。
fn guard_row(e: &Value, thread: &str) -> (String, String, String, i32) {
    let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let block = s("severity") == "block";
    let head = format!("{} {}", if block { "遮断" } else { "警告" }, s("ruleTitle"));
    let meta = format!(
        "{} · {} · {} {}",
        super::team_log::time_label(e.get("tsMs").and_then(Value::as_i64).unwrap_or(0)),
        thread,
        s("tool"),
        s("target")
    );
    let detail = format!(
        "規則: [{}] {}\n理由: {}\n対象: {}\nツール: {}\n範囲: {}",
        s("ruleId"),
        s("ruleTitle"),
        s("reason"),
        s("target"),
        s("tool"),
        if s("scope") == "global" { "共通" } else { "プロジェクト" }
    );
    (head, meta, detail, if block { 2 } else { 1 })
}

/// 項目の種類: 1 行 / 複数行 / 1 行 1 要素の配列。
#[derive(Debug, Clone, Copy, PartialEq)]
enum FieldKind {
    Line,
    Area,
    Lines,
}

/// 種類ごとの編集項目: (キー(`a.b` で入れ子), 画面の名前, 入力の種類)。
fn field_specs(kind: &str) -> Vec<(&'static str, &'static str, FieldKind)> {
    let mut v = vec![("source", "出典", FieldKind::Line), ("supersedes", "置き換えた ID", FieldKind::Line)];
    match kind {
        "rule" => v.extend([
            ("severity", "重大度 (block / warn)", FieldKind::Line),
            ("match.paths", "対象のパス (1 行に 1 つ)", FieldKind::Lines),
            ("match.commands", "対象のコマンド (1 行に 1 つ)", FieldKind::Lines),
            ("reason", "理由", FieldKind::Area),
            ("enforcedBy", "適用方法 (prompt / hook / execpolicy)", FieldKind::Line),
            ("fromFailure", "元の失敗例の ID", FieldKind::Line),
        ]),
        "failure_case" => v.extend([
            ("signature", "署名", FieldKind::Line),
            ("symptom", "症状", FieldKind::Area),
            ("rootCause", "根本原因", FieldKind::Area),
            ("doNot", "やってはいけないこと", FieldKind::Area),
            ("whatWorked", "うまくいった方法", FieldKind::Area),
            ("evidence", "根拠 (1 行に 1 つ)", FieldKind::Lines),
        ]),
        _ => {}
    }
    v
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, k| cur.get(k))
}

fn set_path(v: &mut Value, path: &str, value: Option<Value>) {
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = v;
    for (i, k) in parts.iter().enumerate() {
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        let obj = cur.as_object_mut().unwrap();
        if i + 1 == parts.len() {
            match &value {
                Some(x) => {
                    obj.insert(k.to_string(), x.clone());
                }
                None => {
                    obj.remove(*k);
                }
            }
            return;
        }
        cur = obj.entry(k.to_string()).or_insert_with(|| Value::Object(Map::new()));
    }
}

fn to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(to_text).collect::<Vec<_>>().join("\n"),
        Value::Object(o) => o
            .iter()
            .filter(|(_, x)| !to_text(x).is_empty())
            .map(|(k, x)| format!("{k}: {}", to_text(x).replace('\n', ", ")))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 表示用の front-matter 項目の名前。
fn field_label(key: &str) -> String {
    match key {
        "source" => "出典",
        "createdAt" => "作成日時",
        "createdBy" => "作成者",
        "supersedes" => "置き換えた ID",
        "severity" => "重大度",
        "match" => "対象",
        "reason" => "理由",
        "enforcedBy" => "適用方法",
        "fromFailure" => "元の失敗例",
        "signature" => "署名",
        "symptom" => "症状",
        "rootCause" => "根本原因",
        "doNot" => "やってはいけないこと",
        "whatWorked" => "うまくいった方法",
        "evidence" => "根拠",
        other => other,
    }
    .to_string()
}

/// 読み取り表示に出す項目 (共通の見出し項目と本文を除く)。
fn display_fields(record: &Value) -> Vec<(String, String)> {
    const SKIP: [&str; 8] = ["id", "kind", "title", "scope", "status", "confidence", "updatedAt", "body"];
    let Some(obj) = record.as_object() else { return Vec::new() };
    obj.iter()
        .filter(|(k, v)| !SKIP.contains(&k.as_str()) && !to_text(v).is_empty())
        .map(|(k, v)| {
            let text = to_text(v);
            (field_label(k), if k == "createdAt" { short_time(&text) } else { text })
        })
        .collect()
}

/// 編集フォームの項目 (既存のレコードの値が入る)。
fn edit_fields(kind: &str, record: &Value) -> Vec<(String, String, FieldKind, String)> {
    field_specs(kind)
        .into_iter()
        .map(|(key, label, fk)| {
            let value = get_path(record, key).map(to_text).unwrap_or_default();
            (key.to_string(), label.to_string(), fk, value)
        })
        .collect()
}

/// フォームの値から書き込むレコードを作る。既存のレコードの未知の項目は残す。
fn build_record(
    base: Option<&Value>,
    kind: &str,
    scope: &str,
    title: &str,
    status: &str,
    confidence: &str,
    body: &str,
    fields: &[(String, String, FieldKind, String)],
) -> Value {
    let mut rec = base.cloned().unwrap_or_else(|| json!({ "createdBy": "user" }));
    if !rec.is_object() {
        rec = json!({});
    }
    rec["kind"] = json!(kind);
    rec["scope"] = json!(scope);
    rec["title"] = json!(title);
    rec["status"] = json!(status);
    rec["confidence"] = json!(confidence);
    rec["body"] = json!(body);
    for (key, _, fk, value) in fields {
        let v = match fk {
            FieldKind::Lines => {
                let items: Vec<Value> =
                    value.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| json!(l)).collect();
                if items.is_empty() { None } else { Some(Value::Array(items)) }
            }
            _ => {
                let t = value.trim();
                if t.is_empty() { None } else { Some(json!(t)) }
            }
        };
        set_path(&mut rec, key, v);
    }
    rec
}

// ------------------------------------------------------------------ state

#[derive(Default)]
struct K {
    rows: Vec<RowData>,
    selected: Option<String>,
    raw: Option<Value>,
    creating: bool,
    kind: String,
    fields: Vec<(String, String, FieldKind, String)>,
    pinned_scopes: Vec<(String, String)>,
    guard: Vec<Value>,
    guard_open: HashSet<String>,
}

thread_local! {
    static K: RefCell<K> = RefCell::new(K::default());
}

fn cwd(app: &AppWindow) -> String {
    let c = app.global::<crate::AppState>().get_cwd_text().to_string();
    if !c.is_empty() {
        return c;
    }
    std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

fn scope_wire(app: &AppWindow) -> &'static str {
    if app.global::<KnowledgeState>().get_scope() == 1 { "global" } else { "project" }
}

fn rows_model(rows: &[RowData], selected: &Option<String>) -> ModelRc<KRow> {
    let v: Vec<KRow> = rows
        .iter()
        .map(|r| KRow {
            id: r.id.as_str().into(),
            kind_label: r.kind_label.as_str().into(),
            title: r.title.as_str().into(),
            summary: r.summary.as_str().into(),
            status: r.status.as_str().into(),
            status_label: r.status_label.as_str().into(),
            confidence_label: r.confidence_label.as_str().into(),
            updated: r.updated.as_str().into(),
            selected: selected.as_deref() == Some(r.id.as_str()),
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn field_model(items: &[(String, String, FieldKind, String)]) -> ModelRc<KField> {
    let v: Vec<KField> = items
        .iter()
        .map(|(key, label, fk, value)| KField {
            key: key.as_str().into(),
            label: label.as_str().into(),
            value: value.as_str().into(),
            multiline: *fk != FieldKind::Line,
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn request_list(app: &AppWindow, host: &Host) {
    let st = app.global::<KnowledgeState>();
    let query = st.get_query().to_string();
    let kind = st.get_kind_index();
    if kind == PINNED_INDEX {
        st.set_loading(true);
        host.rpc("knowledge.pinned", "inocode/knowledge/pinned", json!({ "cwd": cwd(app) }));
        return;
    }
    let mut params = json!({ "scope": scope_wire(app), "cwd": cwd(app), "limit": 100 });
    if kind >= 1 {
        params["kind"] = json!(KINDS[(kind - 1) as usize].0);
    }
    st.set_loading(true);
    if query.trim().is_empty() {
        host.rpc("knowledge.list", "inocode/knowledge/list", params);
    } else {
        params["query"] = json!(query.trim());
        host.rpc("knowledge.list", "inocode/knowledge/search", params);
    }
}

fn request_read(app: &AppWindow, host: &Host, id: &str) {
    host.rpc("knowledge.read", "inocode/knowledge/read", json!({ "id": id, "scope": scope_wire(app), "cwd": cwd(app) }));
}

fn show_record(app: &AppWindow, record: &Value) {
    let st = app.global::<KnowledgeState>();
    let s = |k: &str| record.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    st.set_record(KRecord {
        id: s("id").into(),
        title: s("title").into(),
        kind_label: label_of(&KINDS, &s("kind")).into(),
        scope_label: if s("scope") == "global" { "共通" } else { "プロジェクト" }.into(),
        status: s("status").into(),
        status_label: label_of(&STATUSES, &s("status")).into(),
        confidence_label: label_of(&CONFIDENCES, &s("confidence")).into(),
        updated: short_time(&s("updatedAt")).into(),
        body: s("body").into(),
    });
    let items: Vec<KField> = display_fields(record)
        .into_iter()
        .map(|(label, value)| KField { key: "".into(), label: label.into(), value: value.into(), multiline: false })
        .collect();
    st.set_fields(ModelRc::from(Rc::new(VecModel::from(items))));
    st.set_has_record(true);
}

fn refresh_rows(app: &AppWindow) {
    K.with(|k| {
        let k = k.borrow();
        app.global::<KnowledgeState>().set_rows(rows_model(&k.rows, &k.selected));
    });
}

fn render_guard(app: &AppWindow) {
    let rows: Vec<LogRow> = K.with(|k| {
        let k = k.borrow();
        k.guard
            .iter()
            .map(|e| {
                let thread = super::team::thread_name(e.get("threadId").and_then(Value::as_str).unwrap_or(""));
                let (head, meta, detail, tone) = guard_row(e, &thread);
                LogRow {
                    head: head.into(),
                    meta: meta.into(),
                    detail: detail.into(),
                    tone,
                    open: k.guard_open.contains(&guard_key(e)),
                    action_label: "規則を開く".into(),
                    action2_label: "誤検知として記録".into(),
                }
            })
            .collect()
    });
    app.global::<KnowledgeState>().set_guard(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn request_guard(app: &AppWindow, host: &Host) {
    host.rpc("knowledge.guard", "inocode/guard/list", json!({ "cwd": cwd(app), "limit": 100 }));
}

/// 発動の記録から規則を開く: 一覧の画面に戻して、その規則を選ぶ。
fn open_rule(app: &AppWindow, host: &Host, id: &str, scope: &str) {
    let st = app.global::<KnowledgeState>();
    st.set_mode(0);
    st.set_scope(if scope == "global" { 1 } else { 0 });
    st.set_kind_index(0);
    st.set_editing(false);
    K.with(|k| k.borrow_mut().selected = Some(id.to_string()));
    request_list(app, host);
    request_read(app, host, id);
}

// ------------------------------------------------------------------ tab entry points

/// 起動時に 1 回。コールバックの登録をここで行う。
pub fn install(app: &AppWindow, host: &Host) {
    install_extras(app, host);
    let st = app.global::<KnowledgeState>();

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            request_list(&app, &h);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_search(move |_| {
        if let Some(app) = weak.upgrade() {
            request_list(&app, &h);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_scope_changed(move |_| {
        if let Some(app) = weak.upgrade() {
            K.with(|k| *k.borrow_mut() = K::default());
            let st = app.global::<KnowledgeState>();
            st.set_has_record(false);
            st.set_editing(false);
            request_list(&app, &h);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_kind_changed(move |_| {
        if let Some(app) = weak.upgrade() {
            request_list(&app, &h);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_select(move |id| {
        let Some(app) = weak.upgrade() else { return };
        // 常に有効な禁止リストの行は、規則の範囲で読む
        let pinned_scope = K.with(|k| k.borrow().pinned_scopes.iter().find(|(i, _)| i == id.as_str()).map(|(_, s)| s.clone()));
        if let (Some(scope), PINNED_INDEX) = (pinned_scope, app.global::<KnowledgeState>().get_kind_index()) {
            app.global::<KnowledgeState>().set_scope(if scope == "global" { 1 } else { 0 });
        }
        K.with(|k| k.borrow_mut().selected = Some(id.to_string()));
        refresh_rows(&app);
        app.global::<KnowledgeState>().set_editing(false);
        request_read(&app, &h, &id);
    });

    // 新規作成: 空のフォーム
    let weak = app.as_weak();
    st.on_new_record(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<KnowledgeState>();
        K.with(|k| {
            let mut k = k.borrow_mut();
            k.creating = true;
            k.raw = None;
            k.kind = KINDS[0].0.to_string();
            k.fields = edit_fields(&k.kind, &json!({}));
            st.set_edit_fields(field_model(&k.fields));
        });
        st.set_creating(true);
        st.set_edit_kind(0);
        st.set_edit_title("".into());
        st.set_edit_body("".into());
        st.set_edit_status(0);
        st.set_edit_confidence(1);
        st.set_edit_error("".into());
        st.set_editing(true);
    });
    // 新規作成で種類を切り替えたら項目を作り直す (入力済みの値は同じキーなら引き継ぐ)
    let weak = app.as_weak();
    st.on_edit_kind_changed(move |i| {
        let Some(app) = weak.upgrade() else { return };
        K.with(|k| {
            let mut k = k.borrow_mut();
            let old = std::mem::take(&mut k.fields);
            k.kind = KINDS[(i.max(0) as usize).min(KINDS.len() - 1)].0.to_string();
            let mut fresh = edit_fields(&k.kind, &json!({}));
            for f in &mut fresh {
                if let Some(o) = old.iter().find(|o| o.0 == f.0) {
                    f.3 = o.3.clone();
                }
            }
            k.fields = fresh;
            app.global::<KnowledgeState>().set_edit_fields(field_model(&k.fields));
        });
    });
    // 編集: 読み込み済みのレコードから
    let weak = app.as_weak();
    st.on_start_edit(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<KnowledgeState>();
        let ok = K.with(|k| {
            let mut k = k.borrow_mut();
            let Some(raw) = k.raw.clone() else { return false };
            let s = |key: &str| raw.get(key).and_then(Value::as_str).unwrap_or("").to_string();
            k.creating = false;
            k.kind = s("kind");
            k.fields = edit_fields(&k.kind, &raw);
            st.set_edit_fields(field_model(&k.fields));
            st.set_edit_title(s("title").into());
            st.set_edit_body(s("body").into());
            st.set_edit_status(STATUSES.iter().position(|(w, _)| *w == s("status")).unwrap_or(0) as i32);
            st.set_edit_confidence(CONFIDENCES.iter().position(|(w, _)| *w == s("confidence")).unwrap_or(1) as i32);
            true
        });
        if ok {
            st.set_creating(false);
            st.set_edit_error("".into());
            st.set_editing(true);
        }
    });
    let weak = app.as_weak();
    st.on_cancel_edit(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<KnowledgeState>().set_editing(false);
        }
    });
    st.on_field_edited(move |i, text| {
        K.with(|k| {
            if let Some(f) = k.borrow_mut().fields.get_mut(i as usize) {
                f.3 = text.to_string();
            }
        });
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_save(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<KnowledgeState>();
        let title = st.get_edit_title().to_string();
        if title.trim().is_empty() {
            st.set_edit_error("タイトルを入力してください".into());
            return;
        }
        let record = K.with(|k| {
            let k = k.borrow();
            build_record(
                if k.creating { None } else { k.raw.as_ref() },
                &k.kind,
                scope_wire(&app),
                title.trim(),
                STATUSES[(st.get_edit_status().max(0) as usize).min(3)].0,
                CONFIDENCES[(st.get_edit_confidence().max(0) as usize).min(2)].0,
                &st.get_edit_body(),
                &k.fields,
            )
        });
        st.set_edit_error("".into());
        h.rpc("knowledge.write", "inocode/knowledge/write", json!({ "scope": scope_wire(&app), "cwd": cwd(&app), "record": record }));
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_set_status(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some(id) = K.with(|k| k.borrow().selected.clone()) else { return };
        let status = STATUSES[(i.max(0) as usize).min(3)].0;
        h.rpc(
            "knowledge.setStatus",
            "inocode/knowledge/setStatus",
            json!({ "id": id, "scope": scope_wire(&app), "cwd": cwd(&app), "status": status }),
        );
    });
}

/// このタブが開かれるたびに呼ばれる。一覧の再取得を始める。
pub fn opened(app: &AppWindow, host: &Host) {
    match app.global::<KnowledgeState>().get_mode() {
        1 => request_guard(app, host),
        2 => {}
        _ => request_list(app, host),
    }
}

fn install_extras(app: &AppWindow, host: &Host) {
    let st = app.global::<KnowledgeState>();
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_mode_changed(move |m| {
        let Some(app) = weak.upgrade() else { return };
        app.global::<KnowledgeState>().set_note("".into());
        match m {
            1 => request_guard(&app, &h),
            2 => {}
            _ => request_list(&app, &h),
        }
    });
    let weak = app.as_weak();
    st.on_guard_toggled(move |i| {
        let Some(app) = weak.upgrade() else { return };
        K.with(|k| {
            let mut k = k.borrow_mut();
            if let Some(key) = k.guard.get(i as usize).map(guard_key) {
                if !k.guard_open.remove(&key) {
                    k.guard_open.insert(key);
                }
            }
        });
        render_guard(&app);
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_guard_open_rule(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let target = K.with(|k| {
            k.borrow().guard.get(i as usize).map(|e| {
                let s = |key: &str| e.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                (s("ruleId"), s("scope"))
            })
        });
        if let Some((id, scope)) = target {
            open_rule(&app, &h, &id, &scope);
        }
    });
    let h = host.clone();
    st.on_guard_false_positive(move |i| {
        let thread = K.with(|k| {
            k.borrow().guard.get(i as usize).and_then(|e| e.get("threadId").and_then(Value::as_str).map(str::to_string))
        });
        if let Some(thread) = thread {
            h.rpc("knowledge.falsePositive", "inocode/metrics/markFalsePositive", json!({ "threadId": thread, "source": "guard" }));
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_handoff_preview(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<KnowledgeState>();
        let task = st.get_handoff_task().to_string();
        st.set_handoff_text("".into());
        h.rpc("knowledge.handoff", "inocode/knowledge/handoffPreview", json!({ "cwd": cwd(&app), "task": task }));
    });
}

/// クライアントのイベント (Event::Rpc / Event::Notify) が届くたびに呼ばれる。
pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    if let Event::Notify { method, params } = event {
        match method.as_str() {
            "inocode/guard/fired" => {
                K.with(|k| k.borrow_mut().guard.insert(0, params.clone()));
                render_guard(app);
            }
            // 失敗例が自動で記録された: 一覧を読み直す
            "inocode/knowledge/recorded" => {
                if app.global::<KnowledgeState>().get_mode() == 0 {
                    request_list(app, host);
                }
            }
            _ => {}
        }
        return;
    }
    let Event::Rpc { tag, result } = event else { return };
    let st = app.global::<KnowledgeState>();
    match (tag.as_str(), result) {
        ("knowledge.pinned", Ok(v)) => {
            let (rows, scopes, text) = parse_pinned(v);
            st.set_loading(false);
            st.set_note(if rows.is_empty() { "常に有効な禁止リストはありません".into() } else { format!("{} 件", rows.len()).into() });
            st.set_aside(text.into());
            K.with(|k| {
                let mut k = k.borrow_mut();
                k.rows = rows;
                k.pinned_scopes = scopes;
            });
            refresh_rows(app);
        }
        ("knowledge.pinned", Err(e)) => {
            st.set_loading(false);
            st.set_note(e.as_str().into());
        }
        ("knowledge.guard", Ok(v)) => {
            let mut list = payload(v).get("events").and_then(Value::as_array).cloned().unwrap_or_default();
            list.sort_by_key(|e| std::cmp::Reverse(e.get("tsMs").and_then(Value::as_i64).unwrap_or(0)));
            st.set_note(if list.is_empty() { "ガードの発動はまだありません".into() } else { SharedString::default() });
            K.with(|k| k.borrow_mut().guard = list);
            render_guard(app);
        }
        ("knowledge.guard", Err(e)) => st.set_note(e.as_str().into()),
        ("knowledge.handoff", Ok(v)) => {
            st.set_handoff_text(payload(v).get("text").and_then(Value::as_str).unwrap_or("").into());
        }
        ("knowledge.handoff", Err(e)) => st.set_handoff_text(e.as_str().into()),
        ("knowledge.falsePositive", Err(e)) => st.set_note(e.as_str().into()),
        ("knowledge.list", Ok(v)) => {
            st.set_aside("".into());
            let rows = parse_rows(v);
            st.set_loading(false);
            st.set_note(if rows.is_empty() { "該当するレコードはありません".into() } else { SharedString::default() });
            K.with(|k| k.borrow_mut().rows = rows);
            refresh_rows(app);
        }
        ("knowledge.list", Err(e)) => {
            st.set_loading(false);
            st.set_note(e.as_str().into());
            K.with(|k| k.borrow_mut().rows.clear());
            refresh_rows(app);
        }
        ("knowledge.read", Ok(v)) => {
            let record = payload(v).get("record").cloned().unwrap_or(Value::Null);
            if record.is_object() {
                show_record(app, &record);
                K.with(|k| k.borrow_mut().raw = Some(record));
            }
        }
        ("knowledge.read", Err(e)) => st.set_note(e.as_str().into()),
        ("knowledge.write", Ok(v)) => {
            st.set_editing(false);
            let id = payload(v).get("id").and_then(Value::as_str).map(str::to_string);
            if let Some(id) = id {
                K.with(|k| k.borrow_mut().selected = Some(id.clone()));
                request_read(app, host, &id);
            }
            request_list(app, host);
        }
        ("knowledge.write", Err(e)) => st.set_edit_error(e.as_str().into()),
        ("knowledge.setStatus", Ok(_)) => {
            if let Some(id) = K.with(|k| k.borrow().selected.clone()) {
                request_read(app, host, &id);
            }
            request_list(app, host);
        }
        ("knowledge.setStatus", Err(e)) => st.set_note(e.as_str().into()),
        _ => {}
    }
    let _ = st.get_rows().row_count();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("contract")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn list_fixture_becomes_rows() {
        let rows = parse_rows(&fixture("knowledge-list.json"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "rule-no-force-push");
        assert_eq!(rows[0].kind_label, "禁止事項");
        assert_eq!(rows[0].status_label, "検証済み");
        assert_eq!(rows[0].confidence_label, "高");
        assert_eq!(rows[0].updated, "2026-11-10 09:12");
        // search has the same shape; a bare server result works too
        assert_eq!(parse_rows(&fixture("knowledge-search.json")).len(), 1);
        assert_eq!(parse_rows(&json!({ "records": [{ "id": "x", "kind": "fact" }] }))[0].kind_label, "事実");
    }

    #[test]
    fn record_fields_display_and_edit_round_trip() {
        let rec = payload(&fixture("knowledge-read.json")).get("record").cloned().unwrap();
        let shown = display_fields(&rec);
        assert!(shown.iter().any(|(l, v)| l == "重大度" && v == "block"));
        assert!(shown.iter().any(|(l, v)| l == "対象" && v.contains("git push --force*")));
        assert!(!shown.iter().any(|(l, _)| l == "本文"));

        let mut fields = edit_fields("rule", &rec);
        let cmds = fields.iter_mut().find(|f| f.0 == "match.commands").unwrap();
        assert_eq!(cmds.3, "git push --force*\ngit push -f*");
        cmds.3 = "git push --force*".into();
        fields.iter_mut().find(|f| f.0 == "reason").unwrap().3 = "".into();
        let out = build_record(Some(&rec), "rule", "project", "新しい題", "unverified", "low", "本文", &fields);
        assert_eq!(out["title"], "新しい題");
        assert_eq!(out["status"], "unverified");
        assert_eq!(out["match"]["commands"], json!(["git push --force*"]));
        assert!(out.get("reason").is_none(), "an emptied field is removed");
        assert_eq!(out["id"], "rule-no-force-push", "unknown and existing keys stay");
        assert_eq!(out["createdBy"], "user");
    }

    #[test]
    fn new_record_has_no_id_and_kind_fields() {
        let fields = edit_fields("failure_case", &json!({}));
        assert!(fields.iter().any(|f| f.0 == "rootCause"));
        let out = build_record(None, "failure_case", "global", "題", "unverified", "medium", "", &fields);
        assert!(out.get("id").is_none());
        assert_eq!(out["scope"], "global");
        assert_eq!(out["createdBy"], "user");
    }

    #[test]
    fn pinned_rules_become_rows_with_scopes_and_text() {
        let (rows, scopes, text) = parse_pinned(&fixture("knowledge-pinned.json"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "rule-0001");
        assert_eq!(rows[0].summary, "履歴が消える");
        assert_eq!(scopes[0], ("rule-0001".to_string(), "project".to_string()));
        assert!(text.starts_with("禁止事項"));
    }

    #[test]
    fn guard_events_show_blocked_and_warned() {
        let list = payload(&fixture("guard-list.json")).get("events").and_then(Value::as_array).cloned().unwrap();
        let blocked = guard_row(&list[0], "T");
        assert_eq!(blocked.0, "遮断 force push 禁止");
        assert_eq!(blocked.3, 2);
        assert!(blocked.2.contains("[rule-0001]") && blocked.2.contains("理由: 履歴が消える"));
        let mut w = list[0].clone();
        w["severity"] = json!("warn");
        w["toolUseId"] = json!("call_other");
        let warned = guard_row(&w, "T");
        assert!(warned.0.starts_with("警告") && warned.3 == 1);
        assert_ne!(guard_key(&list[0]), guard_key(&w));
    }
}
