//! GUI-only preferences kept next to the version-discovery settings: the colour scheme and the
//! configurable shortcuts (palette, model menu).

use crate::*;
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shortcut {
    pub key: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Shortcut {
    pub(crate) fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        parts.push(self.key.to_uppercase());
        parts.join("+")
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Prefs {
    /// 0 follow the system, 1 light, 2 dark
    pub scheme: i32,
    pub sidebar_open: bool,
    pub palette: Shortcut,
    pub model: Shortcut,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            scheme: 0,
            sidebar_open: true,
            palette: Shortcut { key: "k".into(), ctrl: true, shift: false, alt: false },
            model: Shortcut { key: "m".into(), ctrl: true, shift: true, alt: false },
        }
    }
}

fn path() -> PathBuf {
    let dir = if let Some(p) = std::env::var_os("INOCODE_DESKTOP_DATA") {
        PathBuf::from(p)
    } else if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(p).join("inocode-desktop")
    } else {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local").join("share").join("inocode-desktop")
    };
    dir.join("gui.json")
}

fn shortcut_from(v: &Value, default: Shortcut) -> Shortcut {
    let Some(key) = v.get("key").and_then(Value::as_str).filter(|k| !k.is_empty()) else { return default };
    Shortcut {
        key: key.to_string(),
        ctrl: v.get("ctrl").and_then(Value::as_bool).unwrap_or(false),
        shift: v.get("shift").and_then(Value::as_bool).unwrap_or(false),
        alt: v.get("alt").and_then(Value::as_bool).unwrap_or(false),
    }
}

pub(crate) fn load() -> Prefs {
    let d = Prefs::default();
    let Ok(text) = std::fs::read_to_string(path()) else { return d };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return d };
    Prefs {
        sidebar_open: v.get("sidebarOpen").and_then(Value::as_bool).unwrap_or(true),
        scheme: v.get("scheme").and_then(Value::as_i64).map(|n| (n as i32).clamp(0, 2)).unwrap_or(0),
        palette: v.get("palette").map(|s| shortcut_from(s, d.palette.clone())).unwrap_or(d.palette),
        model: v.get("model").map(|s| shortcut_from(s, d.model.clone())).unwrap_or(d.model),
    }
}

/// The model chosen last: (id, provider).
pub(crate) fn load_last_model() -> Option<(String, String)> {
    let text = std::fs::read_to_string(path()).ok()?;
    let v = serde_json::from_str::<Value>(&text).ok()?;
    let id = v.get("lastModel").and_then(Value::as_str).filter(|c| !c.is_empty())?.to_string();
    let provider = v.get("lastModelProvider").and_then(Value::as_str).filter(|c| !c.is_empty()).unwrap_or("openai").to_string();
    Some((id, provider))
}

pub(crate) fn save_last_model(id: &str, provider: &str) {
    let path = path();
    let mut v = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or_else(|| json!({}));
    v["lastModel"] = json!(id);
    v["lastModelProvider"] = json!(provider);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, v.to_string());
}

/// The working folder that was used last (where a new chat starts after a restart).
pub(crate) fn load_last_cwd() -> Option<String> {
    let text = std::fs::read_to_string(path()).ok()?;
    let v = serde_json::from_str::<Value>(&text).ok()?;
    v.get("lastCwd").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string)
}

pub(crate) fn save_last_cwd(cwd: &str) {
    let path = path();
    let mut v = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or_else(|| json!({}));
    v["lastCwd"] = json!(cwd);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, v.to_string());
}

pub(crate) fn save(p: &Prefs) {
    let s = |x: &Shortcut| json!({ "key": x.key, "ctrl": x.ctrl, "shift": x.shift, "alt": x.alt });
    let mut v = json!({ "scheme": p.scheme, "sidebarOpen": p.sidebar_open, "palette": s(&p.palette), "model": s(&p.model) });
    if let Some(c) = load_last_cwd() {
        v["lastCwd"] = json!(c);
    }
    if let Some((m, p)) = load_last_model() {
        v["lastModel"] = json!(m);
        v["lastModelProvider"] = json!(p);
    }
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, v.to_string());
}

/// Push the preferences into the window's globals.
pub(crate) fn apply(app: &AppWindow, p: &Prefs) {
    app.global::<Theme>().set_scheme(p.scheme);
    app.set_sidebar_open(p.sidebar_open);
    let s = app.global::<ShortcutState>();
    s.set_palette_key(p.palette.key.as_str().into());
    s.set_palette_ctrl(p.palette.ctrl);
    s.set_palette_shift(p.palette.shift);
    s.set_palette_alt(p.palette.alt);
    s.set_model_key(p.model.key.as_str().into());
    s.set_model_ctrl(p.model.ctrl);
    s.set_model_shift(p.model.shift);
    s.set_model_alt(p.model.alt);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_label() {
        let p = Prefs::default();
        assert_eq!(p.palette.label(), "Ctrl+K");
        assert_eq!(p.model.label(), "Ctrl+Shift+M");
    }
}
