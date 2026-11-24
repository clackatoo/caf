//! What comes out of the core: work for the drivers.
//!
//! Directives are grouped by the driver that does them, so each driver
//! handles only its own list, and a host that forgets a driver fails to
//! compile rather than quietly dropping work.

use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use crate::agent::SpawnCommand;
use crate::chat::{AppData, AppItem, NoAppData, NoAppItems};
use crate::ids::{CallId, ChangeNo, ChatId, ItemId, Shelf, Ticket};
use crate::install::{Install, LinkFrom, Patch, SignedInAs};
use crate::limits::ProfileLimits;
use crate::save::{Entry, Header, Saved};
use crate::time::{Mono, Wall};
use crate::tool::{Action, CafTool, Origin};
use crate::usage::LedgerLine;

/// Work for a driver. `A` and `D` are the app's item and data types,
/// which saving carries.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(concrete(A = NoAppItems, D = NoAppData), bound = "A: ts_rs::TS, D: ts_rs::TS")
)]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem, D: AppData")]
pub enum Directive<A: AppItem = NoAppItems, D: AppData = NoAppData> {
    /// For the agent driver.
    Agent(AgentDirective),
    /// For the timer driver.
    Timer(TimerDirective),
    /// For the policy driver.
    Policy(PolicyDirective),
    /// For the viewer driver.
    Viewer(ViewerDirective),
    /// For the storage driver: keeping chats.
    Storage(StorageDirective<A, D>),
    /// For the setup driver: installing agents and signing in to them.
    Setup(SetupDirective),
    /// For the usage driver: the usage ledger.
    Usage(UsageDirective),
    /// For the tools driver: running the tools CAF serves.
    Tool(ToolDirective),
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum AgentDirective {
    /// Start a process in a process group of its own, with its standard
    /// input, output and error piped. It's known as `process` from now on.
    Start {
        /// Its ticket.
        process: Ticket,
        /// How to start it.
        command: SpawnCommand,
        /// The install folder it runs from, if it's CAF's install of an
        /// agent. The driver holds a shared lock on it while the process
        /// runs, so no CAF program on the machine removes it meanwhile, and
        /// as it starts the process it touches the folder's `.used` file
        /// (making it if need be), so a survey can say when the folder was
        /// last used ([`SetupEvent::Surveyed`](crate::event::SetupEvent::Surveyed)).
        hold: Option<PathBuf>,
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum TimerDirective {
    /// Wake the core at this time, replacing any earlier request, or never.
    /// There is only ever one: the core asks for its earliest deadline.
    /// The steady clock may stop while the machine sleeps, so where the
    /// core waits for a time on the wall clock, such as an account
    /// limit's reset, it asks to be woken within five minutes, and looks
    /// again.
    WakeAt(Option<Mono>),
}

/// Work for the policy driver.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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

/// What the agent wants to do, for the policy to decide on.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PermissionRequest {
    /// The tool call it's about, if CAF has seen it: always, for a tool
    /// CAF serves.
    pub item: Option<ItemId>,
    /// What it's called: [`ToolCall::title`](crate::tool::ToolCall::title).
    pub title: String,
    /// Whose tool it is. For a call CAF hasn't seen, what the agent's
    /// adapter says of it, else [`Origin::NotRecorded`].
    pub origin: Origin,
    /// What it does, as CAF reads it: always, for CAF's own tools.
    pub action: Option<Action>,
    /// What the tool was given, as the agent sent it, at most 64 KiB.
    pub input: Option<String>,
}

/// Work for the tools driver: running the tools CAF serves, its own and
/// (*later*) the app's.
///
/// The driver serves them to the agent over MCP (*later*: how), and tells
/// the core of each call that arrives ([`ToolEvent::Called`](crate::event::ToolEvent::Called)); the core
/// decides what's done with it. The driver's side of the contract:
/// - **Every call is answered exactly once**, to the agent, whatever
///   happens: with its result once [`ToolDirective::Run`] finishes it, or
///   as refused or cancelled as the core says. A call the core never
///   answers isn't left hanging: the driver ends it as cancelled when its
///   chat's turn ends, as [`ToolDirective::Cancel`] would.
/// - **It never runs anything the core didn't say to run,** and runs what
///   the core says exactly: the [`Action`] given, with its paths, folder
///   and command as they are, never read again from the model's input.
///   What else the tool takes, which the action doesn't say (an edit's
///   strings, a write's content, a read's `allow_large`, a command's
///   `timeout` and `description`), comes from the `input` given with it,
///   the model's, whole.
/// - **Output is reported in batches,** at most every 100 ms or 16 KiB,
///   and in order.
/// - **A preview changes nothing.** One that fails is answered at once,
///   saying why, and reported as [`ToolEvent::Finished`](crate::event::ToolEvent::Finished)
///   with `done` false: the core asks no one, and sends nothing more.
/// - **A call that goes on after the agent is answered, as a job, is
///   reported [`Detached`](crate::event::ToolEvent::Detached) before the
///   answer is sent**, numbered among its chat's jobs from 1. From then
///   it's no longer its turn's: it's not cancelled as the turn ends, and
///   ends with its own `Finished`, whose `notice` the core sends the agent.
///   A `Cancel` of it after stops it, and answers no one. One the core
///   cancelled before its `Detached` came is cancelled all the same.
/// - **A job outlives its relay's connection**: it's never reported
///   [`Gone`](crate::event::ToolEvent::Gone), as the agent was answered
///   and hears of its end in a notice. (One reported so anyway fails,
///   with no notice.) A job that ends by itself always gives a `notice`.
/// - **A TaskStop ([`Action::Stop`]) is run only for a job of its own
///   chat's still running**: the driver stops it, reports the job's
///   `Finished`, with no `notice`, then the TaskStop's. The job then ends
///   Cancelled, by the agent: one whose `Finished` comes with a notice,
///   having ended by itself first, ends as it says, and the TaskStop is
///   answered "No task {id} in this conversation". Once a TaskStop of a
///   job is run, the job's end with no notice is that stop's, even if
///   the TaskStop was cancelled since, as the driver may have stopped it
///   first.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum ToolDirective {
    /// Work out what this would do, without doing it, and report
    /// [`ToolEvent::Previewed`](crate::event::ToolEvent::Previewed): an edit's diff, before it's asked about.
    Preview {
        /// The call.
        call: CallId,
        /// What it does.
        action: Action,
        /// What the model gave it, whole: for what the action doesn't
        /// say.
        input: serde_json::Value,
    },
    /// Run it: report its output as it comes, then
    /// [`ToolEvent::Finished`](crate::event::ToolEvent::Finished), and answer the agent with its result.
    Run {
        /// The call.
        call: CallId,
        /// What it does.
        action: Action,
        /// What the model gave it, whole: for what the action doesn't
        /// say.
        input: serde_json::Value,
    },
    /// Don't run it: answer the agent that it was refused.
    Refuse {
        /// The call.
        call: CallId,
        /// Why, for the agent.
        why: RefusedCall,
    },
    /// Stop it if it's running, and answer the agent that it was
    /// cancelled. Nothing more is reported of it; anything that comes is
    /// ignored.
    Cancel {
        /// The call.
        call: CallId,
    },
    /// Serve these tools to the chat's relay, with these instructions,
    /// replacing what it was served before: sent when a chat with tools
    /// opens or is restored. A relay that connects for a chat not served
    /// is refused.
    Serve {
        /// The chat.
        chat: ChatId,
        /// CAF's tools it's given, in order.
        tools: Vec<CafTool>,
        /// The chat's folder, as the agent's told it: what the model is
        /// told is its working directory.
        cwd: String,
        /// What the MCP server tells the agent, in a line
        /// (`initialize`'s `instructions`).
        instructions: String,
    },
    /// Stop serving the chat: it's let go. Its connection, if it has one,
    /// is closed. The core has cancelled its calls already
    /// ([`ToolDirective::Cancel`]), and ignores anything reported of them.
    Unserve {
        /// The chat.
        chat: ChatId,
    },
}

/// Why the core refused a call to a tool CAF serves.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum RefusedCall {
    /// The policy or the user said no.
    NotAllowed,
    /// Its chat isn't running a turn.
    NotRunning,
    /// It's not a tool its chat was given.
    NotGiven,
    /// What it was given doesn't make an action, such as a path that
    /// isn't UTF-8, or a field missing: the reason, for the model.
    BadInput(String),
}

/// Work for the viewer driver.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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

/// Work for the storage driver: keeping chats.
///
/// The driver's side of the contract:
/// - **Work on one chat is done in the order it's given.** A chat's
///   journal entries are added in order, after the snapshot before them.
/// - **A chat is locked from when it's loaded or first written until it's
///   released**, across every program using the store: a second program
///   loading it gets [`CannotRestore::InUse`](crate::save::CannotRestore::InUse).
/// - **A write is whole or not there**: a cut-off journal entry is found
///   and dropped when the chat's read, and a snapshot replaces the old
///   one in one step, with its journal.
/// - **A save it can't read is never overwritten or removed**, unless the
///   core deletes it.
/// - **Every directive with a ticket is answered**, once. A
///   [`Release`](StorageDirective::Release) has none: it's done as it's
///   given, before any work given after it.
///
/// And the core's: **while a shelf is being moved, no work for a chat on
/// it, or on the shelf it goes to, is given**, and a move is given only
/// once all work for chats on both is answered. So a store needn't order
/// a move against other work.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(concrete(A = NoAppItems, D = NoAppData), bound = "A: ts_rs::TS, D: ts_rs::TS")
)]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem, D: AppData")]
pub enum StorageDirective<A: AppItem = NoAppItems, D: AppData = NoAppData> {
    /// List a shelf's chats: [`StorageEvent::Listed`](crate::event::StorageEvent::Listed).
    List {
        /// Its ticket.
        ticket: Ticket,
        /// The shelf.
        shelf: Shelf,
    },
    /// Lock a chat and read it: [`StorageEvent::Loaded`](crate::event::StorageEvent::Loaded).
    Load {
        /// Its ticket.
        ticket: Ticket,
        /// Where it's kept.
        shelf: Shelf,
        /// The chat.
        chat: ChatId,
    },
    /// Write a chat whole, locking it if it isn't yet, and empty its
    /// journal.
    Snapshot {
        /// Its ticket.
        ticket: Ticket,
        /// Where it's kept.
        shelf: Shelf,
        /// The chat, whole.
        saved: Box<Saved<A, D>>,
    },
    /// Add to a chat's journal, and replace its header.
    ///
    /// Only to a chat the store holds on that shelf, locked by this
    /// program: one it doesn't isn't added to, and the write fails
    /// ([`StorageEvent::Failed`](crate::event::StorageEvent::Failed)). The
    /// core never asks for one: a chat's first write, and its first after
    /// a write failed or it was let go, is a
    /// [`Snapshot`](StorageDirective::Snapshot).
    Append {
        /// Its ticket.
        ticket: Ticket,
        /// Where it's kept.
        shelf: Shelf,
        /// Its header now.
        header: Header,
        /// What changed, in order.
        entries: Vec<Entry<A, D>>,
    },
    /// Unlock a chat: it's put away, or CAF is shutting down, and it's
    /// written whole already. Or the core has no use for its lock left,
    /// and it's let go as the store has it: a restore found it damaged, it
    /// couldn't be deleted, or it was deleted while its load was under
    /// way and the deletion failed.
    Release {
        /// Where it's kept.
        shelf: Shelf,
        /// The chat.
        chat: ChatId,
    },
    /// Remove a chat for good.
    Delete {
        /// Its ticket.
        ticket: Ticket,
        /// Where it's kept.
        shelf: Shelf,
        /// The chat.
        chat: ChatId,
    },
    /// Move every chat on a shelf to another, such as when framewright
    /// saves a project under a new name. Chats that are loaded move with
    /// their locks. All of the shelf moves, or none of it: a move that
    /// fails leaves every chat where it was. A store refuses a move onto a
    /// shelf that already has chats, as merging shelves isn't supported,
    /// and one chat on `from` open in another program stops the whole
    /// move; either way, it fails and nothing moves.
    MoveShelf {
        /// Its ticket.
        ticket: Ticket,
        /// The shelf.
        from: Shelf,
        /// Where they go.
        to: Shelf,
    },
}

/// Something the host should log.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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

/// Work for the setup driver: installing agents and signing in to them.
///
/// The driver's side of the contract:
/// - **Installs go under one folder every CAF program shares**
///   (`$XDG_DATA_HOME/caf/agents/`), each built in a hidden folder and
///   renamed into place whole, so a cut-off install leaves nothing that
///   looks installed. Half-built folders left by earlier runs are removed.
/// - **One install of a folder at a time**, across every CAF program on the
///   machine: a lock.
/// - **Nothing in use is removed:** removing takes the folder's lock
///   exclusively, and gives up if any process holds it
///   ([`AgentDirective::Start`]'s `hold`).
/// - **Downloads are `https` only, and checked** against their SHA-256
///   before anything is unpacked.
/// - **A sign-in command's output never reaches the core**, except the
///   link and code its recipe names, and a status command's fields only as
///   its recipe names them: so no email or token is ever recorded.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum SetupDirective {
    /// Say what's installed, and what the machine has: the answer is
    /// [`SetupEvent::Surveyed`](crate::event::SetupEvent::Surveyed).
    Survey {
        /// Its ticket.
        ticket: Ticket,
    },
    /// Install into the folder `folder`, reporting progress, then done or
    /// failed.
    Install {
        /// Its ticket.
        ticket: Ticket,
        /// The folder's name, such as `claude-agent-acp-0.98.0`.
        folder: String,
        /// What to install.
        install: Install,
        /// How to patch it once installed, as the last step: see
        /// [`crate::install::Patch`].
        patch: Option<Patch>,
        /// The agent's private folder, such as `claude-agent-acp-private`, if its
        /// recipe gives it one
        /// ([`EnvValue::PrivateFolder`](crate::install::EnvValue::PrivateFolder)):
        /// made beside the installs, readable only by the user, if it isn't
        /// there, and never removed. One that's a link is refused, and the
        /// install fails.
        private: Option<String>,
        /// About how many bytes it takes once installed, as its recipe
        /// says. A download that goes far past it is cut off and thrown
        /// away, so a bad server can't fill the disk.
        size: u64,
    },
    /// Make the folder `folder` from the complete install `from`, an
    /// earlier patch level of the same upstream version, with nothing
    /// downloaded: link `from` into it (copying where it can't), put each
    /// file `from` patched back as it was ([`ORIGINAL_SUFFIX`](crate::install::ORIGINAL_SUFFIX)),
    /// then patch it with `patch` as an install is. `from` is never
    /// changed. Answered as an install is, done or failed; a failed one
    /// leaves nothing behind.
    Prepare {
        /// Its ticket.
        ticket: Ticket,
        /// The folder's name, such as `claude-agent-acp-0.98.0+caf.2`.
        folder: String,
        /// The earlier install's folder, such as
        /// `claude-agent-acp-0.98.0+caf.1`.
        from: String,
        /// The patch to make.
        patch: Patch,
        /// The agent's private folder, if its recipe gives it one: made
        /// as [`Install`](SetupDirective::Install) makes it.
        private: Option<String>,
    },
    /// Remove the folder `folder`, unless something runs from it.
    Remove {
        /// Its ticket.
        ticket: Ticket,
        /// The folder's name.
        folder: String,
        /// Remove it only if it hasn't been used since: if its `.used`
        /// file, or the folder itself if it has none, changed later, a
        /// process started from it since the survey the core decided on,
        /// and it stays (`Removed { removed: false }`). Checked holding the
        /// installs' lock, as it's removed.
        unused_since: Wall,
    },
    /// Run a sign-in command, handing back its link and code as it gives
    /// them, until it ends or is stopped.
    SignIn {
        /// Its ticket.
        ticket: Ticket,
        /// The command, resolved: the adapter as the agent is started, or
        /// a file in its install.
        command: SpawnCommand,
        /// Where its link comes from.
        link: LinkFrom,
        /// What comes before the code it shows, if it shows one.
        code_after: Option<String>,
        /// Whether the user may paste something back to it. Only then is it
        /// given a standard input; otherwise it reads nothing there.
        paste: bool,
    },
    /// Give a running sign-in what the user pasted, with a newline.
    Paste {
        /// The sign-in's ticket.
        ticket: Ticket,
        /// What they pasted.
        text: String,
    },
    /// Run a command and say whether it succeeded, such as signing out.
    Run {
        /// Its ticket.
        ticket: Ticket,
        /// The command, resolved.
        command: SpawnCommand,
    },
    /// Run a status command, and hand back only the fields its recipe
    /// names.
    Status {
        /// Its ticket.
        ticket: Ticket,
        /// The command, resolved.
        command: SpawnCommand,
        /// The JSON pointer of the field that's `true` when signed in.
        signed_in: String,
        /// The JSON pointer of the field naming how, and what its values
        /// mean.
        how: Option<(String, Vec<(String, SignedInAs)>)>,
    },
    /// Stop an install, sign-in or command under way, within about a
    /// second, killing what it runs. Its last report says it was stopped:
    /// - an install's is
    ///   [`InstallFailed(Stopped)`](crate::agent::InstallFailure::Stopped),
    ///   once what it had built is gone;
    /// - a sign-in's, a status command's or another command's is
    ///   [`Ended { ok: false }`](crate::event::SetupEvent::Ended).
    ///
    /// A survey or a removal isn't stopped: it ends by itself, soon. Nor is
    /// work that has ended already, or a ticket the driver never had:
    /// those get no report.
    Stop {
        /// Its ticket.
        ticket: Ticket,
    },
}

/// Work for the usage driver: keeping the usage ledger, which every CAF
/// app on the machine shares, and the prices.
///
/// The driver's side of the contract:
/// - **Lines go under one folder every CAF program shares**
///   (`$XDG_DATA_HOME/caf/usage/`), in a folder per app, a file per month
///   (`clackatoo/2026-11.jsonl`), each line whole: a lock is held while one
///   is added, so two copies of an app never mix theirs. A file for a month
///   that has ended is compressed (`2026-10.jsonl.zst`).
/// - **Lines are only ever added**, never changed or removed, but for a
///   turn's line written again: when a month's late lines are joined into
///   its compressed file, one for a turn it already holds is dropped, and
///   nothing else.
/// - **A line it can't write is kept, and tried again** with the next one,
///   and the failure reported to the host. Nothing it does ever holds up a
///   chat, and it never answers the core: a ledger that fails costs only
///   the summary, never a turn.
/// - **It tells the core the prices** ([`Event::Prices`](crate::Event::Prices)):
///   once as it starts, from what it has, and again whenever they change,
///   because LiteLLM's table was fetched or the user set a price of their
///   own.
///
/// - **It keeps each profile's account limits** in a file of their own
///   beside the ledger, every CAF app's to share
///   (`limits/claude-agent-acp.json`, after the profile's
///   [slug](crate::profiles::Builtin::slug)), and merges each save into
///   what's there under the folder's lock, replacing the file in one
///   step, so two apps never undo each other's later news. It tells the
///   core what it holds as it starts, after the prices, and again
///   whenever a save took in news the core didn't send
///   ([`Event::Limits`](crate::Event::Limits)). A file it can't read or
///   write is reported, and costs only keeping them across restarts.
///
/// A host that keeps no ledger drops these: its chats still record their
/// usage and spend, and its agents' limits are kept while it runs.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum UsageDirective {
    /// Add a turn's line to the ledger.
    Record(Box<LedgerLine>),
    /// Keep a profile's account limits: merge them with those kept,
    /// window by window, the latest heard winning, and dropping what was
    /// heard before the later `cleared`. If what was kept had news of its
    /// own, tell the core the merged limits
    /// ([`Event::Limits`](crate::Event::Limits)).
    SaveLimits(Box<ProfileLimits>),
}
