//! Registration of the chat window's callbacks.

use crate::state::*;
use crate::view::*;
use crate::*;

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    crate::sidebar::register(app, handle);
    crate::model_init::register(handle);
    let weak = app.as_weak();
    let h = handle.clone();
    app.on_submit(move |text| {
        let Some(app) = weak.upgrade() else { return };
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // "/compact" and the like run a command instead of sending text.
        if crate::attach::try_slash(&app, &h, &text) {
            app.set_prompt("".into());
            return;
        }
        if !app.global::<AppState>().get_busy() {
            crate::thread_stats::note_send();
        }
        // A message is never sent without a chosen model: the server's own default would be used.
        let missing = STATE.with(|s| {
            let st = s.borrow();
            let visible = visible_models(&st);
            selected_model(&st, &visible).is_none()
        });
        if missing {
            return;
        }
        // The replies that are ready go in front of the text, one line each.
        let text = crate::reply::compose(&app, text);
        // A message queued behind a running turn shows in the chat when it starts.
        if !app.global::<AppState>().get_busy() {
            messages_with(&app, |m| push(m, "", "user", &text));
        }
        // While a turn runs the message goes to the queue; otherwise it starts a turn.
        crate::prompts::submit(&app, &h, text);
        app.set_prompt("".into());
        app.invoke_scroll_bottom();
    });
    let h = handle.clone();
    app.global::<AppState>().on_new_chat(move || h.send(Command::NewThread));
    let h = handle.clone();
    app.global::<AppState>().on_open_thread(move |id| h.send(Command::OpenThread(id.to_string())));
    let h = handle.clone();
    app.global::<AppState>().on_interrupt(move || h.send(Command::Interrupt));
    let h = handle.clone();
    app.global::<AppState>().on_decide(move |key, decision| {
        h.send(Command::Approve { key: key.to_string(), decision: Decision::parse(&decision) })
    });

    // ---- composer pickers (model, effort, speed, Daybreak, permissions, project)
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<AppState>().on_pick_model(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let before = STATE.with(|s| s.borrow().settings.daybreak);
        // A heading opens or closes its section; nothing is chosen.
        let heading = STATE.with(|s| {
            let mut st = s.borrow_mut();
            let toggled = match shown_models(&st).get(i as usize) {
                Some(Item::Heading(p, open)) => Some((p.clone(), *open)),
                _ => None,
            };
            if let Some((p, open)) = &toggled {
                st.provider_open.insert(p.clone(), !open);
                refresh_pickers(&app, &st);
            }
            toggled.is_some()
        });
        if heading {
            return;
        }
        let mut switch_thread = false;
        change(&app, &h, |st| {
            let Some((m, kind)) = shown_models(st).get(i as usize).and_then(|it| match it {
                Item::Model(m, k) => Some(((*m).clone(), *k)),
                Item::Heading(..) => None,
            }) else { return };
            // A thread keeps its provider: the model of another one is for a new thread.
            if crate::model_init::provider_changed(st, &m.provider) {
                switch_thread = true;
            }
            st.settings.model = Some(m.id.clone());
            st.settings.model_provider = Some(m.provider.clone());
            crate::gui_prefs::save_last_model(&m.id, &m.provider);
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
        if switch_thread {
            h.send(Command::NewThread);
        }
    });
    let weak = app.as_weak();
    app.global::<AppState>().on_search_model(move |q| {
        let Some(app) = weak.upgrade() else { return };
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.model_filter = q.to_string();
            refresh_pickers(&app, &st);
        });
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<AppState>().on_reset_model(move || {
        let Some(app) = weak.upgrade() else { return };
        // The model stays: only the effort and the speed go back to the model's own.
        change(&app, &h, |st| {
            st.settings.effort = None;
            st.settings.service_tier = None;
        });
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<AppState>().on_pick_effort(move |i| {
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
    app.global::<AppState>().on_pick_perm(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some(perm) = PERM_ORDER.get(i as usize).copied() else { return };
        if perm == Perm::Full {
            // Full access asks for confirmation first.
            app.global::<AppState>().set_full_confirm_open(true);
            return;
        }
        change(&app, &h, |st| st.settings.perm = Some(perm));
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<AppState>().on_confirm_full(move || {
        let Some(app) = weak.upgrade() else { return };
        app.global::<AppState>().set_full_confirm_open(false);
        change(&app, &h, |st| st.settings.perm = Some(Perm::Full));
    });
    let weak = app.as_weak();
    app.global::<AppState>().on_cancel_full(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<AppState>().set_full_confirm_open(false);
        }
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<AppState>().on_pick_project(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let cwd = STATE.with(|s| s.borrow().project_cwds.get(i as usize).cloned());
        if let Some(cwd) = cwd {
            change(&app, &h, |st| st.settings.cwd = Some(cwd.clone()));
            h.send(Command::NewThreadIn(cwd));
        }
    });

    let weak = app.as_weak();
    app.global::<AppState>().on_toggle_discovery(move || {
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
            app.global::<AppState>().on_headroom_up(handler);
        } else {
            app.global::<AppState>().on_headroom_down(handler);
        }
    }
    let weak = app.as_weak();
    app.global::<AppState>().on_run_discovery(move || {
        if let Some(app) = weak.upgrade() {
            start_discovery(&app, true);
        }
    });

    let h = handle.clone();
    app.global::<AppState>().on_login(move || h.send(Command::Login));
    let h = handle.clone();
    app.global::<AppState>().on_new_chat_in(move |cwd| h.send(Command::NewThreadIn(cwd.to_string())));
    let h = handle.clone();
    app.global::<AppState>().on_switch_transport(move || h.send(Command::SwitchTransport));
    crate::coding::register(app, handle);
    crate::chat_md::register(app);
    crate::git_panel::register(app, handle);
    crate::reply::register(app, handle);
}
