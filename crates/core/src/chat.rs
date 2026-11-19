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

use std::fmt::Debug;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ids::{AgentName, AgentSessionId, ChatId, ItemId, TurnId};
use crate::question::Question;
use crate::time::Window;

/// An app's own kind of item, such as eyespy's "the agent moved the map
/// here".
///
/// Only the app adds them, never the agent. CAF keeps them in order,
/// saves them with the chat, and never looks inside.
pub trait AppItem: Clone + Debug + PartialEq + Serialize + DeserializeOwned + Send + 'static {
    /// One line saying what it is, such as "Moved the map to Kyiv": for
    /// viewers that don't know the app's type, and for catch-up notes
    /// written for another agent.
    fn summary(&self) -> String;
}

/// The item type of an app with none of its own: it can never be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoAppItems {}

impl AppItem for NoAppItems {
    fn summary(&self) -> String {
        match *self {}
    }
}

/// A chat: the user's conversation with an agent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Questions waiting for an answer, oldest first. They aren't items:
    /// they come and go. Once one is answered, what was decided is written
    /// on the item it was about.
    pub questions: Vec<Question>,
}

/// Where a chat stands with its agent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatState {
    /// Starting its agent or opening its session. Messages sent now wait.
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
    /// Closed by the host. Nothing more happens to it.
    Closed,
}

/// Why a chat lost its session.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The adapter was restarted, for this chat or another sharing it.
    Restarted,
    /// The agent wants the user to sign in first.
    SignInNeeded,
    /// The agent refused to open the session.
    Refused {
        /// What it said, cleaned and capped.
        message: String,
    },
}

/// One turn: something started it, the agent worked, and it ended.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
}

/// What started a turn.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Starter {
    /// The user sent this.
    User(UserMessage),
    /// The agent started it by itself, for example when a background job
    /// it was waiting on finished. *Later.*
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
}

/// A message to an agent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
pub enum Part {
    /// Text. A slash command such as `/compact` must be a message's only
    /// part: adapters look for it in different places.
    Text(String),
    // Images, files and links: later.
}

/// Where a turn is in its life.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Cut off by the host restarting, mid-turn. *Later.*
    CutOff,
}

/// Why a turn failed, and what the user can do about it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Anything else the agent reported.
    Other,
}

/// What the user can do about a failure. CAF does the remedy itself, so
/// every host offers the same ones. *Later*, except `SendAgain`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Remedy {
    /// Send the same message again.
    SendAgain,
    /// Sign in, with CAF's own command for this agent: never one the agent
    /// suggested.
    SignIn,
    /// Start a new chat with the same agent and folder.
    NewChat,
}

/// One thing the agent did in a turn.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[serde(bound = "A: AppItem")]
pub enum ItemKind<A: AppItem = NoAppItems> {
    /// The agent's words, in Markdown.
    Words(String),
    /// The agent's thinking, in Markdown. Only kept if the host shows it.
    Thinking(String),
    /// A tool the agent used.
    Tool(ToolCall<A>),
    /// The agent summarised its memory to make room. *Later.*
    Compacted(Compacted),
    /// Something CAF itself says, such as "continuing after a restart".
    /// *Later.*
    Note(Note),
    /// The app's own item.
    App(A),
    /// Something the agent sent that CAF couldn't read, kept as it came.
    Unknown(Unknown),
}

/// A tool call.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "A: AppItem")]
pub struct ToolCall<A: AppItem = NoAppItems> {
    /// What the agent calls it, as a short title.
    pub title: String,
    /// What kind of tool it is.
    pub kind: ToolKind,
    /// Where it is.
    pub status: ToolStatus,
    /// What it was given, as the agent sent it.
    pub input: Option<serde_json::Value>,
    /// What it gave back, as text.
    pub output: String,
    /// The app's own item for this call, such as the map move one of its
    /// tools made. *Later.*
    pub app: Option<A>,
    // The typed action, file changes and who allowed it: later.
}

/// What kind of tool a call is, as the agent says.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    /// Reading files.
    Read,
    /// Changing files.
    Edit,
    /// Deleting files.
    Delete,
    /// Moving files.
    Move,
    /// Searching.
    Search,
    /// Running a command.
    Execute,
    /// Thinking it through.
    Think,
    /// Fetching from the web.
    Fetch,
    /// Anything else.
    Other,
}

/// Where a tool call is.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    /// Not started yet, such as while waiting for permission.
    Pending,
    /// Running.
    Running,
    /// Finished.
    Done,
    /// Failed, or ended unfinished with its turn.
    Failed,
}

/// The agent compacted its memory. *Later.*
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compacted {}

/// Something CAF says in a turn. *Later.*
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Note {}

/// Something the agent sent that CAF couldn't read.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Unknown {
    /// What it said it was, such as the update's tag.
    pub what: String,
    /// The original data, so nothing is lost.
    pub raw: serde_json::Value,
}
