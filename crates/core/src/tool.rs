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
//! | [`Output`] | [`MOST_OUTPUT`], each line a byte more than its text | its end, in whole lines of at most [`MOST_LINE`] |
//! | [`Diff`] | [`MOST_DIFF`], each line 64 bytes more than its text | whole hunks, then how many more; none past 10,000 lines changed |
//! | [`Image`] | [`MOST_IMAGE`] | all of it, or none, with its size |
//! | [`ToolCall::locations`] | [`MOST_LOCATIONS`] | the first |
//! | [`ToolCall::input`] | [`MOST_INPUT`] | its start |
//! | [`Piece::Unknown`] | [`MOST_INPUT`] | its start |

use serde::{Deserialize, Serialize};

use crate::chat::{AppItem, NoAppItems};
use crate::event::Exit;

/// The most of a [`Text`] kept, in bytes: its start.
pub const MOST_TEXT: usize = 64 * 1024;
/// The most of an [`Output`] kept, in bytes: its end. Each line counts
/// its ending too, so a line costs at least a byte, and empty lines
/// can't make an output any longer than this many lines.
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
    /// What it does, as CAF reads it. For CAF's own tools, set once, when
    /// CAF has the call's whole input, and never changed: `None` until
    /// then, and for a call whose input never came whole. For any other,
    /// [`Action::Tool`] by the tool's name, once its [`origin`](Self::origin)
    /// is known: it changes only if the agent's permission request names
    /// the tool otherwise than its report did. `None` while whose tool it
    /// is isn't known, and for a call saved before CAF kept actions.
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
    /// Who stopped it, for a call to a tool CAF serves that ended
    /// [`Cancelled`](ToolStatus::Cancelled): `None` for any other, and for
    /// a call saved before CAF kept it.
    #[serde(default)]
    pub stopped_by: Option<StoppedBy>,
}

/// Who stopped a call that ended [`Cancelled`](ToolStatus::Cancelled).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum StoppedBy {
    /// The user: they stopped its turn, or closed or deleted its chat. A
    /// turn that ends by itself, or is cut off as its chat is put away or
    /// CAF shuts down, fails the calls still open in it instead, with no
    /// one said to have stopped them.
    User,
    /// The agent, cancelling it while its turn wasn't being stopped, or its
    /// connection to CAF's tools closing.
    Agent,
}

/// The tools CAF serves a chat ([`OpenChat::tools`](crate::OpenChat::tools)):
/// none by default. They aren't saved: the host gives them again when it
/// restores the chat ([`RestoreChat::tools`](crate::RestoreChat::tools)).
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Tools {
    /// CAF's own tools to serve, in the order given, each once.
    pub caf: Vec<CafTool>,
}

impl Tools {
    /// No tools.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// These of CAF's own tools, each once, in the order given.
    #[must_use]
    pub fn caf(tools: impl IntoIterator<Item = CafTool>) -> Self {
        let mut caf = Vec::new();
        for tool in tools {
            if !caf.contains(&tool) {
                caf.push(tool);
            }
        }
        Self { caf }
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.caf.is_empty()
    }
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
/// off in. One the agent says failed while the user stops its turn was
/// cancelled, as an agent of ACP's first version, such as Claude's, has
/// no other word for it. Once a call has ended it stays so, whatever the
/// agent says of it after, though what it gave back may still come.
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
        matches!(
            self,
            Self::Done | Self::Failed | Self::Refused | Self::Cancelled
        )
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
    /// Every one of CAF's own tools.
    pub const ALL: [Self; 4] = [Self::Read, Self::Edit, Self::Write, Self::Bash];

    /// The tool CAF serves as `name`, such as `Read`, if it's one of its
    /// own.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    /// Its name, as CAF serves it, such as `Read`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::Edit => "Edit",
            Self::Write => "Write",
            Self::Bash => "Bash",
        }
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
        match self {
            Self::Read { .. } => ToolKind::Read,
            Self::Edit { .. } | Self::Write { .. } => ToolKind::Edit,
            Self::Run { .. } => ToolKind::Execute,
            Self::Tool { .. } => ToolKind::Other,
        }
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
    /// It ends with its call: [`Ending::Unknown`] if nothing said how.
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
    /// Whether the change makes the file: there was none before. For a
    /// change the agent reports, that it sent nothing of the file before,
    /// as Claude's adapter does first for a Write, whether or not there
    /// was a file, and for a hunk that only adds lines: its next report
    /// of the call usually replaces the diff with the whole truth.
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
    ///
    /// For a change the agent reports, numbered within what it sent,
    /// which may be only part of the file, as Claude's diffs of a hunk or
    /// an edit's text are; and if the agent gave, in the same place in
    /// the call's [`locations`](ToolCall::locations), where that part is
    /// in the file after, from there, both before and after. The numbers
    /// after are then the file's; those before are too, unless an earlier
    /// part of the change added or removed lines.
    pub old: Option<u64>,
    /// Its number in the file after: none if removed. Numbered as
    /// [`old`](Self::old) says.
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

impl Text {
    /// `text`, cut to its first [`MOST_TEXT`] bytes, at a character
    /// boundary, if it's longer.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let (text, cut) = start_of(text.into(), MOST_TEXT);
        Self { text, cut }
    }
}

impl Image {
    /// A picture of type `mime`, such as `image/png`, from its bytes: kept
    /// whole, in base64, if it's at most [`MOST_IMAGE`] bytes, and only
    /// its size otherwise.
    #[must_use]
    pub fn new(mime: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            mime: mime.into(),
            data: (bytes.len() <= MOST_IMAGE).then(|| base64(bytes)),
            size: bytes.len() as u64,
        }
    }

    /// A picture of type `mime` from its bytes in base64, `data`, as an
    /// agent sends one: kept whole if it's at most [`MOST_IMAGE`] bytes
    /// before base64, and only its size otherwise.
    #[must_use]
    pub fn from_base64(mime: impl Into<String>, data: impl Into<String>) -> Self {
        image(mime.into(), data.into())
    }
}

impl Location {
    /// The file at `path`, an absolute path, and the line, numbered from
    /// 1, if it's about one.
    #[must_use]
    pub fn new(path: impl Into<String>, line: Option<u64>) -> Self {
        Self {
            path: path.into(),
            line,
        }
    }
}

/// `bytes` in standard base64, with padding.
fn base64(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for at in 0..4 {
            if at <= chunk.len() {
                text.push(char::from(DIGITS[(n >> (18 - 6 * at)) as usize & 63]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

impl Diff {
    /// The change to the file at `path` from `before`, `None` if there was
    /// no file, to `after`: its hunks, with three lines either side as
    /// they were, whole ones up to [`MOST_DIFF`], each line counting its
    /// text and 64 bytes more. Lines are compared whole, their endings
    /// included, and kept without them, with nothing in them hidden, as an
    /// agent's text is: a control character or one that reorders text
    /// shows as a code.
    ///
    /// Working out hunks takes time that grows with the square of the
    /// lines changed, so past 10,000 lines between the two files, not
    /// counting those the same at either end, it isn't done (and past
    /// 10,000 between all the diffs of one report of a call, for those
    /// after): the diff has
    /// no hunks and one [`more`](Self::more), and says it removes every
    /// one of those lines of `before` and adds every one of `after`.
    #[must_use]
    pub fn new(path: impl Into<String>, before: Option<&str>, after: &str) -> Self {
        let mut budget = MOST_DIFF_WORK;
        Self::within(path, before, after, &mut budget)
    }

    /// [`Diff::new`], its hunks worked out only if the lines to compare fit
    /// in `budget`, which they then use up: what the diffs of one report
    /// share.
    pub(crate) fn within(
        path: impl Into<String>,
        before: Option<&str>,
        after: &str,
        budget: &mut usize,
    ) -> Self {
        use similar::DiffableStr as _;
        // Lines as the diff splits them: `\r\n`, `\n`, or `\r` alone ends
        // one.
        let old = before.unwrap_or_default().tokenize_lines();
        let new = after.tokenize_lines();
        let same_start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let same_end = (old[same_start..].iter().rev())
            .zip(new[same_start..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let removed = old.len() - same_start - same_end;
        let added = new.len() - same_start - same_end;
        if removed + added > *budget {
            return Self {
                path: path.into(),
                new_file: before.is_none(),
                added: added as u64,
                removed: removed as u64,
                hunks: Vec::new(),
                more: 1,
            };
        }
        *budget -= removed + added;
        let diff = similar::TextDiff::from_lines(before.unwrap_or_default(), after);
        let (mut added, mut removed) = (0, 0);
        let mut hunks = Vec::new();
        for group in diff.grouped_ops(CONTEXT) {
            let mut lines = Vec::new();
            for op in &group {
                for change in diff.iter_changes(op) {
                    let (change_of, old, new) = match change.tag() {
                        similar::ChangeTag::Equal => {
                            (LineChange::Kept, change.old_index(), change.new_index())
                        }
                        similar::ChangeTag::Delete => {
                            removed += 1;
                            (LineChange::Removed, change.old_index(), None)
                        }
                        similar::ChangeTag::Insert => {
                            added += 1;
                            (LineChange::Added, None, change.new_index())
                        }
                    };
                    let text = change.value();
                    let text = text.strip_suffix('\n').unwrap_or(text);
                    let text = text.strip_suffix('\r').unwrap_or(text);
                    lines.push(DiffLine {
                        change: change_of,
                        old: old.map(|at| at as u64 + 1),
                        new: new.map(|at| at as u64 + 1),
                        text: crate::core::unhidden(text),
                    });
                }
            }
            hunks.push(Hunk { lines });
        }
        let mut size = 0;
        let kept = (hunks.iter())
            .take_while(|hunk| {
                size += (hunk.lines.iter())
                    .map(|line| line.text.len() + LINE_COST)
                    .sum::<usize>();
                size <= MOST_DIFF
            })
            .count();
        let more = (hunks.len() - kept) as u64;
        hunks.truncate(kept);
        Self {
            path: path.into(),
            new_file: before.is_none(),
            added,
            removed,
            hunks,
            more,
        }
    }
}

impl Diff {
    /// Numbers its lines as if it started at line `first` of the file, both
    /// before and after: for an agent's diff of only part of a file, such
    /// as Claude's, one for each hunk, with where it is in the file after.
    pub(crate) fn number_from(&mut self, first: u64) {
        let by = first.saturating_sub(1);
        for line in self.hunks.iter_mut().flat_map(|hunk| &mut hunk.lines) {
            line.old = line.old.map(|old| old.saturating_add(by));
            line.new = line.new.map(|new| new.saturating_add(by));
        }
    }
}

impl<A: AppItem> ToolCall<A> {
    /// Says its command's output ended, if it has: [`Ending::Unknown`], if
    /// nothing said how, as with an agent's own terminal.
    pub(crate) fn settle_output(&mut self) {
        if !self.status.ended() {
            return;
        }
        for piece in &mut self.result {
            if let Piece::Output(output) = piece
                && output.ending.is_none()
            {
                output.ending = Some(Ending::Unknown);
            }
        }
    }
}

/// What a call the agent reports does, as CAF knows it from whose tool it
/// is, `origin`: the tool, by its name, for any tool but CAF's own, whose
/// actions CAF reads itself from their input. `None` if that isn't known.
pub(crate) fn reported_action(origin: &Origin) -> Option<Action> {
    match origin {
        Origin::App(name) | Origin::Agent(name) | Origin::Server { tool: name, .. } => {
            Some(Action::Tool { name: name.clone() })
        }
        Origin::Caf(_) | Origin::NotRecorded => None,
    }
}

/// The lines kept as they were either side of a diff's changes.
const CONTEXT: usize = 3;

/// The most lines, of both files, a diff's hunks are worked out for, not
/// counting those the same at either end.
pub(crate) const MOST_DIFF_WORK: usize = 10_000;

/// What each line of a diff counts against [`MOST_DIFF`] beyond its text:
/// about what keeping it costs, its numbers and its change.
const LINE_COST: usize = 64;

/// `text`'s first `most` bytes, at a character boundary, and how much was
/// kept if that's not all of it.
fn start_of(mut text: String, most: usize) -> (String, Option<Cut>) {
    if text.len() <= most {
        return (text, None);
    }
    let total = text.len() as u64;
    text.truncate(text.floor_char_boundary(most));
    let kept = text.len() as u64;
    (text, Some(Cut { kept, total }))
}

/// A call's raw input, capped: if its JSON is longer than [`MOST_INPUT`],
/// the start of that JSON as a string, and how much of it was kept.
pub(crate) fn capped_input(input: serde_json::Value) -> (serde_json::Value, Option<Cut>) {
    let json = input.to_string();
    if json.len() <= MOST_INPUT {
        return (input, None);
    }
    let (start, cut) = start_of(json, MOST_INPUT);
    (serde_json::Value::String(start), cut)
}

/// An image of type `mime`, `data` in base64: kept whole if it's at most
/// [`MOST_IMAGE`] bytes, and only its size otherwise.
pub(crate) fn image(mime: String, data: String) -> Image {
    // What base64 of this length decodes to: whitespace and padding
    // don't count.
    let digits = data
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        .count();
    let size = (digits * 3 / 4) as u64;
    Image {
        mime,
        data: (size <= MOST_IMAGE as u64).then_some(data),
        size,
    }
}

/// `locations`, the first [`MOST_LOCATIONS`] of them, and how many more
/// there were.
pub(crate) fn capped_locations(mut locations: Vec<Location>) -> (Vec<Location>, u64) {
    let more = locations.len().saturating_sub(MOST_LOCATIONS) as u64;
    locations.truncate(MOST_LOCATIONS);
    (locations, more)
}

#[cfg(test)]
mod tests;

/// What `lines` of an [`Output`] cost of [`MOST_OUTPUT`], in bytes: each
/// one's text and its ending.
pub(crate) fn output_size<'a>(lines: impl IntoIterator<Item = &'a String>) -> usize {
    lines.into_iter().map(|line| line.len() + 1).sum()
}
