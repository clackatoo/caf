//! What goes into the core: everything that happens.
//!
//! Each event is handed to [`Core::handle`](crate::Core::handle) with the
//! time it happened. The list is closed: drivers report only these.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use crate::acp::Frame;
use crate::agent::{InstallFailure, SpawnCommand};
use crate::chat::{AppData, AppItem, NoAppData, NoAppItems, UserMessage};
use crate::core::OpenChat;
use crate::ids::{AgentName, CallId, ChatId, JobId, QuestionId, Shelf, Ticket, TurnId};
use crate::install::{Platform, SignedInAs};
use crate::limits::ProfileLimits;
use crate::mcp::Key;
use crate::question::Answer;
use crate::save::{CannotRestore, Header, RestoreChat, Saved};
use crate::settings::SettingChoice;
use crate::time::Wall;
use crate::tool::{AppToolSpec, Piece, ServedTool};
use crate::usage::PriceTable;

/// The version of a recording of a run's events (the tokio kit's
/// `Config::record`), as its first line says (`caf_recording`). It goes up
/// whenever an [`Event`] of an older version no longer reads the same; as
/// CAF isn't released yet, nothing migrates an older one, and a reader
/// refuses it. Version 3 came with archiving and the names on notes about
/// settings; version 4 with tool calls' new shape; version 5 with serving
/// CAF's tools ([`Event::Serving`], [`Event::Relay`], chats' tools);
/// version 6 with CAF's own tools' jobs ([`ToolEvent::Detached`],
/// [`HostCommand::Notify`], TaskStop, and what [`ToolEvent::Finished`]
/// says of a shell's folder and a job's notice); version 7 with the app's
/// own tools ([`ServedTool`], [`Serving::app_tools`], and a call's title,
/// bad input and app item).
pub const RECORDING_VERSION: u32 = 7;

/// Something that happened. `A` and `D` are the app's item and data
/// types.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(concrete(A = NoAppItems, D = NoAppData), bound = "A: ts_rs::TS, D: ts_rs::TS")
)]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem, D: AppData")]
pub enum Event<A: AppItem = NoAppItems, D: AppData = NoAppData> {
    /// The host asked for something. The host usually calls the core's
    /// methods, such as [`Core::send`](crate::Core::send), which record
    /// these. They're events too, so a recording holds everything.
    Host(HostCommand<A, D>),
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
        /// Why, in a line, if the policy says: kept on the call
        /// ([`DecidedBy::Policy`](crate::tool::DecidedBy::Policy)).
        /// Recordings made before it was added read as none.
        #[serde(default)]
        reason: Option<String>,
    },
    /// The tools driver's report of a call to a tool CAF serves.
    Tool {
        /// The call.
        call: CallId,
        /// What happened.
        event: ToolEvent<A>,
    },
    /// The storage driver's report.
    Storage {
        /// The directive's ticket.
        ticket: Ticket,
        /// What happened.
        event: StorageEvent<A, D>,
    },
    /// The setup driver's report.
    Setup {
        /// The directive's ticket.
        ticket: Ticket,
        /// What happened.
        event: SetupEvent,
    },
    /// The prices to estimate cost with, whole: the usage driver's report,
    /// as it starts and whenever they change. Every chat's spend is worked
    /// out again with them.
    Prices(PriceTable),
    /// What the usage driver keeps of profiles' account limits: every
    /// profile's it holds, as it starts, and one's again whenever saving
    /// it found news another app had kept. The core merges them into what
    /// it has, window by window, the latest heard winning, and never saves
    /// them back.
    Limits(Vec<ProfileLimits>),
    /// How CAF's tools are served: the tools driver's report, once as it
    /// starts, before any host command. A driver that serves no tools
    /// never sends it, and chats given tools are then refused
    /// ([`Refused::ToolsNotServed`](crate::Refused::ToolsNotServed)).
    Serving(Serving),
    /// A chat's connection to CAF's tools changed: the tools driver's
    /// report.
    Relay {
        /// The chat, as the connection's handshake named it.
        chat: ChatId,
        /// What happened.
        change: RelayChange,
    },
}

/// How CAF's tools are served, for the agent's MCP server `caf`: a relay
/// program, which the agent starts, carries MCP between it and the tools
/// driver's socket.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Serving {
    /// How to start the relay. The core adds the variables that say where
    /// to connect, and as which chat: `CAF_RELAY`, `CAF_RELAY_CHAT` and
    /// `CAF_RELAY_TOKEN`.
    pub relay: SpawnCommand,
    /// The socket the relay connects to.
    pub socket: PathBuf,
    /// This run's key, which each chat's token is made with
    /// ([`mcp::token`](crate::mcp::token)). Written as hex; recordings
    /// and the debugger write `[hidden]` in its place, which reads back
    /// as all zeros.
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub key: Key,
    /// The app's own tools, as registered (`ToolsConfig::app` in the
    /// tokio kit), in order: which a chat may be given, and what the core
    /// knows of each. Recordings made before it was added read as none.
    #[serde(default)]
    pub app_tools: Vec<AppToolSpec>,
}

impl Serving {
    /// Serving through `relay`, which connects to `socket`, with `key`.
    #[must_use]
    pub fn new(relay: SpawnCommand, socket: impl Into<PathBuf>, key: Key) -> Self {
        Self {
            relay,
            socket: socket.into(),
            key,
            app_tools: Vec::new(),
        }
    }
}

/// What happened to a chat's connection to CAF's tools.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum RelayChange {
    /// A relay connected, and was bound to the chat: the newest wins.
    Connected,
    /// The chat's bound connection closed: its calls are gone, and its
    /// tools stop working until a relay connects again.
    Lost,
}

/// What the host asked for.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(concrete(A = NoAppItems, D = NoAppData), bound = "A: ts_rs::TS, D: ts_rs::TS")
)]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem, D: AppData")]
pub enum HostCommand<A: AppItem = NoAppItems, D: AppData = NoAppData> {
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
    /// Tell the agent something, as this turn: a notice.
    Notify {
        /// The chat.
        chat: ChatId,
        /// The id the core gave the turn.
        turn: TurnId,
        /// What it's told.
        text: String,
    },
    /// Stop the chat's running turn, drop its waiting messages, and hold its
    /// waiting notices ([`Core::stop`](crate::Core::stop)).
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
    /// Close a chat: the user is done with it. A saved chat is put away
    /// too.
    CloseChat {
        /// The chat.
        chat: ChatId,
    },
    /// Unarchive a chat.
    Unarchive {
        /// The chat.
        chat: ChatId,
    },
    /// List a shelf's saved chats.
    ListSaved(Shelf),
    /// Restore a saved chat.
    Restore(RestoreChat),
    /// Save a chat whole and let it go from memory.
    PutAway {
        /// The chat.
        chat: ChatId,
    },
    /// Delete a chat, loaded or not, for good.
    Delete {
        /// The chat.
        chat: ChatId,
        /// Where it's kept.
        shelf: Shelf,
    },
    /// Move every chat on a shelf to another.
    MoveShelf {
        /// The shelf.
        from: Shelf,
        /// Where they go.
        to: Shelf,
    },
    /// Choose a setting of a chat's agent.
    SetSetting {
        /// The chat.
        chat: ChatId,
        /// The choice.
        choice: SettingChoice,
    },
    /// Learn what settings an agent offers.
    LearnSettings(AgentName),
    /// Set a chat's data slot. A recording holds it whole.
    SetData {
        /// The chat.
        chat: ChatId,
        /// Its data now.
        data: D,
    },
    /// Stop everything: close every session and stop every adapter.
    Shutdown,
    /// Look at what's installed and what the machine has.
    CheckAgents,
    /// Install an agent.
    Install(AgentName),
    /// Stop installing an agent.
    StopInstall(AgentName),
    /// Sign in to an agent.
    SignIn(AgentName),
    /// Give a sign-in what the user pasted.
    Paste {
        /// The agent.
        agent: AgentName,
        /// What they pasted. It's a one-time code, so recordings write it
        /// as `null`, which reads back as empty.
        #[serde(deserialize_with = "null_as_empty")]
        text: String,
    },
    /// Stop a sign-in.
    StopSignIn(AgentName),
    /// Sign out of an agent.
    SignOut(AgentName),
    /// Add an app item. *Later.*
    AddAppItem(A),
}

/// What happened to an adapter process, in the order it happened.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
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
    Stderr(
        #[serde(deserialize_with = "bytes_or_text")]
        #[cfg_attr(feature = "ts", ts(type = "Array<number> | string"))]
        Vec<u8>,
    ),
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Exit {
    /// With this code.
    Code(i32),
    /// Killed by this signal.
    Signal(i32),
}

/// What the tools driver reports of a call.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem")]
pub enum ToolEvent<A: AppItem = NoAppItems> {
    /// The agent called a tool CAF serves. The core refuses it, unless
    /// its chat is running a turn and was given the tool, and it reads as
    /// an action; otherwise it joins the agent's report of the call, or
    /// makes the item, and asks the policy.
    Called {
        /// The chat, as the connection it came on belongs to: never from
        /// the call itself.
        chat: ChatId,
        /// The tool: CAF's own, or the app's.
        tool: ServedTool,
        /// What it was given, as the model wrote it.
        input: serde_json::Value,
        /// The agent's id for the call, if it sent one with it, such as
        /// Claude's `_meta["claudecode/toolUseId"]`: what joins it to the
        /// agent's report of it.
        agent_call: Option<String>,
        /// The call's title, for the app's own tools, worked out by the
        /// tool from its input, as the core can't read it: `None` for
        /// CAF's own, whose titles the core works out itself.
        #[serde(default)]
        title: Option<String>,
        /// Why the input isn't what the app's tool takes (serde's own
        /// words), if it isn't: the call is refused as
        /// [`RefusedCall::BadInput`](crate::directive::RefusedCall::BadInput)
        /// with it, asking no one. Always `None` for CAF's own tools,
        /// whose input the core reads itself.
        #[serde(default)]
        bad_input: Option<String>,
    },
    /// What [`ToolDirective::Preview`](crate::directive::ToolDirective::Preview)
    /// worked out: put on the call before it's asked about.
    Previewed {
        /// The pieces, such as an edit's diff.
        pieces: Vec<Piece>,
    },
    /// A running command's output changed. `from` counts every line it
    /// has printed, from 0: the core keeps the end of them, and says what
    /// changed in its own terms
    /// ([`Change::OutputChanged`](crate::Change::OutputChanged)).
    Output {
        /// The first line replaced.
        from: u64,
        /// The lines from there, worked out as its terminal shows them.
        lines: Vec<String>,
    },
    /// The agent was answered, and the call goes on, as job `job`: a
    /// command run in the background, or one that ran past its timeout.
    /// Reported before the answer is sent. It stays running, past its
    /// turn's end and the user's Stop, until it finishes
    /// ([`ToolEvent::Finished`]), TaskStop stops it, or its chat is let
    /// go.
    Detached {
        /// Its number among its chat's jobs, from 1: never one the chat
        /// had before in this run.
        job: JobId,
    },
    /// It finished, and the agent was answered: or, for a preview, it
    /// failed (`done` false), and the agent was answered why.
    Finished {
        /// Whether it did what it was asked.
        done: bool,
        /// What it gave back, for the record: for a failure, why, as a
        /// [`Piece::Text`]. A command's output is ended with its
        /// [`ending`](crate::tool::Output::ending). A [`Piece::Diff`] is
        /// the change as made, in place of the preview's diffs; without
        /// one, the preview's stay.
        pieces: Vec<Piece>,
        /// The folder a command's shell ended in, if known: the folder
        /// the chat's next command runs in
        /// ([`Action::Run::folder`](crate::tool::Action::Run)), so the
        /// foreground command that finishes last sets it. Absolute, at
        /// most 4096 bytes: any other is ignored. Its `.` and `..` are
        /// worked out. Never taken from a job's end.
        #[serde(default)]
        folder: Option<String>,
        /// For a job, what its chat's agent is told of its end, as a
        /// notice turn ([`Starter::Notice`](crate::chat::Starter::Notice)):
        /// none is sent without it. Ignored for any other call.
        #[serde(default)]
        notice: Option<String>,
        /// The app's own item for the call
        /// ([`ToolCall::app`](crate::tool::ToolCall::app)), from one of
        /// the app's own tools only: one for any other call is ignored,
        /// with a warning. Recordings made before it was added read as
        /// none.
        #[serde(default = "Option::default")]
        item: Option<A>,
    },
    /// The agent cancelled it, or its connection closed: nothing more
    /// will come of it. Never for a job, which outlives its connection: one
    /// said to be gone fails, with no notice.
    Gone,
}

/// The policy's answer to "is this allowed?".
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum PolicyAnswer {
    /// Allowed, once.
    Allow,
    /// Refused.
    Refuse,
    /// Ask the user. If the host can't show permission questions, this is
    /// a refusal.
    AskUser,
}

/// What the storage driver reports.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(concrete(A = NoAppItems, D = NoAppData), bound = "A: ts_rs::TS, D: ts_rs::TS")
)]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem, D: AppData")]
pub enum StorageEvent<A: AppItem = NoAppItems, D: AppData = NoAppData> {
    /// A shelf's chats, in the order they were made, with those it
    /// couldn't read apart.
    Listed {
        /// What it read.
        chats: Vec<Header>,
        /// What it couldn't, and why.
        unreadable: Vec<(ChatId, CannotRestore)>,
    },
    /// A chat, read and locked: its snapshot with its journal applied, as
    /// it was saved, before it's tidied. A recording holds it whole.
    Loaded(Box<Saved<A, D>>),
    /// A chat couldn't be read: no lock is taken.
    NotLoaded(CannotRestore),
    /// A write, deletion or move is done. A chat deleted goes with its
    /// lock; one written whole is locked.
    Done {
        /// How many bytes a write took in the store, compressed: what
        /// tells the core when a chat's journal has outgrown its snapshot.
        /// 0 for anything else.
        written: u64,
    },
    /// A write, deletion, move, load or list failed, as the system says,
    /// and changed nothing: a failed load takes no lock, a failed write
    /// leaves the lock as it was, and a failed deletion leaves the chat and
    /// its lock. The core tries a write again later, and says so in a
    /// report.
    Failed(String),
}

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

/// Text, read as empty from `null`.
fn null_as_empty<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// What the setup driver reports.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum SetupEvent {
    /// What's installed, and what the machine has.
    Surveyed {
        /// The folder installs live in.
        root: PathBuf,
        /// The install folders there, by name, complete ones only.
        installed: Vec<String>,
        /// The machine's Node major version, if it has Node.
        node: Option<u32>,
        /// Whether it has npm.
        npm: bool,
        /// The machine's platform.
        platform: Platform,
        /// When each complete install folder was last used, by name: the
        /// time its `.used` file was last changed, which happens each time
        /// a process starts from it (see
        /// [`AgentDirective::Start`](crate::directive::AgentDirective::Start)),
        /// or, if it has none, the folder's own, so when it was installed.
        /// The core never removes a folder missing here.
        #[serde(default)]
        used: Vec<(String, Wall)>,
    },
    /// An install moved on.
    Progress {
        /// Bytes done so far.
        done: u64,
    },
    /// An install finished.
    Installed,
    /// An install failed. Nothing is left behind.
    InstallFailed(InstallFailure),
    /// A folder was removed, or wasn't because something runs from it.
    Removed {
        /// Whether it was.
        removed: bool,
    },
    /// A sign-in command gave its link.
    Link(String),
    /// A sign-in command gave a code to type on the site.
    Code(String),
    /// A status command answered: whether signed in, and how.
    Status(Option<SignedInAs>),
    /// A sign-in or other command ended.
    Ended {
        /// Whether it succeeded.
        ok: bool,
    },
    /// It couldn't be started or run, saying why in the driver's own words,
    /// such as "no such file". Never anything the command printed, which
    /// may hold a sign-in link, a code or an email: the core logs this.
    Failed(String),
}
