//! Model discovery across client versions.
//!
//! The backend filters the model catalog by the client version it is told about. The main
//! app-server presents the installed official Codex's version, which can lag behind newer
//! releases. This module probes a few versions above it, each with a short-lived stdio
//! app-server that reads `model/list` and exits, and reports models that only show up at a
//! higher version. They are offered as "unverified": never the default, and flagged when chosen.
//!
//! Results are cached for 24 hours next to the other per-user data of the desktop app.

use crate::client::{self, ModelInfo};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Newest minor release known to exist: the official desktop client's feature table names
/// 0.164 builds. Probing below this floor is never cut short.
pub const KNOWN_REAL_MINOR: u32 = 164;
/// How many minors above the newest known release are probed (user setting, 0..=MAX_HEADROOM).
pub const DEFAULT_HEADROOM: u32 = 4;
pub const MAX_HEADROOM: u32 = 10;
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
/// Minors between probes on the way up to the floor.
const STRIDE: u32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub enabled: bool,
    pub headroom: u32,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { enabled: true, headroom: DEFAULT_HEADROOM }
    }
}

/// What one discovery run found.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub base: String,
    pub headroom: u32,
    /// Unix seconds when the run finished.
    pub at: u64,
    /// (version, model ids) for every probed step, base first.
    pub steps: Vec<(String, Vec<String>)>,
    /// Models that the base version does not list: (raw model/list object, lowest listing version).
    pub extra: Vec<(Value, String)>,
}

impl Outcome {
    /// Extra models as picker entries, flagged with the version they need.
    pub fn extra_models(&self) -> Vec<ModelInfo> {
        let mut out = Vec::new();
        for (raw, min_version) in &self.extra {
            for mut m in client::parse_models(&json!({ "data": [raw] })) {
                m.min_version = Some(min_version.clone());
                out.push(m);
            }
        }
        out
    }

    /// One line per probed version: "0.164.0: 11 件".
    pub fn summary(&self) -> String {
        self.steps.iter().map(|(v, ids)| format!("{v}: {} 件", ids.len())).collect::<Vec<_>>().join(" / ")
    }
}

// ------------------------------------------------------------------ versions

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

fn minor_version(base: (u32, u32, u32), minor: u32) -> String {
    format!("{}.{}.0", base.0, minor)
}

/// Versions to probe, base first. Up to the newest known release in strides, then one minor
/// at a time for `headroom` minors above it.
pub fn plan_steps(base: &str, headroom: u32) -> Vec<String> {
    let Some(b) = parse_version(base) else { return vec![base.to_string()] };
    let floor = b.1.max(KNOWN_REAL_MINOR);
    let mut steps = vec![base.to_string()];
    let mut m = b.1 + STRIDE;
    while m < floor {
        steps.push(minor_version(b, m));
        m += STRIDE;
    }
    if floor > b.1 {
        steps.push(minor_version(b, floor));
    }
    for extra in 1..=headroom.min(MAX_HEADROOM) {
        steps.push(minor_version(b, floor + extra));
    }
    steps
}

/// Highest version the run may probe, for display.
pub fn limit_version(base: &str, headroom: u32) -> String {
    match parse_version(base) {
        Some(b) => minor_version(b, b.1.max(KNOWN_REAL_MINOR) + headroom.min(MAX_HEADROOM)),
        None => format!("0.{}.0", KNOWN_REAL_MINOR + headroom.min(MAX_HEADROOM)),
    }
}

// ------------------------------------------------------------------ run

fn ids_of(list: &[Value]) -> Vec<String> {
    list.iter()
        .filter_map(|m| m.get("model").or_else(|| m.get("id")).and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn set(ids: &[String]) -> BTreeSet<&str> {
    ids.iter().map(String::as_str).collect()
}

/// Probe the planned versions. Stops once the model set stops changing above the newest known
/// release, then narrows down the lowest version that lists each new model.
pub fn run(program: &str, base: &str, headroom: u32) -> Result<Outcome, String> {
    let plan = plan_steps(base, headroom);
    let b = parse_version(base);
    let floor = b.map(|b| b.1.max(KNOWN_REAL_MINOR)).unwrap_or(KNOWN_REAL_MINOR);

    let mut probed: Vec<(String, Vec<Value>)> = Vec::new();
    for (i, version) in plan.iter().enumerate() {
        match client::probe_models(program, version) {
            Ok(list) => {
                let minor = parse_version(version).map(|v| v.1).unwrap_or(0);
                let unchanged = probed.last().map(|(_, prev)| set(&ids_of(prev)) == set(&ids_of(&list))).unwrap_or(false);
                probed.push((version.clone(), list));
                // Above the newest known release an unchanged set means nothing more is coming.
                if i > 0 && minor > floor && unchanged {
                    break;
                }
            }
            Err(e) if i == 0 => return Err(e),
            Err(_) => break,
        }
    }
    let base_ids: Vec<String> = ids_of(&probed[0].1);

    // Narrow down: the first step that lists a new model may sit up to STRIDE-1 minors above
    // the previous probe; look in between so the shown "required version" is exact.
    let mut first_seen: Vec<(String, String)> = Vec::new(); // (model id, version)
    for idx in 1..probed.len() {
        let prev_ids = ids_of(&probed[idx - 1].1);
        let ids = ids_of(&probed[idx].1);
        let new: Vec<&String> = ids.iter().filter(|id| !base_ids.contains(id) && !prev_ids.contains(id)).collect();
        if new.is_empty() {
            continue;
        }
        let mut found_at = probed[idx].0.clone();
        if let (Some(lo), Some(hi), Some(bv)) =
            (parse_version(&probed[idx - 1].0), parse_version(&probed[idx].0), b)
        {
            for m in (lo.1 + 1)..hi.1 {
                let v = minor_version(bv, m);
                if let Ok(list) = client::probe_models(program, &v) {
                    let here = ids_of(&list);
                    if new.iter().any(|id| here.contains(id)) {
                        found_at = v;
                        break;
                    }
                }
            }
        }
        for id in new {
            first_seen.push((id.clone(), found_at.clone()));
        }
    }

    // Raw objects come from the highest probed version that lists the model.
    let mut extra = Vec::new();
    for (id, version) in first_seen {
        let raw = probed
            .iter()
            .rev()
            .find_map(|(_, list)| list.iter().find(|m| ids_of(std::slice::from_ref(m)).contains(&id)))
            .cloned();
        if let Some(raw) = raw {
            extra.push((raw, version));
        }
    }

    Ok(Outcome {
        base: base.to_string(),
        headroom,
        at: now(),
        steps: probed.iter().map(|(v, l)| (v.clone(), ids_of(l))).collect(),
        extra,
    })
}

/// Run discovery on a background thread and hand the result to `done`.
pub fn start(program: String, base: String, headroom: u32, done: impl FnOnce(Result<Outcome, String>) + Send + 'static) {
    std::thread::spawn(move || {
        let result = run(&program, &base, headroom);
        if let Ok(outcome) = &result {
            let _ = save_cache(outcome);
        }
        done(result);
    });
}

// ------------------------------------------------------------------ storage

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("INOCODE_DESKTOP_DATA") {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(p).join("inocode-desktop");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local").join("share").join("inocode-desktop")
}

pub fn load_prefs() -> Prefs {
    let read = || -> Option<Prefs> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(data_dir().join("settings.json")).ok()?).ok()?;
        Some(Prefs {
            enabled: v.get("discoveryEnabled").and_then(Value::as_bool).unwrap_or(true),
            headroom: v
                .get("discoveryHeadroom")
                .and_then(Value::as_u64)
                .map(|n| (n as u32).min(MAX_HEADROOM))
                .unwrap_or(DEFAULT_HEADROOM),
        })
    };
    read().unwrap_or_default()
}

pub fn save_prefs(p: &Prefs) -> std::io::Result<()> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let v = json!({ "discoveryEnabled": p.enabled, "discoveryHeadroom": p.headroom });
    std::fs::write(dir.join("settings.json"), v.to_string())
}

fn save_cache(o: &Outcome) -> std::io::Result<()> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let v = json!({
        "base": o.base,
        "headroom": o.headroom,
        "at": o.at,
        "steps": o.steps.iter().map(|(v, ids)| json!([v, ids])).collect::<Vec<_>>(),
        "extra": o.extra.iter().map(|(m, v)| json!({ "minVersion": v, "model": m })).collect::<Vec<_>>(),
    });
    std::fs::write(dir.join("discovery.json"), v.to_string())
}

pub fn load_cache() -> Option<Outcome> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(data_dir().join("discovery.json")).ok()?).ok()?;
    Some(Outcome {
        base: v.get("base")?.as_str()?.to_string(),
        headroom: v.get("headroom")?.as_u64()? as u32,
        at: v.get("at")?.as_u64()?,
        steps: v
            .get("steps")?
            .as_array()?
            .iter()
            .filter_map(|s| {
                let a = s.as_array()?;
                let ids = a.get(1)?.as_array()?.iter().filter_map(Value::as_str).map(str::to_string).collect();
                Some((a.first()?.as_str()?.to_string(), ids))
            })
            .collect(),
        extra: v
            .get("extra")?
            .as_array()?
            .iter()
            .filter_map(|e| Some((e.get("model")?.clone(), e.get("minVersion")?.as_str()?.to_string())))
            .collect(),
    })
}

/// A cached run is reused for 24 hours while the base version and the setting are unchanged.
pub fn is_fresh(o: &Outcome, base: &str, headroom: u32) -> bool {
    o.base == base && o.headroom == headroom && now().saturating_sub(o.at) < CACHE_TTL_SECS
}

/// "2026-11-10 14:05 UTC" for the settings dialog.
pub fn format_time(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil-from-days (proleptic Gregorian)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, (rem % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_go_up_to_the_known_release_then_headroom() {
        let s = plan_steps("0.155.0", 4);
        assert_eq!(
            s,
            ["0.155.0", "0.158.0", "0.161.0", "0.164.0", "0.165.0", "0.166.0", "0.167.0", "0.168.0"]
        );
        // A base at or above the floor only gets headroom above itself.
        assert_eq!(plan_steps("0.170.0", 2), ["0.170.0", "0.171.0", "0.172.0"]);
        assert_eq!(plan_steps("0.164.0", 0), ["0.164.0"]);
    }

    /// Live run against the real backend (no model call). `cargo test discovery_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn discovery_live() {
        let program = client::find_inocode("").expect("inocode.exe");
        let base = client::base_client_version().expect("official codex version");
        let started = std::time::Instant::now();
        let o = run(program.to_str().unwrap(), &base, DEFAULT_HEADROOM).unwrap();
        println!("took {:?}", started.elapsed());
        println!("steps: {}", o.summary());
        for (m, v) in &o.extra {
            println!("extra: {} needs {v}", m.get("model").and_then(Value::as_str).unwrap_or("?"));
        }
    }

    #[test]
    fn time_format() {
        assert_eq!(format_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_time(1_794_000_000), "2026-11-06 21:20 UTC");
    }
}
