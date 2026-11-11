//! Mock mode: with INOCODE_GUI_MOCK=1, `inocode/*` requests are answered from the JSON files in
//! `ref/contract-fixtures/` (or the folder in INOCODE_GUI_FIXTURES) instead of the server.
//!
//! A fixture for `inocode/knowledge/list` is looked up as, in this order (first match wins):
//! `inocode/knowledge/list.json`, `inocode.knowledge.list.json`, `inocode_knowledge_list.json`,
//! `inocode-knowledge-list.json`, and the same names without the `inocode` prefix.
//! The file holds the `result` object of the response (a top-level `{"result": …}` is unwrapped).

use super::Event;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn enabled() -> bool {
    std::env::var("INOCODE_GUI_MOCK").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

/// Answers `inocode/*` calls in mock mode. Returns false when the request is not mocked.
pub fn mock_rpc(on_event: &impl Fn(Event), tag: &str, method: &str, params: &Value) -> bool {
    if !enabled() || !method.starts_with("inocode/") {
        return false;
    }
    // A request for another provider uses the fixture with that provider as a suffix when there is one
    // (accounts-login-start-claude.json).
    let variant = params.get("provider").and_then(Value::as_str).filter(|p| *p != "openai");
    let result = variant.and_then(|v| fixture_named(method, Some(v))).unwrap_or_else(|| fixture(method));
    on_event(Event::Rpc { tag: tag.to_string(), result });
    true
}

fn fixture_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("INOCODE_GUI_FIXTURES") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let mut starts: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        starts.extend(exe.parent().map(Path::to_path_buf));
    }
    starts.extend(std::env::current_dir().ok());
    for start in starts {
        for dir in start.ancestors() {
            for rel in [["ref", "contract-fixtures"], ["tests/fixtures", "contract"]] {
                let candidate = dir.join(rel[0]).join(rel[1]);
                if candidate.is_dir() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn candidates(method: &str, suffix: Option<&str>) -> Vec<PathBuf> {
    let tail = suffix.map(|s| format!("-{s}")).unwrap_or_default();
    let bare = method.strip_prefix("inocode/").unwrap_or(method);
    let mut names = Vec::new();
    for m in [method, bare] {
        names.push(PathBuf::from(format!("{m}{tail}.json")));
        for sep in [".", "_", "-"] {
            names.push(PathBuf::from(format!("{}{tail}.json", m.replace('/', sep))));
        }
    }
    names
}

fn fixture(method: &str) -> Result<Value, String> {
    match fixture_named(method, None) {
        Some(r) => r,
        None => Err(format!("モック: {method} のフィクスチャがありません")),
    }
}

/// The fixture of `method` (with the provider suffix when given); None when there is no such file.
fn fixture_named(method: &str, suffix: Option<&str>) -> Option<Result<Value, String>> {
    let dir = fixture_dir()?;
    for name in candidates(method, suffix) {
        let path = dir.join(&name);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let value: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => return Some(Err(format!("モック: {} を解析できません: {e}", path.display()))),
            };
            return Some(Ok(match value {
                // ref/contract-fixtures files are {method, request, response, note}: the response is the result.
                Value::Object(mut o) if o.contains_key("response") && (o.contains_key("method") || o.contains_key("request")) => {
                    o.remove("response").unwrap()
                }
                Value::Object(mut o) if o.len() == 1 && o.contains_key("result") => o.remove("result").unwrap(),
                other => other,
            }));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_names_include_area_dash_name() {
        let names: Vec<String> = candidates("inocode/knowledge/list", None).iter().map(|p| p.display().to_string()).collect();
        assert!(names.contains(&"knowledge-list.json".to_string()));
    }

    #[test]
    fn reads_repo_fixtures() {
        if let Some(dir) = fixture_dir() {
            if dir.join("knowledge-list.json").exists() {
                assert!(fixture("inocode/knowledge/list").is_ok());
            }
        }
    }
}
