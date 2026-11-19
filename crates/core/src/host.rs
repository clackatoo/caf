//! What the host tells CAF about itself.
//!
//! The host never speaks ACP. It says, in plain terms, what it can show
//! and how it wants things done, and CAF works out what to declare to
//! each agent. Where the host can't do something an agent behaves better
//! for having declared, CAF declares it anyway and covers the gap itself:
//! forms are declared even to a host that can't show them, so Claude
//! keeps its question tool, and CAF answers any form itself.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The host's description of itself, given when the core is made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// What it can show the user.
    pub shows: Shows,
    /// Which chats share an adapter process.
    pub grouping: Grouping,
    /// How big things may get.
    pub limits: Limits,
    /// How long to wait for things.
    pub timers: Timers,
}

/// What the host can show the user. A `false` here never stops CAF
/// working: CAF answers or leaves out what can't be shown.
#[allow(clippy::struct_excessive_bools)] // one plain answer each
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shows {
    /// Permission questions ("may the agent run this?"). Without them,
    /// whatever the policy driver says is final, and "ask the user"
    /// becomes a refusal.
    pub permission_questions: bool,
    /// Forms the agent asks the user to fill in. *Later.*
    pub forms: bool,
    /// Links the agent asks the user to open, such as a sign-in page.
    /// *Later.*
    pub links: bool,
    /// The agent's thinking. Without it, thinking isn't kept.
    pub thinking: bool,
}

/// Which chats share an adapter process.
///
/// One adapter process can serve many sessions, and Claude's adapter
/// starts a separate `claude` program for each session anyway. Sharing
/// saves memory (Claude's adapter is about 115 MB). The cost is that
/// restarting the process to rescue one session affects the others. CAF
/// tells every affected chat when that happens, whichever is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Grouping {
    /// One process per agent, shared by all its chats: what clackatoo
    /// does today. The default.
    PerAgent,
    /// One process per chat: chats never affect each other, at the cost
    /// of memory. What clackatoo's sandboxed chats will need (CLACKA-229).
    PerChat,
}

/// Size limits. Defaults are what clackatoo uses today.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// The biggest message accepted from an agent. A longer one is
    /// dropped, and refused if it was a request, so nothing waits on it.
    /// 64 MiB.
    pub biggest_message: usize,
    /// How many messages may wait while a turn runs, per chat. 8.
    pub queued_messages: usize,
    /// How many lines of an agent's error output to keep for explaining a
    /// failure, and how long each may be. 4 lines of 300 bytes.
    pub stderr_lines: usize,
    /// See [`Limits::stderr_lines`].
    pub stderr_line_bytes: usize,
    /// The longest failure text from an agent to show. 2 KiB.
    pub failure_text: usize,
}

/// How long to wait for things. Every timer is the host's to set:
/// tests set them all. Defaults are what clackatoo uses today, or, for
/// the new watchdogs, what its timings suggest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timers {
    /// How long an adapter may take to answer `initialize` before the
    /// chats waiting on it are told it's slow. 30 s. (Claude answers in
    /// about 240 ms.)
    pub initialize: Duration,
    /// How long a session may take to open before the chat asks the user
    /// whether to keep waiting, retry or stop. Every message an agent
    /// replays while loading restarts it. 30 s.
    pub session_open: Duration,
    /// How long after Stop the agent may take to end the turn, before
    /// CAF ends it as failed and restarts the session. 15 s.
    pub stop: Duration,
    /// How long CAF keeps listening to an adapter that has gone, for its
    /// last words. 2 s.
    pub gone: Duration,
    /// How long a gentle stop waits before sending SIGTERM, and SIGTERM
    /// before SIGKILL. 2 s.
    pub kill_after: Duration,
    /// How often CAF checks whether an old adapter's processes have all
    /// gone, before starting a new one on the same session. 50 ms.
    pub group_poll: Duration,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            shows: Shows::default(),
            grouping: Grouping::PerAgent,
            limits: Limits::default(),
            timers: Timers::default(),
        }
    }
}

impl Default for Shows {
    fn default() -> Self {
        Self {
            permission_questions: true,
            forms: false,
            links: false,
            thinking: true,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            biggest_message: 64 << 20,
            queued_messages: 8,
            stderr_lines: 4,
            stderr_line_bytes: 300,
            failure_text: 2 << 10,
        }
    }
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            initialize: Duration::from_secs(30),
            session_open: Duration::from_secs(30),
            stop: Duration::from_secs(15),
            gone: Duration::from_secs(2),
            kill_after: Duration::from_secs(2),
            group_poll: Duration::from_millis(50),
        }
    }
}
