//! A chat's record: what the user saw.
//!
//! CAF keeps its own record of every chat, in one format for every
//! agent. The agent keeps its own transcript too, as its memory; CAF's is
//! the record of what happened and what the user saw. Each adapter's
//! profile translates what it sends into this format as it arrives.
//!
//! A chat is a list of [`Turn`]s, and a turn is a list of [`Item`]s.
//! Every turn and item has a permanent id and a [`Window`] of time.
//!
//! `A` is the app's own kind of item ([`AppItem`]), or [`NoAppItems`].

use crate::usage::{Spend, TurnUsage};
use std::fmt::Debug;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ids::{AgentName, AgentSessionId, ChatId, ItemId, TurnId};
use crate::question::Question;
use crate::settings::{ModelUsed, SettingChoice, Settings};
use crate::time::{Wall, Window};

pub use crate::tool::{ToolCall, ToolKind, ToolStatus};

/// An app's own kind of item, such as eyespy's "the agent moved the map
/// here".
///
/// Only the app adds them, never the agent. CAF keeps them in order,
/// saves them with the chat, and never looks inside.
///
/// Its serialized form must use text keys and no raw bytes, as derived
/// structs and enums do, its floats must be finite, and its integers fit
/// in 64 bits: what JSON holds. So one CAF can't read is kept, unchanged,
/// as an [`Unknown`] item. Read old items leniently, with `#[serde(default)]`
/// on fields added later.
///
/// One the type reads but wouldn't write back as it was saved, as an
/// older version of the app reading what a newer one added, is kept
/// unchanged as an [`Unknown`] item too. An item is the app's again only
/// if what the type writes back holds all of what was saved: every key of
/// every map, with what it held; every list, as long, with what it held;
/// and everything else equal. Keys the type adds are fine; nothing else
/// may differ. So never drop, shorten, narrow or replace what the type
/// read, as `skip_serializing_if`, `#[serde(other)]` or a lossy
/// `deserialize_with` would; and keep sets in a stable order, as a
/// `BTreeSet` or `Vec` does, not a `HashSet`, whose order changes from
/// one run to the next. Change a field's type with care: what the old
/// type wrote must be held in what the new one writes of it. A whole
/// number read as a float of the same value is; a float read as a whole
/// number isn't, nor is anything read as a struct that writes it
/// differently, such as a field that was `NoAppData`'s `null` read as a
/// struct.
pub trait AppItem:
    Clone + Debug + PartialEq + Serialize + DeserializeOwned + Send + 'static
{
    /// One line saying what it is, such as "Moved the map to Kyiv": for
    /// viewers that don't know the app's type, and for catch-up notes
    /// written for another agent.
    fn summary(&self) -> String;
}

/// The item type of an app with none of its own: it can never be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum NoAppItems {}

impl AppItem for NoAppItems {
    fn summary(&self) -> String {
        match *self {}
    }
}

/// An app's own data about each chat, such as clackatoo's title and
/// settings, or which of framewright's projects it's about: the chat's
/// data slot.
///
/// CAF saves it with the chat, in MessagePack with field names, and never
/// looks inside; nor does it check what it points at, such as a clip
/// since deleted. It isn't sent to viewers: only its
/// [`label`](AppData::label) is. Keep it small, as it's saved whole each
/// time it's set ([`Core::set_data`](crate::Core::set_data)): what's big
/// belongs in the chat's record, as app items.
///
/// A new chat's is the default. Read old data leniently, with
/// `#[serde(default)]` on fields added later: a chat whose data can't be
/// read isn't restored, and its save is left as it was
/// ([`CannotRestore::AppData`](crate::save::CannotRestore::AppData)), for
/// a version of the app that can. Nor is one the type reads but wouldn't
/// write back as it was saved, as an older version of the app reading
/// what a newer one added: the same rule as [`AppItem`]'s.
pub trait AppData:
    Clone + Debug + Default + PartialEq + Serialize + DeserializeOwned + Send + 'static
{
    /// One line to show for the chat, such as its title: in lists of
    /// saved chats, which show it without loading them, and in viewers,
    /// which don't know the app's type. `None` to show none.
    fn label(&self) -> Option<String>;
}

/// The data slot of an app with no data of its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoAppData;

impl AppData for NoAppData {
    fn label(&self) -> Option<String> {
        None
    }
}

/// A chat: the user's conversation with an agent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
#[serde(bound = "A: AppItem")]
pub struct Chat<A: AppItem = NoAppItems> {
    /// Its id.
    pub id: ChatId,
    /// The agent it's with.
    pub agent: AgentName,
    /// The folder the agent works in.
    pub cwd: PathBuf,
    /// Where it stands with its agent.
    pub state: ChatState,
    /// Its turns, oldest first.
    pub turns: Vec<Turn<A>>,
    /// How full the agent's memory (its context window) is, as it last
    /// said, if it says.
    pub usage: Option<Usage>,
    /// Questions waiting for an answer, oldest first. They aren't items:
    /// they come and go. Once one is answered, what was decided is written
    /// on the item it was about.
    pub questions: Vec<Question>,
    /// The agent's settings the user may choose, such as its model: what
    /// the agent offers, and what was chosen.
    #[serde(default)]
    pub settings: Settings,
    /// What it has spent, over the turns that have a usage record, priced
    /// as the core prices it now. Chats saved before CAF recorded usage
    /// read as having spent nothing.
    #[serde(default)]
    pub spend: Box<Spend>,
    /// When it was archived ([`Core::close_chat`](crate::Core::close_chat)),
    /// while it is: `None` for a chat that isn't. An archived chat stays so
    /// when it's restored, until it's unarchived
    /// ([`Core::unarchive`](crate::Core::unarchive)) or a message is sent
    /// to it. Chats saved before it was added read as `None`; one of them
    /// saved archived is restored archived, as of when it's restored.
    #[serde(default)]
    pub archived: Option<Wall>,
}

/// How full the agent's memory is, as it says.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Usage {
    /// Tokens in use.
    pub used: u64,
    /// Tokens it can hold.
    pub size: u64,
    /// What the session has cost so far, if the agent says.
    pub cost: Option<Cost>,
}

/// An amount of money.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Cost {
    /// How much.
    pub amount: f64,
    /// In what, as an ISO 4217 code such as `USD`.
    pub currency: String,
}

/// Where a chat stands with its agent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum ChatState {
    /// Starting its agent or opening its session. Messages sent now wait.
    ///
    /// It may also be waiting for something that still holds its session
    /// to let it go: another request opening or closing it, or an adapter
    /// process that had it and hasn't gone yet. One session is never open
    /// twice. If that takes as long as [`Timers::session_open`], the user
    /// is asked whether to keep waiting.
    ///
    /// [`Timers::session_open`]: crate::host::Timers::session_open
    Opening,
    /// Open: the agent knows the session by this id.
    Open {
        /// The agent's id for the session, kept to continue it later.
        session: AgentSessionId,
    },
    /// The session is gone, because the agent stopped or couldn't open
    /// it. The next message opens it again, continuing it where the agent
    /// can.
    Lost {
        /// Why.
        reason: Lost,
    },
    /// Closed by the host: the user is done with it. Nothing more happens
    /// to it. A saved chat that's closed is put away too
    /// ([`Core::put_away`](crate::Core::put_away)); restoring it brings it
    /// back [`Idle`](ChatState::Idle).
    Closed,
    /// No session is open, and none is wanted until a message is sent,
    /// which opens it, continuing the agent's session where the agent
    /// can: as a restored chat is. Nothing went wrong.
    Idle,
    /// Waiting for its agent: to be installed, or signed in to, when a
    /// sign-in to it was started while it was signed out. Where the agent
    /// stands is in its [`AgentStatus`](crate::agent::AgentStatus); a view
    /// shows that, with Install, or the sign-in's link. Messages sent now
    /// wait, and stopping the chat stops them, with a note if the agent
    /// isn't installed.
    WaitingForAgent,
}

/// Why a chat lost its session.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Lost {
    /// The adapter couldn't be started.
    CouldNotStart {
        /// What the system said.
        error: String,
    },
    /// The adapter stopped. `stderr` is the last few lines it wrote.
    AgentStopped {
        /// Its last lines of error output, cleaned.
        stderr: Vec<String>,
    },
    /// The adapter was restarted, for this chat or another sharing it; or,
    /// while another chat was mid-turn on it, only this chat's session was
    /// closed, to be opened again.
    Restarted,
    /// The agent wants the user to sign in first.
    SignInNeeded,
    /// The agent refused to open the session.
    Refused {
        /// What it said, cleaned and capped.
        message: String,
    },
    /// The user stopped it opening.
    Stopped,
    /// The adapter can't do what this chat needs.
    Incompatible(Incompatible),
    /// Another chat already has this session open: one session is never
    /// opened twice.
    InUse,
    /// The host shut CAF down.
    ShutDown,
}

/// What an adapter can't do that a chat needs.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Incompatible {
    /// It speaks a version of ACP that CAF doesn't: this one, if it said
    /// which.
    Protocol {
        /// The version it answered with.
        version: Option<u64>,
    },
    /// It can't reach MCP servers over HTTP, and the chat has one. The
    /// chat is refused rather than quietly run without its tools.
    HttpMcp,
    /// Its answer made no sense, such as a session id CAF can't keep.
    BadAnswer,
    /// It isn't the adapter its profile says, and CAF can't turn off the
    /// own tools of one it doesn't know, so it won't run it.
    OwnTools,
}

/// One turn: something started it, the agent worked, and it ended.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
#[serde(bound = "A: AppItem")]
pub struct Turn<A: AppItem = NoAppItems> {
    /// Its id.
    pub id: TurnId,
    /// What started it.
    pub starter: Starter,
    /// Where it is in its life.
    pub life: TurnLife,
    /// When it started (was sent) and ended.
    pub window: Window,
    /// What the agent did, in order.
    pub items: Vec<Item<A>>,
    /// Whether it's history imported from the agent's replay rather than
    /// something CAF saw happen.
    pub imported: bool,
    /// The model that answered it, as the agent said when it was sent, if
    /// the agent says. A change the agent makes by itself during the turn
    /// shows as a note in it.
    #[serde(default)]
    pub model: Option<ModelUsed>,
    /// What it spent, recorded when its prompt was answered, however it
    /// ended, if the agent said. Turns from before CAF recorded usage, and
    /// imported ones, have none.
    #[serde(default)]
    pub spent: Option<Box<TurnUsage>>,
}

/// What started a turn.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Starter {
    /// The user sent this.
    User(UserMessage),
    /// The agent started it by itself, for example when a background job
    /// it was waiting on finished: its words, thinking or tools outside
    /// any prompt. ACP doesn't say when such a turn ends, so it ends when
    /// it has been quiet a while ([`Timers::quiet`]), when the user stops
    /// it, when the next message is sent, or when its session is lost.
    ///
    /// [`Timers::quiet`]: crate::host::Timers::quiet
    Agent,
    /// Another chat's agent sent this. *Later.*
    OtherChat {
        /// The chat it came from.
        from: ChatId,
        /// What it said.
        message: UserMessage,
    },
    /// The app told the agent something. *Later.*
    Notice(String),
    /// CAF itself, to say something between turns, such as that a
    /// session couldn't be continued. Its items are [`Note`]s.
    Caf,
}

/// A message to an agent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct UserMessage {
    /// Its parts, in order.
    pub parts: Vec<Part>,
}

impl UserMessage {
    /// A message of plain text.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            parts: vec![Part::Text(text.into())],
        }
    }
}

/// One part of a message.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Part {
    /// Text. A slash command such as `/compact` must be a message's only
    /// part: adapters look for it in different places.
    Text(String),
    // Images, files and links: later.
}

/// Where a turn is in its life.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum TurnLife {
    /// Waiting to be sent: its chat is opening, or another turn is
    /// running.
    Waiting,
    /// The agent is working on it.
    Running,
    /// Stop was asked for; the agent hasn't finished stopping.
    Stopping,
    /// The agent finished.
    Finished,
    /// Stopped by the user. If it never reached the agent, nothing ran.
    Stopped,
    /// It went wrong.
    Failed(Failure),
    /// Cut off mid-turn by the host shutting down.
    CutOff,
}

/// Why a turn failed, and what the user can do about it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Failure {
    /// What went wrong, in CAF's own words.
    pub reason: FailureReason,
    /// What the agent said about it, cleaned and capped, if anything. It's
    /// the agent's words, never shown as CAF's advice.
    pub agent_said: Option<String>,
    /// What the user can do, if anything.
    pub remedy: Option<Remedy>,
}

/// What went wrong with a turn.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum FailureReason {
    /// The answer hit the model's length limit.
    TooLong,
    /// The agent hit its limit of steps for one turn.
    TooManySteps,
    /// The model refused. The message and everything after it are gone
    /// from the agent's memory.
    Refused,
    /// The agent wants the user to sign in.
    SignInNeeded,
    /// The account or its credentials can't be used: signing in again
    /// doesn't fix it.
    AccessDenied,
    /// The agent's provider refused the request as it stood, such as for a
    /// model it doesn't have. Sending it again won't help.
    BadRequest,
    /// The agent hit a usage limit.
    UsageLimit,
    /// The agent's provider was overloaded or failed.
    ProviderError,
    /// The agent stopped while working on it.
    AgentStopped,
    /// The agent didn't stop when asked, so CAF ended the turn and
    /// restarted the session.
    DidNotStop,
    /// The agent's answer made no sense.
    BadAnswer,
    /// A setting chosen for the chat couldn't be applied before it was
    /// sent, such as a model the agent no longer offers, or a change it
    /// refused: rather than send it with a setting nobody chose, CAF
    /// didn't send it. The setting falls back to the host's default for
    /// the agent, if the agent offers it, which is sent first, and
    /// otherwise to what the agent uses: sending it again uses that.
    SettingNotApplied,
    /// Anything else the agent reported.
    Other,
}

/// What the user can do about a failure. CAF does the remedy itself, so
/// every host offers the same ones: `SendAgain` is sending the message
/// again, and `SignIn` is [`Core::sign_in`](crate::Core::sign_in).
/// `NewChat` is *later*.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Remedy {
    /// Send the same message again.
    SendAgain,
    /// Sign in, with CAF's own command for this agent, from its profile:
    /// never one the agent suggested. Offered only where CAF can run that
    /// command now ([`Core::sign_in`](crate::Core::sign_in) wouldn't
    /// refuse); elsewhere a failure for want of signing in offers what it
    /// did before, if anything. Once the user has signed in, the chat says
    /// so ([`Note::SignedIn`]), and the message can be sent again: CAF
    /// never sends it again by itself.
    SignIn,
    /// Start a new chat with the same agent and folder.
    NewChat,
}

/// One thing the agent did in a turn.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
#[serde(bound = "A: AppItem")]
pub struct Item<A: AppItem = NoAppItems> {
    /// Its id.
    pub id: ItemId,
    /// When it started and ended.
    pub window: Window,
    /// The tool call whose subagent did it, if a subagent did. At most 8
    /// deep, and an agent can never put the app's own items under its own.
    /// *Later.*
    pub parent: Option<ItemId>,
    /// What it is.
    pub kind: ItemKind<A>,
}

/// What an item is. A closed list: `non_exhaustive` lets CAF add kinds
/// without breaking anyone's `match`, but apps add theirs only through
/// [`ItemKind::App`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem")]
pub enum ItemKind<A: AppItem = NoAppItems> {
    /// The agent's words, in Markdown.
    Words(String),
    /// The agent's thinking, in Markdown. Only kept if the host shows it.
    Thinking(String),
    /// A tool the agent used. Boxed, as a call is far bigger than the
    /// other kinds.
    Tool(Box<ToolCall<A>>),
    /// The agent summarised its memory to make room. *Later.*
    Compacted(Compacted),
    /// Something CAF itself says, such as "the agent no longer had this
    /// session".
    Note(Note),
    /// The app's own item.
    App(A),
    /// Something the agent sent that CAF couldn't read, kept as it came.
    Unknown(Unknown),
}

/// The agent compacted its memory. *Later.*
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Compacted {}

/// Something CAF says in a turn.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Note {
    /// The agent no longer had the session the chat continues, so a new
    /// one was opened: it remembers nothing from before.
    SessionGone,
    /// The agent can't continue sessions, so a new one was opened: it
    /// remembers nothing from before.
    CannotContinue,
    /// The agent's session was stuck, and couldn't be closed, so a new one
    /// was opened rather than continue it: it remembers nothing from
    /// before.
    SessionStuck,
    /// The agent isn't installed, and the user chose not to install it now,
    /// so the message that waited for it was stopped.
    NotInstalled,
    /// The agent's install failed, so the message that waited for it was
    /// stopped. Why is in the agent's status.
    InstallFailed,
    /// The user signed in to the agent. A turn that failed for want of it
    /// can be sent again.
    SignedIn,
    /// The agent changed a setting by itself, such as falling back to
    /// another model; the choice follows it.
    AgentChanged {
        /// What it changed to.
        choice: SettingChoice,
        /// The agent's name for it, as it last said: [`Choice::name`](crate::settings::Choice::name).
        name: Option<String>,
    },
    /// A setting chosen for the chat isn't offered by the agent, or no
    /// longer is, such as a model it no longer has. The setting falls back
    /// to the host's default for the agent, if the agent offers it, and
    /// otherwise to what the agent uses. The messages waiting failed if it
    /// wasn't applied yet; if it was, and the agent stopped offering it,
    /// they go on what's used now.
    NotOffered {
        /// What was chosen.
        choice: SettingChoice,
        /// The agent's name for it, as it last said, if it ever did: one
        /// it never offered has none.
        name: Option<String>,
    },
    /// A setting chosen for the chat was refused by the agent. It falls
    /// back as for [`Note::NotOffered`], and the messages waiting failed.
    /// A change the agent never answers isn't refused: it's a stuck
    /// request (see [`Timers::request`](crate::host::Timers::request)),
    /// and the change is sent again as the session opens again.
    NotApplied {
        /// What was chosen.
        choice: SettingChoice,
        /// The agent's name for it, as it last said.
        name: Option<String>,
    },
}

/// Something the agent sent that CAF couldn't read, or an app item a
/// restored chat held that the app can't read now.
///
/// An app item that can't be read keeps its data exactly as it was saved,
/// and is saved back as that app item, unchanged, so a version of the app
/// that can read it gets it back: `what` is `App`, `raw` is its data, and
/// `summary` is the summary it had. Its data is kept as JSON, which holds
/// whatever serde writes with text keys and no raw bytes, as an
/// [`AppItem`] must.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Unknown {
    /// What it said it was, such as the update's tag, or `App`.
    pub what: String,
    /// The original data, so nothing is lost.
    pub raw: serde_json::Value,
    /// What it is, in a line, if CAF knows: an app item's summary.
    #[serde(default)]
    pub summary: Option<String>,
    /// How much of its data was kept, if it was too big to keep whole:
    /// then [`raw`](Self::raw) is the start of its JSON, as a string. Only
    /// a [`Piece::Unknown`](crate::tool::Piece::Unknown) is cut, at
    /// [`MOST_INPUT`](crate::tool::MOST_INPUT). Saves and recordings
    /// made before it was added read as none.
    #[serde(default)]
    pub cut: Option<crate::tool::Cut>,
}
