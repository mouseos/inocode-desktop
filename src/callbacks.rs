//! Registration of the chat window's callbacks.

use crate::state::*;
use crate::view::*;
use crate::*;

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
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
}
