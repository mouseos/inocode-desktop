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
}

/// Order of the permission dropdown rows ("Managed" is appended when requirements restrict).
pub(crate) const PERM_ORDER: [Perm; 4] = [Perm::Ask, Perm::Guardian, Perm::Full, Perm::Server];

pub(crate) fn strings(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    let v: Vec<SharedString> = items.into_iter().map(SharedString::from).collect();
    ModelRc::from(Rc::new(VecModel::from(v)))
}

pub(crate) fn row(label: &str, desc: &str, selected: bool, disabled: bool) -> MenuRow {
    MenuRow { label: label.into(), desc: desc.into(), selected, disabled, locked: false }
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
    st.models.iter().chain(extra).filter(|m| !m.hidden).collect()
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Normal,
    Fast,
    Daybreak,
}

/// The picker's rows narrowed by the search box. A model that offers Fast and/or Daybreak
/// gets "{name} Fast" and "{name} Daybreak" rows right under its own; each row picks one mode.
pub(crate) fn shown_models(st: &State) -> Vec<(&ModelInfo, Kind)> {
    let q = st.model_filter.trim().to_lowercase();
    let mut rows = Vec::new();
    for m in visible_models(st) {
        if !(q.is_empty() || m.name.to_lowercase().contains(&q) || m.id.to_lowercase().contains(&q)) {
            continue;
        }
        rows.push((m, Kind::Normal));
        if m.fast_tier().is_some() {
            rows.push((m, Kind::Fast));
        }
        if m.has_daybreak_variant() {
            rows.push((m, Kind::Daybreak));
        }
    }
    rows
}

/// Selected model: the user's choice, else what the server uses, else the catalog default,
/// else the first visible model.
pub(crate) fn selected_model<'a>(st: &'a State, visible: &[&'a ModelInfo]) -> Option<&'a ModelInfo> {
    let by_id = |id: &str| visible.iter().copied().find(|m| m.id == id);
    st.settings
        .model
        .as_deref()
        .and_then(by_id)
        .or_else(|| by_id(&st.applied_model))
        // Unverified models are never the automatic choice.
        .or_else(|| visible.iter().copied().find(|m| m.is_default && m.min_version.is_none()))
        .or_else(|| visible.iter().copied().find(|m| m.min_version.is_none()))
        .or_else(|| visible.first().copied())
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

