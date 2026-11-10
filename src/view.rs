//! Rebuilding what the UI shows (pickers, project list, discovery dialog).

use crate::state::*;
use crate::*;

/// Rebuild what the model, permission and project pickers show.
pub(crate) fn refresh_pickers(app: &AppWindow, st: &State) {
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
    app.global::<AppState>().set_intelligence(IntelligenceState {
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
    app.global::<AppState>().set_perm(PermState { trigger: trigger.into(), danger, rows: rows(perm_rows) });

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
    let project_rows: Vec<MenuRow> = cwds.iter().map(|c| row(&base(c), c, *c == chosen, false)).collect();
    app.global::<AppState>().set_project(ProjectState {
        location: "ローカルで作業".into(),
        project: if chosen.is_empty() { "プロジェクトを選択".into() } else { base(&chosen).into() },
        projects: rows(project_rows),
    });
    // (kept in State by the caller through project_cwds)
}

pub(crate) fn sync_project_cwds(st: &mut State) {
    let mut cwds: Vec<String> = Vec::new();
    for t in &st.threads {
        if !t.cwd.is_empty() && !cwds.contains(&t.cwd) {
            cwds.push(t.cwd.clone());
        }
    }
    st.project_cwds = cwds;
}

pub(crate) fn messages_with<R>(app: &AppWindow, f: impl FnOnce(&VecModel<Message>) -> R) -> Option<R> {
    let model = app.global::<AppState>().get_messages();
    let vec = model.as_any().downcast_ref::<VecModel<Message>>()?;
    Some(f(vec))
}

pub(crate) fn push(model: &VecModel<Message>, id: &str, role: &str, text: &str) {
    model.push(Message { id: id.into(), role: role.into(), text: text.into(), detail: "".into() });
}

pub(crate) fn rebuild_projects(app: &AppWindow, state: &State) {
    let (flat, filter) = crate::sidebar::mode();
    let chat = |t: &ThreadInfo, depth: i32| {
        let selected = t.id == state.active;
        // Needs you, then working, then error, then unread.
        let status = if selected && state.busy {
            1
        } else if t.status != 0 {
            t.status
        } else if t.unread && !selected {
            2
        } else {
            0
        };
        ChatItem { id: t.id.as_str().into(), title: t.title.as_str().into(), status, selected, depth }
    };
    let shown: Vec<&ThreadInfo> = state
        .threads
        .iter()
        .filter(|t| match filter {
            1 => t.unread,
            2 => t.status == 4,
            _ => true,
        })
        .collect();
    // A child session follows its parent (indented) when the parent is in the same list.
    let ordered = |items: &[&ThreadInfo]| -> Vec<ChatItem> {
        let has = |id: &str| items.iter().any(|t| t.id == id);
        let mut out = Vec::new();
        for t in items.iter().filter(|t| t.parent.is_empty() || !has(&t.parent)) {
            out.push(chat(t, 0));
            for c in items.iter().filter(|c| c.parent == t.id) {
                out.push(chat(c, 1));
            }
        }
        out
    };
    let model = |chats: Vec<ChatItem>| ModelRc::from(Rc::new(VecModel::from(chats)));
    let mut projects: Vec<ProjectItem> = Vec::new();
    // Pinned sessions first, in their own group (not while viewing the archive).
    let pinned: Vec<&ThreadInfo> = shown.iter().copied().filter(|t| t.pinned && filter != 3).collect();
    if !pinned.is_empty() {
        projects.push(ProjectItem { name: "ピン留め".into(), cwd: "".into(), chats: model(ordered(&pinned)), pinned: true });
    }
    let rest: Vec<&ThreadInfo> = shown.iter().copied().filter(|t| !(t.pinned && filter != 3)).collect();
    if flat {
        if !rest.is_empty() {
            projects.push(ProjectItem { name: "セッション".into(), cwd: "".into(), chats: model(ordered(&rest)), pinned: true });
        }
    } else {
        // One group per folder; a folder is keyed by its full path (two folders can share a name).
        let mut groups: Vec<(String, Vec<&ThreadInfo>)> = Vec::new();
        for t in rest {
            match groups.iter_mut().find(|(cwd, _)| *cwd == t.cwd) {
                Some(g) => g.1.push(t),
                None => groups.push((t.cwd.clone(), vec![t])),
            }
        }
        for (cwd, items) in groups {
            let base = std::path::Path::new(&cwd)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "(フォルダなし)".to_string());
            // Same name, different folder: show the parent folder too.
            let dup = projects.iter().any(|p| p.name.as_str() == base) || state.threads.iter().any(|o| {
                o.cwd != cwd && std::path::Path::new(&o.cwd).file_name() == std::path::Path::new(&cwd).file_name()
            });
            let name = if dup && !cwd.is_empty() {
                let parent = std::path::Path::new(&cwd)
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if parent.is_empty() { base } else { format!("{base} — {parent}") }
            } else {
                base
            };
            projects.push(ProjectItem { name: name.into(), cwd: cwd.into(), chats: model(ordered(&items)), pinned: false });
        }
    }
    app.global::<AppState>().set_projects(ModelRc::from(Rc::new(VecModel::from(projects))));
}

pub(crate) fn sandbox_label(t: &str) -> &'static str {
    match t {
        "readOnly" => "read-only",
        "dangerFullAccess" => "danger-full-access",
        _ => "workspace-write",
    }
}

/// Settings dialog contents for the model discovery.
pub(crate) fn refresh_discovery(app: &AppWindow, st: &State) {
    let base = client::base_client_version().unwrap_or_default();
    let limit = discovery::limit_version(&base, st.prefs.headroom);
    let (last, summary) = match &st.outcome {
        Some(o) => (discovery::format_time(o.at), o.summary()),
        None => (String::new(), String::new()),
    };
    app.global::<AppState>().set_discovery(DiscoveryState {
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
pub(crate) fn start_discovery(app: &AppWindow, force: bool) {
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


/// At most this many trailing characters of a tool body are shown.
const DETAIL_MAX: usize = 12000;

pub(crate) fn clip_detail(text: &str) -> String {
    let n = text.chars().count();
    if n <= DETAIL_MAX {
        return text.to_string();
    }
    let tail: String = text.chars().skip(n - DETAIL_MAX).collect();
    format!("…（先頭 {} 文字を省略）\n{tail}", n - DETAIL_MAX)
}

/// Insert or update a tool row by id (keeps its place when it already exists).
pub(crate) fn upsert_tool(model: &VecModel<Message>, id: &str, text: &str, detail: Option<&str>) {
    for i in 0..model.row_count() {
        if let Some(mut row) = model.row_data(i) {
            if row.role == "tool" && row.id == id {
                row.text = text.into();
                if let Some(d) = detail {
                    row.detail = clip_detail(d).into();
                }
                model.set_row_data(i, row);
                return;
            }
        }
    }
    model.push(Message {
        id: id.into(),
        role: "tool".into(),
        text: text.into(),
        detail: detail.map(clip_detail).unwrap_or_default().into(),
    });
}
