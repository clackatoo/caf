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
use std::task::{Context, Poll};
use std::time::Duration;

use caf_core::agent::{EnvChange, SpawnCommand};
use caf_core::directive::{RefusedCall, ToolDirective};
use caf_core::event::{RelayChange, Serving, ToolEvent};
use caf_core::ids::{CallId, ChatId};
use caf_core::mcp::{self, Answer, Connection, Key, Out, Request};
use caf_core::tool::{Action, CafTool, Piece, Text};
use serde::Deserialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::{Instant, Sleep};

/// How the app serves CAF's tools: [`Config::tools`](crate::Config::tools).
#[non_exhaustive]
pub struct ToolsConfig {
    /// The program an agent starts to reach CAF's tools. [`Relay::None`]
    /// by default: no tools are served.
    pub relay: Relay,
    /// What runs the calls. [`NotBuilt`] by default.
    pub runner: Box<dyn Runner>,
    /// The folder the socket relays connect to is made in, which must be
    /// the user's alone. `None` (the default) means `$XDG_RUNTIME_DIR/caf`,
    /// or else `caf-<uid>` in the temporary folder; either is made, mode
    /// 700, if it isn't there.
    pub folder: Option<PathBuf>,
    /// How often an open call's progress is sent: 60 s. For tests.
    #[doc(hidden)]
    pub progress_every: Duration,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            relay: Relay::None,
            runner: Box::new(NotBuilt),
            folder: None,
            progress_every: PROGRESS_EVERY,
        }
    }
}

impl std::fmt::Debug for ToolsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolsConfig")
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
/// drop: a command it started is stopped, say.
pub trait Runner: Send + 'static {
    /// What `action` would do, without doing anything, such as an edit's
    /// diff.
    fn preview(
        &mut self,
        call: &Call,
        action: Action,
    ) -> Pin<Box<dyn Future<Output = Vec<Piece>> + Send>>;
    /// Does `action`, printing a command's output with
    /// [`Call::print`] as it comes.
    fn run(&mut self, call: Call, action: Action) -> Pin<Box<dyn Future<Output = Ran> + Send>>;
}

/// What a call did.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Ran {
    /// Whether it did what it was asked.
    pub done: bool,
    /// What it gave back, for the record
    /// ([`ToolEvent::Finished`]'s `pieces`).
    pub record: Vec<Piece>,
    /// What the model is answered.
    pub answer: Answer,
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
        }
    }
}

/// One call, as the [`Runner`] is given it.
#[derive(Debug)]
pub struct Call {
    chat: ChatId,
    id: CallId,
    prints: mpsc::UnboundedSender<Print>,
}

impl Call {
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

    /// A running command's output changed: the lines from `from` on, as
    /// its terminal shows them, counting every line it has printed from
    /// 0 ([`ToolEvent::Output`]). Batched: what's printed is reported at
    /// most every 100 ms or 16 KiB.
    pub fn print(&self, from: u64, lines: Vec<String>) {
        let _ = self.prints.send(Print {
            call: self.id,
            from,
            lines,
        });
    }
}

/// The runner until CAF's tools are built: every call fails, saying so.
#[derive(Clone, Copy, Debug, Default)]
pub struct NotBuilt;

/// What [`NotBuilt`] says.
const NOT_BUILT: &str = "CAF's tools aren't built yet";

impl Runner for NotBuilt {
    fn preview(
        &mut self,
        _call: &Call,
        _action: Action,
    ) -> Pin<Box<dyn Future<Output = Vec<Piece>> + Send>> {
        Box::pin(std::future::ready(Vec::new()))
    }

    fn run(&mut self, _call: Call, _action: Action) -> Pin<Box<dyn Future<Output = Ran> + Send>> {
        Box::pin(std::future::ready(failed(NOT_BUILT)))
    }
}

/// A call that failed, saying `why` to the record and the model.
fn failed(why: &str) -> Ran {
    Ran::new(false, vec![Piece::Text(Text::new(why))], Answer::error(why))
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
pub(crate) enum Report {
    /// [`Event::Tool`](caf_core::Event::Tool).
    Tool { call: CallId, event: ToolEvent },
    /// [`Event::Relay`](caf_core::Event::Relay).
    Relay { chat: ChatId, change: RelayChange },
}

/// A future the runner gave.
type Running<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// The tools driver.
pub(crate) struct ToolsDriver {
    runner: Box<dyn Runner>,
    /// Where relays connect, if tools are served.
    live: Option<Live>,
    /// What each chat is served.
    served: HashMap<ChatId, Served>,
    /// Each bound connection, by its number.
    conns: HashMap<u64, Conn>,
    /// Each chat's bound connection.
    bound: HashMap<ChatId, u64>,
    /// Each open call.
    calls: BTreeMap<CallId, Each>,
    /// The last call's number.
    last_call: u64,
    prints: mpsc::UnboundedSender<Print>,
    printed: mpsc::UnboundedReceiver<Print>,
    /// Reports waiting to be handed out.
    reports: VecDeque<Report>,
    /// The answer to each call that finished, held until the core has
    /// taken its [`ToolEvent::Finished`]: so the agent can't end its turn
    /// on hearing it before the core knows the call is done.
    held: HashMap<CallId, Held>,
    /// The call whose `Finished` was handed out last, its answer to be
    /// written once the core has it ([`ToolsDriver::taken`]).
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
    instructions: String,
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
struct Each {
    chat: ChatId,
    conn: u64,
    request: Request,
    /// Whether the core has said to run it.
    running: bool,
    preview: Option<Running<Vec<Piece>>>,
    run: Option<Running<Ran>>,
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

/// A [`Call::print`].
#[derive(Debug)]
struct Print {
    call: CallId,
    from: u64,
    lines: Vec<String>,
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

impl ToolsDriver {
    fn with(config: ToolsConfig, live: Option<Live>) -> Self {
        let (prints, printed) = mpsc::unbounded_channel();
        Self {
            runner: config.runner,
            progress_every: config.progress_every.max(Duration::from_millis(1)),
            live,
            served: HashMap::new(),
            conns: HashMap::new(),
            bound: HashMap::new(),
            calls: BTreeMap::new(),
            last_call: 0,
            prints,
            printed,
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
        mut config: ToolsConfig,
        app: &str,
    ) -> Result<(Self, Option<Serving>), StartError> {
        let relay = match std::mem::take(&mut config.relay) {
            Relay::None => return Ok((Self::with(config, None), None)),
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
        mut config: ToolsConfig,
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
        let serving = Serving::new(relay, socket.clone(), Key(key));
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
        Ok((Self::with(config, Some(live)), serving))
    }

    /// Carries out the core's directive.
    pub(crate) fn apply(&mut self, directive: ToolDirective) {
        match directive {
            ToolDirective::Serve {
                chat,
                tools,
                instructions,
            } => {
                self.served.insert(
                    chat,
                    Served {
                        tools,
                        instructions,
                    },
                );
            }
            ToolDirective::Unserve { chat } => {
                self.served.remove(&chat);
                if let Some(conn) = self.bound.get(&chat).copied() {
                    self.close(conn, Closing::Ended);
                }
            }
            ToolDirective::Preview { call, action } => {
                let Some(each) = self.calls.get_mut(&call) else {
                    return;
                };
                let given = Call {
                    chat: each.chat,
                    id: call,
                    prints: self.prints.clone(),
                };
                let runner = &mut self.runner;
                let preview =
                    std::panic::catch_unwind(AssertUnwindSafe(|| runner.preview(&given, action)))
                        .unwrap_or_else(|_| {
                            log::warn!(target: "caf", "the tool runner panicked previewing call {}", call.0);
                            Box::pin(std::future::ready(Vec::new()))
                        });
                each.preview = Some(preview);
            }
            ToolDirective::Run { call, action } => {
                let Some(each) = self.calls.get_mut(&call) else {
                    return;
                };
                let given = Call {
                    chat: each.chat,
                    id: call,
                    prints: self.prints.clone(),
                };
                let runner = &mut self.runner;
                let run = std::panic::catch_unwind(AssertUnwindSafe(|| runner.run(given, action)))
                    .unwrap_or_else(|_| {
                        log::warn!(target: "caf", "the tool runner panicked running call {}", call.0);
                        Box::pin(std::future::ready(failed(PANICKED)))
                    });
                each.preview = None;
                each.running = true;
                each.run = Some(run);
            }
            ToolDirective::Refuse { call, why } => {
                if let Some(each) = self.calls.remove(&call) {
                    self.answer(each.conn, &each.request, &Answer::error(refused(&why)));
                }
            }
            ToolDirective::Cancel { call } => {
                if let Some(each) = self.calls.remove(&call) {
                    // Its work stops before the model hears it did.
                    let (conn, request) = each.stop();
                    self.answer(conn, &request, &Answer::error(CANCELLED));
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

    /// The next report, for the core. Cancel-safe: everything is kept in
    /// the driver between polls.
    pub(crate) async fn next(&mut self) -> Report {
        std::future::poll_fn(|cx| self.poll_next(cx)).await
    }

    fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Report> {
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
    fn pop(&mut self) -> Option<Report> {
        let report = self.reports.pop_front()?;
        if let Report::Tool {
            call,
            event: ToolEvent::Finished { .. },
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
        let mcp = Connection::new(&served.tools, &served.instructions);
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
                            running: false,
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
                            tool,
                            input,
                            agent_call,
                        },
                    });
                }
                Out::Cancelled(request) => {
                    let found = (self.calls.iter())
                        .find(|(_, each)| each.conn == no && each.request == request)
                        .map(|(call, _)| *call);
                    if let Some(call) = found {
                        self.calls.remove(&call);
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
        let ended: Vec<CallId> = (self.calls.iter())
            .filter(|(_, each)| each.conn == no)
            .map(|(call, _)| *call)
            .collect();
        for call in ended {
            let Some(each) = self.calls.remove(&call) else {
                continue;
            };
            if closing == Closing::Ended {
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
            let Some(each) = self.calls.get_mut(&print.call) else {
                continue;
            };
            if !each.running {
                continue;
            }
            let call = print.call;
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
                        previewed.push((*call, Vec::new()));
                    }
                }
            }
            if let Some(run) = &mut each.run {
                match std::panic::catch_unwind(AssertUnwindSafe(|| run.as_mut().poll(cx))) {
                    Ok(Poll::Pending) => {}
                    Ok(Poll::Ready(done)) => ran.push((*call, done)),
                    Err(_) => {
                        log::warn!(target: "caf", "the tool runner panicked running call {}", call.0);
                        ran.push((*call, failed(PANICKED)));
                    }
                }
            }
        }
        for (call, pieces) in previewed {
            if let Some(each) = self.calls.get_mut(&call) {
                each.preview = None;
                self.reports.push_back(Report::Tool {
                    call,
                    event: ToolEvent::Previewed { pieces },
                });
            }
        }
        if !ran.is_empty() {
            // Output printed as it finished is reported before it.
            self.poll_prints(cx);
        }
        for (call, ran) in ran {
            let Some(each) = self.calls.remove(&call) else {
                continue;
            };
            let Each {
                conn,
                request,
                batch,
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
                },
            });
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
            if each.progress_at <= now {
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

impl Each {
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
    fn new(print: Print) -> Self {
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
    fn add(&mut self, print: Print) -> Option<Self> {
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
fn output(call: CallId, batch: Batch) -> Report {
    Report::Tool {
        call,
        event: ToolEvent::Output {
            from: batch.from,
            lines: batch.lines,
        },
    }
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
        fn preview(
            &mut self,
            _call: &Call,
            _action: Action,
        ) -> Pin<Box<dyn Future<Output = Vec<Piece>> + Send>> {
            Box::pin(std::future::ready(vec![Piece::Text(Text::new("preview"))]))
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
        let dir = Dir::new();
        let (runs, run) = mpsc::unbounded_channel();
        let (dropped, drop_seen) = mpsc::unbounded_channel();
        let config = ToolsConfig {
            runner: Box::new(Scripted {
                runs,
                dropped,
                panics,
            }),
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
                    assert_eq!((chat, tool), (CHAT, CafTool::Read));
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
                        tool,
                        input,
                        agent_call,
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
        });
        assert_event(
            h.report().await,
            call,
            |event| matches!(event, ToolEvent::Previewed { pieces } if pieces.len() == 1),
        );
        h.apply(ToolDirective::Run {
            call,
            action: action.clone(),
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
            |event| matches!(event, ToolEvent::Finished { done: true, pieces } if pieces.len() == 1),
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
        let print = |from, lines: &[&str]| Print {
            call: CallId(1),
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
        let config = ToolsConfig {
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
        let config = ToolsConfig {
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
        let config = ToolsConfig {
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
        });
        let run = h.run().await;
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
        let started = Instant::now();
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
            let config = ToolsConfig {
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
        let config = ToolsConfig {
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
        let config = || ToolsConfig {
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
}
