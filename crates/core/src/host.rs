//! What the host tells CAF about itself.
//!
//! The host never speaks ACP. It says, in plain terms, what it can show
//! and how it wants things done, and CAF works out what to declare to
//! each agent. Where the host can't do something an agent behaves better
//! for having declared, CAF declares it anyway and covers the gap itself:
//! forms are declared even to a host that can't show them, so Claude
//! keeps its question tool, and CAF answers any form itself.

use std::hash::BuildHasher;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The host's description of itself, given when the core is made.
///
/// Make one with [`Host::new`] and set any field to change it. Like every
/// settings struct in CAF, it's `non_exhaustive`, so a field added later
/// breaks nobody.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Host {
    /// The host's name, such as `clackatoo`, told to every agent.
    pub name: String,
    /// The host's version, told to every agent.
    pub version: String,
    /// What it can show the user.
    pub shows: Shows,
    /// Which chats share an adapter process.
    pub grouping: Grouping,
    /// How big things may get.
    pub limits: Limits,
    /// How long to wait for things.
    pub timers: Timers,
    /// Where the random part of each new chat's id comes from: the core
    /// makes no randomness of its own, so replaying a run gives out the
    /// same ids. [`Host::new`] draws a fresh one, different in every run,
    /// below 2^53 as every number CAF writes is; set it only in tests.
    /// Two runs given the same seed may give two chats the same id, which
    /// a restore then refuses
    /// ([`Refused::Loaded`](crate::Refused::Loaded)).
    ///
    /// Recordings made before it was added don't have it: they read as 0,
    /// and never made a chat id from it.
    #[serde(default)]
    pub seed: u64,
}

/// What the host can show the user. A `false` here never stops CAF
/// working: CAF answers or leaves out what can't be shown.
#[non_exhaustive]
#[allow(clippy::struct_excessive_bools)] // one plain answer each
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Grouping {
    /// One process per agent, shared by all its chats: what clackatoo
    /// does today. The default.
    PerAgent,
    /// One process per chat: chats never affect each other, at the cost
    /// of memory. What clackatoo's sandboxed chats will need (CLACKA-229).
    PerChat,
}

/// Size limits. Defaults are what clackatoo uses today.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
    /// How many changes each chat keeps for viewers. A viewer further
    /// behind gets the whole chat instead. 4096.
    pub kept_changes: usize,
}

/// How long to wait for things. Every timer is the host's to set:
/// tests set them all. Defaults are what clackatoo uses today, or, for
/// the new watchdogs, what its timings suggest.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Timers {
    /// How long an adapter may take to answer `initialize` before the
    /// chats waiting on it are told it's slow. 30 s. (Claude answers in
    /// about 240 ms.)
    #[cfg_attr(feature = "ts", ts(as = "crate::ts::Duration"))]
    pub initialize: Duration,
    /// How long a session may take to open before the chat asks the user
    /// whether to keep waiting, retry or stop. Every message an agent
    /// replays while loading restarts it. 30 s.
    #[cfg_attr(feature = "ts", ts(as = "crate::ts::Duration"))]
    pub session_open: Duration,
    /// How long after Stop the agent may take to end the turn, before
    /// CAF ends it as failed and restarts the session. 15 s.
    #[cfg_attr(feature = "ts", ts(as = "crate::ts::Duration"))]
    pub stop: Duration,
    /// How long a turn the agent started by itself may be quiet, with no
    /// tool running, before CAF takes it to have ended: ACP doesn't say
    /// when one ends. Long, as a turn ended too soon splits in two when
    /// the agent goes on. 1 min, as clackatoo.
    #[cfg_attr(feature = "ts", ts(as = "crate::ts::Duration"))]
    pub quiet: Duration,
    /// How long any other request CAF sends may go unanswered (closing a
    /// session, changing a setting) before the adapter is treated as stuck
    /// and restarted. A prompt has no such limit: an agent may work on one
    /// for an hour. 60 s.
    #[cfg_attr(feature = "ts", ts(as = "crate::ts::Duration"))]
    pub request: Duration,
    /// How long CAF keeps listening to an adapter that has gone, for its
    /// last words. 2 s.
    #[cfg_attr(feature = "ts", ts(as = "crate::ts::Duration"))]
    pub gone: Duration,
    /// How long a gentle stop waits before sending SIGTERM, and SIGTERM
    /// before SIGKILL. 2 s.
    #[cfg_attr(feature = "ts", ts(as = "crate::ts::Duration"))]
    pub kill_after: Duration,
    /// How often CAF checks whether an old adapter's processes have all
    /// gone, before starting a new one on the same session. 50 ms.
    #[cfg_attr(feature = "ts", ts(as = "crate::ts::Duration"))]
    pub group_poll: Duration,
    /// How long a sign-in may wait for the user to finish it before CAF
    /// stops it, so one nobody finishes doesn't hold chats waiting on it.
    /// 10 min.
    /// Recordings made before it was added don't have it: they read as
    /// the default.
    #[serde(default = "sign_in_default")]
    #[cfg_attr(feature = "ts", ts(as = "Option<crate::ts::Duration>", optional))]
    pub sign_in: Duration,
    /// How long after a chat changes it's saved, so a burst of changes,
    /// such as the agent streaming its words, is saved as one. A crash
    /// loses at most this much. A turn ending, a chat being put away and
    /// shutting down save at once. 1 s.
    /// Recordings made before it was added read as the default.
    #[serde(default = "save_default")]
    #[cfg_attr(feature = "ts", ts(as = "Option<crate::ts::Duration>", optional))]
    pub save: Duration,
}

/// [`Timers::sign_in`]'s default, for hosts written before it.
fn sign_in_default() -> Duration {
    Duration::from_mins(10)
}

/// [`Timers::save`]'s default, for hosts written before it.
fn save_default() -> Duration {
    Duration::from_secs(1)
}

impl Host {
    /// A host of this name and version, with the defaults for the rest,
    /// and a fresh [`seed`](Host::seed), drawn from the randomness the
    /// standard library seeds its hash maps with. Change any other field
    /// by setting it.
    ///
    /// Drawing the seed is the one thing caf-core asks of the system, and
    /// it's here, as the host describes itself, never in the core's rules:
    /// a seed every host had to remember to draw would, the one time it
    /// wasn't, give two runs the same chat ids.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            shows: Shows::default(),
            grouping: Grouping::PerAgent,
            limits: Limits::default(),
            timers: Timers::default(),
            // 53 bits, as every number CAF writes is below 2^53.
            seed: std::hash::RandomState::new().hash_one(0_u8) >> 11,
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
            kept_changes: 4096,
        }
    }
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            initialize: Duration::from_secs(30),
            session_open: Duration::from_secs(30),
            stop: Duration::from_secs(15),
            quiet: Duration::from_mins(1),
            request: Duration::from_secs(60),
            gone: Duration::from_secs(2),
            kill_after: Duration::from_secs(2),
            group_poll: Duration::from_millis(50),
            sign_in: sign_in_default(),
            save: save_default(),
        }
    }
}
