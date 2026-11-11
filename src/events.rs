//! Client events -> UI.

use crate::state::*;
use crate::view::*;
use crate::*;

pub(crate) fn apply(app: &AppWindow, state: &RefCell<State>, event: Event) {
    match event {
        Event::Status(text, level) => {
            app.global::<AppState>().set_disconnected(level == Level::Error && text.starts_with("未接続"));
            app.global::<AppState>().set_status(text.into());
            app.global::<AppState>().set_status_level(match level {
                Level::Info => 0,
                Level::Ok => 1,
                Level::Warn => 2,
                Level::Error => 3,
            });
        }
        Event::Transport(name) => {
            app.global::<AppState>().set_transport(name.into());
            // A new connection: requests of the old one are gone with it.
            let mut st = state.borrow_mut();
            st.pending.clear();
            crate::coding::render_approvals(app, &st);
        }
        Event::Models(list) => {
            let mut st = state.borrow_mut();
            st.settings.programs = list.clone();
            st.models = list;
            st.models_loaded = true;
            crate::settings_page::boot();
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
            crate::coding::on_applied(app, &mut st, &sandbox, &cwd);
            let _ = sandbox_label(&sandbox);
            st.branch = git_branch(&cwd);
            crate::git_panel::set_branch(app, &st.branch);
            st.active_cwd = cwd.clone();
            crate::panel_data::on_thread_active(app, &st.active.clone(), &cwd);
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
        // The sidebar owns the list request (sort, filter, paging), so a list the client reloaded
        // on its own only triggers a refresh with the sidebar's own parameters.
        Event::Threads(_) => crate::sidebar::refresh(),
        Event::ThreadActive { id, title } => {
            crate::sidebar::on_thread_active(&id);
            crate::metrics::reset(app);
            crate::nav::visit(app, crate::nav::Entry::Thread(id.clone()));
            let mut st = state.borrow_mut();
            st.active = id;
            st.busy = false;
            app.global::<AppState>().set_busy(false);
            // Each thread has its own approvals: show the ones of this thread.
            crate::coding::render_approvals(app, &st);
            app.global::<AppState>().set_session_title(if title.is_empty() { "新しいセッション".into() } else { SharedString::from(title) });
            rebuild_projects(app, &st);
        }
        Event::History(list) => {
            let rows: Vec<Message> = list
                .into_iter()
                .map(|Msg { id, role, text, detail }| {
                    let blocks = if role == "assistant" { crate::chat_md::blocks_of(&text) } else { Default::default() };
                    Message { id: id.into(), role: role.into(), text: text.into(), detail: clip_detail(&detail).into(), blocks }
                })
                .collect();
            crate::chat_md::reset();
            app.global::<AppState>().set_messages(ModelRc::from(Rc::new(VecModel::from(rows))));
            app.invoke_scroll_bottom();
            crate::chat_actions::after_history(app);
        }
        Event::AssistantStart => {
            // The answer before is final now; a new one starts.
            crate::chat_md::sync_last(app, true);
            crate::chat_md::reset();
            messages_with(app, |m| push(m, "", "assistant", ""));
        }
        Event::TokenTotals { total, last_total, last_output } => crate::metrics::totals(app, total, last_total, last_output),
        Event::Delta(delta) => {
            crate::metrics::chunk(app, &delta);
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
            crate::chat_md::sync_last(app, false);
            app.invoke_scroll_bottom();
        }
        Event::Tool { id, text, detail } => {
            crate::chat_md::sync_last(app, true);
            // A finished item brings its whole body; an empty one keeps what streamed in.
            messages_with(app, |model| upsert_tool(model, &id, &text, if detail.is_empty() { None } else { Some(&detail) }));
            app.invoke_scroll_bottom();
        }
        Event::ToolDelta { id, delta } => {
            crate::metrics::chunk(app, &delta);
            messages_with(app, |model| {
                for i in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(i) {
                        if row.role == "tool" && row.id == id.as_str() {
                            let joined = format!("{}{}", row.detail, delta);
                            row.detail = clip_detail(&joined).into();
                            model.set_row_data(i, row);
                            return;
                        }
                    }
                }
            });
        }
        Event::Note { id, text, detail } => {
            messages_with(app, |model| upsert_tool(model, &id, &text, Some(&detail)));
            app.invoke_scroll_bottom();
        }
        Event::TurnStarted => {
            crate::metrics::turn_started(app);
            crate::prompts::clear_banner(app);
            let mut st = state.borrow_mut();
            st.busy = true;
            app.global::<AppState>().set_busy(true);
            rebuild_projects(app, &st);
        }
        Event::TurnDone { interrupted } => {
            crate::chat_md::sync_last(app, true);
            crate::metrics::turn_done(app);
            let mut st = state.borrow_mut();
            st.busy = false;
            app.global::<AppState>().set_busy(false);
            rebuild_projects(app, &st);
            crate::panel_data::refresh_terminals(&st.active.clone());
            crate::prompts::on_turn_done(app, &st.active.clone(), interrupted);
            // The turn's plan and file changes end up at the bottom, and the next turn starts new rows.
            messages_with(app, |model| {
                for key in ["turn-diff", "turn-plan"] {
                    if let Some(i) = (0..model.row_count()).find(|i| model.row_data(*i).map(|r| r.role == "tool" && r.id == key).unwrap_or(false)) {
                        if let Some(mut row) = model.row_data(i) {
                            model.remove(i);
                            row.id = format!("{key}-done").into();
                            model.push(row);
                        }
                    }
                }
            });
            if interrupted {
                messages_with(app, |m| push(m, "", "tool", "中断しました"));
                app.invoke_scroll_bottom();
            }
        }
        Event::Approval(info) => {
            let mut st = state.borrow_mut();
            crate::coding::push_pending(&mut st.pending, &info.thread, crate::coding::to_approval(&info));
            crate::coding::render_approvals(app, &st);
        }
        Event::Questions { key, items } => crate::prompts::on_questions(app, key, items),
        Event::ApprovalResolved(key) => {
            crate::prompts::on_resolved(app, &key);
            let mut st = state.borrow_mut();
            st.pending.retain(|(_, a)| a.key != key.as_str());
            crate::coding::render_approvals(app, &st);
        }
        // Rpc / Notify are delivered to the screen tabs (src/tabs) before apply().
        Event::Rpc { tag, result } => {
            crate::prompts::on_rpc(app, &tag, &result);
            crate::attach::on_rpc(app, &tag, &result);
            crate::panel_data::on_rpc(app, &tag, &result);
            crate::settings_page::on_rpc(app, &tag, &result);
            crate::accounts_page::on_rpc(app, &tag, &result);
            crate::sidebar::on_rpc(app, &tag, &result);
            crate::chat_actions::on_rpc(app, &tag, &result);
            crate::coding::on_rpc(app, &tag, &result);
        }
        Event::Notify { method, params } => {
            crate::panel_data::on_notify(app, &method, &params);
            crate::accounts_page::on_notify(app, &method, &params);
            if method == "inocode/accounts/switched" {
                // One line in the chat: which account, and why (alias only).
                let to = params.get("toAlias").and_then(|v| v.as_str()).unwrap_or("");
                let why = match params.get("reason").and_then(|v| v.as_str()) {
                    Some("threshold") => "使用量がしきい値に近づいたため",
                    Some("limitReached") => "上限に達したため",
                    Some("manual") => "手動の切り替え",
                    _ => "",
                };
                let text = if why.is_empty() { format!("アカウントを切り替えました: {to}") } else { format!("アカウントを切り替えました: {to} ({why})") };
                let id = format!("switch:{}", messages_with(app, |m| m.row_count()).unwrap_or(0));
                messages_with(app, |m| upsert_tool(m, &id, &text, None));
                app.invoke_scroll_bottom();
            }
            crate::coding::on_notify(app, &method, &params);
            crate::sidebar::on_notify(&method);
            crate::prompts::on_notify(&method);
        }
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
            // An error is a banner above the composer with a retry button, not a chat message.
            crate::prompts::show_banner(app, &text, 2);
        }
    }
}

