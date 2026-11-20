//! What comes out of the core: work for the drivers.
//!
//! Directives are grouped by the driver that does them, so each driver
//! handles only its own list, and a host that forgets a driver fails to
//! compile rather than quietly dropping work.

use serde::{Deserialize, Serialize};

use crate::agent::SpawnCommand;
use crate::ids::{ChangeNo, ChatId, Ticket};
use crate::time::Mono;

/// Work for a driver.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Directive {
    /// For the agent driver.
    Agent(AgentDirective),
    /// For the timer driver.
    Timer(TimerDirective),
    /// For the policy driver.
    Policy(PolicyDirective),
    /// For the viewer driver.
    Viewer(ViewerDirective),
    /// For the storage driver. *Later.*
    Storage(StorageDirective),
    /// For the host's log.
    Report(Report),
}

/// Work for the agent driver.
///
/// The driver's side of the contract:
/// - **Events for one process are reported in the order they happened**,
///   and frames in the order their lines came, however long each took to
///   parse.
/// - **It delivers bytes, never failing on bad UTF-8**, and reads in
///   bounded steps, so one flooding adapter can't starve the rest.
/// - **Writes never block reading.** It keeps unsent bytes itself, and a
///   write error means the process is gone ([`AgentEvent::WriteFailed`]):
///   it throws the unsent bytes away, writes nothing more, and keeps
///   reading.
/// - **It keeps reading after a process exits**, until its output closes
///   or the core says [`AgentDirective::Forget`]: a process's last words
///   can arrive after its exit.
/// - **It signals process groups, never process numbers**, and never
///   leaves a zombie.
///
/// [`AgentEvent::WriteFailed`]: crate::event::AgentEvent::WriteFailed
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum AgentDirective {
    /// Start a process in a process group of its own, with its standard
    /// input, output and error piped. It's known as `process` from now on.
    Start {
        /// Its ticket.
        process: Ticket,
        /// How to start it.
        command: SpawnCommand,
    },
    /// Write these bytes to its standard input: whole messages, each
    /// ending in a newline.
    Write {
        /// The process.
        process: Ticket,
        /// The bytes.
        bytes: Vec<u8>,
    },
    /// Close its standard input: the gentlest way to stop an adapter.
    CloseInput {
        /// The process.
        process: Ticket,
    },
    /// Send a signal to its whole process group.
    Signal {
        /// The process.
        process: Ticket,
        /// The signal.
        signal: Signal,
    },
    /// Report whether anything is left in its process group.
    CheckGroup {
        /// The process.
        process: Ticket,
    },
    /// Stop reading it and let it go. It must already have exited, or
    /// been killed, or never started
    /// ([`AgentEvent::StartFailed`](crate::event::AgentEvent::StartFailed)).
    /// The core sends it within a fixed time of a process going, even if
    /// its output never closes or its group never empties: five times
    /// [`Timers::gone`](crate::host::Timers::gone), then
    /// [`Timers::kill_after`](crate::host::Timers::kill_after).
    Forget {
        /// The process.
        process: Ticket,
    },
}

/// A signal for a process group.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Signal {
    /// SIGTERM: please stop.
    Term,
    /// SIGKILL: stop now.
    Kill,
}

/// Work for the timer driver.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum TimerDirective {
    /// Wake the core at this time, replacing any earlier request, or never.
    /// There is only ever one: the core asks for its earliest deadline.
    WakeAt(Option<Mono>),
}

/// Work for the policy driver.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum PolicyDirective {
    /// Decide whether the agent may do this, and answer with
    /// [`Event::Policy`](crate::Event::Policy) quoting the ticket. Take as
    /// long as needed: the core carries on meanwhile, and uses the answer
    /// only if the request still stands.
    Decide {
        /// The ticket to answer with.
        ticket: Ticket,
        /// The chat asking.
        chat: ChatId,
        /// What the agent wants to do.
        request: PermissionRequest,
    },
    /// The request was withdrawn or ended before an answer: stop working
    /// on it. An answer that comes anyway is ignored.
    Withdrawn {
        /// Its ticket.
        ticket: Ticket,
    },
}

/// What the agent wants to do, for the policy to decide on. In feature 1
/// it's as the agent described it; the typed action ("run `cargo test` in
/// this folder") comes with the Permission Engine.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// What the agent calls it.
    pub title: String,
    /// What the tool was given, as the agent sent it.
    pub input: Option<String>,
}

/// Work for the viewer driver.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ViewerDirective {
    /// A chat changed: viewers watching it should read its changes, up to
    /// `latest`, with [`Core::read`](crate::Core::read).
    Changed {
        /// The chat.
        chat: ChatId,
        /// The number of its latest change.
        latest: ChangeNo,
    },
}

/// Work for the storage driver. *Later.*
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum StorageDirective {}

/// Something the host should log.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// How much it matters.
    pub level: Level,
    /// What happened, in CAF's words. Agent text in it is cleaned, and
    /// secrets such as the account email Claude's adapter sends are never
    /// included.
    pub message: String,
}

/// How much a report matters.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Level {
    /// Detail, for debugging.
    Debug,
    /// Worth knowing.
    Info,
    /// Something wasn't as it should be, such as an answer to a request
    /// CAF never sent.
    Warn,
}
