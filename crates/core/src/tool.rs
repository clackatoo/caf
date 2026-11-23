//! Tool calls: what a tool did, whoever ran it.
//!
//! A chat's tool calls come from four places ([`Origin`]): CAF's own tools
//! (Read, Edit, Write and Bash), the app's tools, which CAF serves for it,
//! tools of other MCP servers the app gave the chat, and the agent's own,
//! such as Claude's web search. Each is one [`ToolCall`] in the record,
//! however many reports it arrived in.
//!
//! # One call, one item
//!
//! A call to a tool CAF serves arrives twice: the agent reports that it's
//! calling it, and the call itself reaches CAF's tool server. Whichever
//! comes first makes the item, and the other joins it. They're matched by
//! the agent's id for the call, which it sends with the MCP call (Claude
//! as `_meta["claudecode/toolUseId"]`; each profile says where), or, for an
//! agent that sends none, the oldest call it reported in that chat to the
//! same tool with the same input that nothing has joined yet. A report
//! that never gets a partner stays an item of its own.
//!
//! For a tool CAF serves, CAF's own record is the truth: it ran it, so its
//! [`Action`], input, result and status come from CAF, and the agent's
//! report adds only its id and when it started. For the other tools, the
//! agent's report is all there is.
//!
//! # Where it came from, for certain
//!
//! The [`Origin`] of a call CAF serves comes from the connection it
//! arrived on, which belongs to one chat. Any other's comes from what the
//! agent's adapter says of it, such as Claude's `_meta.claudeCode.toolName`.
//! Never from the call's title, nor anything else the model wrote.
//!
//! # The record and the model's answer
//!
//! What's kept here is what's useful to the user: a diff, a command's
//! output in colour. What the model is answered is what's useful to it,
//! and each tool says what that is (*later*, with the tools). For a tool
//! CAF runs, both come from the same run.
//!
//! # Caps
//!
//! Everything here is bounded, and anything cut says how much was kept of
//! how much ([`Cut`]):
//!
//! | What | Most kept | Which part |
//! | --- | --- | --- |
//! | [`Text`] | [`MOST_TEXT`] | its start |
//! | [`Output`] | [`MOST_OUTPUT`] | its end, in whole lines of at most [`MOST_LINE`] |
//! | [`Diff`] | [`MOST_DIFF`] | whole hunks, then how many more |
//! | [`Image`] | [`MOST_IMAGE`] | all of it, or none, with its size |
//! | [`ToolCall::locations`] | [`MOST_LOCATIONS`] | the first |
//! | [`ToolCall::input`] | [`MOST_INPUT`] | its start |

use serde::{Deserialize, Serialize};

use crate::chat::{AppItem, NoAppItems};
use crate::event::Exit;

/// The most of a [`Text`] kept, in bytes: its start.
pub const MOST_TEXT: usize = 64 * 1024;
/// The most of an [`Output`] kept, in bytes: its end.
pub const MOST_OUTPUT: usize = 64 * 1024;
/// The longest line of an [`Output`], in bytes. A longer one is split
/// into lines this long, at character boundaries, before it's kept, so
/// dropping whole lines from the top always keeps an output under
/// [`MOST_OUTPUT`].
pub const MOST_LINE: usize = 16 * 1024;
/// The most of a [`Diff`] kept, in bytes of its lines' text.
pub const MOST_DIFF: usize = 256 * 1024;
/// The biggest [`Image`] kept whole, in bytes before base64.
pub const MOST_IMAGE: usize = 2 * 1024 * 1024;
/// The most [`Location`]s a call keeps.
pub const MOST_LOCATIONS: usize = 256;
/// The most of a call's raw input kept, in bytes of its JSON.
pub const MOST_INPUT: usize = 64 * 1024;

/// A tool call.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
#[serde(bound = "A: AppItem")]
pub struct ToolCall<A: AppItem = NoAppItems> {
    /// What it's called, in a short line. The agent's own title for the
    /// tools it reports; for CAF's own tools, CAF's, from the action, such
    /// as "Read src/main.rs", since the agent's would be a name like
    /// `mcp__caf__Read`.
    pub title: String,
    /// What kind of tool it is: from the [`Action`] for CAF's own tools
    /// (see [`Action::kind`]), else as the agent says.
    pub kind: ToolKind,
    /// Where it is.
    pub status: ToolStatus,
    /// Whose tool it is.
    pub origin: Origin,
    /// What it does, as CAF reads it: set once, when CAF has the call's
    /// whole input, and never changed. `None` until then, and for a call
    /// whose input never came whole.
    pub action: Option<Action>,
    /// What it was given, as the model wrote it. If its JSON was longer
    /// than [`MOST_INPUT`], it's the start of that JSON as a string, and
    /// [`input_cut`](Self::input_cut) says so.
    pub input: Option<serde_json::Value>,
    /// How much of the input was kept, if it was cut.
    pub input_cut: Option<Cut>,
    /// What it gave back, in order.
    pub result: Vec<Piece>,
    /// The files, and places in them, it's about, for a viewer to open:
    /// from the action for CAF's own tools, else as the agent says. At
    /// most [`MOST_LOCATIONS`].
    pub locations: Vec<Location>,
    /// How many more locations there were than were kept.
    pub locations_cut: u64,
    /// Whether it was allowed, and by whom.
    pub decision: Decision,
    /// The app's own item for this call, such as the map move one of its
    /// tools made. *Later*, with the app's tools.
    pub app: Option<A>,
}

/// What kind of tool a call is, for an icon and for counting ("Read 3
/// files").
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
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
///
/// Every call ends Done, Failed, Refused or Cancelled: when its turn ends,
/// one still Pending, Asking or Running is Cancelled if the user stopped
/// the turn, and Failed otherwise, and so is one a restored chat was cut
/// off in.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum ToolStatus {
    /// Not started yet: reported, or waiting for the policy.
    Pending,
    /// Waiting for the user: its permission question is open.
    Asking,
    /// Running.
    Running,
    /// Finished.
    Done,
    /// Went wrong: why is a [`Piece::Text`], or its turn failed under it.
    Failed,
    /// Not allowed, by the policy or the user. It never ran.
    Refused,
    /// Stopped: the user stopped its turn, or the agent cancelled it.
    Cancelled,
}

impl ToolStatus {
    /// Whether it has ended: Done, Failed, Refused or Cancelled.
    #[must_use]
    pub fn ended(self) -> bool {
        todo!()
    }
}

/// Whose tool a call is.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Origin {
    /// One of CAF's own tools.
    Caf(CafTool),
    /// One of the app's tools, which CAF serves for it, by its name.
    /// *Later*, with the app's tools.
    App(String),
    /// A tool of an MCP server the app gave the chat.
    Server {
        /// The server's name, as the app gave it.
        server: String,
        /// The tool's name.
        tool: String,
    },
    /// The agent's own tool, by the agent's name for it, such as
    /// `WebSearch`.
    Agent(String),
    /// Not recorded: a call saved before CAF kept where calls came from.
    NotRecorded,
}

/// One of CAF's own tools.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum CafTool {
    /// Reads a file.
    Read,
    /// Changes part of a file.
    Edit,
    /// Writes a whole file.
    Write,
    /// Runs a command.
    Bash,
}

impl CafTool {
    /// Its name, as CAF serves it, such as `Read`.
    #[must_use]
    pub fn name(self) -> &'static str {
        todo!()
    }
}

/// What a call does, as CAF reads it.
///
/// Paths are absolute, worked out against the chat's folder before
/// anything runs, so an action says exactly which file, whatever `..` the
/// model wrote; one that isn't UTF-8 is refused.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Action {
    /// Read a file.
    Read {
        /// The file.
        path: String,
        /// Which lines, if not all.
        lines: Option<Lines>,
    },
    /// Change part of a file. What changes is its [`Diff`].
    Edit {
        /// The file.
        path: String,
    },
    /// Write a whole file, new or over what was there.
    Write {
        /// The file.
        path: String,
    },
    /// Run a command.
    Run {
        /// The command, as the model wrote it.
        command: String,
        /// The folder it runs in.
        folder: String,
        /// Whether it runs in the background, the call ending as it
        /// starts.
        background: bool,
    },
    /// Any tool that isn't CAF's own: its input is the call's
    /// [`input`](ToolCall::input).
    Tool {
        /// Its name.
        name: String,
    },
}

impl Action {
    /// The kind of tool that does it: Read is [`ToolKind::Read`], Edit and
    /// Write are [`ToolKind::Edit`], Run is [`ToolKind::Execute`], and
    /// another tool is [`ToolKind::Other`].
    #[must_use]
    pub fn kind(&self) -> ToolKind {
        todo!()
    }
}

/// A range of lines in a file, numbered from 1, both ends included.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Lines {
    /// The first.
    pub first: u64,
    /// The last, if not the file's end.
    pub last: Option<u64>,
}

/// A file a call is about, and the line, if it's about one.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Location {
    /// The file, as an absolute path.
    pub path: String,
    /// The line, numbered from 1.
    pub line: Option<u64>,
}

/// One piece of what a call gave back.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Piece {
    /// Plain text, such as a file read.
    Text(Text),
    /// A file's change.
    Diff(Diff),
    /// A command's output. A call has at most one, and while it runs,
    /// [`Change::OutputChanged`](crate::Change::OutputChanged) changes it.
    Output(Output),
    /// A picture.
    Image(Image),
    /// A file or web address it points to.
    Link(Link),
    /// A piece of a kind CAF can't read yet, kept as it came.
    Unknown(crate::chat::Unknown),
}

/// Plain text.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Text {
    /// The text: its start, at most [`MOST_TEXT`].
    pub text: String,
    /// How much was kept, if it was cut.
    pub cut: Option<Cut>,
}

/// How much of something was kept, in bytes, when it was cut.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Cut {
    /// What was kept.
    pub kept: u64,
    /// What there was.
    pub total: u64,
}

/// One file's change, as hunks: runs of changed lines, with three lines
/// either side as they were, as `git diff` shows them.
///
/// CAF works the hunks out, from the file before and after, whether CAF
/// made the change or the agent sent both; viewers draw them as they
/// are, and never compare texts.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Diff {
    /// The file, as an absolute path.
    pub path: String,
    /// Whether the change makes the file: there was none before.
    pub new_file: bool,
    /// How many lines it adds, in all its hunks, kept or not.
    pub added: u64,
    /// How many it removes, likewise.
    pub removed: u64,
    /// Its hunks, in order: whole ones, up to [`MOST_DIFF`].
    pub hunks: Vec<Hunk>,
    /// How many hunks weren't kept.
    pub more: u64,
}

/// A run of changed lines, with those around it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Hunk {
    /// Its lines, in order.
    pub lines: Vec<DiffLine>,
}

/// One line of a hunk.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DiffLine {
    /// Whether it was kept, added or removed.
    pub change: LineChange,
    /// Its number in the file before, numbered from 1: none if added.
    pub old: Option<u64>,
    /// Its number in the file after: none if removed.
    pub new: Option<u64>,
    /// The line, without its ending.
    pub text: String,
}

/// What happened to a line.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum LineChange {
    /// As it was, shown around the changes.
    Kept,
    /// Added.
    Added,
    /// Removed.
    Removed,
}

/// A command's output, as its terminal showed it.
///
/// Its colours are kept, as the terminal's codes for them (SGR); its
/// cursor's movements are worked out before it's kept, so a progress bar
/// is its latest line, not every redraw of it. Kept as lines, the last
/// [`MOST_OUTPUT`] bytes of them, none longer than [`MOST_LINE`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Output {
    /// The lines kept, without their endings.
    pub lines: Vec<String>,
    /// How many lines before them weren't kept.
    pub dropped: u64,
    /// How it ended: `None` while it runs.
    pub ending: Option<Ending>,
}

/// How a command ended.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Ending {
    /// It exited, as this says.
    Exited(Exit),
    /// It ended, but not saying how, as an agent's own commands can.
    Unknown,
}

/// A picture.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Image {
    /// Its type, such as `image/png`.
    pub mime: String,
    /// Its bytes, in base64: none if it was bigger than [`MOST_IMAGE`].
    pub data: Option<String>,
    /// Its size, in bytes before base64.
    pub size: u64,
}

/// A file or web address a call points to.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Link {
    /// The address.
    pub uri: String,
    /// What it's called.
    pub name: String,
}

/// Whether a call was allowed, and by whom.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Decision {
    /// Not decided yet.
    Undecided,
    /// It needed no permission, such as a call the agent made without
    /// asking.
    NotAsked,
    /// Allowed.
    Allowed(DecidedBy),
    /// Refused.
    Refused(DecidedBy),
    /// Not recorded: a call saved before CAF kept who decided.
    NotRecorded,
}

/// Who decided.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum DecidedBy {
    /// The policy, with its reason, if it gave one.
    Policy {
        /// Why, in a line.
        reason: Option<String>,
    },
    /// The user, choosing this option of the question.
    User {
        /// The option's label, as the question showed it.
        option: String,
    },
}
