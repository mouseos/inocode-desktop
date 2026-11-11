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
    refresh_providers();
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: "model.config".into(), method: "config/read".into(), params: json!({ "includeLayers": false }) });
    }
}

/// Ask the lists of the providers that have an account (the models of one provider are only on
/// its own list): the accounts first, then model/list for each.
pub(crate) fn refresh_providers() {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: "model.accounts".into(), method: "inocode/accounts/list".into(), params: json!({ "refresh": false }) });
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if tag == "model.accounts" {
        let Ok(v) = result else { return };
        let mut providers: Vec<String> = Vec::new();
        for a in v.get("accounts").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
            let p = a.get("provider").and_then(Value::as_str).filter(|p| !p.is_empty()).unwrap_or("openai");
            if !providers.iter().any(|x| x == p) {
                providers.push(p.to_string());
            }
        }
        // A provider without an account has no section.
        STATE.with(|s| s.borrow_mut().provider_models.retain(|k, _| providers.contains(k)));
        if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
            for p in &providers {
                h.send(Command::Rpc {
                    tag: format!("model.list:{p}"),
                    method: "model/list".into(),
                    params: json!({ "modelProvider": p, "includeHidden": true, "cursor": null, "limit": 100 }),
                });
            }
        }
        STATE.with(|s| refresh_pickers(app, &s.borrow()));
        return;
    }
    if let Some(p) = tag.strip_prefix("model.list:") {
        let Ok(v) = result else { return };
        let mut list = client::parse_models(v);
        // The list of a provider holds that provider's models, whatever each entry says.
        for m in &mut list {
            m.provider = p.to_string();
        }
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.provider_models.insert(p.to_string(), list);
            refresh_pickers(app, &st);
        });
        apply(app);
        return;
    }
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
            let switch = provider_changed(&st, &provider);
            st.settings.model = Some(id);
            st.settings.model_provider = Some(provider);
            if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
                h.send(Command::SetSettings(st.settings.clone()));
                if switch {
                    h.send(Command::NewThread);
                }
            }
            refresh_pickers(app, &st);
        }
    });
}

/// The provider a model of the picker runs with, and whether the open thread has to be left for it:
/// a thread keeps its provider, so another provider's model starts a new thread.
pub(crate) fn provider_changed(st: &State, provider: &str) -> bool {
    st.thread_provider.as_deref().unwrap_or("openai") != provider
}
