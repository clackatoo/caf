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
//!    speak, the agents on offer, the open chats, and which run of CAF
//!    this is; or [`ToViewer::NoCommonVersion`], and ends the link.
//!    Right after the welcome, before anything else, comes
//!    [`ToViewer::Agents`], where every agent stands, so a viewer knows
//!    that from the start.
//! 3. The viewer watches chats ([`FromViewer::Watch`]): it gets each one
//!    whole ([`ToViewer::Picture`]), then its changes
//!    ([`ToViewer::Changes`]). It may act on them: send, stop, answer,
//!    choose their settings, close. Each act quotes a [`RequestNo`] the viewer chose, and
//!    [`ToViewer::Done`] says how it went.
//!
//! # Opening is the host's decision
//!
//! A viewer never sends an [`OpenChat`]: an MCP server may be a program
//! to run on the machine, or a server reached with secret headers, so a
//! viewer that could send one could run or reach anything. It sends an
//! [`OpenRequest`] (an agent CAF offered, a folder, a new session or one
//! to continue), and the host turns it into an `OpenChat` or refuses
//! with [`Outcome::NotAllowed`].
//!
//! # Resuming
//!
//! A viewer that reconnects may watch a chat again with `since`, the
//! last change it has, and get only what followed. That holds only
//! within one run of CAF: [`ToViewer::Welcome`]'s `run` is different
//! every time CAF starts, and its changes are numbered again. A viewer
//! whose `run` differs from the one it was watching drops its copies
//! and watches with no `since`.
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

use crate::agent::AgentStatus;
use crate::change::Numbered;
use crate::chat::{AppItem, Chat, NoAppItems, UserMessage};
use crate::core::{Refused, SessionChoice};
use crate::ids::{AgentName, ChangeNo, ChatId, QuestionId, Shelf, TurnId};
use crate::question::Answer;
use crate::save::{CannotRestore, SavedChat};
use crate::settings::SettingChoice;
use crate::usage::{BadQuery, ModelPrice, Summary, UsageQuery};

/// The versions of the viewer wire this CAF speaks.
pub const VERSIONS: RangeInclusive<u32> = 1..=1;

/// A message from a viewer to CAF.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
        /// such as after reconnecting to the same run of CAF (see
        /// [`ToViewer::Welcome`]'s `run`). After a restart it's `None`.
        since: Option<ChangeNo>,
    },
    /// Stop following a chat. What was already on its way for it may
    /// still come.
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
    /// Unarchive a chat. See [`Core::unarchive`](crate::Core::unarchive).
    /// Allowed as the rest of acting on chats is ([`Rights::act`]).
    Unarchive {
        /// The viewer's number for this request.
        request: RequestNo,
        /// The chat.
        chat: ChatId,
    },
    /// Choose a setting of a chat's agent, such as its model. See
    /// [`Core::set_setting`](crate::Core::set_setting). Allowed as the
    /// rest of acting on chats is ([`Rights::act`]).
    Set {
        /// The viewer's number for this request.
        request: RequestNo,
        /// The chat.
        chat: ChatId,
        /// The choice.
        choice: SettingChoice,
    },
    /// Ask to restore a saved chat, as [`ToViewer::Saved`] lists them.
    /// The host decides, as for opening one.
    Restore {
        /// The viewer's number for this request.
        request: RequestNo,
        /// The chat.
        chat: ChatId,
        /// Where it's kept.
        shelf: Shelf,
    },
    /// Do something about an agent: install it, sign in or out. The host
    /// decides whether viewers may.
    Agent {
        /// The viewer's number for this request.
        request: RequestNo,
        /// The agent.
        agent: AgentName,
        /// What to do.
        action: AgentAction,
    },
    /// Ask for a usage summary: answered with [`ToViewer::Usage`], or a
    /// [`ToViewer::Done`] saying why not. It covers every CAF app on the
    /// machine, so it's allowed only with [`Rights::usage`].
    Usage {
        /// The viewer's number for this request.
        request: RequestNo,
        /// What it covers.
        query: UsageQuery,
    },
    /// Set the user's own price for a model, or with `None` clear it, so
    /// the next layer prices it again: kept beside the usage ledger, for
    /// every CAF app. Allowed only with [`Rights::usage`].
    SetPrice {
        /// The viewer's number for this request.
        request: RequestNo,
        /// The model, by the agent's id for it, as a [`Bucket`] names it.
        ///
        /// [`Bucket`]: crate::usage::Bucket
        model: String,
        /// Its price, or `None`.
        price: Option<ModelPrice>,
    },
}

/// Something a viewer may ask CAF to do about an agent. See the core's
/// methods of the same names, such as [`Core::install`](crate::Core::install).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum AgentAction {
    /// Install it.
    Install,
    /// Stop installing it.
    StopInstall,
    /// Start signing in to it.
    SignIn,
    /// Give its sign-in what the user pasted.
    Paste(String),
    /// Stop signing in to it.
    StopSignIn,
    /// Sign out of it.
    SignOut,
    /// Learn what settings it offers. See
    /// [`Core::learn_settings`](crate::Core::learn_settings).
    LearnSettings,
    /// Look at the machine again, as the user may have changed it, such as
    /// by installing Node: what's installed and what the machine has, for
    /// every agent, not only this one. See
    /// [`Core::check_agents`](crate::Core::check_agents).
    CheckAgain,
}

/// A message from CAF to a viewer.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem")]
#[expect(
    clippy::large_enum_variant,
    reason = "a picture is a whole chat, sent and dropped: boxing it changes no wire, only Rust code matching it, for one word of size"
)]
pub enum ToViewer<A: AppItem = NoAppItems> {
    /// The answer to [`FromViewer::Hello`].
    Welcome {
        /// The version both will speak: the newest both know.
        version: u32,
        /// The agents a viewer may ask to open a chat with.
        agents: Vec<AgentName>,
        /// The open chats, in the order they were opened.
        chats: Vec<ChatSummary>,
        /// Different every time CAF starts, so a viewer can tell a
        /// restarted CAF from the one it was watching. A `since` holds
        /// only within one run.
        run: String,
        /// What the host lets this viewer do, so it offers only that.
        may: Rights,
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
    /// A watched chat is gone: put away, a saved chat closed and so put
    /// away included, or deleted. Or there's no such chat, CAF has shut
    /// down, or the viewer already watches as many chats as the host
    /// allows: nothing more comes for it. A chat archived but still loaded,
    /// such as one closed that isn't saved, goes on being watched, as it
    /// may be unarchived.
    Ended {
        /// The chat.
        chat: ChatId,
    },
    /// Where every agent stands, installed or not and signed in or not:
    /// right after `Welcome`, before any other message, and again
    /// whenever any of it changes. The whole list, each time.
    ///
    /// It goes to every viewer, but a sign-in's link and code only to
    /// viewers the host lets act on agents (in `caf-driver-tokio`,
    /// `ViewerRules::agents`); the rest get them as `null`. Whoever opens the link and approves it, or types
    /// the code on the provider's site, chooses whose account the agent
    /// signs in to: with a device-code sign-in, a viewer that may only
    /// watch could sign CAF's agent in to its own account, and see the
    /// user's prompts go there.
    Agents(Vec<AgentStatus>),
    /// The saved chats on the shelves the host has listed
    /// ([`Core::list_saved`](crate::Core::list_saved)), loaded or not, in
    /// the order they were made: right after `Agents` if the host has
    /// listed any, and again whenever it changes. The whole list, each
    /// time. Each says what it is with its header, which holds the app's
    /// [label](crate::chat::AppData::label) but never the app's data.
    Saved(Vec<SavedChat>),
    /// How a request went.
    Done {
        /// The viewer's number for it.
        request: RequestNo,
        /// How it went.
        outcome: Outcome,
    },
    /// A usage summary, the answer to [`FromViewer::Usage`].
    Usage {
        /// The viewer's number for the request.
        request: RequestNo,
        /// The summary.
        summary: Box<Summary>,
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
    /// The app's line for it, such as its title
    /// ([`AppData::label`](crate::chat::AppData::label)).
    #[serde(default)]
    pub label: Option<String>,
    /// Where it's kept, if it's saved.
    #[serde(default)]
    pub shelf: Option<Shelf>,
}

/// What the host lets a viewer do, as [`ToViewer::Welcome`] says it, so
/// a view offers only what it may: a request for anything else is
/// answered [`Outcome::NotAllowed`]. Opening a chat isn't here: the host
/// decides each request as it comes, and may allow some and not others.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Rights {
    /// Whether it may send, stop, answer, choose a chat's settings
    /// ([`FromViewer::Set`]) and close.
    pub act: bool,
    /// Whether it may install agents, sign in and out of them, and learn
    /// their settings. A viewer that may not gets no sign-in's link or
    /// code.
    pub agents: bool,
    /// Whether it may read usage summaries and set prices. A summary
    /// covers every CAF app on the machine, not just this one, so it's a
    /// right of its own. A chat's own usage and spend come with watching
    /// it. Welcomes from before it was added read as `false`.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub usage: bool,
}

impl Rights {
    /// What a viewer may do: `act` on chats, and on `agents`. Not `usage`:
    /// give it with [`Rights::with_usage`].
    #[must_use]
    pub fn new(act: bool, agents: bool) -> Self {
        Self {
            act,
            agents,
            usage: false,
        }
    }

    /// These rights, with `usage` as given.
    #[must_use]
    pub fn with_usage(self, usage: bool) -> Self {
        Self { usage, ..self }
    }
}

impl ChatSummary {
    /// A chat's summary, from its record alone: with no label or shelf,
    /// which the core knows ([`Core::summaries`](crate::Core::summaries)).
    #[must_use]
    pub fn of<A: AppItem>(chat: &Chat<A>) -> Self {
        Self {
            id: chat.id,
            agent: chat.agent.clone(),
            cwd: chat.cwd.clone(),
            label: None,
            shelf: None,
        }
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

impl OpenRequest {
    /// A new session with `agent` in `cwd`.
    #[must_use]
    pub fn new(agent: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            agent: AgentName(agent.into()),
            cwd: cwd.into(),
            session: SessionChoice::New,
        }
    }
}

/// How a viewer's request went.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Outcome {
    /// The chat was opened, or restored, with this id. It's in the next
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
    /// CAF has shut down, or is shutting down: nothing was done.
    Gone,
    /// The saved chat couldn't be restored, and its save is as it was.
    CannotRestore(CannotRestore),
    /// This CAF keeps no usage ledger, so there's no summary, and no price
    /// of the user's own can be set.
    NoLedger,
    /// The price couldn't be written beside the usage ledger, as the
    /// system said: the prices are as they were.
    PriceNotSet(String),
    /// The usage summary can't be made for this query.
    BadQuery(BadQuery),
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
    /// [`NeedPicture`] if a change is missing, before the first new one
    /// or between two, or one doesn't fit the chat ([`Chat::apply`]). The
    /// copy keeps the changes it applied before then, and is behind: ask
    /// for the chat whole again.
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
mod sample;
#[cfg(test)]
mod tests;
