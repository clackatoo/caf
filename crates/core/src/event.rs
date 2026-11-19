//! What goes into the core: everything that happens.
//!
//! Each event is handed to [`Core::handle`](crate::Core::handle) with the
//! time it happened. The list is closed: drivers report only these.

use serde::{Deserialize, Deserializer, Serialize};

use crate::acp::Frame;
use crate::chat::{AppItem, NoAppItems, UserMessage};
use crate::core::OpenChat;
use crate::ids::{ChatId, QuestionId, Ticket, TurnId};
use crate::question::Answer;

/// Something that happened.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[serde(bound = "A: AppItem")]
pub enum Event<A: AppItem = NoAppItems> {
    /// The host asked for something. The host usually calls the core's
    /// methods, such as [`Core::send`](crate::Core::send), which record
    /// these. They're events too, so a recording holds everything.
    Host(HostCommand<A>),
    /// Something happened to an adapter process: the agent driver's
    /// report.
    Agent {
        /// The process, by the ticket of the directive that started it.
        process: Ticket,
        /// What happened.
        event: AgentEvent,
    },
    /// The time the core asked to be woken at has come: the timer
    /// driver's report.
    Woken,
    /// The policy driver's answer to "is this allowed?".
    Policy {
        /// The question's ticket.
        ticket: Ticket,
        /// The answer.
        answer: PolicyAnswer,
    },
    /// The storage driver's report. *Later.*
    Storage(StorageEvent),
}

/// What the host asked for.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[serde(bound = "A: AppItem")]
pub enum HostCommand<A: AppItem = NoAppItems> {
    /// Open a chat, under this id.
    OpenChat {
        /// The id the core gave it.
        chat: ChatId,
        /// What to open.
        open: OpenChat,
    },
    /// Send a message, as this turn.
    Send {
        /// The chat.
        chat: ChatId,
        /// The id the core gave the turn.
        turn: TurnId,
        /// The message.
        message: UserMessage,
    },
    /// Stop the chat's running turn, and drop its waiting ones.
    Stop {
        /// The chat.
        chat: ChatId,
    },
    /// Answer a question.
    Answer {
        /// The question.
        question: QuestionId,
        /// The answer.
        answer: Answer,
    },
    /// Close a chat for good.
    CloseChat {
        /// The chat.
        chat: ChatId,
    },
    /// Stop everything: close every session and stop every adapter.
    Shutdown,
    /// Add an app item. *Later.*
    AddAppItem(A),
}

/// What happened to an adapter process, in the order it happened.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
pub enum AgentEvent {
    /// It started.
    Started {
        /// Its process id, for logs only: CAF never signals by number.
        pid: u32,
    },
    /// It couldn't be started.
    StartFailed {
        /// What the system said.
        error: String,
    },
    /// A line it wrote, read as JSON-RPC.
    Frame(Frame),
    /// Bytes it wrote to its error output. The core keeps the last few
    /// lines, for explaining failures, and looks for sign-in prompts and
    /// secrets in them.
    ///
    /// It's written as an array of bytes, and read from one or from text,
    /// so a recording can hold it as text a person can read.
    Stderr(#[serde(deserialize_with = "bytes_or_text")] Vec<u8>),
    /// Its output ended.
    OutputClosed,
    /// Writing to it failed: it's gone, though it may still be writing.
    /// Unsent bytes have been thrown away.
    WriteFailed {
        /// What the system said.
        error: String,
    },
    /// It exited. Output it wrote before exiting may still follow.
    Exited(Exit),
    /// Whether anything is left in its process group: the answer to
    /// [`AgentDirective::CheckGroup`](crate::directive::AgentDirective::CheckGroup).
    Group {
        /// True if every process in it has gone.
        empty: bool,
    },
}

/// How a process exited.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exit {
    /// With this code.
    Code(i32),
    /// Killed by this signal.
    Signal(i32),
}

/// The policy's answer to "is this allowed?".
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyAnswer {
    /// Allowed, once.
    Allow,
    /// Refused.
    Refuse,
    /// Ask the user. If the host can't show permission questions, this is
    /// a refusal.
    AskUser,
}

/// The storage driver's report. *Later.*
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
pub enum StorageEvent {}

/// Bytes, read from an array of bytes or from text.
fn bytes_or_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Text(String),
        Bytes(Vec<u8>),
    }
    Ok(match Either::deserialize(deserializer)? {
        Either::Text(text) => text.into_bytes(),
        Either::Bytes(bytes) => bytes,
    })
}
