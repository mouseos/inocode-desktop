//! Client events -> UI.

use crate::state::*;
use crate::view::*;
use crate::*;

pub(crate) fn apply(app: &AppWindow, state: &RefCell<State>, event: Event) {
    match event {
        Event::Status(text, level) => {
            app.global::<AppState>().set_status(text.into());
            app.global::<AppState>().set_status_level(match level {
                Level::Info => 0,
                Level::Ok => 1,
                Level::Warn => 2,
                Level::Error => 3,
            });
        }
        Event::Transport(name) => app.global::<AppState>().set_transport(name.into()),
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
            app.global::<AppState>().set_cwd_text(cwd.into());
            refresh_pickers(app, &st);
        }
        Event::Account { logged_in, label } => {
            app.global::<AppState>().set_logged_in(logged_in);
            // The avatar shows the first letter of the label (never the e-mail address).
            let initial: String = label.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            app.global::<SidebarState>().set_account_initial(initial.into());
            app.global::<AppState>().set_account_label(label.into());
            if !logged_in {
                app.global::<AppState>().set_status("未ログイン".into());
                app.global::<AppState>().set_status_level(2);
            }
        }
        Event::Usage { percent, text } => {
            app.global::<AppState>().set_context_percent(percent);
            app.global::<AppState>().set_usage_text(text.into());
        }
        Event::RateLimits(text) => app.global::<AppState>().set_rate_text(text.into()),
        Event::Threads(list) => {
            let mut st = state.borrow_mut();
            st.threads = list;
            sync_project_cwds(&mut st);
            rebuild_projects(app, &st);
            refresh_pickers(app, &st);
        }
        Event::ThreadActive { id, title } => {
            crate::sidebar::on_thread_active(&id);
            let mut st = state.borrow_mut();
            st.active = id;
            st.busy = false;
            app.global::<AppState>().set_busy(false);
            app.global::<AppState>().set_session_title(if title.is_empty() { "新しいセッション".into() } else { SharedString::from(title) });
            rebuild_projects(app, &st);
        }
        Event::History(list) => {
            let rows: Vec<Message> = list
                .into_iter()
                .map(|Msg { id, role, text }| Message { id: id.into(), role: role.into(), text: text.into() })
                .collect();
            app.global::<AppState>().set_messages(ModelRc::from(Rc::new(VecModel::from(rows))));
            app.global::<AppState>().set_approvals(ModelRc::from(Rc::new(VecModel::<Approval>::default())));
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
            app.global::<AppState>().set_busy(true);
            rebuild_projects(app, &st);
        }
        Event::TurnDone { interrupted } => {
            let mut st = state.borrow_mut();
            st.busy = false;
            app.global::<AppState>().set_busy(false);
            rebuild_projects(app, &st);
            if interrupted {
                messages_with(app, |m| push(m, "", "tool", "中断しました"));
                app.invoke_scroll_bottom();
            }
        }
        Event::Approval { key, title, detail } => {
            let model = app.global::<AppState>().get_approvals();
            if let Some(vec) = model.as_any().downcast_ref::<VecModel<Approval>>() {
                vec.push(Approval { key: key.into(), title: title.into(), detail: detail.into() });
            }
        }
        Event::ApprovalResolved(key) => {
            let model = app.global::<AppState>().get_approvals();
            if let Some(vec) = model.as_any().downcast_ref::<VecModel<Approval>>() {
                for i in (0..vec.row_count()).rev() {
                    if vec.row_data(i).map(|a| a.key == key.as_str()).unwrap_or(false) {
                        vec.remove(i);
                    }
                }
            }
        }
        // Rpc / Notify are delivered to the screen tabs (src/tabs) before apply().
        Event::Rpc { tag, result } => crate::sidebar::on_rpc(app, &tag, &result),
        Event::Notify { method, .. } => crate::sidebar::on_notify(&method),
        Event::Error(e) => {
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

