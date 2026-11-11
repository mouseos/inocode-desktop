//! 統計の画面(設定の「InoCode」群): モデルごとの費用(枠の消費)と速度。
//! 値はすべて inocode/stats/* から取る。画面側に固定の係数は持たない。
//! 仕様は ref/docs/24-stats-rpc.md。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, StatsBar, StatsModel, StatsState};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const TAG_SUMMARY: &str = "stats.summary";
const TAG_HOURLY: &str = "stats.hourly";
const TAG_DAILY: &str = "stats.daily";
const TAG_ESTIMATE: &str = "stats.estimate";
const TAG_SEED: &str = "stats.seed";

/// 期間の選択肢(日数)。StatsState.periods と同じ順。
const PERIODS: [i64; 3] = [3, 7, 30];
/// 日ごとの推移で読む日数
const DAILY_DAYS: i64 = 14;
/// 1 時間帯の件数がこれより少ないとき、棒を薄くする
const MIN_SAMPLES: i64 = 5;

#[derive(Default)]
struct S {
    summary: Value,
    selected_model: String,
    job_id: String,
}

thread_local! {
    static S: RefCell<S> = RefCell::new(S::default());
}

// ------------------------------------------------------------------ pure helpers (tested)

fn payload(v: &Value) -> &Value {
    v.get("response").unwrap_or(v)
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

/// トークン数 -> "950" / "80K" / "1.2M"
fn tokens_label(n: f64) -> String {
    let trim = |x: f64| {
        let t = format!("{x:.1}");
        t.strip_suffix(".0").map(str::to_string).unwrap_or(t)
    };
    if n >= 1_000_000.0 {
        format!("{}M", trim(n / 1_000_000.0))
    } else if n >= 10_000.0 {
        format!("{}K", (n / 1000.0).round() as i64)
    } else if n >= 1000.0 {
        format!("{}K", trim(n / 1000.0))
    } else {
        format!("{}", n.round() as i64)
    }
}

/// 枠の使用率 -> "18.4%" (1% 未満は小数 2 桁)
fn pct_label(p: f64) -> String {
    if p.abs() < 1.0 && p != 0.0 {
        format!("{p:.2}%")
    } else {
        format!("{p:.1}%")
    }
}

fn pct_of(v: Option<&Value>, k: &str) -> String {
    v.and_then(|v| num(v, k)).map(pct_label).unwrap_or_else(|| "—".into())
}

fn speed_label(v: Option<f64>) -> String {
    v.map(|s| format!("{}", s.round() as i64)).unwrap_or_else(|| "—".into())
}

/// 100 万トークンあたりの枠の消費。サンプルが少ない(insufficient か null)ときは「データが足りない」。
fn per_million_label(m: &Value) -> String {
    let q = m.get("quotaPerMillion").filter(|q| q.is_object());
    if m.get("insufficient").and_then(Value::as_bool).unwrap_or(false) || q.is_none() {
        return "100万トークンあたり: データが足りない".into();
    }
    let q = q.unwrap();
    let one = |k: &str| {
        let w = q.get(k);
        format!("{} / {}", pct_of(w, "primaryPercent"), pct_of(w, "secondaryPercent"))
    };
    format!(
        "100万トークンあたり(5時間 / 週)  入力 {}  キャッシュ {}  出力 {}",
        one("uncachedInput"),
        one("cachedInput"),
        one("output")
    )
}

fn model_names(summary: &Value) -> Vec<String> {
    summary.get("models").and_then(Value::as_array).map(|a| a.iter().map(|m| str_of(m, "model").to_string()).collect()).unwrap_or_default()
}

/// summary -> 表の行
fn model_rows(summary: &Value, selected: &str) -> Vec<StatsModel> {
    let Some(models) = summary.get("models").and_then(Value::as_array) else { return vec![] };
    models
        .iter()
        .map(|m| {
            let t = m.get("tokens");
            let tok = |k: &str| t.and_then(|t| num(t, k)).map(tokens_label).unwrap_or_else(|| "—".into());
            let q = m.get("quotaUsed");
            let name = str_of(m, "model");
            StatsModel {
                name: name.into(),
                uncached: tok("uncachedInput").into(),
                cached: tok("cachedInput").into(),
                output: tok("output").into(),
                q5: pct_of(q, "primaryPercent").into(),
                qw: pct_of(q, "secondaryPercent").into(),
                speed: speed_label(num(m, "avgTokensPerSecond")).into(),
                per_million: per_million_label(m).into(),
                selected: !selected.is_empty() && name == selected,
            }
        })
        .collect()
}

/// hourly -> 24 本の棒。件数の少ない時間帯は薄く、件数 0 の時間帯は棒なし。
fn hour_bars(hourly: &Value) -> Vec<StatsBar> {
    let hours = hourly.get("hours").and_then(Value::as_array).cloned().unwrap_or_default();
    let max = hours.iter().filter_map(|h| num(h, "avgTokensPerSecond")).fold(0.0_f64, f64::max);
    hours
        .iter()
        .map(|h| {
            let hour = h.get("hour").and_then(Value::as_i64).unwrap_or(0);
            let samples = h.get("samples").and_then(Value::as_i64).unwrap_or(0);
            let avg = num(h, "avgTokensPerSecond");
            StatsBar {
                top: avg.map(|a| format!("{}", a.round() as i64)).unwrap_or_default().into(),
                label: if hour % 6 == 0 { format!("{hour}時") } else { String::new() }.into(),
                sub: "".into(),
                ratio: match avg {
                    Some(a) if max > 0.0 && samples > 0 => (a / max) as f32,
                    _ => 0.0,
                },
                faint: samples < MIN_SAMPLES,
            }
        })
        .collect()
}

/// daily -> 日ごとの棒(5時間枠の消費)。下に日付と tok/s。
fn day_bars(daily: &Value) -> Vec<StatsBar> {
    let days = daily.get("days").and_then(Value::as_array).cloned().unwrap_or_default();
    let max = days.iter().filter_map(|d| num(d, "quotaPrimaryPercent")).fold(0.0_f64, f64::max);
    days.iter()
        .map(|d| {
            let date = str_of(d, "date");
            let q = num(d, "quotaPrimaryPercent");
            StatsBar {
                top: q.map(pct_label).unwrap_or_default().into(),
                label: date.get(5..).unwrap_or(date).into(),
                sub: num(d, "avgTokensPerSecond").map(|s| format!("{} tok/s", s.round() as i64)).unwrap_or_default().into(),
                ratio: match q {
                    Some(q) if max > 0.0 => (q / max) as f32,
                    _ => 0.0,
                },
                faint: false,
            }
        })
        .collect()
}

/// 入力欄の数 ("1,200" "" など) -> トークン数。空は 0。
fn parse_count(text: &str) -> Result<i64, String> {
    let t: String = text.chars().filter(|c| !c.is_whitespace() && *c != ',').collect();
    if t.is_empty() {
        return Ok(0);
    }
    t.parse::<i64>().ok().filter(|n| *n >= 0).ok_or_else(|| format!("「{text}」は数として読めません"))
}

/// 見積もりの入力 -> estimate のパラメータ
fn estimate_params(model: &str, days: i64, uncached: &str, cached: &str, output: &str) -> Result<Value, String> {
    if model.is_empty() {
        return Err("モデルを選んでください".into());
    }
    let (u, c, o) = (parse_count(uncached)?, parse_count(cached)?, parse_count(output)?);
    if u + c + o == 0 {
        return Err("トークン数を入力してください".into());
    }
    Ok(json!({ "model": model, "days": days, "tokens": { "uncachedInput": u, "cachedInput": c, "output": o } }))
}

fn seconds_label(secs: f64) -> String {
    let s = secs.round() as i64;
    if s < 60 {
        format!("約 {s} 秒")
    } else {
        format!("約 {} 分 {} 秒", s / 60, s % 60)
    }
}

/// estimate の応答 -> 1 行の結果
fn estimate_label(v: &Value) -> String {
    if v.get("insufficient").and_then(Value::as_bool).unwrap_or(false) {
        return "データが足りない".into();
    }
    let p = num(v, "primaryPercent").map(pct_label).unwrap_or_else(|| "—".into());
    let w = num(v, "secondaryPercent").map(pct_label).unwrap_or_else(|| "—".into());
    let t = num(v, "seconds").map(seconds_label).unwrap_or_else(|| "—".into());
    format!("5時間枠 約 {p}、週枠 約 {w}、{t}")
}

/// seedProgress の通知 -> (進み具合 0〜1, 表示, 終わったか)
fn seed_progress(p: &Value) -> (f32, String, bool) {
    let done = p.get("done").and_then(Value::as_bool).unwrap_or(false);
    let total = p.get("filesTotal").and_then(Value::as_i64).unwrap_or(0);
    let finished = p.get("filesDone").and_then(Value::as_i64).unwrap_or(0);
    let added = p.get("samplesAdded").and_then(Value::as_i64).unwrap_or(0);
    if done {
        return (1.0, format!("取り込み完了 · {added} 件"), true);
    }
    let ratio = if total > 0 { (finished as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
    (ratio, format!("{finished} / {total} ファイル · {added} 件"), false)
}

// ------------------------------------------------------------------ screen

fn days_of(app: &AppWindow) -> i64 {
    PERIODS[(app.global::<StatsState>().get_period().max(0) as usize).min(PERIODS.len() - 1)]
}

fn request_hourly(app: &AppWindow, host: &Host) {
    let model = S.with(|s| s.borrow().selected_model.clone());
    let model = if model.is_empty() { Value::Null } else { json!(model) };
    host.rpc(TAG_HOURLY, "inocode/stats/hourly", json!({ "days": days_of(app), "model": model }));
}

fn refresh(app: &AppWindow, host: &Host) {
    host.rpc(TAG_SUMMARY, "inocode/stats/summary", json!({ "days": days_of(app), "model": Value::Null, "provider": Value::Null }));
    request_hourly(app, host);
    host.rpc(TAG_DAILY, "inocode/stats/daily", json!({ "days": DAILY_DAYS }));
}

fn render_models(app: &AppWindow) {
    let st = app.global::<StatsState>();
    let (rows, selected, names) = S.with(|s| {
        let s = s.borrow();
        (model_rows(&s.summary, &s.selected_model), s.selected_model.clone(), model_names(&s.summary))
    });
    st.set_empty(rows.is_empty());
    st.set_models(ModelRc::from(Rc::new(VecModel::from(rows))));
    st.set_selected_model(selected.into());
    let est = st.get_est_model().to_string();
    if !names.contains(&est) {
        st.set_est_model(names.first().cloned().unwrap_or_default().into());
    }
}

pub fn install(app: &AppWindow, host: &Host) {
    let st = app.global::<StatsState>();

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            refresh(&app, &h);
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_set_period(move |i| {
        let Some(app) = weak.upgrade() else { return };
        app.global::<StatsState>().set_period(i);
        refresh(&app, &h);
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_select_model(move |name| {
        let Some(app) = weak.upgrade() else { return };
        S.with(|s| {
            let mut s = s.borrow_mut();
            s.selected_model = if s.selected_model == name.as_str() { String::new() } else { name.to_string() };
        });
        render_models(&app);
        request_hourly(&app, &h);
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_estimate(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<StatsState>();
        match estimate_params(&st.get_est_model(), days_of(&app), &st.get_est_uncached(), &st.get_est_cached(), &st.get_est_output()) {
            Ok(p) => h.rpc(TAG_ESTIMATE, "inocode/stats/estimate", p),
            Err(e) => st.set_est_result(e.into()),
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_seed(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<StatsState>();
        st.set_seeding(true);
        st.set_seed_ratio(0.0);
        st.set_seed_text("".into());
        h.rpc(TAG_SEED, "inocode/stats/seedFromCodexHistory", json!({ "path": Value::Null }));
    });
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    let st = app.global::<StatsState>();
    match event {
        Event::Rpc { tag, result } => match (tag.as_str(), result) {
            (TAG_SUMMARY, Ok(v)) => {
                st.set_note("".into());
                S.with(|s| s.borrow_mut().summary = payload(v).clone());
                render_models(app);
            }
            (TAG_HOURLY, Ok(v)) => st.set_hours(ModelRc::from(Rc::new(VecModel::from(hour_bars(payload(v)))))),
            (TAG_DAILY, Ok(v)) => st.set_days(ModelRc::from(Rc::new(VecModel::from(day_bars(payload(v)))))),
            (TAG_ESTIMATE, Ok(v)) => st.set_est_result(estimate_label(payload(v)).into()),
            (TAG_ESTIMATE, Err(e)) => st.set_est_result(e.as_str().into()),
            (TAG_SEED, Ok(v)) => {
                S.with(|s| s.borrow_mut().job_id = str_of(payload(v), "jobId").to_string());
            }
            (TAG_SEED, Err(e)) => {
                st.set_seeding(false);
                st.set_seed_text(e.as_str().into());
            }
            (TAG_SUMMARY, Err(e)) | (TAG_HOURLY, Err(e)) | (TAG_DAILY, Err(e)) => st.set_note(e.as_str().into()),
            _ => {}
        },
        Event::Notify { method, params } if method == "inocode/stats/seedProgress" => {
            let (ratio, text, done) = seed_progress(params);
            st.set_seed_ratio(ratio);
            st.set_seed_text(text.into());
            if done {
                st.set_seeding(false);
                refresh(app, host);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract").join(name);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        v["response"].clone()
    }

    #[test]
    fn numbers_are_short_and_readable() {
        assert_eq!(tokens_label(950.0), "950");
        assert_eq!(tokens_label(1200.0), "1.2K");
        assert_eq!(tokens_label(80_000.0), "80K");
        assert_eq!(tokens_label(1_200_000.0), "1.2M");
        assert_eq!(tokens_label(2_000_000.0), "2M");
        assert_eq!(pct_label(18.43), "18.4%");
        assert_eq!(pct_label(0.05), "0.05%");
        assert_eq!(pct_label(0.0), "0.0%");
    }

    #[test]
    fn model_rows_say_when_data_is_short() {
        let rows = model_rows(&fixture("stats-summary.json"), "gpt-5.6-sol");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "gpt-5.6-luna");
        assert_eq!((rows[0].uncached.as_str(), rows[0].output.as_str()), ("1.2M", "80K"));
        assert_eq!((rows[0].q5.as_str(), rows[0].qw.as_str(), rows[0].speed.as_str()), ("12.0%", "3.0%", "62"));
        assert!(rows[0].per_million.contains("22.0% / 5.5%"));
        assert!(!rows[0].selected);
        assert!(rows[1].selected);
        assert!(rows[1].per_million.contains("データが足りない"));
        assert!(model_rows(&json!({}), "").is_empty());
        assert_eq!(model_names(&fixture("stats-summary.json")).len(), 2);
        // 窓ごとに値が無いときは「—」
        let m = json!({ "model": "m", "quotaPerMillion": { "uncachedInput": { "primaryPercent": 2.0, "secondaryPercent": null } }, "insufficient": false });
        assert!(per_million_label(&m).contains("入力 2.0% / —"));
    }

    #[test]
    fn hourly_bars_fade_with_few_samples() {
        let bars = hour_bars(&fixture("stats-hourly.json"));
        assert_eq!(bars.len(), 24);
        // 件数 0 の時間は棒なし
        assert_eq!(bars[0].ratio, 0.0);
        assert_eq!(bars[0].top, "");
        // 件数が少ない(2 件)時間は薄い、多い時間は濃い
        assert!(bars[1].faint && bars[1].ratio > 0.0);
        assert!(!bars[3].faint);
        assert!(bars.iter().any(|b| (b.ratio - 1.0).abs() < 1e-6));
        assert_eq!(bars[6].label, "6時");
        assert_eq!(bars[7].label, "");
        assert_eq!(bars[5].ratio, 0.0);
    }

    #[test]
    fn daily_bars_show_date_and_speed() {
        let bars = day_bars(&fixture("stats-daily.json"));
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].label, "11-11");
        assert!(bars[0].sub.ends_with("tok/s"));
        assert!(bars.iter().any(|b| (b.ratio - 1.0).abs() < 1e-6));
    }

    #[test]
    fn estimate_input_and_result() {
        let p = estimate_params("gpt-5.6-luna", 3, "200,000", " 800000", "").unwrap();
        assert_eq!(p["tokens"], json!({ "uncachedInput": 200000, "cachedInput": 800000, "output": 0 }));
        assert_eq!(p["days"], 3);
        assert!(estimate_params("", 3, "1", "", "").is_err());
        assert!(estimate_params("m", 3, "", "", "").is_err());
        assert!(estimate_params("m", 3, "abc", "", "").is_err());
        assert!(estimate_params("m", 3, "-5", "", "").is_err());
        assert_eq!(estimate_label(&fixture("stats-estimate.json")), "5時間枠 約 1.4%、週枠 約 0.3%、約 45 秒");
        assert_eq!(estimate_label(&json!({ "insufficient": true, "primaryPercent": null })), "データが足りない");
        assert_eq!(seconds_label(45.0), "約 45 秒");
    }

    #[test]
    fn seed_progress_reads_the_notification() {
        let (r, t, done) = seed_progress(&fixture("stats-seedProgress.json"));
        assert!((r - 120.0 / 340.0).abs() < 1e-6);
        assert_eq!(t, "120 / 340 ファイル · 2210 件");
        assert!(!done);
        let (r, t, done) = seed_progress(&json!({ "done": true, "samplesAdded": 5000, "filesDone": 340, "filesTotal": 340 }));
        assert_eq!((r, t.as_str(), done), (1.0, "取り込み完了 · 5000 件", true));
    }
}
