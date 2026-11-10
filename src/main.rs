#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod client;
mod discovery;

use client::{Command, Config, Decision, Event, Level, ModelInfo, Msg, Perm, Settings, ThreadInfo};
use discovery::{Outcome, Prefs};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

slint::include_modules!();

/// UI-side state that is not part of the Slint models.
#[derive(Default)]
struct State {
    threads: Vec<ThreadInfo>,
    active: String,
    active_cwd: String,
    busy: bool,
    models: Vec<ModelInfo>,
    models_loaded: bool,
    settings: Settings,
    /// What the server reported for the active thread.
    applied_model: String,
    applied_effort: String,
    applied_perm: Option<Perm>,
    /// configRequirements/read: allowed approval policies / sandbox modes.
    allowed_policies: Option<Vec<String>>,
    allowed_sandboxes: Option<Vec<String>>,
    /// Project folders offered by the project picker, in row order.
    project_cwds: Vec<String>,
    /// Models found only by probing newer client versions ("unverified").
    extra: Vec<ModelInfo>,
    prefs: Prefs,
    outcome: Option<Outcome>,
    discovery_running: bool,
    discovery_started: bool,
    /// Text of the model search box.
    model_filter: String,
}

/// Order of the permission dropdown rows ("Managed" is appended when requirements restrict).
const PERM_ORDER: [Perm; 4] = [Perm::Ask, Perm::Guardian, Perm::Full, Perm::Server];

fn strings(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    let v: Vec<SharedString> = items.into_iter().map(SharedString::from).collect();
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn row(label: &str, desc: &str, selected: bool, disabled: bool) -> MenuRow {
    MenuRow { label: label.into(), desc: desc.into(), selected, disabled, locked: false }
}

fn rows(items: Vec<MenuRow>) -> ModelRc<MenuRow> {
    ModelRc::from(Rc::new(VecModel::from(items)))
}

/// Official reasoning labels (composer.mode.local.reasoning.*), Japanese.
fn effort_label(e: &str) -> String {
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
fn visible_models(st: &State) -> Vec<&ModelInfo> {
    let extra = st.extra.iter().filter(|e| !st.models.iter().any(|m| m.id == e.id));
    st.models.iter().chain(extra).filter(|m| !m.hidden).collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Normal,
    Fast,
    Daybreak,
}

/// The picker's rows narrowed by the search box. A model that offers Fast and/or Daybreak
/// gets "{name} Fast" and "{name} Daybreak" rows right under its own; each row picks one mode.
fn shown_models(st: &State) -> Vec<(&ModelInfo, Kind)> {
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
fn selected_model<'a>(st: &'a State, visible: &[&'a ModelInfo]) -> Option<&'a ModelInfo> {
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

fn current_perm(st: &State) -> Perm {
    st.settings.perm.or(st.applied_perm).unwrap_or(Perm::Ask)
}

fn perm_allowed(st: &State, perm: Perm) -> bool {
    let ok = |list: &Option<Vec<String>>, v: &str| list.as_ref().map(|l| l.iter().any(|x| x == v)).unwrap_or(true);
    match perm {
        Perm::Ask | Perm::Guardian => ok(&st.allowed_policies, "on-request") && ok(&st.allowed_sandboxes, "workspace-write"),
        Perm::Full => ok(&st.allowed_policies, "never") && ok(&st.allowed_sandboxes, "danger-full-access"),
        Perm::Server => true,
    }
}

/// Rebuild what the model, permission and project pickers show.
fn refresh_pickers(app: &AppWindow, st: &State) {
    let visible = visible_models(st);
    let sel = selected_model(st, &visible);

    // Reasoning effort
    let efforts: Vec<String> = sel.map(|m| m.efforts.clone()).unwrap_or_default();
    let eff = st
        .settings
        .effort
        .clone()
        .filter(|e| efforts.contains(e))
        .or_else(|| Some(st.applied_effort.clone()).filter(|e| efforts.contains(e)))
        .or_else(|| sel.map(|m| m.default_effort.clone()));
    let ei = eff.as_ref().and_then(|e| efforts.iter().position(|x| x == e));
    let eff_text = ei.map(|i| effort_label(&efforts[i])).unwrap_or_default();

    // Speed: Standard plus the model's service tiers.
    let tier_sel: Option<String> = match &st.settings.service_tier {
        Some(t) => t.clone(),
        None => sel.and_then(|m| m.default_service_tier.clone()),
    };
    // Models: the Default row, then the list.
    let explicit = st.settings.model.is_some();
    let model_rows: Vec<MenuRow> = shown_models(st)
        .iter()
        .map(|(m, kind)| {
            let label = match kind {
                Kind::Normal => m.name.clone(),
                Kind::Fast => format!("{} Fast", m.name),
                Kind::Daybreak => format!("{} Daybreak", m.name),
            };
            // The checked row is the one that matches the model and its mode.
            let fast_on = m.fast_tier().map(|t| tier_sel.as_deref() == Some(t)).unwrap_or(false);
            let mode = if st.settings.daybreak && m.has_daybreak_variant() {
                Kind::Daybreak
            } else if fast_on {
                Kind::Fast
            } else {
                Kind::Normal
            };
            let on = explicit && sel.map(|s| s.id == m.id).unwrap_or(false) && mode == *kind;
            row(&label, "", on, false)
        })
        .collect();
    let trigger = match sel {
        Some(m) if eff_text.is_empty() => m.name.clone(),
        Some(m) => format!("{} {}", m.name, eff_text),
        None => "モデルを選択".to_string(),
    };
    app.set_intelligence(IntelligenceState {
        trigger: trigger.into(),
        efforts: strings(efforts.iter().map(|e| effort_label(e))),
        effort_index: ei.map(|i| i as i32).unwrap_or(-1),
        effort_enabled: !efforts.is_empty(),
        effort_label: eff_text.clone().into(),
        models: rows(model_rows),
        default_row: row("デフォルト", "おすすめのモデルセット", !explicit, false),
        loading: !st.models_loaded,
        ultra_warning: eff.as_deref() == Some("ultra"),
        can_reset: explicit || st.settings.effort.is_some() || st.settings.service_tier.is_some(),
    });

    // Permissions
    let cur = current_perm(st);
    let managed = st.allowed_policies.is_some() || st.allowed_sandboxes.is_some();
    let disabled_desc = "requirements.toml により無効になっています";
    let mut perm_rows = vec![
        {
            let ok = perm_allowed(st, Perm::Ask);
            row("承認を求める", if ok { "外部ファイルの編集やインターネットの利用について、常に確認を求めます" } else { disabled_desc }, cur == Perm::Ask, !ok)
        },
        {
            let ok = perm_allowed(st, Perm::Guardian);
            row("代わりに承認", if ok { "安全でない可能性があると検出された操作についてのみ確認を求めます" } else { disabled_desc }, cur == Perm::Guardian, !ok)
        },
        {
            let ok = perm_allowed(st, Perm::Full);
            row("フルアクセス", if ok { "インターネットとコンピューター上のすべてのファイルに無制限でアクセスします" } else { disabled_desc }, cur == Perm::Full, !ok)
        },
        row("カスタム（config.toml）", "config.toml で定義された権限を使用します", cur == Perm::Server, false),
    ];
    if managed {
        perm_rows.push(row("管理対象", "Enterprise ポリシーで管理されています", false, true));
    }
    let (trigger, danger) = match cur {
        Perm::Ask => ("承認を依頼", false),
        Perm::Guardian => ("代わりに承認", false),
        Perm::Full => ("フルアクセス", true),
        Perm::Server => ("カスタム", false),
    };
    app.set_perm(PermState { trigger: trigger.into(), danger, rows: rows(perm_rows) });

    // Projects: folders of the known threads.
    let mut cwds: Vec<String> = Vec::new();
    for t in &st.threads {
        if !t.cwd.is_empty() && !cwds.contains(&t.cwd) {
            cwds.push(t.cwd.clone());
        }
    }
    let chosen = st.settings.cwd.clone().unwrap_or_else(|| st.active_cwd.clone());
    let base = |p: &str| {
        std::path::Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.to_string())
    };
    let project_rows: Vec<MenuRow> = cwds.iter().map(|c| row(&base(c), &client::scrub(c), *c == chosen, false)).collect();
    app.set_project(ProjectState {
        location: "ローカルで作業".into(),
        project: if chosen.is_empty() { "プロジェクトを選択".into() } else { base(&chosen).into() },
        projects: rows(project_rows),
    });
    // (kept in State by the caller through project_cwds)
}

fn sync_project_cwds(st: &mut State) {
    let mut cwds: Vec<String> = Vec::new();
    for t in &st.threads {
        if !t.cwd.is_empty() && !cwds.contains(&t.cwd) {
            cwds.push(t.cwd.clone());
        }
    }
    st.project_cwds = cwds;
}

fn messages_with<R>(app: &AppWindow, f: impl FnOnce(&VecModel<Message>) -> R) -> Option<R> {
    let model = app.get_messages();
    let vec = model.as_any().downcast_ref::<VecModel<Message>>()?;
    Some(f(vec))
}

fn push(model: &VecModel<Message>, id: &str, role: &str, text: &str) {
    model.push(Message { id: id.into(), role: role.into(), text: text.into() });
}

fn rebuild_projects(app: &AppWindow, state: &State) {
    // Group threads by the last component of their working directory.
    let mut groups: BTreeMap<String, (String, Vec<ChatItem>)> = BTreeMap::new();
    for t in &state.threads {
        let name = std::path::Path::new(&t.cwd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "(フォルダなし)".to_string());
        let selected = t.id == state.active;
        groups.entry(name).or_insert_with(|| (t.cwd.clone(), Vec::new())).1.push(ChatItem {
            id: t.id.as_str().into(),
            title: client::mask_emails(&t.title).into(),
            status: if selected && state.busy { 1 } else { 0 },
            selected,
            depth: 0,
        });
    }
    let projects: Vec<ProjectItem> = groups
        .into_iter()
        .map(|(name, (cwd, chats))| ProjectItem {
            name: name.into(),
            cwd: cwd.into(),
            chats: ModelRc::from(Rc::new(VecModel::from(chats))),
        })
        .collect();
    app.set_projects(ModelRc::from(Rc::new(VecModel::from(projects))));
}

fn sandbox_label(t: &str) -> &'static str {
    match t {
        "readOnly" => "read-only",
        "dangerFullAccess" => "danger-full-access",
        _ => "workspace-write",
    }
}

/// Settings dialog contents for the model discovery.
fn refresh_discovery(app: &AppWindow, st: &State) {
    let base = client::base_client_version().unwrap_or_default();
    let limit = discovery::limit_version(&base, st.prefs.headroom);
    let (last, summary) = match &st.outcome {
        Some(o) => (discovery::format_time(o.at), o.summary()),
        None => (String::new(), String::new()),
    };
    app.set_discovery(DiscoveryState {
        base: base.into(),
        limit: limit.into(),
        headroom: st.prefs.headroom as i32,
        last: last.into(),
        summary: summary.into(),
        enabled: st.prefs.enabled,
        running: st.discovery_running,
    });
}

/// Run (or reuse the 24-hour cache of) the version discovery.
fn start_discovery(app: &AppWindow, force: bool) {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        if st.discovery_running {
            return;
        }
        if !st.prefs.enabled {
            st.extra.clear();
            refresh_pickers(app, &st);
            refresh_discovery(app, &st);
            return;
        }
        let Some(base) = client::base_client_version() else {
            refresh_discovery(app, &st);
            return;
        };
        let Ok(program) = client::find_inocode(&std::env::var("INOCODE_BIN").unwrap_or_default()) else {
            refresh_discovery(app, &st);
            return;
        };
        if !force {
            let cached = st.outcome.clone().or_else(discovery::load_cache);
            if let Some(o) = cached.filter(|o| discovery::is_fresh(o, &base, st.prefs.headroom)) {
                st.extra = o.extra_models();
                st.outcome = Some(o);
                refresh_pickers(app, &st);
                refresh_discovery(app, &st);
                return;
            }
        }
        st.discovery_running = true;
        refresh_discovery(app, &st);
        let weak = app.as_weak();
        discovery::start(program.to_string_lossy().to_string(), base, st.prefs.headroom, move |res| {
            let _ = slint::invoke_from_event_loop(move || {
                let Some(app) = weak.upgrade() else { return };
                STATE.with(|s| {
                    let mut st = s.borrow_mut();
                    st.discovery_running = false;
                    if let Ok(o) = res {
                        st.extra = o.extra_models();
                        st.outcome = Some(o);
                    }
                    refresh_pickers(&app, &st);
                    refresh_discovery(&app, &st);
                });
            });
        });
    });
}

fn apply(app: &AppWindow, state: &RefCell<State>, event: Event) {
    match event {
        Event::Status(text, level) => {
            app.set_status(client::scrub(&text).into());
            app.set_status_level(match level {
                Level::Info => 0,
                Level::Ok => 1,
                Level::Warn => 2,
                Level::Error => 3,
            });
        }
        Event::Transport(name) => app.set_transport(name.into()),
        Event::Models(list) => {
            let mut st = state.borrow_mut();
            st.settings.programs = list.clone();
            st.models = list;
            st.models_loaded = true;
            refresh_pickers(app, &st);
            // Discovery runs once the main app-server has its own list.
            let first = !st.discovery_started;
            st.discovery_started = true;
            drop(st);
            if first {
                start_discovery(app, false);
            }
        }
        Event::Requirements { policies, sandboxes } => {
            let mut st = state.borrow_mut();
            st.allowed_policies = policies;
            st.allowed_sandboxes = sandboxes;
            refresh_pickers(app, &st);
        }
        Event::Daybreak(on) => {
            let mut st = state.borrow_mut();
            st.settings.daybreak = on;
            refresh_pickers(app, &st);
        }
        Event::Applied { model, effort, approval, sandbox, reviewer, cwd } => {
            let mut st = state.borrow_mut();
            st.applied_model = model;
            st.applied_effort = effort;
            if !approval.is_empty() {
                st.applied_perm = Some(Perm::from_server(&approval, &sandbox, &reviewer));
            }
            let _ = sandbox_label(&sandbox);
            st.active_cwd = cwd.clone();
            app.set_cwd_text(client::scrub(&cwd).into());
            refresh_pickers(app, &st);
        }
        Event::Account { logged_in, label } => {
            app.set_logged_in(logged_in);
            app.set_account_label(label.into());
            if !logged_in {
                app.set_status("未ログイン".into());
                app.set_status_level(2);
            }
        }
        Event::Usage { percent, text } => {
            app.set_context_percent(percent);
            app.set_usage_text(text.into());
        }
        Event::RateLimits(text) => app.set_rate_text(text.into()),
        Event::Threads(list) => {
            let mut st = state.borrow_mut();
            st.threads = list;
            sync_project_cwds(&mut st);
            rebuild_projects(app, &st);
            refresh_pickers(app, &st);
        }
        Event::ThreadActive { id, title } => {
            let mut st = state.borrow_mut();
            st.active = id;
            st.busy = false;
            app.set_busy(false);
            app.set_session_title(if title.is_empty() { "新しいセッション".into() } else { SharedString::from(title) });
            rebuild_projects(app, &st);
        }
        Event::History(list) => {
            let rows: Vec<Message> = list
                .into_iter()
                .map(|Msg { id, role, text }| Message { id: id.into(), role: role.into(), text: text.into() })
                .collect();
            app.set_messages(ModelRc::from(Rc::new(VecModel::from(rows))));
            app.set_approvals(ModelRc::from(Rc::new(VecModel::<Approval>::default())));
            app.invoke_scroll_bottom();
        }
        Event::AssistantStart => {
            messages_with(app, |m| push(m, "", "assistant", ""));
        }
        Event::Delta(delta) => {
            messages_with(app, |model| {
                let n = model.row_count();
                match model.row_data(n.wrapping_sub(1)) {
                    Some(mut last) if last.role == "assistant" => {
                        last.text.push_str(&delta);
                        model.set_row_data(n - 1, last);
                    }
                    _ => push(model, "", "assistant", &delta),
                }
            });
            app.invoke_scroll_bottom();
        }
        Event::Tool { id, text } => {
            messages_with(app, |model| {
                for i in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(i) {
                        if row.role == "tool" && row.id == id.as_str() {
                            row.text = text.as_str().into();
                            model.set_row_data(i, row);
                            return;
                        }
                    }
                }
                push(model, &id, "tool", &text);
            });
            app.invoke_scroll_bottom();
        }
        Event::TurnStarted => {
            let mut st = state.borrow_mut();
            st.busy = true;
            app.set_busy(true);
            rebuild_projects(app, &st);
        }
        Event::TurnDone { interrupted } => {
            let mut st = state.borrow_mut();
            st.busy = false;
            app.set_busy(false);
            rebuild_projects(app, &st);
            if interrupted {
                messages_with(app, |m| push(m, "", "tool", "中断しました"));
                app.invoke_scroll_bottom();
            }
        }
        Event::Approval { key, title, detail } => {
            let model = app.get_approvals();
            if let Some(vec) = model.as_any().downcast_ref::<VecModel<Approval>>() {
                vec.push(Approval { key: key.into(), title: title.into(), detail: detail.into() });
            }
        }
        Event::ApprovalResolved(key) => {
            let model = app.get_approvals();
            if let Some(vec) = model.as_any().downcast_ref::<VecModel<Approval>>() {
                for i in (0..vec.row_count()).rev() {
                    if vec.row_data(i).map(|a| a.key == key.as_str()).unwrap_or(false) {
                        vec.remove(i);
                    }
                }
            }
        }
        Event::Error(e) => {
            let e = client::scrub(&e);
            // A model found only through version discovery is not verified with InoCode:
            // when its turn fails, say so as part of the error.
            let unverified = {
                let st = state.borrow();
                st.settings
                    .model
                    .as_deref()
                    .and_then(|id| st.extra.iter().find(|m| m.id == id))
                    .and_then(|m| m.min_version.clone().map(|v| (m.name.clone(), v)))
            };
            let text = match unverified {
                Some((name, v)) => format!(
                    "エラー: {e}\n「{name}」は InoCode で動作が確認されていません（必要な公式バージョン {v}）。"
                ),
                None => format!("エラー: {e}"),
            };
            messages_with(app, |m| push(m, "", "assistant", &text));
            app.invoke_scroll_bottom();
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Run `f` on the shared state, push the changed settings to the client, and redraw the pickers.
fn change(app: &AppWindow, h: &client::Handle, f: impl FnOnce(&mut State)) {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        f(&mut st);
        h.send(Command::SetSettings(st.settings.clone()));
        refresh_pickers(app, &st);
    });
}

fn main() {
    let app = AppWindow::new().unwrap();
    app.set_messages(ModelRc::from(Rc::new(VecModel::<Message>::default())));
    app.set_approvals(ModelRc::from(Rc::new(VecModel::<Approval>::default())));
    app.set_projects(ModelRc::from(Rc::new(VecModel::<ProjectItem>::default())));
    STATE.with(|s| {
        s.borrow_mut().prefs = discovery::load_prefs();
        refresh_pickers(&app, &s.borrow());
        refresh_discovery(&app, &s.borrow());
    });

    // Default: start `inocode app-server --listen stdio://` as a child process.
    // INOCODE_WS_URL connects to an already running app-server over WebSocket instead;
    // clicking the connection status in the top bar switches between the two.
    let ws_url = std::env::var("INOCODE_WS_URL").ok();
    let cfg = Config {
        use_stdio: ws_url.is_none() && std::env::var("INOCODE_STDIO").map(|v| v != "0").unwrap_or(true),
        ws_url: ws_url.unwrap_or_else(|| "ws://127.0.0.1:8765".into()),
        stdio_program: std::env::var("INOCODE_BIN").unwrap_or_default(),
    };

    // The state is only touched on the UI thread; events cross over via invoke_from_event_loop.
    let weak = app.as_weak();
    let handle = Rc::new(client::spawn(cfg, move |event| {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                STATE.with(|s| apply(&app, s, event));
            }
        });
    }));

    let weak = app.as_weak();
    let h = handle.clone();
    app.on_submit(move |text| {
        let Some(app) = weak.upgrade() else { return };
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        messages_with(&app, |m| push(m, "", "user", &text));
        h.send(Command::Prompt(text));
        app.set_prompt("".into());
        app.invoke_scroll_bottom();
    });
    let h = handle.clone();
    app.on_new_chat(move || h.send(Command::NewThread));
    let h = handle.clone();
    app.on_open_thread(move |id| h.send(Command::OpenThread(id.to_string())));
    let h = handle.clone();
    app.on_interrupt(move || h.send(Command::Interrupt));
    let h = handle.clone();
    app.on_decide(move |key, decision| {
        h.send(Command::Approve { key: key.to_string(), decision: Decision::parse(&decision) })
    });

    // ---- composer pickers (model, effort, speed, Daybreak, permissions, project)
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_pick_model(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let before = STATE.with(|s| s.borrow().settings.daybreak);
        change(&app, &h, |st| {
            let Some((m, kind)) = shown_models(st).get(i as usize).map(|(m, k)| ((*m).clone(), *k)) else { return };
            st.settings.model = Some(m.id.clone());
            // Each row picks one mode: Daybreak on for the Daybreak row, Fast for the Fast row,
            // and the normal row turns both off.
            st.settings.daybreak = kind == Kind::Daybreak;
            st.settings.service_tier = match (kind, m.fast_tier()) {
                (Kind::Fast, Some(t)) => Some(Some(t.to_string())),
                _ if matches!(st.settings.service_tier, Some(Some(_))) => Some(None),
                _ => st.settings.service_tier.clone(),
            };
            if !st.settings.effort.as_ref().map(|e| m.efforts.contains(e)).unwrap_or(false) {
                st.settings.effort = Some(m.default_effort.clone()).filter(|e| !e.is_empty());
            }
        });
        let after = STATE.with(|s| s.borrow().settings.daybreak);
        if after != before {
            h.send(Command::SetDaybreak(after));
        }
    });
    let weak = app.as_weak();
    app.on_search_model(move |q| {
        let Some(app) = weak.upgrade() else { return };
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.model_filter = q.to_string();
            refresh_pickers(&app, &st);
        });
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_pick_default(move || {
        let Some(app) = weak.upgrade() else { return };
        let before = STATE.with(|s| s.borrow().settings.daybreak);
        change(&app, &h, |st| {
            st.settings.model = None;
            st.settings.effort = None;
            st.settings.daybreak = false;
            if matches!(st.settings.service_tier, Some(Some(_))) {
                st.settings.service_tier = Some(None);
            }
        });
        if before {
            h.send(Command::SetDaybreak(false));
        }
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_reset_model(move || {
        let Some(app) = weak.upgrade() else { return };
        change(&app, &h, |st| {
            st.settings.model = None;
            st.settings.effort = None;
            st.settings.service_tier = None;
        });
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_pick_effort(move |i| {
        let Some(app) = weak.upgrade() else { return };
        change(&app, &h, |st| {
            let visible = visible_models(st);
            let efforts = selected_model(st, &visible).map(|m| m.efforts.clone()).unwrap_or_default();
            if let Some(e) = efforts.get(i as usize) {
                st.settings.effort = Some(e.clone());
            }
        });
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_pick_perm(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some(perm) = PERM_ORDER.get(i as usize).copied() else { return };
        if perm == Perm::Full {
            // Full access asks for confirmation first.
            app.set_full_confirm_open(true);
            return;
        }
        change(&app, &h, |st| st.settings.perm = Some(perm));
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_confirm_full(move || {
        let Some(app) = weak.upgrade() else { return };
        app.set_full_confirm_open(false);
        change(&app, &h, |st| st.settings.perm = Some(Perm::Full));
    });
    let weak = app.as_weak();
    app.on_cancel_full(move || {
        if let Some(app) = weak.upgrade() {
            app.set_full_confirm_open(false);
        }
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_pick_project(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let cwd = STATE.with(|s| s.borrow().project_cwds.get(i as usize).cloned());
        if let Some(cwd) = cwd {
            change(&app, &h, |st| st.settings.cwd = Some(cwd.clone()));
            h.send(Command::NewThreadIn(cwd));
        }
    });

    let weak = app.as_weak();
    app.on_toggle_discovery(move || {
        let Some(app) = weak.upgrade() else { return };
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.prefs.enabled = !st.prefs.enabled;
            let _ = discovery::save_prefs(&st.prefs);
        });
        start_discovery(&app, false);
    });
    for up in [true, false] {
        let weak = app.as_weak();
        let handler = move || {
            let Some(app) = weak.upgrade() else { return };
            STATE.with(|s| {
                let mut st = s.borrow_mut();
                let h = st.prefs.headroom;
                st.prefs.headroom = if up { (h + 1).min(discovery::MAX_HEADROOM) } else { h.saturating_sub(1) };
                let _ = discovery::save_prefs(&st.prefs);
            });
            start_discovery(&app, false);
        };
        if up {
            app.on_headroom_up(handler);
        } else {
            app.on_headroom_down(handler);
        }
    }
    let weak = app.as_weak();
    app.on_run_discovery(move || {
        if let Some(app) = weak.upgrade() {
            start_discovery(&app, true);
        }
    });

    let h = handle.clone();
    app.on_login(move || h.send(Command::Login));
    let h = handle.clone();
    app.on_new_chat_in(move |cwd| h.send(Command::NewThreadIn(cwd.to_string())));
    let h = handle.clone();
    app.on_switch_transport(move || h.send(Command::SwitchTransport));
    app.run().unwrap();
    // Closing the window: stop the client so the app-server child it started exits too.
    handle.shutdown();
}
