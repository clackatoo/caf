//! Saving chats, and bringing them back.
//!
//! **Everything a chat is, is saved**: its record, the agent's session to
//! continue, and the app's data slot ([`AppData`]). What belongs to one
//! run isn't: processes, requests in flight, timers. A rule, not a
//! judgement: nothing that holds a [`Ticket`](crate::Ticket) or a
//! [`Mono`](crate::time::Mono) is saved. The chat's own types are what's
//! saved, so there's no second copy to forget a field in. Its questions
//! are saved with its record, and dropped when it's restored, as what
//! asked them is gone.
//!
//! **The storage driver stores; the core decides when**
//! ([`StorageDirective`](crate::directive::StorageDirective)). Soon after a chat changes
//! ([`Timers::save`](crate::host::Timers::save)), the changes since its
//! last save are added to its journal; now and then, it's written whole,
//! as a snapshot, and its journal starts again empty. Restoring reads the
//! snapshot and applies the journal with [`Chat::apply`], the same code
//! every viewer uses.
//!
//! **Where is the app's**: each chat is kept on a [`Shelf`] the app names,
//! and the store decides what a shelf is: a folder, rows in a database.
//! `caf-save` has the format and a file store.
//!
//! **Restoring is lazy.** A restored chat starts no agent until a message
//! is sent, and then continues its session: resumed where the agent can,
//! or loaded with what the agent replays thrown away, as CAF has the
//! record already.
//!
//! **What was under way is tidied, as a restore finds it**, the same as
//! shutting down does ([`Core::shutdown`](crate::Core::shutdown)), as a
//! save may come at any moment:
//! - a turn running or stopping is [cut off](crate::chat::TurnLife::CutOff);
//! - its unfinished tool calls have [failed](crate::chat::ToolStatus::Failed);
//! - a message waiting to be sent is [stopped](crate::chat::TurnLife::Stopped):
//!   never sent by itself, as what the user meant then may not stand now;
//! - questions are dropped, as what asked them is gone;
//! - the chat is [idle](crate::chat::ChatState::Idle).
//!
//! **A chat whose agent the host no longer offers still comes back**, with
//! its record, and sending to it is refused
//! ([`Refused::NoSuchAgent`](crate::Refused::NoSuchAgent)). So does one
//! whose agent now has the cautious profile, whose own tools CAF can't
//! turn off: sending to it is refused
//! ([`Refused::OwnTools`](crate::Refused::OwnTools)). If the host
//! offers no agent by its name but exactly one with its built-in profile
//! ([`Header::profile`]), as when the host renamed it, the chat comes back
//! with that one, under its new name.
//!
//! [`Chat::apply`]: crate::Chat::apply

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::McpServer;
use crate::change::Change;
use crate::chat::{AppData, AppItem, Chat, NoAppData, NoAppItems};
use crate::ids::{AgentName, AgentSessionId, ChatId, Shelf};
use crate::time::Wall;

/// What's saved of a chat, whole: a snapshot.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(concrete(A = NoAppItems, D = NoAppData), bound = "A: ts_rs::TS, D: ts_rs::TS")
)]
#[serde(bound = "A: AppItem, D: AppData")]
pub struct Saved<A: AppItem = NoAppItems, D: AppData = NoAppData> {
    /// What lists of saved chats show.
    pub header: Header,
    /// Its record.
    pub chat: Chat<A>,
    /// The agent's session to continue, kept even once lost: what the
    /// agent knows it by.
    pub session: Option<AgentSessionId>,
    /// The app's data.
    pub data: D,
}

/// What a list of saved chats shows of each, read without loading it.
///
/// It doesn't name its shelf: a store lists a shelf at a time, and moves
/// a shelf's chats without reading them.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Header {
    /// The chat.
    pub chat: ChatId,
    /// The agent, by the name the host gave it.
    pub agent: AgentName,
    /// The agent's built-in profile, if it has one, such as
    /// `claude-agent-acp`: what finds it again if the host renames it.
    pub profile: Option<String>,
    /// The folder the agent works in.
    pub cwd: PathBuf,
    /// When the chat was made.
    pub made: Wall,
    /// When it last changed.
    pub changed: Wall,
    /// The app's line for it, such as its title ([`AppData::label`]).
    pub label: Option<String>,
    /// The first line of its first message, cut to 200 characters, if it
    /// has one: to tell chats with no label apart.
    pub first: Option<String>,
    /// How many turns it has.
    pub turns: u64,
    /// Whether it was closed: archived.
    pub closed: bool,
    /// When it was archived, if it was ([`Chat::archived`](crate::Chat::archived)).
    /// Headers written before it was added read as `None`, even if
    /// `closed`.
    #[serde(default)]
    pub archived: Option<Wall>,
}

/// One entry in a chat's journal: what changed since its last save.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(concrete(A = NoAppItems, D = NoAppData), bound = "A: ts_rs::TS, D: ts_rs::TS")
)]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem, D: AppData")]
pub enum Entry<A: AppItem = NoAppItems, D: AppData = NoAppData> {
    /// A change to its record, as viewers get them, but squashed: a
    /// save's worth of streamed words to one item is one change.
    Change(Change<A>),
    /// The session to continue changed.
    Session(Option<AgentSessionId>),
    /// The app set its data.
    Data(D),
}

/// What to restore.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RestoreChat {
    /// The chat.
    pub chat: ChatId,
    /// Where it's kept.
    pub shelf: Shelf,
    /// Tools to give its agent, as MCP servers. They aren't saved, as they
    /// hold tokens and things that last one run, such as sockets: the
    /// host gives them again.
    pub mcp_servers: Vec<McpServer>,
    /// What the app tells the agent, as
    /// [`OpenChat::instructions`](crate::OpenChat::instructions): given
    /// again, as it isn't saved.
    pub instructions: Option<String>,
}

impl RestoreChat {
    /// Restore `chat`, kept on `shelf`, with no tools of the host's.
    #[must_use]
    pub fn new(chat: ChatId, shelf: Shelf) -> Self {
        Self {
            chat,
            shelf,
            mcp_servers: Vec::new(),
            instructions: None,
        }
    }
}

/// A saved chat, as [`Core::saved`](crate::Core::saved) lists it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SavedChat {
    /// Where it's kept.
    pub shelf: Shelf,
    /// What's shown of it.
    pub header: Header,
    /// Whether it's loaded.
    pub state: SavedState,
}

/// Whether a saved chat is loaded.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum SavedState {
    /// Only saved.
    Saved,
    /// Being restored.
    Restoring,
    /// Loaded: [`Core::chat`](crate::Core::chat) has it.
    Loaded,
    /// The last restore failed.
    Failed(CannotRestore),
}

/// Why a chat couldn't be restored. Its save is left as it was, whatever
/// the reason.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum CannotRestore {
    /// The store has no such chat on that shelf.
    NotFound,
    /// Another program has it open: one chat is never open twice.
    InUse,
    /// It was saved by a newer CAF, in this version of the format, which
    /// this one can't read. It's left untouched for a newer one.
    Newer {
        /// The format's version.
        version: u32,
    },
    /// Its save is damaged: what's wrong, in the store's words.
    Damaged(String),
    /// The app's data slot can't be read by the app's type, as serde says.
    AppData(String),
    /// The store failed, as the system says, such as a disk error.
    Store(String),
}
