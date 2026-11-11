//! Questions from the agent, the follow-up queue while a turn runs, the error banner and compaction.
//!
//! Official: item/tool/requestUserInput, mcpServer/elicitation/request, turn/steer,
//! thread/queue/* (experimental), thread/compact/start, the `error` notification.
//! Claude Desktop: "Claude is asking: …" with Skip / Submit, Queued messages with Send now /
//! Edit in composer / Remove from queue, recovery buttons on failed turns, Compact this session
//! (c96d752d0, cf74bc599, c227f4d5e, c3e34355f).

use crate::state::*;
use crate::*;
use client::QuestionInfo;
use serde_json::{json, Value};

#[derive(Default)]
struct Data {
    qkey: String,
    items: Vec<QuestionInfo>,
    idx: usize,
    answers: Vec<(String, Vec<String>)>,
    last_prompt: String,
    /// (queued submission id, text) of the active thread.
    queue: Vec<(String, String)>,
}

thread_local! {
    /// Codex: follow-up behavior "steer" (desktop.followUpQueueMode); the default is the queue.
    static FOLLOW_STEER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static DATA: RefCell<Data> = RefCell::new(Data::default());
}

fn rpc(tag: &str, method: &str, params: Value) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: tag.to_string(), method: method.to_string(), params });
    }
}

fn active() -> Option<String> {
    STATE.with(|s| Some(s.borrow().active.clone()).filter(|a| !a.is_empty()))
}

fn input(text: &str) -> Value {
    json!([{ "type": "text", "text": text, "text_elements": [] }])
}

fn show_question(app: &AppWindow) {
    let s = app.global::<PromptState>();
    DATA.with(|d| {
        let d = d.borrow();
        match d.items.get(d.idx) {
            Some(q) => {
                s.set_question_header(q.header.as_str().into());
                s.set_question_text(q.question.as_str().into());
                s.set_question_options(strings(q.options.clone()));
                s.set_question_secret(q.secret);
                s.set_question_progress(if d.items.len() > 1 { format!("{} / {}", d.idx + 1, d.items.len()).into() } else { "".into() });
                s.set_question_open(true);
            }
            None => s.set_question_open(false),
        }
    });
}

/// A request for answers arrived.
pub(crate) fn on_questions(app: &AppWindow, key: String, items: Vec<QuestionInfo>) {
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        d.qkey = key;
        d.items = items;
        d.idx = 0;
        d.answers.clear();
    });
    show_question(app);
}

/// The request was resolved (here or by the server).
pub(crate) fn on_resolved(app: &AppWindow, key: &str) {
    let mine = DATA.with(|d| d.borrow().qkey == key);
    if mine {
        DATA.with(|d| d.borrow_mut().qkey.clear());
        app.global::<PromptState>().set_question_open(false);
    }
}

fn record(app: &AppWindow, answer: Option<String>) {
    let done = DATA.with(|d| {
        let mut d = d.borrow_mut();
        let Some(q) = d.items.get(d.idx) else { return None };
        let id = q.id.clone();
        d.answers.push((id, answer.into_iter().collect()));
        d.idx += 1;
        if d.idx >= d.items.len() {
            Some((d.qkey.clone(), std::mem::take(&mut d.answers)))
        } else {
            None
        }
    });
    match done {
        Some((key, answers)) => {
            if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
                h.send(Command::Answer { key, answers });
            }
        }
        None => show_question(app),
    }
}

pub(crate) fn clear_banner(app: &AppWindow) {
    app.global::<PromptState>().set_banner_level(0);
}

pub(crate) fn show_banner(app: &AppWindow, text: &str, level: i32) {
    let s = app.global::<PromptState>();
    s.set_banner_text(text.lines().next().unwrap_or(text).into());
    s.set_banner_level(level);
}

pub(crate) fn set_follow_steer(on: bool) {
    FOLLOW_STEER.with(|f| f.set(on));
}

/// Called by the composer: send now, or queue behind the running turn.
pub(crate) fn submit(app: &AppWindow, h: &client::Handle, text: String) {
    DATA.with(|d| d.borrow_mut().last_prompt = text.clone());
    let extra = crate::attach::take_inputs(app);
    if app.global::<AppState>().get_busy() && FOLLOW_STEER.with(|f| f.get()) && extra.is_empty() {
        // Steer: the message goes into the running turn at once.
        crate::view::messages_with(app, |m| crate::view::push(m, "", "user", &text));
        h.send(Command::Steer(text));
    } else if app.global::<AppState>().get_busy() {
        if let Some(tid) = active() {
            let mut inputs = input(&text);
            if let Some(a) = inputs.as_array_mut() {
                a.extend(extra);
            }
            rpc(
                "prompt.queue.add",
                "thread/queue/add",
                json!({ "threadId": tid, "input": inputs, "clientUserMessageId": format!("ino-{}", now_nanos()) }),
            );
        }
    } else if extra.is_empty() {
        h.send(Command::Prompt(text));
    } else {
        h.send(Command::PromptWith(text, extra));
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn list_queue() {
    if let Some(tid) = active() {
        rpc("prompt.queue.list", "thread/queue/list", json!({ "threadId": tid }));
    }
}

fn set_queue_model(app: &AppWindow) {
    let rows: Vec<QueueRow> = DATA.with(|d| {
        d.borrow().queue.iter().map(|(id, text)| QueueRow { id: id.as_str().into(), text: text.as_str().into() }).collect()
    });
    app.global::<PromptState>().set_queue(ModelRc::from(Rc::new(VecModel::from(rows))));
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let weak = app.as_weak();
    app.global::<PromptState>().on_answer(move |text| {
        if let Some(app) = weak.upgrade() {
            record(&app, Some(text.to_string()));
        }
    });
    let weak = app.as_weak();
    app.global::<PromptState>().on_skip_question(move || {
        if let Some(app) = weak.upgrade() {
            record(&app, None);
        }
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<PromptState>().on_queue_act(move |kind, id| {
        let Some(app) = weak.upgrade() else { return };
        let Some(tid) = active() else { return };
        let text = DATA.with(|d| d.borrow().queue.iter().find(|(q, _)| *q == id.as_str()).map(|(_, t)| t.clone()));
        let Some(text) = text else { return };
        match kind.as_str() {
            "now" => h.send(Command::Steer(text)),
            "edit" => app.set_prompt(text.into()),
            _ => {}
        }
        rpc("prompt.queue.delete", "thread/queue/delete", json!({ "threadId": tid, "queuedSubmissionId": id.to_string() }));
    });
    let weak = app.as_weak();
    let h = handle.clone();
    app.global::<PromptState>().on_banner_retry(move || {
        let Some(app) = weak.upgrade() else { return };
        clear_banner(&app);
        let text = DATA.with(|d| d.borrow().last_prompt.clone());
        if !text.is_empty() {
            h.send(Command::Prompt(text));
        }
    });
    let weak = app.as_weak();
    app.global::<PromptState>().on_banner_close(move || {
        if let Some(app) = weak.upgrade() {
            clear_banner(&app);
        }
    });
    let weak = app.as_weak();
    app.global::<PromptState>().on_compact(move || {
        let Some(app) = weak.upgrade() else { return };
        if let Some(tid) = active() {
            app.global::<PromptState>().set_compacting(true);
            rpc("prompt.compact", "thread/compact/start", json!({ "threadId": tid }));
        }
    });
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    match tag {
        "prompt.queue.list" => {
            if let Ok(v) = result {
                let rows: Vec<(String, String)> = v
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|q| {
                                let text = q.pointer("/input/0/text").and_then(Value::as_str).unwrap_or("").to_string();
                                (q.get("id").and_then(Value::as_str).unwrap_or("").to_string(), text)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                DATA.with(|d| d.borrow_mut().queue = rows);
                set_queue_model(app);
            }
        }
        "prompt.queue.add" | "prompt.queue.delete" => match result {
            Ok(_) => list_queue(),
            Err(e) => show_banner(app, &format!("キューを更新できません: {e}"), 1),
        },
        "prompt.compact" => {
            app.global::<PromptState>().set_compacting(false);
            if let Err(e) = result {
                show_banner(app, &format!("圧縮できません: {e}"), 1);
            }
        }
        _ => {}
    }
}

pub(crate) fn on_notify(method: &str) {
    if method == "thread/queue/changed" {
        list_queue();
    }
}

/// The turn ended: start the next queued message (a stopped turn leaves the queue alone).
pub(crate) fn on_turn_done(app: &AppWindow, thread: &str, interrupted: bool) {
    let first = DATA.with(|d| d.borrow().queue.first().cloned());
    if let Some((_, text)) = first.filter(|_| !interrupted && !thread.is_empty()) {
        crate::view::messages_with(app, |m| crate::view::push(m, "", "user", &text));
        rpc("prompt.queue.start", "thread/queue/start", json!({ "threadId": thread }));
    }
}
