//! 根拠タブ。このファイルと ui/tabs/evidence.slint だけを編集して実装する。
//! 呼び出し元は src/tabs/mod.rs (登録は 1 か所)。
//!
//! app-server の `inocode/evidence/{listSources,readSource,listClaims,graph,verifyDocument}` を使う
//! (ref/docs/09-overnight-contract.md 4 章)。モックの応答は `{method, request, response}` の形なので、
//! `response` があればそれを使う。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, ESource, ESpan, EClaim, EvidenceState, Finding, GEdge, GNode, VSource};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

fn payload(result: &Value) -> &Value {
    result.get("response").unwrap_or(result)
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn short_time(t: &str) -> String {
    t.replace('T', " ").replace('Z', "").chars().take(16).collect()
}

fn kind_label(kind: &str) -> String {
    match kind {
        "statute" => "法令",
        "document" => "文書",
        "case" => "判例",
        "claim" => "主張",
        "span" => "引用箇所",
        "source" => "原資料",
        other => other,
    }
    .to_string()
}

fn status_label(status: &str) -> String {
    match status {
        "supported" => "根拠あり",
        "unsupported" => "根拠なし",
        "retracted" => "撤回",
        "verified" => "検証済み",
        "unverified" => "未検証",
        other => other,
    }
    .to_string()
}

fn claim_type_label(t: &str) -> String {
    match t {
        "fact" => "事実",
        "legal" => "法的主張",
        "inference" => "推論",
        "hypothesis" => "仮説",
        other => other,
    }
    .to_string()
}

fn confidence_label(c: &str) -> String {
    match c {
        "high" => "確実",
        "medium" => "たぶん",
        "low" => "未確認",
        other => other,
    }
    .to_string()
}

fn finding_label(kind: &str) -> String {
    match kind {
        "quote_missing" => "引用が原文に無い",
        "uncited" => "引用が無い",
        "novel_term" => "原資料に無い語",
        other => other,
    }
    .to_string()
}

// ------------------------------------------------------------------ pure helpers (tested)

#[derive(Debug, Clone, PartialEq)]
struct SourceData {
    id: String,
    title: String,
    meta: String,
}

fn source_meta(src: &Value) -> String {
    let mut parts = vec![kind_label(&s(src, "kind"))];
    let at = short_time(&s(src, "retrievedAt"));
    if !at.is_empty() {
        parts.push(format!("取得 {at}"));
    }
    let v = s(src, "version");
    if !v.is_empty() {
        parts.push(v);
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" · ")
}

fn parse_sources(result: &Value) -> Vec<SourceData> {
    payload(result)
        .get("sources")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|x| !s(x, "id").is_empty())
                .map(|x| SourceData { id: s(x, "id"), title: s(x, "title"), meta: source_meta(x) })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
struct ClaimData {
    id: String,
    text: String,
    type_label: String,
    status: String,
    status_label: String,
    confidence_label: String,
    spans: i32,
    span_ids: Vec<String>,
    /// 「AI」「自分」
    author: String,
}

/// createdBy -> 画面の言葉
fn author_label(created_by: &str) -> String {
    match created_by {
        "" => "".to_string(),
        "user" => "自分".to_string(),
        _ => "AI".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
struct SpanData {
    id: String,
    source_id: String,
    quote: String,
}

fn parse_spans(result: &Value) -> Vec<SpanData> {
    payload(result)
        .get("spans")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|x| !s(x, "id").is_empty())
                .map(|x| SpanData { id: s(x, "id"), source_id: s(x, "sourceId"), quote: s(x, "quote") })
                .collect()
        })
        .unwrap_or_default()
}

/// 引用 1 件の表示: `原資料名「引用の頭」`(長い引用は切る)
fn span_label(span: &SpanData, sources: &[SourceData]) -> String {
    let title = sources.iter().find(|x| x.id == span.source_id).map(|x| x.title.as_str()).unwrap_or("");
    let quote: String = span.quote.chars().take(40).collect();
    let quote = if span.quote.chars().count() > 40 { format!("{quote}…") } else { quote };
    if title.is_empty() { format!("「{quote}」") } else { format!("{title}「{quote}」") }
}

/// 主張の根拠(引用の本文)を 1 行 1 件で。ID は出さない。
fn evidence_lines(claim: &ClaimData, spans: &[SpanData], sources: &[SourceData]) -> String {
    claim
        .span_ids
        .iter()
        .filter_map(|id| spans.iter().find(|sp| &sp.id == id))
        .map(|sp| span_label(sp, sources))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 原文の中でドラッグした範囲(UTF-8 のバイト位置 2 つ。逆向きでもよい)->
/// (開始の文字位置, 終了の文字位置, 選んだ文字列)。範囲が空、空白だけ、文字の途中のとき None。
fn selection_to_quote(text: &str, a: i32, b: i32) -> Option<(usize, usize, String)> {
    let (lo, hi) = (a.min(b).max(0) as usize, a.max(b).max(0) as usize);
    let picked = text.get(lo..hi)?;
    if picked.trim().is_empty() {
        return None;
    }
    let start = text[..lo].chars().count();
    Some((start, start + picked.chars().count(), picked.to_string()))
}

fn claim_filter(all: &[ClaimData], index: i32) -> Vec<ClaimData> {
    match CLAIM_FILTERS.get(index.max(0) as usize) {
        Some((wire, _)) => all.iter().filter(|c| c.status == *wire).cloned().collect(),
        None => all.to_vec(),
    }
}

fn claim_filter_labels(all: &[ClaimData]) -> Vec<String> {
    let mut v: Vec<String> = CLAIM_FILTERS
        .iter()
        .map(|(wire, label)| format!("{} {}", label, all.iter().filter(|c| c.status == *wire).count()))
        .collect();
    v.push("すべて".to_string());
    v
}

/// 検索語を含むか(空なら全部)。大文字小文字は区別しない。
fn matches_query(text: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || text.to_lowercase().contains(&q)
}

fn parse_claims(result: &Value) -> Vec<ClaimData> {
    payload(result)
        .get("claims")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|c| !s(c, "id").is_empty())
                .map(|c| ClaimData {
                    id: s(c, "id"),
                    text: s(c, "text"),
                    type_label: claim_type_label(&s(c, "type")),
                    status: s(c, "status"),
                    status_label: status_label(&s(c, "status")),
                    confidence_label: confidence_label(&s(c, "confidence")),
                    spans: c.get("spanIds").and_then(Value::as_array).map(|a| a.len() as i32).unwrap_or(0),
                    span_ids: c
                        .get("spanIds")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                        .unwrap_or_default(),
                    author: author_label(&s(c, "createdBy")),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
struct NodeData {
    id: String,
    kind: String,
    label: String,
    status: String,
    supported: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct EdgeData {
    from: String,
    to: String,
    kind: String,
}

fn parse_graph(result: &Value) -> (Vec<NodeData>, Vec<EdgeData>) {
    let g = payload(result);
    let nodes = g
        .get("nodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|n| !s(n, "id").is_empty())
                .map(|n| NodeData {
                    id: s(n, "id"),
                    kind: s(n, "kind"),
                    label: s(n, "label"),
                    status: s(n, "status"),
                    // supported が無いときは根拠ありとして扱う
                    supported: n.get("supported").and_then(Value::as_bool).unwrap_or(true),
                })
                .collect()
        })
        .unwrap_or_default();
    let edges = g
        .get("edges")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|e| EdgeData { from: s(e, "from"), to: s(e, "to"), kind: s(e, "kind") })
                .collect()
        })
        .unwrap_or_default();
    (nodes, edges)
}

const NODE_W: f32 = 210.0;
const NODE_H: f32 = 58.0;
const H_GAP: f32 = 70.0;
const V_GAP: f32 = 18.0;
const MARGIN: f32 = 24.0;

#[derive(Debug, Clone, PartialEq)]
struct Placed {
    id: String,
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct Layout {
    placed: Vec<Placed>,
    /// (始点のノード番号, 終点のノード番号, SVG のパス)
    edges: Vec<(usize, usize, String)>,
    width: f32,
    height: f32,
}

/// 左から右へ、有向辺の最長経路で層を決め、層の中は前の層の位置の平均で並べる (交差を減らす)。
fn layout(nodes: &[NodeData], edges: &[EdgeData]) -> Layout {
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let links: Vec<(usize, usize)> = edges
        .iter()
        .filter_map(|e| Some((*index.get(e.from.as_str())?, *index.get(e.to.as_str())?)))
        .collect();
    // 層: 最長経路 (循環があっても止まるよう反復回数を制限する)
    let mut layer = vec![0usize; nodes.len()];
    for _ in 0..nodes.len() {
        let mut changed = false;
        for &(a, b) in &links {
            if a != b && layer[b] < layer[a] + 1 && layer[a] < nodes.len() {
                layer[b] = layer[a] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let depth = layer.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    let mut row_of = vec![0f32; nodes.len()];
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for l in 0..depth {
        let mut members: Vec<usize> = (0..nodes.len()).filter(|&i| layer[i] == l).collect();
        if l > 0 {
            let bary = |i: usize| -> f32 {
                let preds: Vec<f32> =
                    links.iter().filter(|(a, b)| *b == i && layer[*a] < l).map(|(a, _)| row_of[*a]).collect();
                if preds.is_empty() { f32::MAX } else { preds.iter().sum::<f32>() / preds.len() as f32 }
            };
            members.sort_by(|&a, &b| bary(a).partial_cmp(&bary(b)).unwrap_or(std::cmp::Ordering::Equal));
        }
        for (r, &i) in members.iter().enumerate() {
            row_of[i] = r as f32;
        }
        columns[l] = members;
    }
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
    let mut placed = vec![Placed { id: String::new(), x: 0.0, y: 0.0 }; nodes.len()];
    for (l, members) in columns.iter().enumerate() {
        for (r, &i) in members.iter().enumerate() {
            placed[i] = Placed {
                id: nodes[i].id.clone(),
                x: MARGIN + l as f32 * (NODE_W + H_GAP),
                y: MARGIN + r as f32 * (NODE_H + V_GAP),
            };
        }
    }
    let path = |a: usize, b: usize| -> String {
        let (x1, y1) = (placed[a].x + NODE_W, placed[a].y + NODE_H / 2.0);
        let (x2, y2) = (placed[b].x, placed[b].y + NODE_H / 2.0);
        let dx = ((x2 - x1).abs() / 2.0).max(24.0);
        // 曲線 + 終点の矢じり
        format!(
            "M {x1} {y1} C {} {y1} {} {y2} {x2} {y2} M {x2} {y2} L {} {} M {x2} {y2} L {} {}",
            x1 + dx,
            x2 - dx,
            x2 - 7.0,
            y2 - 4.0,
            x2 - 7.0,
            y2 + 4.0
        )
    };
    Layout {
        edges: links.iter().map(|&(a, b)| (a, b, path(a, b))).collect(),
        width: MARGIN * 2.0 + depth as f32 * NODE_W + depth.saturating_sub(1) as f32 * H_GAP,
        height: MARGIN * 2.0 + rows as f32 * NODE_H + rows.saturating_sub(1) as f32 * V_GAP,
        placed,
    }
}

#[derive(Debug, Clone, PartialEq)]
struct FindingData {
    kind_label: String,
    text: String,
    range: String,
}

fn parse_findings(result: &Value) -> Vec<FindingData> {
    payload(result)
        .get("findings")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|f| {
                    let start = f.pointer("/position/start").and_then(Value::as_u64);
                    let end = f.pointer("/position/end").and_then(Value::as_u64);
                    FindingData {
                        kind_label: finding_label(&s(f, "kind")),
                        text: s(f, "text"),
                        range: match (start, end) {
                            (Some(a), Some(b)) => format!("{a}–{b} 文字目"),
                            _ => String::new(),
                        },
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

// 書き込み(importSource / addQuote / addClaim / setClaimStatus)

const SOURCE_KINDS: [&str; 4] = ["statute", "document", "case", "other"];
const CLAIM_TYPES: [&str; 4] = ["fact", "legal", "inference", "hypothesis"];
const CLAIM_CONFIDENCES: [&str; 3] = ["high", "medium", "low"];
/// 主張の絞り込み: 0 = 根拠なし、1 = 根拠あり、2 = 撤回、3 = すべて
const CLAIM_FILTERS: [(&str, &str); 3] = [("unsupported", "根拠なし"), ("supported", "根拠あり"), ("retracted", "撤回")];
const CLAIM_STATUSES: [&str; 3] = ["supported", "unsupported", "retracted"];

fn nonempty(t: &str) -> Option<String> {
    let t = t.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

/// 「ファイルを選ぶ」-> importSource のパラメータ。題名はファイル名から、種別は「その他」にする(あとから直せる)。
fn import_file_params(path: &str) -> Result<Value, String> {
    let path = nonempty(path).ok_or("ファイルが選ばれていません")?;
    let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let stem = std::path::Path::new(&path).file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| name.clone());
    Ok(json!({ "path": path, "fileName": name, "title": stem, "kind": "other" }))
}

/// 「貼り付け」-> importSource のパラメータ。題名は本文の最初の行(40 文字まで)。
fn import_text_params(text: &str) -> Result<Value, String> {
    let body = nonempty(text).ok_or("クリップボードに文字がありません")?;
    let first = body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let title: String = first.chars().take(40).collect();
    Ok(json!({ "text": text, "title": title, "kind": "other" }))
}

/// 題名・種別・版のあとから直す -> updateSource のパラメータ。
fn update_source_params(id: &str, title: &str, kind: i32, version: &str) -> Result<Value, String> {
    let title = nonempty(title).ok_or("題名を入力してください")?;
    Ok(json!({
        "id": id,
        "title": title,
        "kind": SOURCE_KINDS[(kind.max(0) as usize).min(SOURCE_KINDS.len() - 1)],
        "version": version.trim(),
    }))
}

/// 主張の追加フォーム -> addClaim のパラメータ。
fn claim_params(text: &str, ty: i32, conf: i32, span_ids: &[String], premises: &[String]) -> Result<Value, String> {
    let text = nonempty(text).ok_or("主張の本文を入力してください")?;
    Ok(json!({
        "text": text,
        "type": CLAIM_TYPES[(ty.max(0) as usize).min(CLAIM_TYPES.len() - 1)],
        "confidence": CLAIM_CONFIDENCES[(conf.max(0) as usize).min(CLAIM_CONFIDENCES.len() - 1)],
        "spanIds": span_ids,
        "premiseIds": premises,
        "createdBy": "user",
    }))
}

/// 主張の追加フォームの 2 つのチェック一覧(引用・前提)。絞り込みの語を反映して作り直す。
fn push_claim_form(app: &AppWindow) {
    let st = app.global::<EvidenceState>();
    let (span_q, premise_q) = (st.get_span_query().to_string(), st.get_premise_query().to_string());
    E.with(|e| {
        let mut e = e.borrow_mut();
        let span_idx: Vec<usize> = (0..e.spans.len()).filter(|i| matches_query(&span_label(&e.spans[*i], &e.sources), &span_q)).collect();
        let span_rows: Vec<VSource> = span_idx
            .iter()
            .map(|i| {
                let sp = &e.spans[*i];
                VSource { id: sp.id.as_str().into(), title: span_label(sp, &e.sources).into(), checked: e.span_checked.contains(&sp.id) }
            })
            .collect();
        let prem_idx: Vec<usize> = (0..e.claims.len()).filter(|i| matches_query(&e.claims[*i].text, &premise_q)).collect();
        let prem_rows: Vec<VSource> = prem_idx
            .iter()
            .map(|i| {
                let c = &e.claims[*i];
                VSource { id: c.id.as_str().into(), title: c.text.as_str().into(), checked: e.premise_checked.contains(&c.id) }
            })
            .collect();
        e.span_view = span_idx;
        e.premise_view = prem_idx;
        st.set_span_options(model(span_rows));
        st.set_premise_options(model(prem_rows));
    });
}

fn install_write(app: &AppWindow, host: &Host) {
    let st = app.global::<EvidenceState>();

    let weak = app.as_weak();
    st.on_add_source(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<EvidenceState>();
        st.set_write_note("".into());
        st.set_adding_source(true);
    });
    let weak = app.as_weak();
    st.on_cancel_source(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<EvidenceState>().set_adding_source(false);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_pick_file(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = rfd::FileDialog::new().set_title("原資料にするファイルを選ぶ").pick_file() else { return };
        let st = app.global::<EvidenceState>();
        match import_file_params(&path.to_string_lossy()) {
            Ok(p) => {
                st.set_write_note("".into());
                h.rpc("evidence.w.source", "inocode/evidence/importSource", p);
            }
            Err(e) => st.set_write_note(e.into()),
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_paste_text(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<EvidenceState>();
        let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
        match import_text_params(&text) {
            Ok(p) => {
                st.set_write_note("".into());
                h.rpc("evidence.w.source", "inocode/evidence/importSource", p);
            }
            Err(e) => st.set_write_note(e.into()),
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_save_detail(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<EvidenceState>();
        let Some(id) = E.with(|e| e.borrow().selected_source.clone()) else { return };
        match update_source_params(&id, &st.get_detail_title(), st.get_detail_kind(), &st.get_detail_version()) {
            Ok(mut p) => {
                st.set_quote_note("".into());
                p["cwd"] = json!(cwd(&app));
                h.rpc("evidence.w.update", "inocode/evidence/updateSource", p);
            }
            Err(e) => st.set_quote_note(e.into()),
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_add_selection(move |a, c| {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<EvidenceState>();
        let Some(source) = E.with(|e| e.borrow().selected_source.clone()) else { return };
        let text = E.with(|e| e.borrow().source_text.clone());
        if let Some((start, end, _)) = selection_to_quote(&text, a, c) {
            st.set_quote_note("".into());
            h.rpc("evidence.w.quote", "inocode/evidence/addQuote", json!({ "sourceId": source, "start": start, "end": end, "cwd": cwd(&app) }));
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_remove_quote(move |id| {
        let Some(app) = weak.upgrade() else { return };
        h.rpc("evidence.w.rmquote", "inocode/evidence/removeQuote", json!({ "spanId": id.to_string(), "cwd": cwd(&app) }));
    });

    let weak = app.as_weak();
    st.on_add_claim(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<EvidenceState>();
        E.with(|e| {
            let mut e = e.borrow_mut();
            e.premise_checked.clear();
            // 直前に引用にした箇所は、最初からチェックを入れる
            e.span_checked = e.last_spans.iter().cloned().collect();
        });
        st.set_span_query("".into());
        st.set_premise_query("".into());
        push_claim_form(&app);
        st.set_new_claim_text("".into());
        st.set_new_claim_type(0);
        st.set_new_claim_confidence(1);
        st.set_write_note("".into());
        st.set_adding_claim(true);
    });
    let weak = app.as_weak();
    st.on_cancel_claim(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<EvidenceState>().set_adding_claim(false);
        }
    });
    let weak = app.as_weak();
    st.on_toggle_premise(move |i| {
        let Some(app) = weak.upgrade() else { return };
        E.with(|e| {
            let mut e = e.borrow_mut();
            if let Some(id) = e.premise_view.get(i as usize).and_then(|k| e.claims.get(*k)).map(|c| c.id.clone()) {
                if !e.premise_checked.remove(&id) {
                    e.premise_checked.insert(id);
                }
            }
        });
        push_claim_form(&app);
    });
    let weak = app.as_weak();
    st.on_toggle_span(move |i| {
        let Some(app) = weak.upgrade() else { return };
        E.with(|e| {
            let mut e = e.borrow_mut();
            if let Some(id) = e.span_view.get(i as usize).and_then(|k| e.spans.get(*k)).map(|x| x.id.clone()) {
                if !e.span_checked.remove(&id) {
                    e.span_checked.insert(id);
                }
            }
        });
        push_claim_form(&app);
    });
    let weak = app.as_weak();
    st.on_form_filter_changed(move || {
        if let Some(app) = weak.upgrade() {
            push_claim_form(&app);
        }
    });
    let weak = app.as_weak();
    st.on_claim_filter_changed(move |_| {
        if let Some(app) = weak.upgrade() {
            push_claims(&app);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_save_claim(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<EvidenceState>();
        let (premises, spans): (Vec<String>, Vec<String>) = E.with(|e| {
            let e = e.borrow();
            (
                e.claims.iter().filter(|c| e.premise_checked.contains(&c.id)).map(|c| c.id.clone()).collect(),
                e.spans.iter().filter(|x| e.span_checked.contains(&x.id)).map(|x| x.id.clone()).collect(),
            )
        });
        match claim_params(&st.get_new_claim_text(), st.get_new_claim_type(), st.get_new_claim_confidence(), &spans, &premises) {
            Ok(mut p) => {
                st.set_write_note("".into());
                p["cwd"] = json!(cwd(&app));
                h.rpc("evidence.w.claim", "inocode/evidence/addClaim", p);
            }
            Err(e) => st.set_write_note(e.into()),
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_claim_set_status(move |id, i| {
        let Some(app) = weak.upgrade() else { return };
        let id = id.to_string();
        let status = CLAIM_STATUSES[(i.max(0) as usize).min(CLAIM_STATUSES.len() - 1)];
        h.rpc("evidence.w.status", "inocode/evidence/setClaimStatus", json!({ "id": id, "status": status, "cwd": cwd(&app) }));
    });
}

// ------------------------------------------------------------------ state

#[derive(Default)]
struct E {
    sources: Vec<SourceData>,
    selected_source: Option<String>,
    claims: Vec<ClaimData>,
    nodes: Vec<NodeData>,
    edges: Vec<EdgeData>,
    layout: Option<Layout>,
    selected_node: Option<String>,
    verify_checked: Vec<bool>,
    /// 追加した引用箇所の ID(主張の追加フォームに自動で入れる)
    last_spans: Vec<String>,
    /// 引用箇所の一覧(全原資料)
    spans: Vec<SpanData>,
    /// いま開いている原資料の原文
    source_text: String,
    /// 主張の追加フォームでチェックされた引用の ID・前提の主張の ID
    span_checked: HashSet<String>,
    premise_checked: HashSet<String>,
    /// 絞り込み後に画面に出ている項目の元の添字
    span_view: Vec<usize>,
    premise_view: Vec<usize>,
}

thread_local! {
    static E: RefCell<E> = RefCell::new(E::default());
}

fn cwd(app: &AppWindow) -> String {
    let c = app.global::<crate::AppState>().get_cwd_text().to_string();
    if !c.is_empty() {
        return c;
    }
    std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn push_sources(app: &AppWindow) {
    E.with(|e| {
        let e = e.borrow();
        let st = app.global::<EvidenceState>();
        st.set_sources(model(
            e.sources
                .iter()
                .map(|x| ESource {
                    id: x.id.as_str().into(),
                    title: x.title.as_str().into(),
                    meta: x.meta.as_str().into(),
                    selected: e.selected_source.as_deref() == Some(x.id.as_str()),
                })
                .collect(),
        ));
        let checked = |i: usize| e.verify_checked.get(i).copied().unwrap_or(true);
        st.set_verify_sources(model(
            e.sources
                .iter()
                .enumerate()
                .map(|(i, x)| VSource { id: x.id.as_str().into(), title: x.title.as_str().into(), checked: checked(i) })
                .collect(),
        ));
    });
}

/// 開いている原資料の引用の一覧
fn push_source_spans(app: &AppWindow) {
    E.with(|e| {
        let e = e.borrow();
        let rows: Vec<ESpan> = e
            .spans
            .iter()
            .filter(|x| e.selected_source.as_deref() == Some(x.source_id.as_str()))
            .map(|x| ESpan { id: x.id.as_str().into(), text: x.quote.chars().take(60).collect::<String>().into() })
            .collect();
        app.global::<EvidenceState>().set_source_spans(model(rows));
    });
}

fn push_claims(app: &AppWindow) {
    E.with(|e| {
        let e = e.borrow();
        let st = app.global::<EvidenceState>();
        let shown = claim_filter(&e.claims, st.get_claim_filter());
        let labels: Vec<slint::SharedString> = claim_filter_labels(&e.claims).into_iter().map(Into::into).collect();
        st.set_claim_tags(model(labels));
        st.set_claims(model(
            shown
                .iter()
                .map(|c| EClaim {
                    id: c.id.as_str().into(),
                    text: c.text.as_str().into(),
                    type_label: c.type_label.as_str().into(),
                    status: c.status.as_str().into(),
                    status_label: c.status_label.as_str().into(),
                    confidence_label: c.confidence_label.as_str().into(),
                    evidence: evidence_lines(c, &e.spans, &e.sources).into(),
                    author: c.author.as_str().into(),
                })
                .collect(),
        ));
        st.set_claims_empty(if shown.is_empty() {
            if e.claims.is_empty() { "主張はまだありません".into() } else { "この状態の主張はありません".into() }
        } else {
            slint::SharedString::default()
        });
    });
}

fn push_graph(app: &AppWindow) {
    E.with(|e| {
        let e = e.borrow();
        let st = app.global::<EvidenceState>();
        let Some(lay) = &e.layout else {
            st.set_nodes(model(Vec::new()));
            st.set_edges(model(Vec::new()));
            return;
        };
        st.set_nodes(model(
            e.nodes
                .iter()
                .zip(&lay.placed)
                .map(|(n, p)| GNode {
                    id: n.id.as_str().into(),
                    kind_label: kind_label(&n.kind).into(),
                    label: n.label.as_str().into(),
                    supported: n.supported,
                    selected: e.selected_node.as_deref() == Some(n.id.as_str()),
                    x: p.x,
                    y: p.y,
                    w: NODE_W,
                    h: NODE_H,
                })
                .collect(),
        ));
        st.set_edges(model(
            lay.edges
                .iter()
                .map(|(a, b, path)| {
                    let kind = e.edges.iter().find(|x| x.from == e.nodes[*a].id && x.to == e.nodes[*b].id).map(|x| x.kind.as_str()).unwrap_or("");
                    let tone = if !e.nodes[*b].supported {
                        3
                    } else {
                        match kind {
                            "premise" => 1,
                            "cites" => 2,
                            _ => 0,
                        }
                    };
                    GEdge { commands: path.as_str().into(), tone }
                })
                .collect(),
        ));
        st.set_canvas_w(lay.width);
        st.set_canvas_h(lay.height);
    });
}

fn show_node_detail(app: &AppWindow) {
    E.with(|e| {
        let e = e.borrow();
        let st = app.global::<EvidenceState>();
        let Some(id) = &e.selected_node else { return st.set_has_node(false) };
        let Some(n) = e.nodes.iter().find(|n| &n.id == id) else { return st.set_has_node(false) };
        let label_of = |x: &str| e.nodes.iter().find(|n| n.id == x).map(|n| n.label.clone()).unwrap_or_else(|| x.to_string());
        let edge_kind = |k: &str| -> String {
            match k {
                "supports" => "支える引用箇所",
                "premise" => "前提",
                "cites" => "出典",
                other => other,
            }
            .to_string()
        };
        let mut links: Vec<String> = Vec::new();
        for ed in &e.edges {
            if ed.from == n.id {
                links.push(format!("{}: {}", edge_kind(&ed.kind), label_of(&ed.to)));
            } else if ed.to == n.id {
                let rev = match ed.kind.as_str() {
                    "supports" => "支えている主張",
                    "premise" => "前提としている主張",
                    "cites" => "引用している箇所",
                    other => other,
                }
                .to_string();
                links.push(format!("{rev}: {}", label_of(&ed.from)));
            }
        }
        st.set_node_title(n.label.as_str().into());
        st.set_node_meta(
            format!(
                "{} · {} · {}",
                kind_label(&n.kind),
                status_label(&n.status),
                if n.supported { "根拠あり" } else { "根拠なし" }
            )
            .into(),
        );
        st.set_node_links(links.join("\n").into());
        st.set_has_node(true);
    });
}

fn request_for_tab(app: &AppWindow, host: &Host, tab: i32) {
    let cwd = cwd(app);
    match tab {
        0 | 3 => {
            host.rpc("evidence.sources", "inocode/evidence/listSources", json!({ "cwd": cwd, "limit": 100 }));
            host.rpc("evidence.spans", "inocode/evidence/listSpans", json!({ "cwd": cwd }));
        }
        1 => {
            host.rpc("evidence.sources", "inocode/evidence/listSources", json!({ "cwd": cwd, "limit": 100 }));
            host.rpc("evidence.spans", "inocode/evidence/listSpans", json!({ "cwd": cwd }));
            host.rpc("evidence.claims", "inocode/evidence/listClaims", json!({ "cwd": cwd, "limit": 100 }));
        }
        2 => host.rpc("evidence.graph", "inocode/evidence/graph", json!({ "cwd": cwd })),
        _ => {}
    }
}

// ------------------------------------------------------------------ tab entry points

/// 起動時に 1 回。コールバックの登録をここで行う。
pub fn install(app: &AppWindow, host: &Host) {
    install_write(app, host);
    let st = app.global::<EvidenceState>();

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            request_for_tab(&app, &h, app.global::<EvidenceState>().get_tab());
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_tab_changed(move |i| {
        if let Some(app) = weak.upgrade() {
            request_for_tab(&app, &h, i);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_select_source(move |id| {
        let Some(app) = weak.upgrade() else { return };
        E.with(|e| e.borrow_mut().selected_source = Some(id.to_string()));
        push_sources(&app);
        h.rpc("evidence.source", "inocode/evidence/readSource", json!({ "id": id.to_string(), "cwd": cwd(&app) }));
    });
    let weak = app.as_weak();
    st.on_select_node(move |id| {
        let Some(app) = weak.upgrade() else { return };
        E.with(|e| e.borrow_mut().selected_node = Some(id.to_string()));
        push_graph(&app);
        show_node_detail(&app);
    });
    let weak = app.as_weak();
    st.on_toggle_verify_source(move |i| {
        let Some(app) = weak.upgrade() else { return };
        E.with(|e| {
            let mut e = e.borrow_mut();
            let n = e.sources.len();
            if e.verify_checked.len() < n {
                e.verify_checked.resize(n, true);
            }
            if let Some(c) = e.verify_checked.get_mut(i as usize) {
                *c = !*c;
            }
        });
        push_sources(&app);
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_verify(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<EvidenceState>();
        let text = st.get_verify_text().to_string();
        if text.trim().is_empty() {
            st.set_verify_note("検証する文書を入力してください".into());
            return;
        }
        let ids: Vec<String> = E.with(|e| {
            let e = e.borrow();
            e.sources
                .iter()
                .enumerate()
                .filter(|(i, _)| e.verify_checked.get(*i).copied().unwrap_or(true))
                .map(|(_, s)| s.id.clone())
                .collect()
        });
        st.set_verifying(true);
        st.set_verify_note("".into());
        h.rpc("evidence.verify", "inocode/evidence/verifyDocument", json!({ "cwd": cwd(&app), "text": text, "sourceIds": ids }));
    });
}

/// このタブが開かれるたびに呼ばれる。現在の画面のデータを取り直す。
pub fn opened(app: &AppWindow, host: &Host) {
    // 文書の検証でも原資料の一覧が要る
    request_for_tab(app, host, app.global::<EvidenceState>().get_tab());
}

/// クライアントのイベント (Event::Rpc / Event::Notify) が届くたびに呼ばれる。
pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    let Event::Rpc { tag, result } = event else { return };
    let st = app.global::<EvidenceState>();
    match (tag.as_str(), result) {
        ("evidence.sources", Ok(v)) => {
            st.set_note("".into());
            E.with(|e| {
                let mut e = e.borrow_mut();
                e.sources = parse_sources(v);
                let n = e.sources.len();
                e.verify_checked.resize(n, true);
            });
            push_sources(app);
            // 引用の表示に原資料名を使うので、主張の一覧も作り直す
            push_claims(app);
        }
        ("evidence.spans", Ok(v)) => {
            E.with(|e| e.borrow_mut().spans = parse_spans(v));
            push_source_spans(app);
            push_claims(app);
            push_claim_form(app);
        }
        ("evidence.source", Ok(v)) => {
            let p = payload(v);
            let src = p.get("source").cloned().unwrap_or(Value::Null);
            st.set_source_title(s(&src, "title").into());
            let mut meta = source_meta(&src);
            let hash = s(&src, "hash");
            if !hash.is_empty() {
                meta = format!("{meta} · {hash}");
            }
            st.set_source_meta(meta.into());
            st.set_detail_title(s(&src, "title").into());
            st.set_detail_version(s(&src, "version").into());
            st.set_detail_kind(SOURCE_KINDS.iter().position(|k| *k == s(&src, "kind")).map(|i| i as i32).unwrap_or(3));
            st.set_source_text(s(p, "text").into());
            E.with(|e| e.borrow_mut().source_text = s(p, "text"));
            st.set_has_source(true);
            push_source_spans(app);
        }
        ("evidence.claims", Ok(v)) => {
            st.set_note("".into());
            E.with(|e| e.borrow_mut().claims = parse_claims(v));
            push_claims(app);
        }
        ("evidence.graph", Ok(v)) => {
            st.set_note("".into());
            let (nodes, edges) = parse_graph(v);
            let lay = layout(&nodes, &edges);
            E.with(|e| {
                let mut e = e.borrow_mut();
                e.nodes = nodes;
                e.edges = edges;
                e.layout = Some(lay);
                e.selected_node = None;
            });
            st.set_has_node(false);
            push_graph(app);
        }
        ("evidence.verify", Ok(v)) => {
            let found = parse_findings(v);
            st.set_verifying(false);
            st.set_verify_note(if found.is_empty() { "指摘はありません".into() } else { format!("{} 件の指摘", found.len()).into() });
            st.set_findings(model(
                found
                    .into_iter()
                    .map(|f| Finding { kind_label: f.kind_label.into(), text: f.text.into(), range: f.range.into() })
                    .collect(),
            ));
        }
        ("evidence.verify", Err(e)) => {
            st.set_verifying(false);
            st.set_verify_note(e.as_str().into());
        }
        ("evidence.w.source", Ok(v)) => {
            st.set_adding_source(false);
            let id = payload(v).get("id").and_then(Value::as_str).map(str::to_string);
            if let Some(id) = id {
                E.with(|e| e.borrow_mut().selected_source = Some(id.clone()));
                host.rpc("evidence.source", "inocode/evidence/readSource", json!({ "id": id, "cwd": cwd(app) }));
            }
            request_for_tab(app, host, 0);
        }
        ("evidence.w.source", Err(e)) => st.set_write_note(e.as_str().into()),
        // 題名・種別・版を直した: 一覧と、開いている原資料を読み直す
        ("evidence.w.update", Ok(_)) => {
            st.set_detail_open(false);
            request_for_tab(app, host, 0);
            if let Some(id) = E.with(|e| e.borrow().selected_source.clone()) {
                host.rpc("evidence.source", "inocode/evidence/readSource", json!({ "id": id, "cwd": cwd(app) }));
            }
        }
        ("evidence.w.update", Err(e)) => st.set_quote_note(e.as_str().into()),
        ("evidence.w.quote", Ok(v)) => {
            let span = s(payload(v), "spanId");
            st.set_quote_note("".into());
            E.with(|e| e.borrow_mut().last_spans.push(span));
            host.rpc("evidence.spans", "inocode/evidence/listSpans", json!({ "cwd": cwd(app) }));
        }
        ("evidence.w.quote", Err(e)) => st.set_quote_note(e.as_str().into()),
        // 引用を捨てた: 引用の一覧と、それを根拠にしていた主張(根拠なしに戻る)を読み直す
        ("evidence.w.rmquote", Ok(_)) => {
            host.rpc("evidence.spans", "inocode/evidence/listSpans", json!({ "cwd": cwd(app) }));
            host.rpc("evidence.claims", "inocode/evidence/listClaims", json!({ "cwd": cwd(app), "limit": 100 }));
        }
        ("evidence.w.rmquote", Err(e)) => st.set_quote_note(e.as_str().into()),
        ("evidence.w.claim", Ok(_)) => {
            st.set_adding_claim(false);
            E.with(|e| e.borrow_mut().last_spans.clear());
            request_for_tab(app, host, 1);
        }
        ("evidence.w.claim", Err(e)) => st.set_write_note(e.as_str().into()),
        ("evidence.w.status", Ok(_)) => request_for_tab(app, host, 1),
        (t, Err(e)) if t.starts_with("evidence.") => st.set_note(e.as_str().into()),
        _ => {}
    }
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
    fn sources_and_claims() {
        let src = parse_sources(&fixture("evidence-listSources.json"));
        assert_eq!(src.len(), 2);
        assert_eq!(src[0].meta, "法令 · 取得 2026-11-10 10:00 · 令和7年6月1日施行");
        let claims = parse_claims(&fixture("evidence-listClaims.json"));
        assert_eq!(claims.len(), 3);
        assert_eq!(claims[0].spans, 1);
        assert_eq!(claims[2].status_label, "根拠なし");
        assert_eq!(claims[2].type_label, "法的主張");
    }

    #[test]
    fn claims_are_filtered_by_status_and_show_their_quotes() {
        let sources = parse_sources(&fixture("evidence-listSources.json"));
        let spans = parse_spans(&fixture("evidence-listSpans.json"));
        assert!(spans.len() >= 2);
        let claims = parse_claims(&fixture("evidence-listClaims.json"));
        assert_eq!(claims[0].author, "AI");
        assert_eq!(claims[0].confidence_label, "確実");
        let first = evidence_lines(&claims[0], &spans, &sources);
        assert!(first.starts_with("刑法 第199条(殺人)「人を殺した者は"), "{first}");
        assert!(!first.contains("span-"), "ids are not shown");
        // 既定は「根拠なし」
        let unsupported = claim_filter(&claims, 0);
        assert!(!unsupported.is_empty() && unsupported.iter().all(|c| c.status == "unsupported"));
        assert_eq!(claim_filter(&claims, 3).len(), claims.len());
        let labels = claim_filter_labels(&claims);
        assert_eq!(labels.len(), 4);
        assert!(labels[0].starts_with("根拠なし ") && labels[3] == "すべて");
    }

    #[test]
    fn drag_selection_becomes_character_offsets() {
        let text = "第百九十九条 人を殺した者は、死刑";
        // "第百九十九条 " は 6 文字 = 18 + 1 バイト
        let lo = "第百九十九条 ".len() as i32;
        let hi = lo + "人を殺した者は".len() as i32;
        let (s, e, q) = selection_to_quote(text, lo, hi).unwrap();
        assert_eq!((s, e, q.as_str()), (7, 14, "人を殺した者は"));
        // 逆向きにドラッグしても同じ
        assert_eq!(selection_to_quote(text, hi, lo).unwrap().2, "人を殺した者は");
        // 空、空白だけ、文字の途中は引用にしない
        assert!(selection_to_quote(text, 5, 5).is_none());
        assert!(selection_to_quote(text, 18, 19).is_none());
        assert!(selection_to_quote(text, 1, 4).is_none());
        assert!(selection_to_quote(text, 0, 9999).is_none());
    }

    #[test]
    fn long_quotes_are_cut_and_lists_filter_by_words() {
        let long = SpanData { id: "s".into(), source_id: "x".into(), quote: "あ".repeat(60) };
        let label = span_label(&long, &[]);
        assert_eq!(label.chars().count(), 40 + 3, "「」と … を含む");
        assert!(label.ends_with("…」"));
        assert!(matches_query("刑法 第199条", "199"));
        assert!(matches_query("Abc", " abc "));
        assert!(matches_query("何でも", ""));
        assert!(!matches_query("刑法", "民法"));
    }

    #[test]
    fn graph_layout_is_left_to_right_and_flags_unsupported() {
        let (nodes, edges) = parse_graph(&fixture("evidence-graph.json"));
        assert_eq!(nodes.len(), 7);
        assert_eq!(edges.len(), 5);
        // 根拠のないノードが見分けられる
        let unsupported: Vec<&str> = nodes.iter().filter(|n| !n.supported).map(|n| n.id.as_str()).collect();
        assert_eq!(unsupported, ["claim-003"]);
        let lay = layout(&nodes, &edges);
        let x = |id: &str| lay.placed.iter().find(|p| p.id == id).unwrap().x;
        // claim -> span -> source は左から右へ進む
        assert!(x("claim-001") < x("span-001"));
        assert!(x("span-001") < x("src-criminal-code-199"));
        assert!(x("claim-002") > x("claim-001"), "a premise sits to the right of the claim");
        // 重なるノードは無い
        for (i, a) in lay.placed.iter().enumerate() {
            for b in &lay.placed[i + 1..] {
                assert!(a.x != b.x || (a.y - b.y).abs() >= NODE_H, "{} overlaps {}", a.id, b.id);
            }
        }
        assert!(lay.width > 0.0 && lay.height > 0.0);
        assert_eq!(lay.edges.len(), 5);
        assert!(lay.edges[0].2.starts_with("M "));
    }

    #[test]
    fn layout_survives_cycles_and_empty_graphs() {
        let n = |id: &str| NodeData { id: id.into(), kind: "claim".into(), label: id.into(), status: "".into(), supported: true };
        let e = |a: &str, b: &str| EdgeData { from: a.into(), to: b.into(), kind: "premise".into() };
        let lay = layout(&[n("a"), n("b")], &[e("a", "b"), e("b", "a"), e("a", "a"), e("a", "zzz")]);
        assert_eq!(lay.placed.len(), 2);
        let empty = layout(&[], &[]);
        assert_eq!((empty.width, empty.height), (MARGIN * 2.0, MARGIN * 2.0));
    }

    #[test]
    fn findings() {
        let f = parse_findings(&fixture("evidence-verifyDocument.json"));
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].kind_label, "原資料に無い語");
        assert_eq!(f[0].text, "強盗致死罪");
        assert_eq!(f[0].range, "22–27 文字目");
        assert_eq!(f[2].kind_label, "引用が無い");
    }

    #[test]
    fn importing_needs_no_typing() {
        // ファイル: 題名はファイル名、種別は「その他」
        let f = import_file_params("D:/docs/告訴状 2026.pdf").unwrap();
        assert_eq!((f["fileName"].as_str(), f["title"].as_str(), f["kind"].as_str()), (Some("告訴状 2026.pdf"), Some("告訴状 2026"), Some("other")));
        assert_eq!(f["path"], "D:/docs/告訴状 2026.pdf");
        assert!(import_file_params("  ").is_err());
        // 貼り付け: 題名は最初の行
        let t = import_text_params("\n  第百九十九条 人を殺した者は\n本文").unwrap();
        assert_eq!(t["title"], "第百九十九条 人を殺した者は");
        assert_eq!(t["kind"], "other");
        assert!(t["text"].as_str().unwrap().contains("本文"));
        let long = import_text_params(&"あ".repeat(100)).unwrap();
        assert_eq!(long["title"].as_str().unwrap().chars().count(), 40);
        assert!(import_text_params(" \n ").is_err());
    }

    #[test]
    fn source_details_can_be_fixed_later() {
        let p = update_source_params("src-1", " 新しい題 ", 0, " 第2版 ").unwrap();
        assert_eq!((p["id"].as_str(), p["title"].as_str(), p["kind"].as_str(), p["version"].as_str()), (Some("src-1"), Some("新しい題"), Some("statute"), Some("第2版")));
        assert!(update_source_params("src-1", "  ", 1, "").is_err());
    }

    #[test]
    fn claim_params_split_spans_and_carry_premises() {
        let spans: Vec<String> = ["span-1", "span-2", "span-3"].iter().map(|x| x.to_string()).collect();
        let p = claim_params("被告人は…", 1, 0, &spans, &["claim-001".to_string()]).unwrap();
        assert_eq!(p["type"], "legal");
        assert_eq!(p["confidence"], "high");
        assert_eq!(p["spanIds"], json!(["span-1", "span-2", "span-3"]));
        assert_eq!(p["premiseIds"], json!(["claim-001"]));
        assert_eq!(p["createdBy"], "user");
        assert!(claim_params("", 0, 0, &[], &[]).is_err());
    }
}
