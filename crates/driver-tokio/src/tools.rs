//! The tools driver: serves CAF's tools to agents over MCP, through
//! relays, and runs the calls the core says to run.
//!
//! An agent given CAF's tools starts a relay ([`caf_relay`]) as an MCP
//! server of its session. The relay connects to the socket this driver
//! listens on, and says which chat it serves with a token only that chat's
//! sent servers hold ([`caf_core::mcp::token`]). The driver binds the
//! connection to the chat, and answers MCP on it with a
//! [`caf_core::mcp::Connection`]: the chat a call is for is always the
//! one its connection was bound to, never anything the call says.
//!
//! Each call is numbered ([`CallId`]) and handed to the core
//! ([`ToolEvent::Called`]), which decides what's done with it; the
//! driver previews and runs what it says with the [`Runner`], and
//! answers the model. Every call is answered at most once: with what it
//! gave, or refused, or cancelled. One the agent cancelled, or whose
//! connection closed, is answered never, as nobody's left to hear it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::io::Read as _;
use std::os::unix::fs::{DirBuilderExt as _, FileTypeExt as _, MetadataExt as _};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use caf_core::agent::{EnvChange, SpawnCommand};
use caf_core::chat::{AppItem, NoAppItems};
use caf_core::directive::{RefusedCall, ToolDirective};
use caf_core::event::{RelayChange, Serving, ToolEvent};
use caf_core::ids::{CallId, ChatId, JobId};
use caf_core::mcp::{self, Answer, AnswerPart, AppListing, Connection, Key, Out, Request};
use caf_core::tool::{Action, CafTool, Piece, ServedTool, Text};
use caf_tools::{Commands, Files};
use serde::Deserialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::{Instant, Sleep};

use crate::app_tools::{AppTools, Done, Erased};
use crate::files::DiskFiles;
use crate::pty::PtyCommands;
use crate::runner::CafRunner;

/// How the app serves CAF's tools, and its own: [`Config::tools`](crate::Config::tools).
#[non_exhaustive]
pub struct ToolsConfig<A: AppItem = NoAppItems> {
    /// The app's own tools ([`AppTools`]): none by default. A chat is
    /// given them by name ([`Tools::app`](caf_core::tool::Tools::app)).
    pub app: AppTools<A>,
    /// The program an agent starts to reach CAF's tools. [`Relay::None`]
    /// by default: no tools are served.
    pub relay: Relay,
    /// What runs the calls: `None` (the default) for CAF's own runner,
    /// which reads and writes through [`files`](Self::files) and runs
    /// commands with [`commands`](Self::commands).
    pub runner: Option<Box<dyn Runner>>,
    /// Where CAF's own runner reads and writes files: the disk
    /// ([`DiskFiles`]) by default, or the app's editor buffers.
    pub files: Arc<dyn Files>,
    /// Where CAF's own runner runs commands: a pty of its own for each
    /// ([`PtyCommands`]) by default, or the app's visible terminals.
    pub commands: Arc<dyn Commands>,
    /// The folder the socket relays connect to is made in, which must be
    /// the user's alone. `None` (the default) means `$XDG_RUNTIME_DIR/caf`,
    /// or else `caf-<uid>` in the temporary folder; either is made, mode
    /// 700, if it isn't there.
    pub folder: Option<PathBuf>,
    /// How often an open call's progress is sent: 60 s. For tests.
    #[doc(hidden)]
    pub progress_every: Duration,
}

impl<A: AppItem> Default for ToolsConfig<A> {
    fn default() -> Self {
        Self {
            app: AppTools::new(),
            relay: Relay::None,
            runner: None,
            files: Arc::new(DiskFiles),
            commands: Arc::new(PtyCommands),
            folder: None,
            progress_every: PROGRESS_EVERY,
        }
    }
}

impl<A: AppItem> std::fmt::Debug for ToolsConfig<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolsConfig")
            .field("app", &self.app)
            .field("relay", &self.relay)
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}

/// The program an agent starts as CAF's MCP server.
#[non_exhaustive]
#[derive(Clone, Debug, Default)]
pub enum Relay {
    /// None: CAF serves no tools. A chat given some is refused
    /// ([`Refused::ToolsNotServed`](caf_core::Refused::ToolsNotServed)).
    #[default]
    None,
    /// This program, run again, which must call
    /// [`caf_relay::relay_if_asked`] first thing in its `main`.
    ThisProgram,
    /// This command, which must do as [`caf_relay::relay_if_asked`]
    /// does, such as the `caf-relay` program.
    Command(SpawnCommand),
}

/// Why CAF couldn't start serving its tools ([`Caf::try_start`](crate::Caf::try_start)).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartError {
    /// The relay didn't answer its check as [`caf_relay::relay_if_asked`]
    /// does: most likely the program's `main` doesn't call it first.
    NoRelayHook {
        /// The relay's program.
        relay: PathBuf,
        /// What it did instead, such as printing something else.
        why: String,
    },
    /// The socket's path is longer than a unix socket's may be: give
    /// [`ToolsConfig::folder`] a shorter folder.
    SocketPathTooLong {
        /// The path.
        path: PathBuf,
    },
    /// Anything else that keeps CAF from serving, as the system says,
    /// such as a socket folder others can reach.
    Serving(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRelayHook { relay, why } => write!(
                f,
                "CAF's relay {} {why}: call caf_relay::relay_if_asked() first thing in its main",
                relay.display()
            ),
            Self::SocketPathTooLong { path } => write!(
                f,
                "the socket for CAF's tools, {}, is {} bytes long, and a unix socket's path \
                 may be at most {SOCKET_PATH_MOST}: give ToolsConfig::folder a shorter folder",
                path.display(),
                path.as_os_str().len()
            ),
            Self::Serving(why) => write!(f, "CAF can't serve its tools: {why}"),
        }
    }
}

impl std::error::Error for StartError {}

/// What runs calls to CAF's tools, for the tools driver.
///
/// Cancelling a call is dropping its future, so a runner cleans up on
/// drop: a command it started is stopped, say. So is stopping a job
/// (TaskStop): the driver drops the job's future, and never asks the
/// runner.
pub trait Runner: Send + 'static {
    /// What `action` would do, without doing anything, such as an edit's
    /// diff: or, if it can't be done, what the call ends with, at once,
    /// asking no one, such as an edit of a file never read.
    fn preview(&mut self, call: &Call, action: Action) -> Preview;
    /// Does `action`, printing a command's output with
    /// [`Call::print`] as it comes. It may answer the model before it
    /// ends, and go on as a job ([`Call::answer_now`]).
    fn run(&mut self, call: Call, action: Action) -> Pin<Box<dyn Future<Output = Ran> + Send>>;
    /// The chat is let go, or no longer served tools: whatever the runner
    /// keeps for it can go. Its calls have all been dropped already.
    fn let_go(&mut self, chat: ChatId) {
        let _ = chat;
    }
    /// Call `call` won't be run, or has gone, whether it was previewed or
    /// not: whatever the runner keeps for it can go.
    fn forget(&mut self, call: CallId) {
        let _ = call;
    }
}

/// What a [`Runner`] previews: what the call would do, or what it ends
/// with, failed, if it can't be done.
pub type Preview = Pin<Box<dyn Future<Output = Result<Vec<Piece>, Ran>> + Send>>;

/// What a call did.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Ran {
    /// Whether it did what it was asked.
    pub done: bool,
    /// What it gave back, for the record
    /// ([`ToolEvent::Finished`]'s `pieces`).
    pub record: Vec<Piece>,
    /// What the model is answered. Unused for a job: the model was
    /// answered when it became one.
    pub answer: Answer,
    /// The folder a command's shell ended in, if known, as an absolute
    /// path: where the chat's next command runs.
    pub folder: Option<String>,
    /// For a job, what the agent is told of its end, in a notice turn.
    /// A job ending by itself without one is given a plain one saying it
    /// failed: only TaskStop's stop ([`Action::Stop`]) ends a job with
    /// none, so the core tells a job that ended just as it was stopped
    /// from one it stopped.
    pub notice: Option<String>,
}

impl Ran {
    /// A call that did what it was asked if `done`, recording `record` and
    /// answering the model `answer`.
    #[must_use]
    pub fn new(done: bool, record: Vec<Piece>, answer: Answer) -> Self {
        Self {
            done,
            record,
            answer,
            folder: None,
            notice: None,
        }
    }

    /// A call that failed, saying `why` to the record and the model.
    #[must_use]
    pub fn failed(why: impl Into<String>) -> Self {
        let why = why.into();
        Self::new(
            false,
            vec![Piece::Text(Text::new(why.clone()))],
            Answer::error(why),
        )
    }

    /// With the folder its command's shell ended in.
    #[must_use]
    pub fn with_folder(mut self, folder: impl Into<String>) -> Self {
        self.folder = Some(folder.into());
        self
    }

    /// With what the agent is told of a job's end.
    #[must_use]
    pub fn with_notice(mut self, notice: impl Into<String>) -> Self {
        self.notice = Some(notice.into());
        self
    }
}

/// One call, as the [`Runner`] is given it.
#[derive(Debug)]
pub struct Call {
    chat: ChatId,
    id: CallId,
    cwd: PathBuf,
    input: serde_json::Value,
    agent_call: Option<String>,
    prints: mpsc::UnboundedSender<Print>,
    jobs: Jobs,
    job: Option<JobId>,
}

/// The last job number each chat was given, shared by the driver and its
/// calls: so a job's number is known as its answer is made.
type Jobs = Arc<Mutex<HashMap<ChatId, u64>>>;

impl Call {
    /// Call `id` in `chat`, telling `prints` what it prints, with job
    /// numbers of its own: for [`try_tool`](crate::try_tool).
    pub(crate) fn new(
        chat: ChatId,
        id: CallId,
        cwd: PathBuf,
        input: serde_json::Value,
        agent_call: Option<String>,
        prints: mpsc::UnboundedSender<Print>,
    ) -> Self {
        Self {
            chat,
            id,
            cwd,
            input,
            agent_call,
            prints,
            jobs: Jobs::default(),
            job: None,
        }
    }

    /// The chat it's for.
    #[must_use]
    pub fn chat(&self) -> ChatId {
        self.chat
    }

    /// Its number.
    #[must_use]
    pub fn id(&self) -> CallId {
        self.id
    }

    /// The chat's folder: what the model is told is its working
    /// directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// What the model gave it, whole: for what its action doesn't say,
    /// such as an edit's strings. Its paths, folder and command are the
    /// action's, never read from here.
    #[must_use]
    pub fn input(&self) -> &serde_json::Value {
        &self.input
    }

    /// A running command's output changed: the lines from `from` on, as
    /// its terminal shows them, counting every line it has printed from
    /// 0 ([`ToolEvent::Output`]). Batched: what's printed is reported at
    /// most every 100 ms or 16 KiB.
    pub fn print(&self, from: u64, lines: Vec<String>) {
        let _ = self.prints.send(Print::Lines {
            call: self.id,
            from,
            lines,
        });
    }

    /// The agent's own id for the call, if it gave one, such as Claude's
    /// `tool_use` id: for a job's notice.
    #[must_use]
    pub fn agent_call(&self) -> Option<&str> {
        self.agent_call.as_deref()
    }

    /// Answers the model now, with what `answer` makes of the call's job
    /// number, and goes on as that job: the run's future carries on, and
    /// what it gives at the end is the job's record, and its notice
    /// ([`Ran::with_notice`]). Reported as
    /// [`ToolEvent::Detached`] before the answer is sent. Once only: a
    /// second does nothing, and gives the job it is already.
    pub fn answer_now(&mut self, answer: impl FnOnce(JobId) -> Answer) -> JobId {
        if let Some(job) = self.job {
            return job;
        }
        let job = {
            let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            let last = jobs.entry(self.chat).or_default();
            *last += 1;
            JobId(*last)
        };
        self.job = Some(job);
        let _ = self.prints.send(Print::Detach {
            call: self.id,
            job,
            answer: answer(job),
        });
        job
    }

    /// The job it went on as, if it did.
    #[must_use]
    pub fn job(&self) -> Option<JobId> {
        self.job
    }
}

/// The runner that fails every call, saying CAF's tools aren't built:
/// what a runner did before CAF's own were.
#[derive(Clone, Copy, Debug, Default)]
pub struct NotBuilt;

/// What [`NotBuilt`] says.
const NOT_BUILT: &str = "CAF's tools aren't built yet";

impl Runner for NotBuilt {
    fn preview(&mut self, _call: &Call, _action: Action) -> Preview {
        Box::pin(std::future::ready(Ok(Vec::new())))
    }

    fn run(&mut self, _call: Call, _action: Action) -> Pin<Box<dyn Future<Output = Ran> + Send>> {
        Box::pin(std::future::ready(failed(NOT_BUILT)))
    }
}

/// A call that failed, saying `why` to the record and the model.
fn failed(why: &str) -> Ran {
    Ran::failed(why)
}

/// How long a relay has to say which chat it serves.
const HANDSHAKE_TIME: Duration = Duration::from_secs(10);
/// The longest handshake read.
const HANDSHAKE_MOST: usize = 4096;
/// The longest line read from a relay: one over closes its connection.
const LINE_MOST: usize = mcp::MOST_LINE;
/// Output is reported at most this long after it's printed...
const BATCH_TIME: Duration = Duration::from_millis(100);
/// ...or once this much is waiting.
const BATCH_BYTES: usize = 16 * 1024;
/// How often an open call's progress is sent, so the agent doesn't time
/// it out while it waits for the user or runs.
const PROGRESS_EVERY: Duration = Duration::from_secs(60);
/// How long the relay check may take.
const CHECK_TIME: Duration = Duration::from_secs(10);
/// The longest path a unix socket takes, without its closing NUL.
const SOCKET_PATH_MOST: usize = 107;
/// How many lines may wait to be written to a relay: an agent that stops
/// reading while more come has its relay closed.
const WRITES_WAITING: usize = 256;
/// How long one line may take to write to a relay, before it's given up.
const WRITE_TIME: Duration = Duration::from_secs(10);
/// How long to wait before taking connections again after failing to.
const ACCEPT_AGAIN: Duration = Duration::from_millis(100);
/// How long shutting down waits for answers to be written.
const FINISH_TIME: Duration = Duration::from_secs(1);

/// What the driver reports, for the core.
#[derive(Debug)]
pub(crate) enum Report<A: AppItem = NoAppItems> {
    /// [`Event::Tool`](caf_core::Event::Tool).
    Tool { call: CallId, event: ToolEvent<A> },
    /// [`Event::Relay`](caf_core::Event::Relay).
    Relay { chat: ChatId, change: RelayChange },
}

/// A future the runner gave.
type Running<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What a call ended with: what it did, and the app's item, which only
/// the app's own tools give.
type Ended<A> = (Ran, Option<A>);

/// The tools driver.
pub(crate) struct ToolsDriver<A: AppItem = NoAppItems> {
    runner: Box<dyn Runner>,
    /// The app's own tools.
    app: AppTools<A>,
    /// Where relays connect, if tools are served.
    live: Option<Live>,
    /// What each chat is served.
    served: HashMap<ChatId, Served>,
    /// Each bound connection, by its number.
    conns: HashMap<u64, Conn>,
    /// Each chat's bound connection.
    bound: HashMap<ChatId, u64>,
    /// Each open call.
    calls: BTreeMap<CallId, Each<A>>,
    /// The last call's number.
    last_call: u64,
    prints: mpsc::UnboundedSender<Print>,
    printed: mpsc::UnboundedReceiver<Print>,
    /// Each chat's last job number.
    jobs: Jobs,
    /// Reports waiting to be handed out.
    reports: VecDeque<Report<A>>,
    /// The answer to each call that finished, or became a job, held until
    /// the core has taken its [`ToolEvent::Finished`] or
    /// [`ToolEvent::Detached`]: so the agent can't end its turn, or stop
    /// the job, on hearing it before the core knows.
    held: HashMap<CallId, Held>,
    /// The call whose `Finished` or `Detached` was handed out last, its
    /// answer to be written once the core has it ([`ToolsDriver::taken`]).
    finished: Option<CallId>,
    /// One sleep, moved to the next batch or progress due.
    sleep: Pin<Box<Sleep>>,
    /// The tasks writing to relays, waited for as CAF shuts down. Each
    /// gives its connection's number if it gave up, as the socket broke or
    /// a line took too long.
    writers: JoinSet<Option<u64>>,
    /// [`ToolsConfig::progress_every`].
    progress_every: Duration,
}

/// The socket relays connect to, and the connections not yet bound.
struct Live {
    listener: UnixListener,
    /// Removed when dropped.
    socket: SocketFile,
    key: [u8; 32],
    from_conns: mpsc::Sender<(u64, FromConn)>,
    to_driver: mpsc::Receiver<(u64, FromConn)>,
    /// Each connection's reading task, bound or not.
    readers: HashMap<u64, AbortHandle>,
    last_conn: u64,
    /// After failing to take a connection, such as with too many files
    /// open, when to try again.
    accept_again: Option<Instant>,
}

/// The socket's file, removed when it's dropped.
struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct Served {
    tools: Vec<CafTool>,
    /// The app's own tools, by name.
    app: Vec<String>,
    instructions: String,
    /// The chat's folder.
    cwd: PathBuf,
}

/// A finished call's answer, waiting for the core to take its `Finished`.
struct Held {
    conn: u64,
    request: Request,
    answer: Answer,
}

/// A bound connection.
struct Conn {
    /// Its writing task, stopped at once if it's lost or replaced.
    writer: AbortHandle,
    chat: ChatId,
    mcp: Connection,
    /// Lines for its writing task, at most [`WRITES_WAITING`]. Dropped,
    /// the task ends the socket.
    write: mpsc::Sender<Vec<u8>>,
}

/// An open call.
struct Each<A: AppItem> {
    chat: ChatId,
    conn: u64,
    request: Request,
    /// The agent's own id for it.
    agent_call: Option<String>,
    /// The app's tool it calls, if it's one of the app's, and the call's
    /// title.
    app: Option<(Arc<dyn Erased<A>>, String)>,
    /// Whether the core has said to run it.
    running: bool,
    /// The job it became, once the model was answered and it went on:
    /// its request is answered, and it's no longer its connection's.
    job: Option<JobId>,
    preview: Option<Running<Result<Vec<Piece>, Ended<A>>>>,
    run: Option<Running<Ended<A>>>,
    /// When its progress is next sent.
    progress_at: Instant,
    /// Output printed and not yet reported.
    batch: Option<Batch>,
}

struct Batch {
    from: u64,
    lines: Vec<String>,
    bytes: usize,
    /// When it's reported, at the latest.
    due: Instant,
}

/// Lines a call printed.
#[derive(Debug)]
struct Printed {
    from: u64,
    lines: Vec<String>,
}

/// What a call tells the driver as it runs.
#[derive(Debug)]
pub(crate) enum Print {
    /// [`Call::print`].
    Lines {
        call: CallId,
        from: u64,
        lines: Vec<String>,
    },
    /// [`Call::answer_now`].
    Detach {
        call: CallId,
        job: JobId,
        answer: Answer,
    },
}

/// What a connection's reading task tells the driver.
enum FromConn {
    /// It said which chat it serves, with the right token.
    Hello { chat: ChatId, write: OwnedWriteHalf },
    /// It didn't: why, for the log.
    Refused(String),
    /// A line, without its newline.
    Line(Vec<u8>),
    /// It closed, or broke: why, if it broke.
    Closed(Option<String>),
}

/// Why a connection is closed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Closing {
    /// It closed, or broke: its calls are gone, and its chat lost its
    /// tools.
    Lost,
    /// A newer one came for its chat: its calls are gone.
    Replaced,
    /// Its chat was let go, or CAF is shutting down: its calls are
    /// answered as cancelled, and the core isn't told.
    Ended,
}

/// The handshake a relay sends.
#[derive(Deserialize)]
struct Hello {
    caf_relay: u32,
    chat: ChatId,
    token: String,
}

impl<A: AppItem> ToolsDriver<A> {
    /// A driver as `config` says, serving relays at `live`, if any, from
    /// the tools' `folder`.
    /// `folder` is the tools' folder, and the name of this driver's own in
    /// its `out/`.
    fn with(config: ToolsConfig<A>, live: Option<Live>, folder: Option<(&Path, &str)>) -> Self {
        let (prints, printed) = mpsc::unbounded_channel();
        let runner = match (config.runner, folder) {
            (Some(runner), _) => runner,
            (None, Some((folder, out))) => Box::new(CafRunner::new(
                config.files,
                config.commands,
                &folder.join("out"),
                out,
            )),
            // Nothing is served, so nothing is run.
            (None, None) => Box::new(NotBuilt),
        };
        Self {
            runner,
            app: config.app,
            progress_every: config.progress_every.max(Duration::from_millis(1)),
            live,
            served: HashMap::new(),
            conns: HashMap::new(),
            bound: HashMap::new(),
            calls: BTreeMap::new(),
            last_call: 0,
            prints,
            printed,
            jobs: Jobs::default(),
            reports: VecDeque::new(),
            held: HashMap::new(),
            finished: None,
            sleep: Box::pin(tokio::time::sleep_until(far_off())),
            writers: JoinSet::new(),
        }
    }

    /// Starts serving as `config` says, for the app named `app`: checks
    /// the relay, listens on the socket and draws the key, giving what
    /// the core is told ([`Event::Serving`](caf_core::Event::Serving)).
    /// Nothing, for [`Relay::None`]. Must be called on a tokio runtime.
    ///
    /// # Errors
    ///
    /// Why it can't serve, for the app's developer.
    pub(crate) fn start(
        mut config: ToolsConfig<A>,
        app: &str,
    ) -> Result<(Self, Option<Serving>), StartError> {
        let relay = match std::mem::take(&mut config.relay) {
            Relay::None => return Ok((Self::with(config, None, None), None)),
            Relay::ThisProgram => {
                let program = std::env::current_exe()
                    .map_err(|e| StartError::Serving(format!("it can't find this program: {e}")))?;
                SpawnCommand::new(program)
            }
            Relay::Command(command) => command,
        };
        sendable(&relay).map_err(|why| {
            StartError::Serving(format!(
                "its relay {} can't be sent to an agent: {why}",
                relay.program.display()
            ))
        })?;
        check_relay(&relay)?;
        let (driver, serving) = Self::listen(config, relay, app)?;
        Ok((driver, Some(serving)))
    }

    /// Serves through `relay`, unchecked.
    fn listen(
        mut config: ToolsConfig<A>,
        relay: SpawnCommand,
        app: &str,
    ) -> Result<(Self, Serving), StartError> {
        let folder = match config.folder.take() {
            Some(folder) => folder,
            None => default_folder(),
        };
        // A second CAF in the same process gets a name of its own.
        static STARTED: AtomicU64 = AtomicU64::new(0);
        let nth = STARTED.fetch_add(1, Ordering::Relaxed);
        // Its output files' folder is named for this process too: one
        // that's gone left its for the next to remove.
        let out = match nth {
            0 => std::process::id().to_string(),
            nth => format!("{}-{nth}", std::process::id()),
        };
        let name = match nth {
            0 => format!("{}-{}.sock", file_name(app), std::process::id()),
            nth => format!("{}-{}-{nth}.sock", file_name(app), std::process::id()),
        };
        let socket = folder.join(name);
        // Before the folder is made, so a path too long leaves nothing.
        if socket.as_os_str().len() > SOCKET_PATH_MOST {
            return Err(StartError::SocketPathTooLong { path: socket });
        }
        // The agent is sent it as text.
        if socket.to_str().is_none() {
            return Err(StartError::Serving(format!(
                "its socket's path, {}, isn't UTF-8, so it can't be sent to an agent",
                socket.display()
            )));
        }
        private_folder(&folder).map_err(StartError::Serving)?;
        let (listener, file) = listen(&socket)?;
        let key = random_key().map_err(StartError::Serving)?;
        let mut serving = Serving::new(relay, socket.clone(), Key(key));
        serving.app_tools = config.app.specs();
        let (from_conns, to_driver) = mpsc::channel(64);
        let live = Live {
            listener,
            socket: file,
            key,
            from_conns,
            to_driver,
            readers: HashMap::new(),
            last_conn: 0,
            accept_again: None,
        };
        Ok((
            Self::with(config, Some(live), Some((&folder, &out))),
            serving,
        ))
    }

    /// Carries out the core's directive.
    pub(crate) fn apply(&mut self, directive: ToolDirective) {
        match directive {
            ToolDirective::Serve {
                chat,
                tools,
                app,
                instructions,
                cwd,
                ..
            } => {
                self.served.insert(
                    chat,
                    Served {
                        tools,
                        app,
                        instructions,
                        cwd: PathBuf::from(cwd),
                    },
                );
            }
            ToolDirective::Unserve { chat } => {
                self.served.remove(&chat);
                if let Some(conn) = self.bound.get(&chat).copied() {
                    self.close(conn, Closing::Ended);
                }
                // The core cancelled them already: any still here, whose
                // connection was lost, stop now.
                // Its job numbers go on counting: none is ever used twice.
                self.calls.retain(|_, each| each.chat != chat);
                let runner = &mut self.runner;
                if std::panic::catch_unwind(AssertUnwindSafe(|| runner.let_go(chat))).is_err() {
                    log::warn!(target: "caf", "the tool runner panicked letting go of chat {}", chat.0);
                }
            }
            ToolDirective::Preview {
                call,
                action,
                input,
            } => {
                let Some(given) = self.call_for(call, input) else {
                    return;
                };
                let Some(each) = self.calls.get_mut(&call) else {
                    return;
                };
                let runner = &mut self.runner;
                let input = given.input.clone();
                let preview = std::panic::catch_unwind(AssertUnwindSafe(|| match &each.app {
                    Some((tool, _)) => match tool.preview(&given, &input) {
                        Some(preview) => Box::pin(async move { preview.await.map_err(ended) })
                            as Running<Result<Vec<Piece>, Ended<A>>>,
                        None => Box::pin(std::future::ready(Ok(Vec::new()))),
                    },
                    None => {
                        let preview = runner.preview(&given, action);
                        Box::pin(async move { preview.await.map_err(|ran| (ran, None)) })
                    }
                }))
                .unwrap_or_else(|_| {
                    log::warn!(target: "caf", "the tool runner panicked previewing call {}", call.0);
                    Box::pin(std::future::ready(Err((failed(PANICKED), None))))
                });
                each.preview = Some(preview);
            }
            ToolDirective::Run {
                call,
                action,
                input,
            } => {
                if let Action::Stop { job } = action {
                    self.stop_job(call, job);
                    self.wake_at_next();
                    return;
                }
                let Some(given) = self.call_for(call, input) else {
                    return;
                };
                let Some(each) = self.calls.get_mut(&call) else {
                    return;
                };
                let runner = &mut self.runner;
                let run = std::panic::catch_unwind(AssertUnwindSafe(|| match &each.app {
                    Some((tool, _)) => {
                        let input = given.input.clone();
                        let run = tool.run(given, &input);
                        Box::pin(async move { ended(run.await) }) as Running<Ended<A>>
                    }
                    None => {
                        let run = runner.run(given, action);
                        Box::pin(async move { (run.await, None) })
                    }
                }))
                .unwrap_or_else(|_| {
                    log::warn!(target: "caf", "the tool runner panicked running call {}", call.0);
                    Box::pin(std::future::ready((failed(PANICKED), None)))
                });
                each.preview = None;
                each.running = true;
                each.run = Some(run);
            }
            ToolDirective::Refuse { call, why } => {
                self.forget(call);
                if let Some(each) = self.calls.remove(&call) {
                    self.answer(each.conn, &each.request, &Answer::error(refused(&why)));
                }
            }
            ToolDirective::Cancel { call } => {
                self.forget(call);
                if let Some(each) = self.calls.remove(&call) {
                    // Its work stops before the model hears it did. A job's
                    // model was answered, unless that answer still waits.
                    let job = each.job.is_some();
                    let (conn, request) = each.stop();
                    if !job || self.held.remove(&call).is_some() {
                        self.answer(conn, &request, &Answer::error(CANCELLED));
                    }
                } else if let Some(held) = self.held.remove(&call) {
                    // It finished as the core stopped it: the work was
                    // done, so the model hears what it gave.
                    self.answer(held.conn, &held.request, &held.answer);
                }
            }
            // No other kind is given yet.
            _ => {}
        }
        self.wake_at_next();
    }

    /// Tells the runner `call` won't run, or has gone.
    fn forget(&mut self, call: CallId) {
        let runner = &mut self.runner;
        if std::panic::catch_unwind(AssertUnwindSafe(|| runner.forget(call))).is_err() {
            log::warn!(target: "caf", "the tool runner panicked forgetting call {}", call.0);
        }
    }

    /// The call `call` is, for the runner: `None` if it's gone.
    fn call_for(&self, call: CallId, input: serde_json::Value) -> Option<Call> {
        let each = self.calls.get(&call)?;
        let cwd =
            (self.served.get(&each.chat)).map_or_else(PathBuf::new, |served| served.cwd.clone());
        Some(Call {
            chat: each.chat,
            id: call,
            cwd,
            input,
            agent_call: each.agent_call.clone(),
            prints: self.prints.clone(),
            jobs: Arc::clone(&self.jobs),
            job: None,
        })
    }

    /// Runs TaskStop `call`, of job `job`: stops the job, if it's its
    /// chat's and still running, and reports its end, then the
    /// TaskStop's.
    fn stop_job(&mut self, call: CallId, job: JobId) {
        let Some(chat) = self.calls.get(&call).map(|each| each.chat) else {
            return;
        };
        let found = (self.calls.iter())
            .find(|(_, each)| each.chat == chat && each.job == Some(job))
            .map(|(&target, _)| target);
        let ran =
            match found.and_then(|target| self.calls.remove(&target).map(|each| (target, each))) {
                Some((target, mut each)) => {
                    if let Some(batch) = each.batch.take() {
                        self.reports.push_back(output(target, batch));
                    }
                    // Dropped, its command stops.
                    let _ = each.stop();
                    // Any answer it still had waiting is the model's.
                    if let Some(held) = self.held.remove(&target) {
                        self.answer(held.conn, &held.request, &held.answer);
                    }
                    self.reports.push_back(Report::Tool {
                        call: target,
                        event: ToolEvent::Finished {
                            done: false,
                            pieces: Vec::new(),
                            folder: None,
                            notice: None,
                            item: None,
                        },
                    });
                    Ran::new(
                        true,
                        vec![Piece::Text(Text::new(caf_tools::job_stopped(job.0)))],
                        Answer::text(caf_tools::job_stopped(job.0)),
                    )
                }
                None => Ran::failed(caf_tools::no_job(job.0)),
            };
        if let Some(each) = self.calls.get_mut(&call) {
            each.running = true;
            each.preview = None;
            each.run = Some(Box::pin(std::future::ready((ran, None))));
        }
    }

    /// The next report, for the core. Cancel-safe: everything is kept in
    /// the driver between polls.
    pub(crate) async fn next(&mut self) -> Report<A> {
        std::future::poll_fn(|cx| self.poll_next(cx)).await
    }

    fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Report<A>> {
        if let Some(report) = self.pop() {
            return Poll::Ready(report);
        }
        // Each writer done with its socket is let go. One that gave up
        // leaves its connection dead: it's closed, as lost.
        while let Poll::Ready(Some(ended)) = self.writers.poll_join_next(cx) {
            if let Ok(Some(no)) = ended
                && let Some(conn) = self.conns.get(&no)
            {
                log::warn!(target: "caf", "a relay for chat {} was closed: it couldn't be written to", conn.chat.0);
                self.close(no, Closing::Lost);
            }
        }
        if let Some(report) = self.pop() {
            return Poll::Ready(report);
        }
        self.poll_accept(cx);
        self.poll_conns(cx);
        self.poll_prints(cx);
        self.poll_calls(cx);
        self.poll_due(cx);
        match self.pop() {
            Some(report) => Poll::Ready(report),
            None => Poll::Pending,
        }
    }

    /// The next report, noting a call's `Finished`, whose answer waits for
    /// it.
    fn pop(&mut self) -> Option<Report<A>> {
        let report = self.reports.pop_front()?;
        if let Report::Tool {
            call,
            event: ToolEvent::Finished { .. } | ToolEvent::Detached { .. },
        } = &report
        {
            self.finished = Some(*call);
        }
        Some(report)
    }

    /// The core has taken the report [`ToolsDriver::next`] gave last: if it
    /// was a call's `Finished`, the agent is answered now.
    pub(crate) fn taken(&mut self) {
        let Some(call) = self.finished.take() else {
            return;
        };
        if let Some(held) = self.held.remove(&call) {
            self.answer(held.conn, &held.request, &held.answer);
        }
    }

    /// Takes new connections, each read on a task of its own.
    fn poll_accept(&mut self, cx: &mut Context<'_>) {
        let Some(live) = &mut self.live else {
            return;
        };
        if live.accept_again.is_some_and(|at| at > Instant::now()) {
            return;
        }
        live.accept_again = None;
        while let Poll::Ready(accepted) = live.listener.poll_accept(cx) {
            match accepted {
                Ok((stream, _)) => {
                    live.last_conn += 1;
                    let no = live.last_conn;
                    let reader =
                        tokio::spawn(read_conn(no, stream, live.key, live.from_conns.clone()));
                    live.readers.insert(no, reader.abort_handle());
                }
                Err(error) => {
                    log::warn!(target: "caf", "a relay's connection couldn't be taken: {error}");
                    // Tried again shortly, as nothing may wake the task.
                    live.accept_again = Some(Instant::now() + ACCEPT_AGAIN);
                    self.wake_at_next();
                    return;
                }
            }
        }
    }

    /// Takes what connections said.
    fn poll_conns(&mut self, cx: &mut Context<'_>) {
        loop {
            let Some(live) = &mut self.live else {
                return;
            };
            let Poll::Ready(Some((no, said))) = live.to_driver.poll_recv(cx) else {
                return;
            };
            match said {
                FromConn::Hello { chat, write } => self.hello(no, chat, write),
                FromConn::Refused(why) => {
                    live.readers.remove(&no);
                    log::warn!(target: "caf", "a relay was refused: {why}");
                }
                FromConn::Line(line) => {
                    let outs = match self.conns.get_mut(&no) {
                        Some(conn) => conn.mcp.take(&line),
                        None => continue,
                    };
                    self.took(no, outs);
                }
                FromConn::Closed(why) => {
                    if let Some(why) = why {
                        log::warn!(target: "caf", "a relay's connection broke: {why}");
                    }
                    if self.conns.contains_key(&no) {
                        self.close(no, Closing::Lost);
                    } else if let Some(live) = &mut self.live {
                        live.readers.remove(&no);
                    }
                }
            }
        }
    }

    /// A relay said which chat it serves, with the right token: it's
    /// bound, unless the chat isn't served. The newest connection for a
    /// chat wins, as a new session's relay may connect before the old
    /// one has gone.
    fn hello(&mut self, no: u64, chat: ChatId, write: OwnedWriteHalf) {
        let Some(served) = self.served.get(&chat) else {
            log::warn!(target: "caf", "a relay for chat {} was refused: it isn't served tools", chat.0);
            if let Some(reader) = self.live.as_mut().and_then(|live| live.readers.remove(&no)) {
                reader.abort();
            }
            return;
        };
        let listings = (served.app.iter())
            .filter_map(|name| self.app.spec(name))
            .map(|spec| {
                let schema = spec.schema.clone().unwrap_or_default();
                AppListing::new(
                    &spec.name,
                    &spec.title,
                    &spec.description,
                    schema,
                    spec.hints,
                )
            })
            .collect();
        let mcp = Connection::new(&served.tools, &served.instructions).with_app_tools(listings);
        if let Some(old) = self.bound.get(&chat).copied() {
            self.close(old, Closing::Replaced);
        }
        let (lines, mut written) = mpsc::channel::<Vec<u8>>(WRITES_WAITING);
        let writer = self.writers.spawn(async move {
            let mut write = write;
            while let Some(line) = written.recv().await {
                // An agent that stops reading isn't waited for forever.
                match tokio::time::timeout(WRITE_TIME, write.write_all(&line)).await {
                    Ok(Ok(())) => {}
                    _ => return Some(no),
                }
            }
            let _ = write.shutdown().await;
            None
        });
        self.conns.insert(
            no,
            Conn {
                writer,
                chat,
                mcp,
                write: lines,
            },
        );
        self.bound.insert(chat, no);
        self.reports.push_back(Report::Relay {
            chat,
            change: RelayChange::Connected,
        });
    }

    /// Does what a connection's MCP said to.
    fn took(&mut self, no: u64, outs: Vec<Out>) {
        for out in outs {
            let Some(conn) = self.conns.get(&no) else {
                return;
            };
            let chat = conn.chat;
            match out {
                Out::Write(bytes) => {
                    if !self.send(no, bytes) {
                        return;
                    }
                }
                Out::Call {
                    request,
                    tool,
                    input,
                    agent_call,
                } => {
                    self.last_call += 1;
                    let call = CallId(self.last_call);
                    self.calls.insert(
                        call,
                        Each {
                            chat,
                            conn: no,
                            request,
                            agent_call: agent_call.clone(),
                            app: None,
                            running: false,
                            job: None,
                            preview: None,
                            run: None,
                            progress_at: Instant::now() + self.progress_every,
                            batch: None,
                        },
                    );
                    self.reports.push_back(Report::Tool {
                        call,
                        event: ToolEvent::Called {
                            chat,
                            tool: ServedTool::Caf(tool),
                            input,
                            agent_call,
                            title: None,
                            bad_input: None,
                        },
                    });
                }
                Out::AppCall {
                    request,
                    name,
                    input,
                    agent_call,
                } => self.app_called(no, chat, request, name, input, agent_call),
                // A job's request was answered: it's no longer the agent's
                // to cancel, and goes on.
                Out::Cancelled(request) => {
                    let found = (self.calls.iter())
                        .find(|(_, each)| {
                            each.conn == no && each.request == request && each.job.is_none()
                        })
                        .map(|(call, _)| *call);
                    if let Some(call) = found {
                        self.calls.remove(&call);
                        self.forget(call);
                        self.reports.push_back(Report::Tool {
                            call,
                            event: ToolEvent::Gone,
                        });
                    }
                }
                Out::Close(why) => {
                    log::warn!(target: "caf", "a relay for chat {} was closed: {why}", chat.0);
                    self.close(no, Closing::Lost);
                    return;
                }
                // No other kind is given yet.
                _ => {}
            }
        }
        self.wake_at_next();
    }

    /// The agent called the app's tool `name` on connection `no`, of
    /// `chat`: its input is read, and the call's title worked out, before
    /// the core hears of it.
    fn app_called(
        &mut self,
        no: u64,
        chat: ChatId,
        request: Request,
        name: String,
        input: serde_json::Value,
        agent_call: Option<String>,
    ) {
        let (Some(tool), Some(spec)) = (self.app.named(&name).cloned(), self.app.spec(&name))
        else {
            // A connection lists only tools the app has, so it never is.
            self.answer(
                no,
                &request,
                &Answer::error(format!("There's no tool named `{name}` here.")),
            );
            return;
        };
        let (title, bad_input) =
            match std::panic::catch_unwind(AssertUnwindSafe(|| tool.title(&input))) {
                Ok(Ok(title)) => (title, None),
                Ok(Err(why)) => (spec.title.clone(), Some(why)),
                Err(_) => {
                    log::warn!(target: "caf", "the app's tool {name} panicked titling a call");
                    (spec.title.clone(), None)
                }
            };
        self.last_call += 1;
        let call = CallId(self.last_call);
        self.calls.insert(
            call,
            Each {
                chat,
                conn: no,
                request,
                agent_call: agent_call.clone(),
                app: Some((tool, title.clone())),
                running: false,
                job: None,
                preview: None,
                run: None,
                progress_at: Instant::now() + self.progress_every,
                batch: None,
            },
        );
        self.reports.push_back(Report::Tool {
            call,
            event: ToolEvent::Called {
                chat,
                tool: ServedTool::App(name),
                input,
                agent_call,
                title: Some(title),
                bad_input,
            },
        });
    }

    /// Closes a connection, ending its calls.
    fn close(&mut self, no: u64, closing: Closing) {
        if let Some(reader) = self.live.as_mut().and_then(|live| live.readers.remove(&no)) {
            reader.abort();
        }
        let Some(mut conn) = self.conns.remove(&no) else {
            return;
        };
        if self.bound.get(&conn.chat) == Some(&no) {
            self.bound.remove(&conn.chat);
        }
        // A job isn't its connection's: its model was answered, and it
        // goes on, unless its chat is let go or CAF shuts down.
        let ended: Vec<CallId> = (self.calls.iter())
            .filter(|(_, each)| {
                each.conn == no && (each.job.is_none() || closing == Closing::Ended)
            })
            .map(|(call, _)| *call)
            .collect();
        for call in ended {
            self.forget(call);
            let Some(each) = self.calls.remove(&call) else {
                continue;
            };
            if each.job.is_some() {
                drop(each);
            } else if closing == Closing::Ended {
                // Its work stops before the model hears it did.
                let (_, request) = each.stop();
                if let Some(bytes) = conn.mcp.answer(&request, &Answer::error(CANCELLED)) {
                    let _ = conn.write.try_send(bytes);
                }
            } else {
                drop(each);
                self.reports.push_back(Report::Tool {
                    call,
                    event: ToolEvent::Gone,
                });
            }
        }
        let held: Vec<CallId> = (self.held.iter())
            .filter(|(_, held)| held.conn == no)
            .map(|(call, _)| *call)
            .collect();
        for call in held {
            if let Some(held) = self.held.remove(&call)
                && closing == Closing::Ended
                && let Some(bytes) = conn.mcp.answer(&held.request, &Answer::error(CANCELLED))
            {
                let _ = conn.write.try_send(bytes);
            }
        }
        if closing != Closing::Ended {
            // Nobody's left to answer: its socket closes now.
            conn.writer.abort();
        }
        if closing == Closing::Lost {
            self.reports.push_back(Report::Relay {
                chat: conn.chat,
                change: RelayChange::Lost,
            });
        }
        // Dropping its sender ends its writing task, once what's waiting
        // is written, and with it the socket.
    }

    /// Answers a call on its connection, if it's still there.
    fn answer(&mut self, no: u64, request: &Request, answer: &Answer) {
        if let Some(bytes) =
            (self.conns.get_mut(&no)).and_then(|conn| conn.mcp.answer(request, answer))
        {
            self.send(no, bytes);
        }
    }

    /// Writes a line to a connection, if it's still there. One whose
    /// agent has stopped reading, with [`WRITES_WAITING`] lines waiting
    /// already, is closed as lost: false then.
    fn send(&mut self, no: u64, bytes: Vec<u8>) -> bool {
        let Some(conn) = self.conns.get(&no) else {
            return false;
        };
        match conn.write.try_send(bytes) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                log::warn!(target: "caf", "a relay for chat {} was closed: its agent stopped reading", conn.chat.0);
                self.close(no, Closing::Lost);
                false
            }
            // Its writing task gave up, as the socket broke or a line took
            // too long.
            Err(mpsc::error::TrySendError::Closed(_)) => {
                log::warn!(target: "caf", "a relay for chat {} was closed: it couldn't be written to", conn.chat.0);
                self.close(no, Closing::Lost);
                false
            }
        }
    }

    /// Batches what calls printed.
    fn poll_prints(&mut self, cx: &mut Context<'_>) {
        while let Poll::Ready(Some(print)) = self.printed.poll_recv(cx) {
            let (call, from, lines) = match print {
                Print::Lines { call, from, lines } => (call, from, lines),
                Print::Detach { call, job, answer } => {
                    self.detach(call, job, answer);
                    continue;
                }
            };
            let Some(each) = self.calls.get_mut(&call) else {
                continue;
            };
            if !each.running {
                continue;
            }
            let print = Printed { from, lines };
            match &mut each.batch {
                Some(batch) => {
                    if let Some(earlier) = batch.add(print) {
                        self.reports.push_back(output(call, earlier));
                    }
                }
                None => each.batch = Some(Batch::new(print)),
            }
            if each
                .batch
                .as_ref()
                .is_some_and(|batch| batch.bytes >= BATCH_BYTES)
                && let Some(batch) = each.batch.take()
            {
                self.reports.push_back(output(call, batch));
            }
        }
        self.wake_at_next();
    }

    /// Takes what previews and runs finished.
    fn poll_calls(&mut self, cx: &mut Context<'_>) {
        let mut previewed = Vec::new();
        let mut ran = Vec::new();
        for (call, each) in &mut self.calls {
            if let Some(preview) = &mut each.preview {
                match std::panic::catch_unwind(AssertUnwindSafe(|| preview.as_mut().poll(cx))) {
                    Ok(Poll::Pending) => {}
                    Ok(Poll::Ready(pieces)) => previewed.push((*call, pieces)),
                    Err(_) => {
                        log::warn!(target: "caf", "the tool runner panicked previewing call {}", call.0);
                        previewed.push((*call, Err((failed(PANICKED), None))));
                    }
                }
            }
            if let Some(run) = &mut each.run {
                match std::panic::catch_unwind(AssertUnwindSafe(|| run.as_mut().poll(cx))) {
                    Ok(Poll::Pending) => {}
                    Ok(Poll::Ready(done)) => ran.push((*call, done)),
                    Err(_) => {
                        log::warn!(target: "caf", "the tool runner panicked running call {}", call.0);
                        ran.push((*call, (failed(PANICKED), None)));
                    }
                }
            }
        }
        for (call, previewed) in previewed {
            let Some(each) = self.calls.get_mut(&call) else {
                continue;
            };
            each.preview = None;
            match previewed {
                Ok(pieces) => self.reports.push_back(Report::Tool {
                    call,
                    event: ToolEvent::Previewed { pieces },
                }),
                // It can't be done: it ends now, and the model hears why
                // once the core knows.
                Err(ran) => {
                    if let Some(each) = self.calls.remove(&call) {
                        self.finish_call(call, each, ran);
                    }
                }
            }
        }
        if !ran.is_empty() {
            // Output printed as it finished is reported before it.
            self.poll_prints(cx);
        }
        for (call, ran) in ran {
            if let Some(each) = self.calls.remove(&call) {
                self.finish_call(call, each, ran);
            }
        }
    }

    /// Reports `call`'s end, as `ran` says, and holds its answer until the
    /// core has it: unless it's a job, whose model was answered already.
    fn finish_call(&mut self, call: CallId, each: Each<A>, (ran, item): Ended<A>) {
        let Each {
            conn,
            request,
            batch,
            job,
            agent_call,
            app,
            ..
        } = each;
        if let Some(batch) = batch {
            self.reports.push_back(output(call, batch));
        }
        self.reports.push_back(Report::Tool {
            call,
            event: ToolEvent::Finished {
                done: ran.done,
                pieces: ran.record,
                folder: ran.folder,
                // Only a TaskStop's stop ends a job without one
                // ([`ToolsDriver::stop_job`]): that's how the core tells
                // them apart.
                notice: job.map(|job| {
                    (ran.notice.clone()).unwrap_or_else(|| match &app {
                        Some((_, title)) => {
                            app_notice(job, agent_call.as_deref(), title, ran.done, &ran.answer)
                        }
                        None => failed_notice(job, agent_call.as_deref()),
                    })
                }),
                item,
            },
        });
        if job.is_none() {
            self.held.insert(
                call,
                Held {
                    conn,
                    request,
                    answer: ran.answer,
                },
            );
        }
    }

    /// `call`'s runner answered the model now, and goes on as job `job`:
    /// reported, its answer held until the core has it. Nothing, for a
    /// call that's gone, or not running, or a job already.
    fn detach(&mut self, call: CallId, job: JobId, answer: Answer) {
        let Some(each) = self.calls.get_mut(&call) else {
            return;
        };
        if !each.running || each.job.is_some() {
            return;
        }
        each.job = Some(job);
        // Answered, it needs no more progress.
        each.progress_at = far_off();
        if let Some(batch) = each.batch.take() {
            self.reports.push_back(output(call, batch));
        }
        self.reports.push_back(Report::Tool {
            call,
            event: ToolEvent::Detached { job },
        });
        self.held.insert(
            call,
            Held {
                conn: each.conn,
                request: each.request.clone(),
                answer,
            },
        );
    }

    /// Reports batches and sends progress that are due.
    fn poll_due(&mut self, cx: &mut Context<'_>) {
        let now = Instant::now();
        let mut progress = Vec::new();
        for (call, each) in &mut self.calls {
            if each.batch.as_ref().is_some_and(|batch| batch.due <= now)
                && let Some(batch) = each.batch.take()
            {
                self.reports.push_back(output(*call, batch));
            }
            if each.job.is_none() && each.progress_at <= now {
                while each.progress_at <= now {
                    each.progress_at += self.progress_every;
                }
                let text = if each.running {
                    "Running"
                } else {
                    "Waiting for permission"
                };
                progress.push((each.conn, each.request.clone(), text));
            }
        }
        for (no, request, text) in progress {
            if let Some(bytes) =
                (self.conns.get_mut(&no)).and_then(|conn| conn.mcp.progress(&request, text))
            {
                self.send(no, bytes);
            }
        }
        self.wake_at_next();
        if self.sleep.as_mut().poll(cx).is_ready() {
            // It's due already: poll again at once, so it's waited for.
            cx.waker().wake_by_ref();
        }
    }

    /// Moves the sleep to the next batch or progress due.
    fn wake_at_next(&mut self) {
        let next = (self.calls.values())
            .flat_map(|each| [Some(each.progress_at), each.batch.as_ref().map(|b| b.due)])
            .flatten()
            .chain(self.live.as_ref().and_then(|live| live.accept_again))
            .min()
            .unwrap_or_else(far_off);
        if self.sleep.deadline() != next {
            self.sleep.as_mut().reset(next);
        }
    }

    /// Ends serving, as CAF shuts down: every open call is answered as
    /// cancelled, every connection closed, and the socket removed. Waits
    /// a moment for the answers to be written.
    pub(crate) async fn finish(&mut self) {
        for (_, held) in std::mem::take(&mut self.held) {
            self.answer(held.conn, &held.request, &held.answer);
        }
        let conns: Vec<u64> = self.conns.keys().copied().collect();
        for conn in conns {
            self.close(conn, Closing::Ended);
        }
        self.calls.clear();
        if let Some(live) = self.live.take() {
            for (_, reader) in live.readers {
                reader.abort();
            }
            drop(live.socket);
        }
        // Some were stopped, as their relays were lost: that's no matter.
        let mut writers = std::mem::take(&mut self.writers);
        let _ = tokio::time::timeout(FINISH_TIME, async {
            while writers.join_next().await.is_some() {}
        })
        .await;
    }
}

impl<A: AppItem> Each<A> {
    /// Drops its work, giving back where to answer it.
    fn stop(self) -> (u64, Request) {
        let Self {
            conn,
            request,
            preview,
            run,
            ..
        } = self;
        drop(preview);
        drop(run);
        (conn, request)
    }
}

impl Batch {
    /// A batch of one print, due [`BATCH_TIME`] from now.
    fn new(print: Printed) -> Self {
        Self {
            from: print.from,
            bytes: print.lines.iter().map(String::len).sum(),
            lines: print.lines,
            due: Instant::now() + BATCH_TIME,
        }
    }

    /// Adds `print` to the batch, if it follows on from it: the lines
    /// from its `from` replace what was there. If it starts past the
    /// batch's end, leaving a gap, it starts a batch of its own, and the
    /// one before is given back, to be reported first.
    fn add(&mut self, print: Printed) -> Option<Self> {
        let end = self.from + self.lines.len() as u64;
        if print.from > end {
            return Some(std::mem::replace(self, Self::new(print)));
        }
        if print.from <= self.from {
            self.from = print.from;
            self.lines = print.lines;
        } else {
            self.lines
                .truncate(usize::try_from(print.from - self.from).unwrap_or(usize::MAX));
            self.lines.extend(print.lines);
        }
        self.bytes = self.lines.iter().map(String::len).sum();
        None
    }
}

/// A batch, as reported.
fn output<A: AppItem>(call: CallId, batch: Batch) -> Report<A> {
    Report::Tool {
        call,
        event: ToolEvent::Output {
            from: batch.from,
            lines: batch.lines,
        },
    }
}

/// The notice of job `job`'s end, for a runner that gave none, such as
/// one that panicked.
/// It's laid out as `caf_tools::notice` lays one out, but for the output
/// file, which isn't known: the task's id, the agent's id for its call if
/// known (`tool_use`), its status and a summary.
fn failed_notice(job: JobId, tool_use: Option<&str>) -> String {
    let mut out = format!("<task-notification>\n<task-id>{}</task-id>", job.0);
    if let Some(tool_use) = tool_use {
        let escaped = caf_tools::escape(tool_use);
        out.push_str(&format!("\n<tool-use-id>{escaped}</tool-use-id>"));
    }
    out.push_str(&format!(
        "\n<status>failed</status>\n<summary>Background task {} failed</summary>\n</task-notification>",
        job.0
    ));
    out
}

/// The notice of the end of job `job`, a call to one of the app's tools
/// titled `title`: as `caf_tools::notice` lays one out, with what it
/// ended with, `done` or not, as its result, which the model wasn't told
/// when it became a job.
fn app_notice(
    job: JobId,
    tool_use: Option<&str>,
    title: &str,
    done: bool,
    answer: &Answer,
) -> String {
    let (status, how) = match done {
        true => ("completed", "completed"),
        false => ("failed", "failed"),
    };
    let mut out = format!("<task-notification>\n<task-id>{}</task-id>", job.0);
    if let Some(tool_use) = tool_use {
        let escaped = caf_tools::escape(tool_use);
        out.push_str(&format!("\n<tool-use-id>{escaped}</tool-use-id>"));
    }
    let result: Vec<&str> = (answer.content.iter())
        .filter_map(|part| match part {
            AnswerPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    out.push_str(&format!(
        "\n<status>{status}</status>\n<summary>Background task \"{}\" {how}</summary>",
        caf_tools::escape(title)
    ));
    if !result.is_empty() {
        let result = caf_tools::escape(&result.join("\n"));
        out.push_str(&format!("\n<result>{result}</result>"));
    }
    out.push_str("\n</task-notification>");
    out
}

/// What the model is told of a call stopped with its turn.
const CANCELLED: &str = "Cancelled: the user stopped this turn";
/// What the model and the record are told of a call whose runner panicked.
const PANICKED: &str = "CAF's tool failed unexpectedly";

/// What the model is told of a call the core refused.
fn refused(why: &RefusedCall) -> String {
    match why {
        RefusedCall::NotAllowed => {
            "Not allowed: the user, or their rules, refused this call".into()
        }
        RefusedCall::NotRunning => {
            "Refused: this conversation isn't running a turn, so nothing may be called now".into()
        }
        RefusedCall::NotGiven => "Refused: this conversation wasn't given that tool".into(),
        RefusedCall::BadInput(why) => format!("Refused: {why}"),
        _ => "Refused".into(),
    }
}

/// What a call to one of the app's tools ended with, as the driver keeps
/// it.
fn ended<A: AppItem>(done: Done<A>) -> Ended<A> {
    let Done {
        done,
        answer,
        record,
        item,
        ..
    } = done;
    (Ran::new(done, record, answer), item)
}

/// A time that never comes.
fn far_off() -> Instant {
    Instant::now() + Duration::from_secs(86400 * 365 * 30)
}

/// Reads a connection: its handshake, then its lines, for the driver.
async fn read_conn(
    no: u64,
    stream: tokio::net::UnixStream,
    key: [u8; 32],
    to: mpsc::Sender<(u64, FromConn)>,
) {
    let (read, write) = stream.into_split();
    let mut read = BufReader::new(read);
    let hello =
        match tokio::time::timeout(HANDSHAKE_TIME, read_line(&mut read, HANDSHAKE_MOST)).await {
            Err(_) => Err("it said nothing for 10 s".to_owned()),
            Ok(Err(why)) => Err(why),
            Ok(Ok(None)) => Err("it closed before saying which chat it serves".to_owned()),
            Ok(Ok(Some(line))) => check_hello(&line, &key),
        };
    let chat = match hello {
        Ok(chat) => chat,
        Err(why) => {
            let _ = to.send((no, FromConn::Refused(why))).await;
            return;
        }
    };
    if to
        .send((no, FromConn::Hello { chat, write }))
        .await
        .is_err()
    {
        return;
    }
    loop {
        let said = match read_line(&mut read, LINE_MOST).await {
            Ok(Some(line)) => FromConn::Line(line),
            Ok(None) => FromConn::Closed(None),
            Err(why) => FromConn::Closed(Some(why)),
        };
        let closed = matches!(said, FromConn::Closed(_));
        if to.send((no, said)).await.is_err() || closed {
            return;
        }
    }
}

/// The chat a handshake names, if its token is right.
fn check_hello(line: &[u8], key: &[u8; 32]) -> Result<ChatId, String> {
    let hello: Hello =
        serde_json::from_slice(line).map_err(|e| format!("its handshake didn't read: {e}"))?;
    if hello.caf_relay != 1 {
        return Err(format!(
            "it speaks version {} of the relay's handshake, not 1",
            hello.caf_relay
        ));
    }
    if !mcp::token_is(&Key(*key), hello.chat, &hello.token) {
        return Err(format!("its token for chat {} was wrong", hello.chat.0));
    }
    Ok(hello.chat)
}

/// One line, without its newline, or `None` at the end; an error if it's
/// longer than `most`, or the read failed.
async fn read_line(
    read: &mut (impl AsyncBufRead + Unpin),
    most: usize,
) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let buf = read.fill_buf().await.map_err(|e| e.to_string())?;
        if buf.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err("it closed partway through a line".into())
            };
        }
        let (take, end) = match memchr::memchr(b'\n', buf) {
            Some(at) => (at, true),
            None => (buf.len(), false),
        };
        if line.len() + take > most {
            return Err(format!("it sent a line over {most} bytes"));
        }
        line.extend_from_slice(&buf[..take]);
        read.consume(take + usize::from(end));
        if end {
            return Ok(Some(line));
        }
    }
}

/// Why `relay` can't be sent to an agent as an MCP server, if it can't:
/// the agent is sent its program, arguments and variables as text, sets
/// them over its own environment, and starts it in a folder of its own.
fn sendable(relay: &SpawnCommand) -> Result<(), &'static str> {
    if relay.cwd.is_some() {
        return Err("it has a folder to start in, and an agent starts it in its own");
    }
    if relay.program.to_str().is_none() {
        return Err("its program's path isn't UTF-8");
    }
    if relay.args.iter().any(|arg| arg.to_str().is_none()) {
        return Err("an argument isn't UTF-8");
    }
    for change in &relay.env {
        match change {
            EnvChange::Set(name, value) if name.to_str().is_some() && value.to_str().is_some() => {}
            EnvChange::Set(..) => return Err("a variable's name or value isn't UTF-8"),
            _ => return Err("it removes a variable, and an agent can only set them"),
        }
    }
    Ok(())
}

/// Checks that `relay` has the hook: run with
/// [`caf_relay::CHECK_VAR`], it must print [`caf_relay::CHECK_ANSWER`].
fn check_relay(relay: &SpawnCommand) -> Result<(), StartError> {
    let no_hook = |why: String| StartError::NoRelayHook {
        relay: relay.program.clone(),
        why,
    };
    // Run as the check, this program went on to start CAF: checking again
    // would start it again, without end.
    if asked_to_check(std::env::var_os(caf_relay::CHECK_VAR).as_deref()) {
        return Err(no_hook(format!(
            "was run as CAF's relay check ({} is set), but went on to start CAF",
            caf_relay::CHECK_VAR
        )));
    }
    let program = relay.program.display();
    let mut command = std::process::Command::new(&relay.program);
    command.args(&relay.args);
    for change in &relay.env {
        match change {
            EnvChange::Set(name, value) => {
                command.env(name, value);
            }
            EnvChange::Remove(name) => {
                command.env_remove(name);
            }
            // No other kind is given yet.
            _ => {}
        }
    }
    command
        .env(caf_relay::CHECK_VAR, "1")
        .env_remove(caf_relay::RELAY_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| StartError::Serving(format!("its relay {program} couldn't be run: {e}")))?;
    // Read as it comes, without waiting: a process it started may hold
    // its output open after it has gone. Dropped as the check ends, so
    // nothing is left reading it.
    let mut stdout = child.stdout.take().expect("it's piped");
    if let Err(e) = rustix::io::ioctl_fionbio(&stdout, true) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(StartError::Serving(format!(
            "its relay {program} couldn't be checked: {e}"
        )));
    }
    let mut out = Vec::new();
    let read = |stdout: &mut std::process::ChildStdout, out: &mut Vec<u8>| {
        let mut buf = [0; 512];
        while out.len() < 4096 {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
        }
    };
    let started = std::time::Instant::now();
    let status = loop {
        read(&mut stdout, &mut out);
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < CHECK_TIME => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(no_hook(format!(
                    "didn't answer its check in {} s",
                    CHECK_TIME.as_secs()
                )));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(StartError::Serving(format!(
                    "its relay {program} couldn't be checked: {e}"
                )));
            }
        }
    };
    // What it wrote as it went.
    read(&mut stdout, &mut out);
    drop(stdout);
    let expected = format!("{}\n", caf_relay::CHECK_ANSWER);
    if status.success() && out == expected.as_bytes() {
        return Ok(());
    }
    Err(no_hook(format!(
        "didn't answer its check ({status}, printing {:?})",
        String::from_utf8_lossy(&out)
    )))
}

/// Whether [`caf_relay::CHECK_VAR`], as it's set, asks for the check.
fn asked_to_check(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

/// `$XDG_RUNTIME_DIR/caf`, or `caf-<uid>` in the temporary folder.
fn default_folder() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(runtime) if runtime.is_absolute() => runtime.join("caf"),
        _ => std::env::temp_dir().join(format!("caf-{}", rustix::process::geteuid().as_raw())),
    }
}

/// Makes `folder`, mode 700, if it isn't there, and checks it's this
/// user's alone.
fn private_folder(folder: &Path) -> Result<(), String> {
    let shown = folder.display();
    if let Err(error) = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)
    {
        return Err(format!(
            "the folder for CAF's tools' socket, {shown}, couldn't be made: {error}"
        ));
    }
    let meta = std::fs::symlink_metadata(folder)
        .map_err(|e| format!("the folder for CAF's tools' socket, {shown}, can't be read: {e}"))?;
    if !meta.is_dir() {
        return Err(format!(
            "the folder for CAF's tools' socket, {shown}, isn't a folder"
        ));
    }
    if meta.uid() != rustix::process::geteuid().as_raw() || meta.mode() & 0o077 != 0 {
        return Err(format!(
            "the folder for CAF's tools' socket, {shown}, must be this user's alone (mode 700)"
        ));
    }
    Ok(())
}

/// `app` as part of a file's name: anything but letters, digits, `.`,
/// `_` and `-` becomes `_`.
fn file_name(app: &str) -> String {
    let name: String = (app.chars())
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.starts_with('.') {
        format!("caf{name}")
    } else {
        name
    }
}

/// Listens on `socket`, whose path fits, replacing a socket left there by
/// an earlier process with the same id, once connecting to it shows
/// nothing's listening.
fn listen(socket: &Path) -> Result<(UnixListener, SocketFile), StartError> {
    let shown = socket.display();
    if let Ok(meta) = std::fs::symlink_metadata(socket) {
        if !meta.file_type().is_socket() {
            return Err(StartError::Serving(format!(
                "its socket {shown} is taken by something else"
            )));
        }
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Err(StartError::Serving(format!(
                "its socket {shown} is in use by another program"
            )));
        }
        let _ = std::fs::remove_file(socket);
    }
    let listener = std::os::unix::net::UnixListener::bind(socket)
        .map_err(|e| StartError::Serving(format!("it can't listen on {shown}: {e}")))?;
    // Removed if anything after fails.
    let file = SocketFile(socket.to_owned());
    let listener = listener
        .set_nonblocking(true)
        .and_then(|()| UnixListener::from_std(listener))
        .map_err(|e| StartError::Serving(format!("it can't listen on {shown}: {e}")))?;
    Ok((listener, file))
}

/// 32 bytes from the system's randomness.
fn random_key() -> Result<[u8; 32], String> {
    let mut key = [0; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut key))
        .map_err(|e| format!("it can't make its key: {e}"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use caf_core::tool::Text;
    use serde_json::{Value, json};
    use tokio::io::AsyncReadExt as _;
    use tokio::net::UnixStream;
    use tokio::net::unix::OwnedReadHalf;
    use tokio::sync::oneshot;

    use super::*;

    /// A folder of the test's own, short so the socket's path fits,
    /// removed when dropped.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            static NO: AtomicU64 = AtomicU64::new(0);
            let no = NO.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("ct{}-{no}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A run the test finishes: its call, its action, and where its
    /// answer goes.
    struct Run {
        call: Call,
        action: Action,
        finish: oneshot::Sender<Ran>,
    }

    /// A runner whose runs the test finishes, telling it when one is
    /// dropped.
    struct Scripted {
        runs: mpsc::UnboundedSender<Run>,
        dropped: mpsc::UnboundedSender<CallId>,
        forgot: mpsc::UnboundedSender<CallId>,
        panics: bool,
    }

    /// Says its call's run was dropped.
    struct Dropped(CallId, mpsc::UnboundedSender<CallId>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            let _ = self.1.send(self.0);
        }
    }

    impl Runner for Scripted {
        fn preview(&mut self, _call: &Call, action: Action) -> Preview {
            let previewed = match action {
                Action::Edit { path } if path == "/fail" => Err(failed("it can't be")),
                _ => Ok(vec![Piece::Text(Text::new("preview"))]),
            };
            Box::pin(std::future::ready(previewed))
        }

        fn forget(&mut self, call: CallId) {
            let _ = self.forgot.send(call);
        }

        fn run(&mut self, call: Call, action: Action) -> Pin<Box<dyn Future<Output = Ran> + Send>> {
            assert!(!self.panics, "the runner panics");
            let (finish, finished) = oneshot::channel();
            let dropped = Dropped(call.id(), self.dropped.clone());
            let _ = self.runs.send(Run {
                call,
                action,
                finish,
            });
            Box::pin(async move {
                let _dropped = dropped;
                finished
                    .await
                    .unwrap_or_else(|_| failed("the test let it go"))
            })
        }
    }

    /// A driver, run on a task of its own, given directives and handing
    /// out reports through channels. Dropping `directives` shuts it down.
    /// What the test tells the driver's task.
    enum Cmd {
        Apply(ToolDirective),
        /// The core has taken the last report ([`ToolsDriver::taken`]).
        Taken,
        /// How many tasks writing to relays it still holds.
        Writers(oneshot::Sender<usize>),
    }

    struct Harness {
        directives: Option<mpsc::UnboundedSender<Cmd>>,
        reports: mpsc::UnboundedReceiver<Report>,
        runs: mpsc::UnboundedReceiver<Run>,
        dropped: mpsc::UnboundedReceiver<CallId>,
        forgot: mpsc::UnboundedReceiver<CallId>,
        serving: Serving,
        finished: Option<tokio::task::JoinHandle<()>>,
        _dir: Dir,
    }

    const CHAT: ChatId = ChatId(uuid::Uuid::from_u128(
        0x0193_0000_0000_7000_8000_0000_0000_1000,
    ));

    fn harness() -> Harness {
        harness_with(false)
    }

    fn harness_with(panics: bool) -> Harness {
        harness_of(panics, true)
    }

    /// A harness whose driver is told the core took a report only when
    /// the test says ([`Harness::taken`]), if not `auto_taken`.
    fn harness_of(panics: bool, auto_taken: bool) -> Harness {
        harness_serving(panics, auto_taken, AppTools::new())
    }

    /// A harness whose driver serves the app's tools `app` too.
    fn harness_serving(panics: bool, auto_taken: bool, app: AppTools) -> Harness {
        let dir = Dir::new();
        let (runs, run) = mpsc::unbounded_channel();
        let (dropped, drop_seen) = mpsc::unbounded_channel();
        let (forgot, forgotten) = mpsc::unbounded_channel();
        let config: ToolsConfig = ToolsConfig {
            app,
            runner: Some(Box::new(Scripted {
                runs,
                dropped,
                forgot,
                panics,
            })),
            folder: Some(dir.0.clone()),
            ..ToolsConfig::default()
        };
        let (mut driver, serving) =
            ToolsDriver::listen(config, SpawnCommand::new("/bin/false"), "test").unwrap();
        let (directives, mut given) = mpsc::unbounded_channel();
        let (reported, reports) = mpsc::unbounded_channel();
        let finished = tokio::spawn(async move {
            loop {
                tokio::select! {
                    cmd = given.recv() => match cmd {
                        Some(Cmd::Apply(directive)) => driver.apply(directive),
                        Some(Cmd::Taken) => driver.taken(),
                        Some(Cmd::Writers(reply)) => {
                            let _ = reply.send(driver.writers.len());
                        }
                        None => break,
                    },
                    report = driver.next() => {
                        let _ = reported.send(report);
                        if auto_taken {
                            driver.taken();
                        }
                    }
                }
            }
            driver.finish().await;
        });
        Harness {
            directives: Some(directives),
            reports,
            runs: run,
            dropped: drop_seen,
            forgot: forgotten,
            serving,
            finished: Some(finished),
            _dir: dir,
        }
    }

    impl Harness {
        fn apply(&self, directive: ToolDirective) {
            self.directives
                .as_ref()
                .unwrap()
                .send(Cmd::Apply(directive))
                .unwrap();
        }

        /// The core has taken the last report.
        fn taken(&self) {
            self.directives.as_ref().unwrap().send(Cmd::Taken).unwrap();
        }

        async fn writers(&self) -> usize {
            let (reply, replied) = oneshot::channel();
            self.directives
                .as_ref()
                .unwrap()
                .send(Cmd::Writers(reply))
                .unwrap();
            replied.await.unwrap()
        }

        fn serve(&self, chat: ChatId) {
            self.apply(ToolDirective::Serve {
                chat,
                tools: CafTool::ALL.to_vec(),
                app: Vec::new(),
                cwd: "/work".into(),
                instructions: "Use them.".into(),
            });
        }

        /// Serves `chat` Read and the app's tools `app`.
        fn serve_app(&self, chat: ChatId, app: &[&str]) {
            self.apply(ToolDirective::Serve {
                chat,
                tools: vec![CafTool::Read],
                app: app.iter().map(|&name| name.to_owned()).collect(),
                cwd: "/work".into(),
                instructions: "Use them.".into(),
            });
        }

        async fn report_within(&mut self, wait: Duration) -> Report {
            tokio::time::timeout(wait, self.reports.recv())
                .await
                .expect("a report in time")
                .expect("the driver is running")
        }

        async fn report(&mut self) -> Report {
            tokio::time::timeout(Duration::from_secs(10), self.reports.recv())
                .await
                .expect("a report in time")
                .expect("the driver is running")
        }

        async fn run(&mut self) -> Run {
            tokio::time::timeout(Duration::from_secs(10), self.runs.recv())
                .await
                .expect("a run in time")
                .unwrap()
        }

        async fn dropped(&mut self) -> CallId {
            tokio::time::timeout(Duration::from_secs(10), self.dropped.recv())
                .await
                .expect("a run dropped in time")
                .unwrap()
        }

        /// No report is waiting, once the driver has had its turn.
        async fn quiet(&mut self) {
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            assert!(self.reports.try_recv().is_err(), "no report");
        }

        /// Shuts the driver down, and waits for it to finish.
        async fn shut_down(&mut self) {
            self.directives = None;
            self.finished.take().unwrap().await.unwrap();
        }

        async fn connect(&mut self, chat: ChatId) -> Client {
            let token = mcp::token(&self.serving.key, chat);
            self.connect_with(chat, &token).await
        }

        async fn connect_with(&mut self, chat: ChatId, token: &str) -> Client {
            let stream = UnixStream::connect(&self.serving.socket).await.unwrap();
            let (read, write) = stream.into_split();
            let mut client = Client {
                read: BufReader::new(read),
                write,
            };
            let hello = json!({"caf_relay": 1, "chat": chat, "token": token});
            client.send(&hello).await;
            client
        }

        /// A relay for `chat`, bound and initialized.
        async fn bound(&mut self, chat: ChatId) -> Client {
            let mut client = self.connect(chat).await;
            assert_connected(self.report().await, chat);
            client.init().await;
            client
        }

        /// Calls Read as `id`, and waits for the call's report: its number.
        async fn called(&mut self, client: &mut Client, id: u64, progress: bool) -> CallId {
            client.call(id, progress).await;
            match self.report().await {
                Report::Tool {
                    call,
                    event: ToolEvent::Called { chat, tool, .. },
                } => {
                    assert_eq!((chat, tool), (CHAT, ServedTool::Caf(CafTool::Read)));
                    call
                }
                other => panic!("expected Called, got {other:?}"),
            }
        }
    }

    fn assert_connected(report: Report, expected: ChatId) {
        match report {
            Report::Relay {
                chat,
                change: RelayChange::Connected,
            } => assert_eq!(chat, expected),
            other => panic!("expected Connected, got {other:?}"),
        }
    }

    fn assert_event(report: Report, expected: CallId, what: impl Fn(&ToolEvent) -> bool) {
        match &report {
            Report::Tool { call, event } if *call == expected && what(event) => {}
            other => panic!("unexpected report: {other:?}"),
        }
    }

    /// The agent's end of a relay.
    struct Client {
        read: BufReader<OwnedReadHalf>,
        write: OwnedWriteHalf,
    }

    impl Client {
        async fn send(&mut self, message: &Value) {
            let mut line = serde_json::to_vec(message).unwrap();
            line.push(b'\n');
            self.write.write_all(&line).await.unwrap();
        }

        /// The next line, with no time limit: for tests on a paused clock.
        async fn recv_untimed(&mut self) -> Value {
            let mut line = Vec::new();
            self.read.read_until(b'\n', &mut line).await.unwrap();
            assert!(!line.is_empty(), "the connection closed");
            serde_json::from_slice(&line).unwrap()
        }

        async fn recv(&mut self) -> Value {
            tokio::time::timeout(Duration::from_secs(10), self.recv_untimed())
                .await
                .expect("a line in time")
        }

        /// Whether CAF closed the connection, having sent nothing more.
        async fn closed(&mut self) -> bool {
            let mut rest = Vec::new();
            tokio::time::timeout(Duration::from_secs(10), self.read.read_to_end(&mut rest))
                .await
                .expect("closed in time")
                .is_ok_and(|_| rest.is_empty())
        }

        async fn init(&mut self) {
            self.send(&json!({
                "jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}},
            }))
            .await;
            let answer = self.recv().await;
            assert_eq!(answer["id"], 0, "{answer}");
            self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .await;
        }

        /// Calls `tool` with `arguments`, as `id`.
        async fn call_tool(&mut self, id: u64, tool: &str, arguments: Value) {
            self.send(&json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": tool, "arguments": arguments, "_meta": {"claudecode/toolUseId": format!("toolu_{id}")}},
            }))
            .await;
        }

        async fn call(&mut self, id: u64, progress: bool) {
            let mut meta = json!({"claudecode/toolUseId": format!("toolu_{id}")});
            if progress {
                meta["progressToken"] = json!(format!("p{id}"));
            }
            self.send(&json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": "Read", "arguments": {"file_path": "/etc/hostname"}, "_meta": meta},
            }))
            .await;
        }

        /// The next answer is to a ping sent now: so nothing else was
        /// waiting to be read.
        async fn nothing_waiting(&mut self) {
            self.send(&json!({"jsonrpc": "2.0", "id": 999, "method": "ping"}))
                .await;
            let answer = self.recv().await;
            assert_eq!(
                answer["id"], 999,
                "nothing came before the ping's answer: {answer}"
            );
        }
    }

    fn text_of(answer: &Value) -> &str {
        answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_call_is_numbered_previewed_run_and_answered() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        client.call(7, false).await;
        let call = match h.report().await {
            Report::Tool {
                call,
                event:
                    ToolEvent::Called {
                        chat,
                        tool: ServedTool::Caf(tool),
                        input,
                        agent_call,
                        title: None,
                        bad_input: None,
                    },
            } => {
                assert_eq!((chat, tool), (CHAT, CafTool::Read));
                assert_eq!(input, json!({"file_path": "/etc/hostname"}));
                assert_eq!(agent_call.as_deref(), Some("toolu_7"));
                call
            }
            other => panic!("expected Called, got {other:?}"),
        };
        let action = Action::Read {
            path: "/etc/hostname".into(),
            lines: None,
        };
        h.apply(ToolDirective::Preview {
            call,
            action: action.clone(),
            input: serde_json::Value::Null,
        });
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Previewed { pieces } if pieces.len() == 1),
        );
        h.apply(ToolDirective::Run {
            call,
            action: action.clone(),
            input: serde_json::Value::Null,
        });
        let run = h.run().await;
        assert_eq!(
            (run.call.chat(), run.call.id(), &run.action),
            (CHAT, call, &action)
        );
        run.call.print(0, vec!["a".into(), "b".into()]);
        run.call.print(1, vec!["B".into(), "c".into()]);
        run.finish
            .send(Ran::new(
                true,
                vec![Piece::Text(Text::new("read"))],
                Answer::text("ok"),
            ))
            .unwrap();
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Output { from: 0, lines } if *lines == ["a", "B", "c"]),
        );
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Finished { done: true, pieces, .. } if pieces.len() == 1),
        );
        let answer = client.recv().await;
        assert_eq!(answer["id"], 7);
        assert_eq!(text_of(&answer), "ok");
        assert_eq!(answer["result"]["isError"], false);
        client.nothing_waiting().await;
        h.quiet().await;
    }

    #[tokio::test]
    async fn output_waits_at_most_a_batchs_time() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        h.apply(ToolDirective::Run {
            call,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        let run = h.run().await;
        run.call.print(0, vec!["one".into()]);
        // Reported without the run finishing.
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Output { from: 0, lines } if *lines == ["one"]),
        );
        // 16 KiB is reported at once, and what follows starts a batch of
        // its own.
        run.call.print(1, vec!["x".repeat(BATCH_BYTES)]);
        run.call.print(2, vec!["two".into()]);
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Output { from: 1, lines } if lines.len() == 1),
        );
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Output { from: 2, lines } if *lines == ["two"]),
        );
        drop(run);
    }

    #[test]
    fn a_batch_keeps_what_the_terminal_would_show() {
        let print = |from, lines: &[&str]| Printed {
            from,
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        };
        let mut batch = Batch::new(print(3, &["a", "b"]));
        assert!(batch.add(print(5, &["c"])).is_none());
        assert_eq!(
            (batch.from, &batch.lines),
            (3, &["a", "b", "c"].map(String::from).to_vec())
        );
        assert!(batch.add(print(4, &["B"])).is_none());
        assert_eq!(
            (batch.from, &batch.lines),
            (3, &["a", "B"].map(String::from).to_vec())
        );
        assert!(batch.add(print(1, &["z"])).is_none());
        assert_eq!(
            (batch.from, &batch.lines),
            (1, &["z"].map(String::from).to_vec())
        );
        assert_eq!(batch.bytes, 1);
        let earlier = batch.add(print(9, &["far"])).expect("a gap ends the batch");
        assert_eq!(
            (earlier.from, &earlier.lines),
            (1, &["z"].map(String::from).to_vec())
        );
        assert_eq!(
            (batch.from, &batch.lines),
            (9, &["far"].map(String::from).to_vec())
        );
    }

    #[tokio::test]
    async fn a_refused_call_is_answered_with_why() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        h.apply(ToolDirective::Refuse {
            call,
            why: RefusedCall::BadInput("file_path is missing".into()),
        });
        let answer = client.recv().await;
        assert_eq!(answer["result"]["isError"], true);
        assert!(
            text_of(&answer).contains("file_path is missing"),
            "{answer}"
        );
        // Told again, nothing more is sent.
        h.apply(ToolDirective::Cancel { call });
        client.nothing_waiting().await;
        h.quiet().await;
    }

    #[tokio::test]
    async fn a_stopped_call_is_dropped_then_answered_as_cancelled_once() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        h.apply(ToolDirective::Run {
            call,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        let run = h.run().await;
        h.apply(ToolDirective::Cancel { call });
        assert_eq!(h.dropped().await, call);
        let answer = client.recv().await;
        assert_eq!(answer["id"], 1);
        assert_eq!(answer["result"]["isError"], true);
        assert_eq!(text_of(&answer), CANCELLED);
        // What the runner does after is nobody's.
        run.call.print(0, vec!["late".into()]);
        let _ = run
            .finish
            .send(Ran::new(true, Vec::new(), Answer::text("late")));
        h.apply(ToolDirective::Cancel { call });
        client.nothing_waiting().await;
        h.quiet().await;
    }

    #[tokio::test]
    async fn a_call_finished_as_it_is_stopped_is_answered_once() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        h.apply(ToolDirective::Run {
            call,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        let run = h.run().await;
        // Both at once: whichever the driver takes first, the agent hears
        // one answer.
        run.finish
            .send(Ran::new(true, Vec::new(), Answer::text("ok")))
            .unwrap();
        h.apply(ToolDirective::Cancel { call });
        let answer = client.recv().await;
        assert_eq!(answer["id"], 1);
        client.nothing_waiting().await;
    }

    #[tokio::test]
    async fn a_call_the_agent_cancels_is_dropped_gone_and_never_answered() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        h.apply(ToolDirective::Run {
            call,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        let _run = h.run().await;
        client
            .send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}}))
            .await;
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Gone)
        });
        assert_eq!(h.dropped().await, call);
        // The core's Cancel, crossing the agent's, is answered by nobody.
        h.apply(ToolDirective::Cancel { call });
        client.nothing_waiting().await;
        h.quiet().await;
    }

    #[tokio::test]
    async fn a_dropped_connection_ends_its_calls_then_its_chat_hears_lost() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let waiting = h.called(&mut client, 1, false).await;
        let running = h.called(&mut client, 2, false).await;
        h.apply(ToolDirective::Run {
            call: running,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        let _run = h.run().await;
        drop(client);
        let mut gone = vec![];
        for _ in 0..2 {
            match h.report().await {
                Report::Tool {
                    call,
                    event: ToolEvent::Gone,
                } => gone.push(call),
                other => panic!("expected Gone, got {other:?}"),
            }
        }
        gone.sort();
        assert_eq!(gone, [waiting, running]);
        match h.report().await {
            Report::Relay {
                chat,
                change: RelayChange::Lost,
            } => assert_eq!(chat, CHAT),
            other => panic!("expected Lost, got {other:?}"),
        }
        assert_eq!(h.dropped().await, running);
    }

    #[tokio::test]
    async fn the_newest_relay_for_a_chat_wins() {
        let mut h = harness();
        h.serve(CHAT);
        let mut old = h.bound(CHAT).await;
        let call = h.called(&mut old, 1, false).await;
        let mut new = h.connect(CHAT).await;
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Gone)
        });
        assert_connected(h.report().await, CHAT);
        assert!(old.closed().await);
        new.init().await;
        let next = h.called(&mut new, 1, false).await;
        assert_ne!(next, call, "a call's number is never given twice");
        h.quiet().await;
    }

    #[tokio::test]
    async fn a_relay_with_the_wrong_token_is_closed() {
        let mut h = harness();
        h.serve(CHAT);
        // Another chat's token, good for that chat, which is served too.
        let other = ChatId(uuid::Uuid::from_u128(7));
        h.serve(other);
        let wrong = mcp::token(&h.serving.key, other);
        let mut client = h.connect_with(CHAT, &wrong).await;
        assert!(client.closed().await);
        let mut client = h.connect_with(CHAT, "not even hex").await;
        assert!(client.closed().await);
        h.quiet().await;
    }

    #[tokio::test]
    async fn a_relay_for_a_chat_not_served_is_closed() {
        let mut h = harness();
        let mut client = h.connect(CHAT).await;
        assert!(client.closed().await);
        h.serve(CHAT);
        h.apply(ToolDirective::Unserve { chat: CHAT });
        let mut client = h.connect(CHAT).await;
        assert!(client.closed().await);
        h.quiet().await;
    }

    #[tokio::test]
    async fn a_relay_that_sends_junk_first_is_closed() {
        let mut h = harness();
        let stream = UnixStream::connect(&h.serving.socket).await.unwrap();
        let (read, mut write) = stream.into_split();
        write.write_all(b"hello\n").await.unwrap();
        let mut client = Client {
            read: BufReader::new(read),
            write,
        };
        assert!(client.closed().await);
        h.quiet().await;
    }

    #[tokio::test]
    async fn a_chat_let_go_has_its_calls_answered_as_cancelled_and_its_relay_closed() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        h.apply(ToolDirective::Run {
            call,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        let _run = h.run().await;
        h.apply(ToolDirective::Unserve { chat: CHAT });
        assert_eq!(h.dropped().await, call);
        let answer = client.recv().await;
        assert_eq!(
            (answer["id"].clone(), answer["result"]["isError"].clone()),
            (json!(1), json!(true))
        );
        assert!(client.closed().await);
        h.quiet().await;
    }

    #[tokio::test]
    async fn shutting_down_answers_every_open_call_and_removes_the_socket() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        h.called(&mut client, 1, false).await;
        let socket = h.serving.socket.clone();
        assert!(socket.exists());
        h.shut_down().await;
        let answer = client.recv().await;
        assert_eq!(
            (answer["id"].clone(), answer["result"]["isError"].clone()),
            (json!(1), json!(true))
        );
        assert!(client.closed().await);
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn a_runner_that_panics_fails_its_call() {
        let mut h = harness_with(true);
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        h.apply(ToolDirective::Run {
            call,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Finished { done: false, .. })
        });
        let answer = client.recv().await;
        assert_eq!(answer["result"]["isError"], true);
        assert_eq!(text_of(&answer), PANICKED);
    }

    #[tokio::test]
    async fn an_open_call_with_a_token_hears_progress_every_minute() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let quiet = h.called(&mut client, 1, false).await;
        let call = h.called(&mut client, 2, true).await;
        // From here, the clock moves only when everything waits.
        tokio::time::pause();
        let started = Instant::now();
        let progress = client.recv_untimed().await;
        assert_eq!(progress["method"], "notifications/progress", "{progress}");
        assert_eq!(progress["params"]["progressToken"], "p2");
        assert_eq!(progress["params"]["message"], "Waiting for permission");
        // Not before its minute is up. While the test waits for the line,
        // the paused clock may move on to later timers, so there's no
        // upper bound.
        assert!(started.elapsed() >= PROGRESS_EVERY - Duration::from_secs(1));
        tokio::time::resume();
        h.apply(ToolDirective::Run {
            call,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        let run = h.run().await;
        tokio::time::pause();
        let progress = client.recv_untimed().await;
        assert_eq!(progress["params"]["message"], "Running", "{progress}");
        tokio::time::resume();
        // Once it's answered, no more.
        run.finish
            .send(Ran::new(true, Vec::new(), Answer::text("ok")))
            .unwrap();
        let answer = client.recv().await;
        assert_eq!(answer["id"], 2);
        h.apply(ToolDirective::Refuse {
            call: quiet,
            why: RefusedCall::NotAllowed,
        });
        let answer = client.recv().await;
        assert_eq!(answer["id"], 1);
        client.nothing_waiting().await;
    }

    #[test]
    fn a_relay_without_the_hook_fails_the_check_naming_it() {
        let why = check_relay(&SpawnCommand::new("/bin/true")).unwrap_err();
        assert!(
            matches!(&why, StartError::NoRelayHook { relay, .. } if relay == Path::new("/bin/true"))
        );
        let why = why.to_string();
        assert!(why.contains("caf_relay::relay_if_asked()"), "{why}");
        let why = check_relay(&SpawnCommand::new("/no/such/program"))
            .unwrap_err()
            .to_string();
        assert!(why.contains("/no/such/program"), "{why}");
    }

    #[test]
    fn a_relay_that_answers_passes_the_check() {
        let mut command = SpawnCommand::new("/bin/sh");
        command.args = vec![
            "-c".into(),
            format!(
                "[ \"${}\" = 1 ] && echo '{}'",
                caf_relay::CHECK_VAR,
                caf_relay::CHECK_ANSWER
            )
            .into(),
        ];
        check_relay(&command).unwrap();
    }

    #[tokio::test]
    async fn a_socket_path_too_long_fails_naming_it() {
        let dir = Dir::new();
        let folder = dir.0.join("x".repeat(100));
        let config: ToolsConfig = ToolsConfig {
            folder: Some(folder.clone()),
            ..ToolsConfig::default()
        };
        let Err(why) = ToolsDriver::listen(config, SpawnCommand::new("/bin/false"), "test") else {
            panic!("it listened");
        };
        assert!(
            matches!(&why, StartError::SocketPathTooLong { path } if path.starts_with(&folder))
        );
        assert!(!folder.exists(), "the folder wasn't made");
        let why = why.to_string();
        assert!(why.contains(&folder.display().to_string()), "{why}");
        assert!(why.contains("ToolsConfig::folder"), "{why}");
    }

    #[tokio::test]
    async fn a_folder_others_can_reach_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = Dir::new();
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config: ToolsConfig = ToolsConfig {
            folder: Some(dir.0.clone()),
            ..ToolsConfig::default()
        };
        let Err(why) = ToolsDriver::listen(config, SpawnCommand::new("/bin/false"), "test") else {
            panic!("it listened");
        };
        let why = why.to_string();
        assert!(why.contains("mode 700"), "{why}");
    }

    #[tokio::test]
    async fn the_folder_is_made_the_users_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = Dir::new();
        let folder = dir.0.join("tools");
        let config: ToolsConfig = ToolsConfig {
            folder: Some(folder.clone()),
            ..ToolsConfig::default()
        };
        let (_driver, serving) =
            ToolsDriver::listen(config, SpawnCommand::new("/bin/false"), "my app").unwrap();
        let mode = std::fs::metadata(&folder).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // Another test's CAF in this process may have come first, and been
        // numbered before it.
        let name = serving.socket.file_name().unwrap().to_str().unwrap();
        let first = format!("my_app-{}", std::process::id());
        assert_eq!(serving.socket.parent(), Some(&*folder));
        assert!(
            name == format!("{first}.sock")
                || (name.starts_with(&format!("{first}-")) && name.ends_with(".sock")),
            "{name}"
        );
        assert_ne!(serving.key.0, [0; 32]);
    }

    #[test]
    fn a_long_line_is_refused_and_a_cut_one_is_an_error() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut read = BufReader::with_capacity(4, &b"abc\ndefghij\nkl"[..]);
            assert_eq!(read_line(&mut read, 8).await, Ok(Some(b"abc".to_vec())));
            assert!(read_line(&mut read, 5).await.is_err());
            let mut read = BufReader::new(&b"abc\nkl"[..]);
            read_line(&mut read, 8).await.unwrap();
            assert!(read_line(&mut read, 8).await.is_err());
            let mut read = BufReader::new(&b""[..]);
            assert_eq!(read_line(&mut read, 8).await, Ok(None));
        });
    }

    #[tokio::test]
    async fn a_call_that_finished_as_the_core_cancelled_it_is_answered_with_what_it_gave() {
        let mut h = harness_of(false, false);
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        h.apply(ToolDirective::Run {
            call,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        let run = h.run().await;
        run.finish
            .send(Ran::new(true, Vec::new(), Answer::text("ok")))
            .unwrap();
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Finished { .. })
        });
        // The core stopped the call before it took its Finished: the work
        // was done, so the model hears what it gave, once.
        h.apply(ToolDirective::Cancel { call });
        h.taken();
        let answer = client.recv().await;
        assert_eq!(answer["id"], 1);
        assert_eq!(text_of(&answer), "ok");
        assert_eq!(answer["result"]["isError"], false);
        client.nothing_waiting().await;
    }

    #[tokio::test]
    async fn a_relay_that_stops_reading_for_long_is_closed_as_lost() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        // One answer, more than its socket holds, and the agent reads none.
        let big = h.called(&mut client, 2, false).await;
        h.apply(ToolDirective::Run {
            call: big,
            action: Action::Read {
                path: "/x".into(),
                lines: None,
            },
            input: serde_json::Value::Null,
        });
        let run = h.run().await;
        // Before its answer is written, so before the write's time starts.
        let started = Instant::now();
        run.finish
            .send(Ran::new(
                true,
                Vec::new(),
                Answer::text("x".repeat(2 << 20)),
            ))
            .unwrap();
        assert_event(h.report().await, big, |event| {
            matches!(event, ToolEvent::Finished { .. })
        });
        // The clock moves only once everything waits.
        tokio::time::pause();
        // Waited for longer than a line may take to write.
        let wait = WRITE_TIME * 6;
        assert_event(h.report_within(wait).await, call, |event| {
            matches!(event, ToolEvent::Gone)
        });
        match h.report_within(wait).await {
            Report::Relay {
                chat,
                change: RelayChange::Lost,
            } => assert_eq!(chat, CHAT),
            other => panic!("expected Lost, got {other:?}"),
        }
        assert!(started.elapsed() >= WRITE_TIME, "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_relay_that_stops_reading_is_closed_as_lost() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        // Asked for its tools over and over, and never reading the answers.
        let list = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n";
        for _ in 0..4000 {
            if client.write.write_all(list).await.is_err() {
                break;
            }
        }
        match h.report().await {
            Report::Relay {
                chat,
                change: RelayChange::Lost,
            } => assert_eq!(chat, CHAT),
            other => panic!("expected Lost, got {other:?}"),
        }
        // Its socket ends, with what was written before; its writer was
        // stopped, which shutting down takes in its stride.
        let mut rest = Vec::new();
        let ended =
            tokio::time::timeout(Duration::from_secs(10), client.read.read_to_end(&mut rest));
        assert!(ended.await.is_ok(), "the socket ended");
        h.shut_down().await;
    }

    #[tokio::test]
    async fn a_closed_relays_writer_is_let_go() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        assert_eq!(h.writers().await, 1);
        h.apply(ToolDirective::Unserve { chat: CHAT });
        assert!(client.closed().await);
        let reaped = tokio::time::timeout(Duration::from_secs(10), async {
            while h.writers().await > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(reaped.is_ok(), "the writer was never let go");
    }

    #[tokio::test]
    async fn a_relay_has_ten_seconds_to_say_which_chat_it_serves() {
        let h = harness();
        let stream = UnixStream::connect(&h.serving.socket).await.unwrap();
        let (read, write) = stream.into_split();
        let mut client = Client {
            read: BufReader::new(read),
            write,
        };
        tokio::time::pause();
        let started = Instant::now();
        let mut rest = Vec::new();
        let _ = client.read.read_to_end(&mut rest).await;
        assert!(rest.is_empty());
        assert!(
            started.elapsed() >= HANDSHAKE_TIME,
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_line_over_the_limit_closes_its_relay_as_lost() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        let mut line = vec![b' '; LINE_MOST + 1];
        line.push(b'\n');
        let _ = client.write.write_all(&line).await;
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Gone)
        });
        match h.report().await {
            Report::Relay {
                chat,
                change: RelayChange::Lost,
            } => assert_eq!(chat, CHAT),
            other => panic!("expected Lost, got {other:?}"),
        }
        assert!(client.closed().await);
    }

    #[test]
    fn only_check_set_to_1_is_a_check() {
        use std::ffi::OsStr;
        assert!(asked_to_check(Some(OsStr::new("1"))));
        assert!(!asked_to_check(Some(OsStr::new("0"))));
        assert!(!asked_to_check(Some(OsStr::new(""))));
        assert!(!asked_to_check(None));
    }

    #[test]
    fn a_relay_that_answers_and_leaves_its_output_open_passes() {
        // Something it started still holds its output once it has gone.
        let mut command = SpawnCommand::new("/bin/sh");
        command.args = vec![
            "-c".into(),
            format!(
                "[ \"${}\" = 1 ] && echo '{}'; sleep 3 &",
                caf_relay::CHECK_VAR,
                caf_relay::CHECK_ANSWER
            )
            .into(),
        ];
        let started = std::time::Instant::now();
        check_relay(&command).unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn a_relay_command_the_agent_cant_be_sent_is_refused() {
        use std::os::unix::ffi::OsStrExt as _;
        let unsendable = [
            {
                let mut command = SpawnCommand::new("/bin/true");
                command.cwd = Some("/tmp".into());
                command
            },
            {
                let mut command = SpawnCommand::new("/bin/true");
                command.env = vec![EnvChange::Remove("HOME".into())];
                command
            },
            {
                let mut command = SpawnCommand::new("/bin/true");
                command.args = vec![std::ffi::OsStr::from_bytes(b"\xff").into()];
                command
            },
            {
                let mut command = SpawnCommand::new("/bin/true");
                command.env = vec![EnvChange::Set(
                    "A".into(),
                    std::ffi::OsStr::from_bytes(b"\xff").into(),
                )];
                command
            },
            SpawnCommand::new(std::ffi::OsStr::from_bytes(b"/bin/\xff")),
        ];
        for command in unsendable {
            let dir = Dir::new();
            let config: ToolsConfig = ToolsConfig {
                relay: Relay::Command(command.clone()),
                folder: Some(dir.0.clone()),
                ..ToolsConfig::default()
            };
            let Err(why) = ToolsDriver::start(config, "test") else {
                panic!("{command:?} was taken");
            };
            assert!(matches!(why, StartError::Serving(_)), "{why:?}");
            assert!(why.to_string().contains("can't be sent"), "{why}");
        }
        // Nor a socket whose path isn't UTF-8.
        let dir = Dir::new();
        let config: ToolsConfig = ToolsConfig {
            folder: Some(dir.0.join(std::ffi::OsStr::from_bytes(b"\xff"))),
            ..ToolsConfig::default()
        };
        let Err(why) = ToolsDriver::listen(config, SpawnCommand::new("/bin/false"), "test") else {
            panic!("a socket path that isn't UTF-8 was taken");
        };
        assert!(why.to_string().contains("UTF-8"), "{why}");
    }

    #[tokio::test]
    async fn two_cafs_in_one_process_have_sockets_of_their_own() {
        let dir = Dir::new();
        let config = || ToolsConfig::<NoAppItems> {
            folder: Some(dir.0.clone()),
            ..ToolsConfig::default()
        };
        let (_one, first) =
            ToolsDriver::listen(config(), SpawnCommand::new("/bin/false"), "app").unwrap();
        let (_two, second) =
            ToolsDriver::listen(config(), SpawnCommand::new("/bin/false"), "app").unwrap();
        assert_ne!(first.socket, second.socket);
        assert!(first.socket.exists() && second.socket.exists());
        UnixStream::connect(&first.socket).await.unwrap();
    }

    #[tokio::test]
    async fn a_socket_in_use_is_refused_and_a_stale_one_replaced() {
        let dir = Dir::new();
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("s.sock");
        let live = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let Err(why) = listen(&path) else {
            panic!("it took a socket in use");
        };
        assert!(why.to_string().contains("in use"), "{why}");
        assert!(path.exists());
        drop(live);
        // Its file is left behind, as a process that died leaves it.
        assert!(path.exists());
        listen(&path).unwrap();
    }

    /// Runs `call` as `action`, giving back the test's end of the run.
    async fn running(h: &mut Harness, call: CallId, action: Action) -> Run {
        h.apply(ToolDirective::Run {
            call,
            action,
            input: serde_json::Value::Null,
        });
        h.run().await
    }

    fn a_read() -> Action {
        Action::Read {
            path: "/x".into(),
            lines: None,
        }
    }

    #[tokio::test]
    async fn a_preview_that_fails_ends_the_call_and_answers_why_once_the_core_has_it() {
        let mut h = harness_of(false, false);
        h.serve(CHAT);
        let mut client = h.connect(CHAT).await;
        assert_connected(h.report().await, CHAT);
        h.taken();
        client.init().await;
        client.call(3, false).await;
        let call = match h.report().await {
            Report::Tool { call, .. } => call,
            other => panic!("expected Called, got {other:?}"),
        };
        h.taken();
        h.apply(ToolDirective::Preview {
            call,
            action: Action::Edit {
                path: "/fail".into(),
            },
            input: serde_json::Value::Null,
        });
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Finished { done: false, pieces, .. } if pieces.len() == 1),
        );
        // Not before the core has it.
        client.nothing_waiting().await;
        h.taken();
        let answer = client.recv().await;
        assert_eq!(answer["id"], 3);
        assert_eq!(answer["result"]["isError"], true);
        assert_eq!(answer["result"]["content"][0]["text"], "it can't be");
    }

    #[tokio::test]
    async fn a_job_is_answered_after_its_detached_is_taken_and_ends_with_its_notice() {
        let mut h = harness_of(false, false);
        h.serve(CHAT);
        let mut client = h.connect(CHAT).await;
        assert_connected(h.report().await, CHAT);
        h.taken();
        client.init().await;
        client.call(4, false).await;
        let call = match h.report().await {
            Report::Tool { call, .. } => call,
            other => panic!("expected Called, got {other:?}"),
        };
        h.taken();
        let mut run = running(&mut h, call, a_read()).await;
        assert_eq!(run.call.agent_call(), Some("toolu_4"));
        run.call.print(0, vec!["before".into()]);
        let job = run
            .call
            .answer_now(|job| Answer::text(format!("job {}", job.0)));
        assert_eq!(job, JobId(1));
        // A second gives the same job, and says nothing.
        assert_eq!(run.call.answer_now(|_| Answer::text("again")), job);
        // What it printed first is reported before it's a job.
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Output { from: 0, .. })
        });
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Detached { job: JobId(1) })
        });
        client.nothing_waiting().await;
        h.taken();
        let answer = client.recv().await;
        assert_eq!(
            (
                answer["id"].clone(),
                answer["result"]["content"][0]["text"].clone()
            ),
            (json!(4), json!("job 1"))
        );
        run.call.print(1, vec!["after".into()]);
        run.finish
            .send(Ran::new(true, Vec::new(), Answer::text("unheard")).with_notice("it ended"))
            .unwrap();
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Output { from: 1, .. })
        });
        h.taken();
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Finished { done: true, notice: Some(notice), .. } if notice == "it ended"),
        );
        h.taken();
        // The model was answered once, long ago.
        client.nothing_waiting().await;
    }

    #[tokio::test]
    async fn a_job_outlives_its_relay_and_task_stop_stops_it() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        let mut run = running(&mut h, call, a_read()).await;
        run.call.answer_now(|_| Answer::text("in the background"));
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Detached { job: JobId(1) })
        });
        assert_eq!(client.recv().await["id"], 1);
        // The agent cancelling its answered request leaves it running.
        client
            .send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}}))
            .await;
        client.nothing_waiting().await;
        h.quiet().await;
        // Its relay goes: the job doesn't.
        drop(client);
        match h.report().await {
            Report::Relay {
                change: RelayChange::Lost,
                ..
            } => {}
            other => panic!("expected Lost, got {other:?}"),
        }
        h.quiet().await;
        let mut client = h.bound(CHAT).await;
        // A second job is numbered on.
        let second = h.called(&mut client, 2, false).await;
        let mut other = running(&mut h, second, a_read()).await;
        assert_eq!(other.call.answer_now(|_| Answer::text("too")), JobId(2));
        assert_event(h.report().await, second, |event| {
            matches!(event, ToolEvent::Detached { job: JobId(2) })
        });
        assert_eq!(client.recv().await["id"], 2);
        let stop = h.called(&mut client, 3, false).await;
        h.apply(ToolDirective::Run {
            call: stop,
            action: Action::Stop { job: JobId(1) },
            input: serde_json::Value::Null,
        });
        assert_eq!(h.dropped().await, call);
        assert_event(h.report().await, call, |event| {
            matches!(
                event,
                ToolEvent::Finished {
                    done: false,
                    notice: None,
                    ..
                }
            )
        });
        assert_event(h.report().await, stop, |event| {
            matches!(event, ToolEvent::Finished { done: true, .. })
        });
        let answer = client.recv().await;
        assert_eq!(
            (
                answer["id"].clone(),
                answer["result"]["content"][0]["text"].clone()
            ),
            (json!(3), json!("Stopped task 1"))
        );
        // It's gone now.
        let again = h.called(&mut client, 4, false).await;
        h.apply(ToolDirective::Run {
            call: again,
            action: Action::Stop { job: JobId(1) },
            input: serde_json::Value::Null,
        });
        assert_event(h.report().await, again, |event| {
            matches!(event, ToolEvent::Finished { done: false, .. })
        });
        let answer = client.recv().await;
        assert_eq!(answer["result"]["isError"], true);
        assert_eq!(
            answer["result"]["content"][0]["text"],
            "No task 1 in this conversation"
        );
        // The core cancels a job: it stops, and no one's answered.
        h.apply(ToolDirective::Cancel { call: second });
        assert_eq!(h.dropped().await, second);
        client.nothing_waiting().await;
        drop(other);
    }

    #[tokio::test]
    async fn letting_a_chat_go_stops_its_jobs_whatever_their_relay() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        let mut run = running(&mut h, call, a_read()).await;
        run.call.answer_now(|_| Answer::text("in the background"));
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Detached { .. })
        });
        drop(client);
        let _lost = h.report().await;
        h.apply(ToolDirective::Unserve { chat: CHAT });
        assert_eq!(h.dropped().await, call);
        h.quiet().await;
    }

    #[tokio::test]
    async fn a_job_ending_without_a_notice_is_given_one_so_it_isnt_taken_as_stopped() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let call = h.called(&mut client, 1, false).await;
        let mut run = running(&mut h, call, a_read()).await;
        run.call.answer_now(|_| Answer::text("in the background"));
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Detached { .. })
        });
        run.finish.send(failed("it broke")).unwrap();
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Finished { done: false, notice: Some(notice), .. }
                if notice == "<task-notification>\n<task-id>1</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<status>failed</status>\n<summary>Background task 1 failed</summary>\n</task-notification>")
        });
    }

    #[tokio::test]
    async fn the_runner_forgets_a_call_refused_or_cancelled() {
        let mut h = harness();
        h.serve(CHAT);
        let mut client = h.bound(CHAT).await;
        let refused = h.called(&mut client, 1, false).await;
        h.apply(ToolDirective::Refuse {
            call: refused,
            why: RefusedCall::NotAllowed,
        });
        let cancelled = h.called(&mut client, 2, false).await;
        h.apply(ToolDirective::Cancel { call: cancelled });
        for expected in [refused, cancelled] {
            let forgot = tokio::time::timeout(Duration::from_secs(10), h.forgot.recv())
                .await
                .unwrap();
            assert_eq!(forgot, Some(expected));
        }
    }

    /// A clip to trim.
    #[derive(Deserialize, schemars::JsonSchema)]
    struct Trim {
        /// The clip.
        clip: u64,
    }

    /// The app's tool trim_clip, which previews, and its run waits for
    /// `go` and says when it's dropped on `dropped`.
    struct Trimmer {
        go: Arc<tokio::sync::Notify>,
        dropped: mpsc::UnboundedSender<u64>,
    }

    impl crate::AppTool for Trimmer {
        type Input = Trim;

        fn spec(&self) -> crate::ToolSpec {
            let mut spec = crate::ToolSpec::new("trim_clip");
            spec.title = "Trim a clip".into();
            spec.description = "Trims a clip.".into();
            spec.hints.destructive = true;
            spec.previews = true;
            spec
        }

        fn title(&self, input: &Trim) -> String {
            format!("Trim clip {}", input.clip)
        }

        fn preview(
            &self,
            _call: &Call,
            input: &Trim,
        ) -> Option<crate::app_tools::BoxFuture<Result<Vec<Piece>, Done>>> {
            let clip = input.clip;
            Some(Box::pin(async move {
                match clip {
                    0 => Err(Done::failed("There's no clip 0")),
                    clip => Ok(vec![Piece::Text(Text::new(format!(
                        "Would trim clip {clip}"
                    )))]),
                }
            }))
        }

        fn run(&self, mut call: Call, input: Trim) -> crate::app_tools::BoxFuture<Done> {
            let go = Arc::clone(&self.go);
            let dropped = DroppedClip(input.clip, self.dropped.clone());
            Box::pin(async move {
                let _dropped = dropped;
                call.print(0, vec![format!("trimming {}", input.clip)]);
                if input.clip == 9 {
                    call.answer_now(|job| Answer::text(format!("Trimming as job {}", job.0)));
                }
                go.notified().await;
                Done::answer(format!("Trimmed clip {}", input.clip))
                    .with_record(vec![Piece::Text(Text::new("trimmed"))])
            })
        }
    }

    /// Says its clip's run was dropped.
    struct DroppedClip(u64, mpsc::UnboundedSender<u64>);

    impl Drop for DroppedClip {
        fn drop(&mut self) {
            let _ = self.1.send(self.0);
        }
    }

    /// A harness serving trim_clip, whose runs end as `go` is notified.
    fn trimming() -> (
        Harness,
        Arc<tokio::sync::Notify>,
        mpsc::UnboundedReceiver<u64>,
    ) {
        let go = Arc::new(tokio::sync::Notify::new());
        let (dropped, drops) = mpsc::unbounded_channel();
        let trimmer = Trimmer {
            go: Arc::clone(&go),
            dropped,
        };
        let app = AppTools::new().with(trimmer).unwrap();
        (harness_serving(false, true, app), go, drops)
    }

    /// Calls trim_clip as `id` and gives back its report's call and event.
    async fn trim(
        h: &mut Harness,
        client: &mut Client,
        id: u64,
        input: Value,
    ) -> (CallId, ToolEvent) {
        client.call_tool(id, "trim_clip", input).await;
        match h.report().await {
            Report::Tool { call, event } => (call, event),
            other => panic!("expected Called, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_apps_tools_are_listed_after_cafs_and_the_core_knows_them() {
        let (mut h, _go, _drops) = trimming();
        let mut hints = caf_core::tool::Hints::default();
        hints.destructive = true;
        assert_eq!(
            h.serving.app_tools,
            [caf_core::tool::AppToolSpec::new(
                "trim_clip",
                "Trim a clip",
                hints,
                true
            )]
        );
        h.serve_app(CHAT, &["trim_clip"]);
        let mut client = h.bound(CHAT).await;
        client
            .send(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .await;
        let listed = client.recv().await;
        let tools = listed["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Read", "trim_clip"]);
        let trim = &tools[1];
        assert_eq!(trim["title"], "Trim a clip");
        assert_eq!(trim["description"], "Trims a clip.");
        assert_eq!(
            trim["inputSchema"]["properties"]["clip"]["description"],
            "The clip."
        );
        assert_eq!(trim["annotations"]["destructiveHint"], true);
        assert_eq!(trim["annotations"]["readOnlyHint"], false);
    }

    #[tokio::test]
    async fn an_app_tools_call_is_read_titled_previewed_run_and_answered() {
        let (mut h, go, _drops) = trimming();
        h.serve_app(CHAT, &["trim_clip"]);
        let mut client = h.bound(CHAT).await;
        let (call, event) = trim(&mut h, &mut client, 3, json!({"clip": 3})).await;
        match event {
            ToolEvent::Called {
                chat,
                tool,
                input,
                agent_call,
                title,
                bad_input,
            } => {
                assert_eq!((chat, tool), (CHAT, ServedTool::App("trim_clip".into())));
                assert_eq!(input, json!({"clip": 3}));
                assert_eq!(agent_call.as_deref(), Some("toolu_3"));
                assert_eq!(title.as_deref(), Some("Trim clip 3"));
                assert_eq!(bad_input, None);
            }
            other => panic!("expected Called, got {other:?}"),
        }
        let action = Action::Tool {
            name: "trim_clip".into(),
        };
        h.apply(ToolDirective::Preview {
            call,
            action: action.clone(),
            input: json!({"clip": 3}),
        });
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Previewed { pieces } if *pieces == [Piece::Text(Text::new("Would trim clip 3"))]),
        );
        h.apply(ToolDirective::Run {
            call,
            action,
            input: json!({"clip": 3}),
        });
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Output { from: 0, lines } if *lines == ["trimming 3"]),
        );
        go.notify_one();
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Finished { done: true, pieces, notice: None, .. } if pieces.len() == 1),
        );
        let answer = client.recv().await;
        assert_eq!(answer["id"], 3);
        assert_eq!(text_of(&answer), "Trimmed clip 3");
        client.nothing_waiting().await;
        h.quiet().await;
    }

    #[tokio::test]
    async fn an_app_tools_call_whose_input_doesnt_fit_says_why_titled_as_the_tool() {
        let (mut h, _go, _drops) = trimming();
        h.serve_app(CHAT, &["trim_clip"]);
        let mut client = h.bound(CHAT).await;
        let (call, event) = trim(&mut h, &mut client, 4, json!({"clip": "four"})).await;
        let why = match event {
            ToolEvent::Called {
                title,
                bad_input: Some(why),
                ..
            } => {
                assert_eq!(title.as_deref(), Some("Trim a clip"));
                why
            }
            other => panic!("expected Called with bad input, got {other:?}"),
        };
        assert!(why.starts_with("invalid type"), "{why}");
        h.apply(ToolDirective::Refuse {
            call,
            why: RefusedCall::BadInput(why.clone()),
        });
        let answer = client.recv().await;
        assert_eq!(answer["result"]["isError"], true);
        assert_eq!(text_of(&answer), crate::app_tools::bad_input(&why));
    }

    #[tokio::test]
    async fn an_app_tools_preview_that_fails_ends_its_call() {
        let (mut h, _go, _drops) = trimming();
        h.serve_app(CHAT, &["trim_clip"]);
        let mut client = h.bound(CHAT).await;
        let (call, _) = trim(&mut h, &mut client, 5, json!({"clip": 0})).await;
        h.apply(ToolDirective::Preview {
            call,
            action: Action::Tool {
                name: "trim_clip".into(),
            },
            input: json!({"clip": 0}),
        });
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Finished { done: false, .. })
        });
        let answer = client.recv().await;
        assert_eq!(text_of(&answer), "There's no clip 0");
    }

    #[tokio::test]
    async fn a_cancelled_app_tools_call_has_its_future_dropped() {
        let (mut h, _go, mut drops) = trimming();
        h.serve_app(CHAT, &["trim_clip"]);
        let mut client = h.bound(CHAT).await;
        let (call, _) = trim(&mut h, &mut client, 6, json!({"clip": 6})).await;
        h.apply(ToolDirective::Run {
            call,
            action: Action::Tool {
                name: "trim_clip".into(),
            },
            input: json!({"clip": 6}),
        });
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Output { .. })
        });
        h.apply(ToolDirective::Cancel { call });
        let dropped = tokio::time::timeout(Duration::from_secs(10), drops.recv()).await;
        assert_eq!(dropped.unwrap(), Some(6));
        let answer = client.recv().await;
        assert_eq!(text_of(&answer), CANCELLED);
    }

    #[tokio::test]
    async fn an_app_tool_answering_early_goes_on_as_a_job_ending_with_its_answer_as_notice() {
        let (mut h, go, _drops) = trimming();
        h.serve_app(CHAT, &["trim_clip"]);
        let mut client = h.bound(CHAT).await;
        let (call, _) = trim(&mut h, &mut client, 9, json!({"clip": 9})).await;
        h.apply(ToolDirective::Run {
            call,
            action: Action::Tool {
                name: "trim_clip".into(),
            },
            input: json!({"clip": 9}),
        });
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Output { .. })
        });
        assert_event(h.report().await, call, |event| {
            matches!(event, ToolEvent::Detached { job: JobId(1) })
        });
        let answer = client.recv().await;
        assert_eq!(text_of(&answer), "Trimming as job 1");
        go.notify_one();
        let notice = match h.report().await {
            Report::Tool {
                event:
                    ToolEvent::Finished {
                        done: true,
                        notice: Some(notice),
                        ..
                    },
                ..
            } => notice,
            other => panic!("expected Finished, got {other:?}"),
        };
        assert_eq!(
            notice,
            "<task-notification>\n<task-id>1</task-id>\n<tool-use-id>toolu_9</tool-use-id>\n\
             <status>completed</status>\n<summary>Background task \"Trim clip 9\" completed</summary>\n\
             <result>Trimmed clip 9</result>\n</task-notification>"
        );
        client.nothing_waiting().await;
    }

    #[tokio::test]
    async fn an_app_tool_not_given_the_chat_isnt_listed_or_called() {
        let (mut h, _go, _drops) = trimming();
        h.serve_app(CHAT, &[]);
        let mut client = h.bound(CHAT).await;
        client.call_tool(1, "trim_clip", json!({"clip": 1})).await;
        let answer = client.recv().await;
        assert_eq!(answer["result"]["isError"], true);
        assert_eq!(text_of(&answer), "There's no tool named `trim_clip` here.");
        h.quiet().await;
    }
}
