//! The model a new chat starts with: what was chosen last (gui.json), else the config.toml's
//! `model`; never the server's own default. When neither is on the model list nothing is
//! chosen, and nothing is sent until the user picks one.

use crate::state::*;
use crate::view::refresh_pickers;
use crate::*;
use serde_json::{json, Value};

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
}

pub(crate) fn register(handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
}

/// The model list arrived: choose, and read the config.toml's model for what is still open.
pub(crate) fn on_models(app: &AppWindow) {
    apply(app);
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: "model.config".into(), method: "config/read".into(), params: json!({ "includeLayers": false }) });
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if tag != "model.config" {
        return;
    }
    let Ok(v) = result else { return };
    let cfg = v.get("config").unwrap_or(v);
    let id = cfg.get("model").and_then(Value::as_str).filter(|m| !m.is_empty());
    let provider = cfg.get("model_provider").and_then(Value::as_str).filter(|p| !p.is_empty()).map(str::to_string);
    STATE.with(|s| s.borrow_mut().config_model = id.map(|m| (m.to_string(), provider)));
    apply(app);
}

/// Tell the client what is chosen now.
pub(crate) fn push(st: &State) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::SetSettings(st.settings.clone()));
    }
}

/// Choose the first model of a thread that has none yet.
fn apply(app: &AppWindow) {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        if st.settings.model.is_some() || st.model_locked {
            return;
        }
        if let Some((id, provider)) = initial_model(&st) {
            st.settings.model = Some(id);
            st.settings.model_provider = Some(provider);
            if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
                h.send(Command::SetSettings(st.settings.clone()));
            }
            refresh_pickers(app, &st);
        }
    });
}
