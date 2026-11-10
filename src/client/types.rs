//! Protocol-level types: events sent to the UI, commands from it, settings and model data.

use serde_json::Value;
use std::sync::mpsc::Sender;


#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Info,
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub id: String,
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadInfo {
    pub id: String,
    pub title: String,
    pub cwd: String,
    /// In the server-side "Pinned" section.
    pub pinned: bool,
    /// `readStates[id].firstUnread` is set.
    pub unread: bool,
    /// Revision of the read state, needed to mark it read or unread.
    pub revision: String,
    /// 0 idle, 1 running, 3 system error, 4 waiting for approval or input.
    pub status: i32,
    /// parentThreadId (a child session), empty for top-level sessions.
    pub parent: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
    pub hidden: bool,
    pub is_default: bool,
    /// `availableAccessPrograms.cyber` as advertised by model/list for this caller
    /// ("standard", "daybreakBlue", "daybreakRed").
    pub programs: Vec<String>,
    /// Service tiers (id, name, description); the first speed option is always "standard".
    pub service_tiers: Vec<(String, String, String)>,
    pub default_service_tier: Option<String>,
    /// Set for models only found by probing a newer client version: the lowest version
    /// that listed them. Such models are "unverified" in InoCode.
    pub min_version: Option<String>,
}

impl ModelInfo {
    /// The model advertises a Daybreak access program for this caller.
    pub fn daybreak(&self) -> bool {
        self.programs.iter().any(|p| p.starts_with("daybreak"))
    }

    /// Program to request for a turn: the Daybreak program when the choice is on,
    /// "standard" when off, and nothing when the model advertises no programs.
    pub fn cyber_program(&self, daybreak: bool) -> Option<&'static str> {
        let has = |p: &str| self.programs.iter().any(|x| x == p);
        if daybreak {
            if has("daybreakBlue") {
                Some("daybreakBlue")
            } else if has("daybreakRed") {
                Some("daybreakRed")
            } else {
                None
            }
        } else if has("standard") {
            Some("standard")
        } else {
            None
        }
    }

    /// The service tier that the "Fast" row selects (the model's single faster tier).
    pub fn fast_tier(&self) -> Option<&str> {
        match self.service_tiers.as_slice() {
            [(id, _, _)] => Some(id.as_str()),
            _ => None,
        }
    }

    /// The model has a "Daybreak" variant row: a standard mode plus a Daybreak program.
    /// Daybreak-only models (no "standard") are listed as plain rows.
    pub fn has_daybreak_variant(&self) -> bool {
        self.programs.iter().any(|p| p == "standard") && self.daybreak()
    }

    /// Program for a turn: the Daybreak program when the choice is on, "standard" when it is
    /// off, and for a model that only has a Daybreak program, that program.
    pub fn program_for(&self, daybreak: bool) -> Option<&'static str> {
        self.cyber_program(daybreak).or_else(|| {
            if self.programs.iter().any(|p| p == "standard") {
                None
            } else {
                self.cyber_program(true)
            }
        })
    }
}

/// Permission modes of the composer's permission dropdown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Perm {
    /// "Custom (config.toml)": no override, the server's own configuration applies.
    Server,
    /// "Ask for approval"
    Ask,
    /// "Approve for me": an automatic reviewer answers approval requests.
    Guardian,
    /// "Full access"
    Full,
}

impl Perm {
    pub(super) fn policy(self) -> Option<&'static str> {
        match self {
            Perm::Server => None,
            Perm::Ask | Perm::Guardian => Some("on-request"),
            Perm::Full => Some("never"),
        }
    }
    pub(super) fn sandbox_mode(self) -> Option<&'static str> {
        match self {
            Perm::Server => None,
            Perm::Ask | Perm::Guardian => Some("workspace-write"),
            Perm::Full => Some("danger-full-access"),
        }
    }
    /// Reviewer as spelled in thread/settings/update and in turn/start.
    pub(super) fn reviewer(self, turn: bool) -> Option<&'static str> {
        match self {
            Perm::Server => None,
            Perm::Guardian => Some(if turn { "auto_review" } else { "guardian_subagent" }),
            Perm::Ask | Perm::Full => Some("user"),
        }
    }
    /// What the server reports (policy, sandbox type, reviewer) mapped back to a mode.
    pub fn from_server(approval: &str, sandbox: &str, reviewer: &str) -> Perm {
        match (approval, sandbox, reviewer) {
            ("never", "dangerFullAccess", _) => Perm::Full,
            ("on-request", "workspaceWrite", "auto_review") => Perm::Guardian,
            ("on-request", "workspaceWrite", _) => Perm::Ask,
            _ => Perm::Server,
        }
    }
}

/// Thread settings chosen in the UI (the desktop client keeps these as the thread's
/// "latest thread settings" and pushes changes with thread/settings/update).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// None: not chosen. Some(None): Standard speed. Some(Some(id)): that service tier.
    pub service_tier: Option<Option<String>>,
    pub perm: Option<Perm>,
    pub daybreak: bool,
    /// Model catalog from model/list, used to resolve the program sent with each turn.
    pub programs: Vec<ModelInfo>,
    pub cwd: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            model: None,
            effort: None,
            service_tier: None,
            perm: None,
            daybreak: false,
            programs: Vec::new(),
            cwd: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Models(Vec<ModelInfo>),
    Account { logged_in: bool, label: String },
    /// Context usage of the active thread (percent of the window) and a text summary.
    Usage { percent: i32, text: String },
    RateLimits(String),
    /// configRequirements/read: allowed approval policies / sandbox modes (None = unrestricted).
    Requirements { policies: Option<Vec<String>>, sandboxes: Option<Vec<String>> },
    /// thread.daybreakEnabled as stored by the server.
    Daybreak(bool),
    /// What the server reports for the thread that was just started or resumed.
    Applied { model: String, effort: String, approval: String, sandbox: String, reviewer: String, cwd: String },
    Status(String, Level),
    Threads(Vec<ThreadInfo>),
    ThreadActive { id: String, title: String },
    /// Replace the conversation (new or resumed thread).
    History(Vec<Msg>),
    AssistantStart,
    Delta(String),
    /// Insert or update a command / tool row.
    Tool { id: String, text: String },
    TurnStarted,
    TurnDone { interrupted: bool },
    Approval { key: String, title: String, detail: String },
    ApprovalResolved(String),
    Error(String),
    /// Which transport is in use ("WS" or "stdio").
    Transport(String),
    /// Response to a Command::Rpc (the tag is the caller's); the error is the JSON-RPC message.
    Rpc { tag: String, result: Result<Value, String> },
    /// A server notification the client does not handle itself (the `inocode/*` ones).
    Notify { method: String, params: Value },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

impl Decision {
    pub fn parse(s: &str) -> Decision {
        match s {
            "accept" => Decision::Accept,
            "session" => Decision::AcceptForSession,
            "cancel" => Decision::Cancel,
            _ => Decision::Decline,
        }
    }
    pub(super) fn wire(self) -> &'static str {
        match self {
            Decision::Accept => "accept",
            Decision::AcceptForSession => "acceptForSession",
            Decision::Decline => "decline",
            Decision::Cancel => "cancel",
        }
    }
}

pub enum Command {
    Prompt(String),
    NewThread,
    NewThreadIn(String),
    SetSettings(Settings),
    SetDaybreak(bool),
    Login,
    OpenThread(String),
    Interrupt,
    Approve { key: String, decision: Decision },
    SwitchTransport,
    /// Generic request for the screen tabs; answered with Event::Rpc.
    #[allow(dead_code)]
    Rpc { tag: String, method: String, params: Value },
    Quit,
}

pub struct Handle {
    pub(super) tx: Sender<Command>,
    pub(super) thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Stop the client and wait until the app-server child it started is gone.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(t) = self.thread.lock().ok().and_then(|mut g| g.take()) {
            let _ = t.join();
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub ws_url: String,
    pub stdio_program: String,
    pub use_stdio: bool,
}
