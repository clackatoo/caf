//! The viewer wire: the messages between CAF and one viewer.
//!
//! A **viewer** is anything that shows chats: clackatoo's window, a web
//! page through the TypeScript caf-sdk, the debugger. It speaks to CAF
//! with [`FromViewer`] and hears back [`ToViewer`]. This module only
//! defines the messages and the viewer's side of following a chat
//! ([`ChatCopy`]): it does no I/O, so every transport and every language
//! uses the same messages. `caf_driver_tokio::viewer::serve` is CAF's side
//! for tokio.
//!
//! # A link, in order
//!
//! 1. The viewer sends [`FromViewer::Hello`] with the versions it speaks.
//!    Anything else first ends the link.
//! 2. CAF answers [`ToViewer::Welcome`] with the newest version both
//!    speak, the agents on offer and the open chats; or
//!    [`ToViewer::NoCommonVersion`], and ends the link.
//! 3. The viewer watches chats ([`FromViewer::Watch`]): it gets each one
//!    whole ([`ToViewer::Picture`]), then its changes
//!    ([`ToViewer::Changes`]). It may act on them: send, stop, answer,
//!    close. Each act quotes a [`RequestNo`] the viewer chose, and
//!    [`ToViewer::Done`] says how it went.
//!
//! # Opening is the host's decision
//!
//! A viewer never sends an [`OpenChat`]: its MCP servers are programs to
//! run, so a viewer that could send one could run programs on the
//! machine. It sends an [`OpenRequest`] (an agent CAF offered, a folder,
//! a new session or one to continue), and the host turns it into an
//! `OpenChat` or refuses with [`Outcome::NotAllowed`].
//!
//! # Compatibility
//!
//! - The two ends use the older of their newest versions.
//! - What a version sends never changes. A later version may add
//!   messages, variants and fields, and a field it adds is always one an
//!   older reader can do without.
//! - A viewer that meets a message it doesn't know ignores it.
//! - A viewer that meets a [`Change`] it doesn't know asks for the chat
//!   whole again: [`FromViewer::Unwatch`], then [`FromViewer::Watch`]
//!   with no `since`.
//! - A viewer that meets a reason it doesn't know, such as a new
//!   [`Refused`] or [`Lost`], shows it as unknown, with its name.
//! - `samples/viewer/v1.json` holds one of every message of version 1.
//!   A test checks it, so version 1 can't change by accident.
//!
//! # Numbers
//!
//! Every number on the wire is below 2^53, so JavaScript reads it
//! exactly: CAF's ids count up from 1, and times are milliseconds.
//!
//! [`Change`]: crate::change::Change
//! [`OpenChat`]: crate::OpenChat
//! [`Lost`]: crate::chat::Lost

use std::ops::RangeInclusive;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::change::Numbered;
use crate::chat::{AppItem, Chat, NoAppItems, UserMessage};
use crate::core::{Refused, SessionChoice};
use crate::ids::{AgentName, ChangeNo, ChatId, QuestionId, TurnId};
use crate::question::Answer;

/// The versions of the viewer wire this CAF speaks.
pub const VERSIONS: RangeInclusive<u32> = 1..=1;

/// A message from a viewer to CAF.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum FromViewer {
    /// The first message: the versions the viewer speaks.
    Hello {
        /// The oldest.
        min: u32,
        /// The newest.
        max: u32,
    },
    /// Follow a chat. CAF sends it whole first, unless `since` is a
    /// change CAF still has, and then its changes as they happen.
    Watch {
        /// The chat.
        chat: ChatId,
        /// The last change the viewer has, if it has the chat already,
        /// such as after reconnecting.
        since: Option<ChangeNo>,
    },
    /// Stop following a chat.
    Unwatch {
        /// The chat.
        chat: ChatId,
    },
    /// Ask to open a chat. The host decides.
    Open {
        /// The viewer's number for this request.
        request: RequestNo,
        /// What it asks for.
        open: OpenRequest,
    },
    /// Send a message. See [`Core::send`](crate::Core::send).
    Send {
        /// The viewer's number for this request.
        request: RequestNo,
        /// The chat.
        chat: ChatId,
        /// The message.
        message: UserMessage,
    },
    /// Stop the chat's turn. See [`Core::stop`](crate::Core::stop).
    Stop {
        /// The viewer's number for this request.
        request: RequestNo,
        /// The chat.
        chat: ChatId,
    },
    /// Answer a question. See [`Core::answer`](crate::Core::answer).
    Answer {
        /// The viewer's number for this request.
        request: RequestNo,
        /// The question.
        question: QuestionId,
        /// The answer.
        answer: Answer,
    },
    /// Close a chat. See [`Core::close_chat`](crate::Core::close_chat).
    Close {
        /// The viewer's number for this request.
        request: RequestNo,
        /// The chat.
        chat: ChatId,
    },
}

/// A message from CAF to a viewer.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem")]
pub enum ToViewer<A: AppItem = NoAppItems> {
    /// The answer to [`FromViewer::Hello`].
    Welcome {
        /// The version both will speak: the newest both know.
        version: u32,
        /// The agents a viewer may ask to open a chat with.
        agents: Vec<AgentName>,
        /// The open chats, in the order they were opened.
        chats: Vec<ChatSummary>,
    },
    /// No version in common: CAF ends the link after sending this.
    NoCommonVersion {
        /// The oldest CAF speaks.
        min: u32,
        /// The newest CAF speaks.
        max: u32,
    },
    /// The open chats changed, because a chat was opened or closed by any
    /// viewer or by the host. The whole list, each time.
    Chats(Vec<ChatSummary>),
    /// A watched chat, whole: the first answer to [`FromViewer::Watch`],
    /// and again whenever the viewer has fallen behind. It may come at any
    /// time, with any number: it replaces what the viewer had.
    Picture {
        /// The chat.
        chat: Chat<A>,
        /// The last change it includes.
        latest: ChangeNo,
    },
    /// A watched chat's changes after the last ones sent, oldest first.
    Changes {
        /// The chat.
        chat: ChatId,
        /// The changes.
        changes: Vec<Numbered<A>>,
    },
    /// A watched chat is closed, or CAF has shut down: nothing more comes
    /// for it.
    Ended {
        /// The chat.
        chat: ChatId,
    },
    /// How a request went.
    Done {
        /// The viewer's number for it.
        request: RequestNo,
        /// How it went.
        outcome: Outcome,
    },
}

/// An open chat, as the list of chats shows it: enough to draw the list
/// without watching every chat. Names and the like are the app's own.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ChatSummary {
    /// Its id.
    pub id: ChatId,
    /// The agent it's with.
    pub agent: AgentName,
    /// The folder the agent works in.
    pub cwd: PathBuf,
}

impl ChatSummary {
    /// A chat's summary.
    #[must_use]
    pub fn of<A: AppItem>(chat: &Chat<A>) -> Self {
        let _ = chat;
        todo!()
    }
}

/// A viewer's number for one of its requests, quoted back in
/// [`ToViewer::Done`]. The viewer chooses them; CAF never reads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RequestNo(pub u64);

/// What a viewer asks for when it asks to open a chat. The host turns it
/// into an [`OpenChat`](crate::OpenChat), adding anything else, such as
/// its tools, or refuses.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OpenRequest {
    /// One of the agents [`ToViewer::Welcome`] offered.
    pub agent: AgentName,
    /// The folder, which must be absolute.
    pub cwd: PathBuf,
    /// A new session, or one to continue.
    pub session: SessionChoice,
}

/// How a viewer's request went.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Outcome {
    /// The chat was opened, with this id. It's in the next
    /// [`ToViewer::Chats`] too.
    Opened(ChatId),
    /// The message was taken, as this turn.
    Sent(TurnId),
    /// Done: stopped, answered or closed.
    Done,
    /// The core refused it, and nothing changed.
    Refused(Refused),
    /// The host doesn't let viewers do this.
    NotAllowed,
}

/// A viewer's copy of one chat, and how far it has read: what a viewer
/// keeps for each chat it watches.
///
/// Every SDK has one, and every one turns the same messages into the same
/// chat: the conformance fixtures in `fixtures/apply/` check it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatCopy<A: AppItem = NoAppItems> {
    chat: Chat<A>,
    latest: ChangeNo,
}

/// A copy can't follow the changes it was given: a change is missing, or
/// one names something the copy doesn't have. Ask for the chat whole
/// again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeedPicture;

impl<A: AppItem> ChatCopy<A> {
    /// A copy of a chat sent whole, as in [`ToViewer::Picture`].
    #[must_use]
    pub fn new(chat: Chat<A>, latest: ChangeNo) -> Self {
        Self { chat, latest }
    }

    /// The chat, as far as the copy has read.
    #[must_use]
    pub fn chat(&self) -> &Chat<A> {
        &self.chat
    }

    /// The last change the copy has.
    #[must_use]
    pub fn latest(&self) -> ChangeNo {
        self.latest
    }

    /// Applies changes, oldest first, as in [`ToViewer::Changes`].
    /// Changes it already has are skipped.
    ///
    /// # Errors
    ///
    /// [`NeedPicture`] if a change is missing before the first new one,
    /// or one doesn't fit the chat ([`Chat::apply`]). The copy keeps the
    /// changes it applied before then, and is behind: ask for the chat
    /// whole again.
    pub fn take(&mut self, changes: &[Numbered<A>]) -> Result<(), NeedPicture> {
        for each in changes {
            if each.no <= self.latest {
                continue;
            }
            if each.no.0 != self.latest.0 + 1 {
                return Err(NeedPicture);
            }
            self.chat.apply(&each.change).map_err(|_| NeedPicture)?;
            self.latest = each.no;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
