//! UI-side state that is not part of the Slint models, and helpers over it.

use crate::view::refresh_pickers;
use crate::*;

/// UI-side state that is not part of the Slint models.
#[derive(Default)]
pub(crate) struct State {
    pub(crate) threads: Vec<ThreadInfo>,
    pub(crate) active: String,
    pub(crate) active_cwd: String,
    /// Current git branch of `active_cwd` (empty when not a repository).
    pub(crate) branch: String,
    pub(crate) busy: bool,
    pub(crate) models: Vec<ModelInfo>,
    pub(crate) models_loaded: bool,
    pub(crate) settings: Settings,
    /// Approvals that wait, with the thread each belongs to (a thread switch keeps them).
    pub(crate) pending: Vec<(String, Approval)>,
    /// What the server reported for the active thread.
    pub(crate) applied_model: String,
    pub(crate) applied_effort: String,
    pub(crate) applied_perm: Option<Perm>,
    /// configRequirements/read: allowed approval policies / sandbox modes.
    pub(crate) allowed_policies: Option<Vec<String>>,
    pub(crate) allowed_sandboxes: Option<Vec<String>>,
    /// Project folders offered by the project picker, in row order.
    pub(crate) project_cwds: Vec<String>,
    /// Models found only by probing newer client versions ("unverified").
    pub(crate) extra: Vec<ModelInfo>,
    pub(crate) prefs: Prefs,
    pub(crate) outcome: Option<Outcome>,
    pub(crate) discovery_running: bool,
    pub(crate) discovery_started: bool,
    /// Text of the model search box.
    pub(crate) model_filter: String,
    /// Providers whose section of the model list the user opened (true) or closed (false).
    pub(crate) provider_open: std::collections::HashMap<String, bool>,
    /// The models of each provider that has an account (model/list { modelProvider }).
    pub(crate) provider_models: std::collections::HashMap<String, Vec<ModelInfo>>,
    /// The provider the open thread runs with (a thread keeps it): None until known.
    pub(crate) thread_provider: Option<String>,
    /// A thread was just opened: the model it reports is the one to show (and use).
    pub(crate) sync_model: bool,
    /// The open thread is an existing one: what it has recorded is the choice, even none.
    pub(crate) model_locked: bool,
    /// The model the config.toml names (`model`, `model_provider`), once read.
    pub(crate) config_model: Option<(String, Option<String>)>,
}

/// Order of the permission dropdown rows ("Managed" is appended when requirements restrict).
pub(crate) const PERM_ORDER: [Perm; 4] = [Perm::Ask, Perm::Guardian, Perm::Full, Perm::Server];

pub(crate) fn strings(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    let v: Vec<SharedString> = items.into_iter().map(SharedString::from).collect();
    ModelRc::from(Rc::new(VecModel::from(v)))
}

pub(crate) fn row(label: &str, desc: &str, selected: bool, disabled: bool) -> MenuRow {
    MenuRow { label: label.into(), desc: desc.into(), selected, disabled, locked: false, heading: false, expanded: false }
}

pub(crate) fn rows(items: Vec<MenuRow>) -> ModelRc<MenuRow> {
    ModelRc::from(Rc::new(VecModel::from(items)))
}

/// Official reasoning labels (composer.mode.local.reasoning.*), Japanese.
pub(crate) fn effort_label(e: &str) -> String {
    match e {
        "none" => "なし",
        "minimal" => "最小",
        "low" => "軽",
        "medium" => "中",
        "high" => "高",
        "xhigh" => "極高",
        "max" => "マックス",
        "ultra" => "ウルトラ",
        "persistent" => "パーシステント",
        other => other,
    }
    .to_string()
}

/// Models offered in the picker: the ones the server lists as visible (not hidden).
pub(crate) fn visible_models(st: &State) -> Vec<&ModelInfo> {
    let extra = st.extra.iter().filter(|e| !st.models.iter().any(|m| m.id == e.id));
    let mut all: Vec<&ModelInfo> = Vec::new();
    // The lists asked per provider first, then what the server lists on its own (the default
    // provider's); a model is in once.
    let mut keys: Vec<&String> = st.provider_models.keys().collect();
    keys.sort();
    for k in keys {
        all.extend(st.provider_models[k].iter());
    }
    for m in st.models.iter().chain(extra) {
        if !all.iter().any(|x| x.id == m.id && x.provider == m.provider) {
            all.push(m);
        }
    }
    all.retain(|m| !m.hidden);
    all
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Normal,
    Fast,
    Daybreak,
}

/// A row of the model list: the heading of a provider (open or closed), or a model in one mode.
pub(crate) enum Item<'a> {
    Heading(String, bool),
    Model(&'a ModelInfo, Kind),
}

/// The model the picker shows as chosen. Only what was chosen, or what the thread recorded;
/// never a default of the server and never another model when that one is gone.
pub(crate) fn selected_model<'a>(st: &'a State, visible: &[&'a ModelInfo]) -> Option<&'a ModelInfo> {
    let find = |id: &str, provider: Option<&str>| {
        visible.iter().copied().find(|m| m.id == id && provider.map(|p| m.provider == p).unwrap_or(true))
    };
    if let Some(id) = st.settings.model.as_deref() {
        return find(id, st.settings.model_provider.as_deref());
    }
    None
}

/// The picker's rows narrowed by the search box, one section per provider: a heading and under
/// it the models. A model that offers Fast and/or Daybreak gets "{name} Fast" and "{name} Daybreak"
/// rows right under its own; each row picks one mode. The section of the chosen model is open
/// unless the user closed it; the others are closed unless the user opened them; a search opens
/// every section that has a hit.
pub(crate) fn shown_models(st: &State) -> Vec<Item<'_>> {
    let q = st.model_filter.trim().to_lowercase();
    let visible = visible_models(st);
    let chosen = selected_model(st, &visible).map(|m| m.provider.clone());
    let mut providers: Vec<String> = Vec::new();
    for m in &visible {
        if !providers.contains(&m.provider) {
            providers.push(m.provider.clone());
        }
    }
    let mut rows = Vec::new();
    for p in providers {
        let hits: Vec<&&ModelInfo> = visible
            .iter()
            .filter(|m| m.provider == p)
            .filter(|m| q.is_empty() || m.name.to_lowercase().contains(&q) || m.id.to_lowercase().contains(&q))
            .collect();
        if hits.is_empty() {
            continue;
        }
        let open = if !q.is_empty() { true } else { st.provider_open.get(&p).copied().unwrap_or(chosen.as_deref() == Some(p.as_str())) };
        rows.push(Item::Heading(p.clone(), open));
        if !open {
            continue;
        }
        for m in hits {
            rows.push(Item::Model(m, Kind::Normal));
            if m.fast_tier().is_some() {
                rows.push(Item::Model(m, Kind::Fast));
            }
            if m.has_daybreak_variant() {
                rows.push(Item::Model(m, Kind::Daybreak));
            }
        }
    }
    rows
}

/// The model the next thread starts with when the user has not picked one in this run: what was
/// chosen last, then the config.toml's `model`; none when neither is on the list.
pub(crate) fn initial_model(st: &State) -> Option<(String, String)> {
    let visible = visible_models(st);
    let has = |id: &str, p: Option<&str>| visible.iter().find(|m| m.id == id && p.map(|p| m.provider == p).unwrap_or(true)).map(|m| (m.id.clone(), m.provider.clone()));
    if let Some((id, p)) = crate::gui_prefs::load_last_model() {
        if let Some(found) = has(&id, Some(&p)) {
            return Some(found);
        }
    }
    let (id, p) = st.config_model.clone()?;
    has(&id, p.as_deref())
}

pub(crate) fn current_perm(st: &State) -> Perm {
    st.settings.perm.or(st.applied_perm).unwrap_or(Perm::Ask)
}

pub(crate) fn perm_allowed(st: &State, perm: Perm) -> bool {
    let ok = |list: &Option<Vec<String>>, v: &str| list.as_ref().map(|l| l.iter().any(|x| x == v)).unwrap_or(true);
    match perm {
        Perm::Ask | Perm::Guardian => ok(&st.allowed_policies, "on-request") && ok(&st.allowed_sandboxes, "workspace-write"),
        Perm::Full => ok(&st.allowed_policies, "never") && ok(&st.allowed_sandboxes, "danger-full-access"),
        Perm::Server => true,
    }
}

thread_local! {
    pub(crate) static TAB_HOST: RefCell<Option<tabs::Host>> = const { RefCell::new(None) };
    pub(crate) static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Run `f` on the shared state, push the changed settings to the client, and redraw the pickers.
pub(crate) fn change(app: &AppWindow, h: &client::Handle, f: impl FnOnce(&mut State)) {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        f(&mut st);
        h.send(Command::SetSettings(st.settings.clone()));
        refresh_pickers(app, &st);
    });
}

