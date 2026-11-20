//! The agent driver: adapter processes and their pipes.
//!
//! It knows nothing about which agent it runs, or about ACP beyond
//! splitting lines: it starts programs, carries bytes and reports what
//! happens, keeping the contract in [`AgentDirective`]'s docs.
//!
//! Each process is started in a process group of its own and watched
//! through a pidfd. Signals go to the whole group through the pidfd, so
//! the `claude` programs Claude's adapter starts stop with it. Lines of
//! 256 KiB or more are parsed on a blocking thread, and handed on in
//! order.
//!
//! # How it works
//!
//! Each process has a task of its own, which owns its pipes and its pidfd
//! and does everything for it, so nothing it does waits on another
//! process. Everything it reports goes through one queue, in order:
//! [`AgentEvent::Started`] first, then frames in line order, error output,
//! and the rest as it happens. A line of 256 KiB or more holds its place
//! in the queue while a blocking thread parses it, one at a time per
//! process, and what comes after it waits behind it.
//!
//! Nothing in the task waits on one thing alone: it reads, writes, hands
//! on what it has read, and watches for exit all at once. So a core slow
//! to take events doesn't stop writing, and a full input pipe doesn't
//! stop reading. Reading pauses only while the queue is full, so a flood
//! is held back by the pipe rather than by memory.

use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use caf_core::Ticket;
use caf_core::acp::{self, Frame, Framer, Line};
use caf_core::agent::{EnvChange, SpawnCommand};
use caf_core::directive::{AgentDirective, Signal};
use caf_core::event::{AgentEvent, Exit};
use rustix::process::{Pid, PidfdFlags, WaitId, WaitIdOptions, WaitIdStatus};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::net::unix::pipe;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::setup::{Lock, lock_file};

/// Limits for the agent driver.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLimits {
    /// The biggest line accepted. Must match the core's
    /// [`Limits::biggest_message`](caf_core::host::Limits::biggest_message).
    /// 64 MiB.
    pub biggest_line: usize,
    /// Lines this long or longer are parsed off the task. 256 KiB.
    pub parse_elsewhere: usize,
    /// How much of one process's output may wait, parsed or not, to be
    /// handed on before reading it pauses, so a flood is held back by the
    /// pipe rather than by memory. 64 MiB.
    pub parsing: usize,
    /// How much is read before yielding to other tasks. 1 MiB.
    pub read_step: usize,
    /// How much input may wait unsent for one process. Two rules hold it
    /// to that, each ending in [`AgentEvent::WriteFailed`], after which
    /// nothing more is written to the process:
    /// - **Stuck:** more than this waiting, with none of it taken for
    ///   [`stuck`](Self::stuck), fails it. While the process takes some,
    ///   the clock starts again, so a process that's reading isn't failed
    ///   by this rule however much waits.
    /// - **Ceiling:** more than four times this waiting fails it at once,
    ///   moving or not, so memory stays bounded. That includes a single
    ///   write too big for it.
    ///
    /// 64 MiB, so the ceiling is 256 MiB.
    pub unsent: usize,
    /// How long more than [`unsent`](Self::unsent) may wait with none of
    /// it taken before the process is treated as stuck and reported gone.
    /// 30 s.
    pub stuck: Duration,
    /// How long a process's pipes may stay open with nothing coming
    /// through, once it has exited, before they're dropped. Time spent with
    /// reading paused, waiting for the core, doesn't count. 10 s.
    pub drain: Duration,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            biggest_line: 64 << 20,
            parse_elsewhere: 256 << 10,
            parsing: 64 << 20,
            read_step: 1 << 20,
            unsent: 64 << 20,
            stuck: Duration::from_secs(30),
            drain: Duration::from_secs(10),
        }
    }
}

/// Runs adapter processes for the core.
///
/// It keeps the contract written on [`AgentDirective`]. What else a host
/// should know:
///
/// - **Linux 5.4 or later** is needed: each process is watched and reaped
///   through a pidfd. On an older kernel every start fails.
/// - **Signals** go to a process's whole group through its pidfd, with
///   `PIDFD_SIGNAL_PROCESS_GROUP` (Linux 6.9 on), never by process number:
///   the pidfd holds the kernel's record of the process, and its group
///   hangs off that record for as long as anything is in it, so a signal
///   can't reach another group that took the number, even once the
///   process is reaped. Once the group is empty, or on an older kernel,
///   the process alone is signalled. SIGKILL goes to it too, in case it
///   left its group; SIGTERM doesn't, so a process that left its group
///   while something stayed in it isn't sent SIGTERM itself.
/// - **Before Linux 6.9** a group can't be seen once its leader is
///   reaped, so [`AgentDirective::CheckGroup`] says it's empty then, even
///   if something the process started is still running.
/// - **Exits:** each process is reaped exactly once, through its pidfd,
///   as soon as it exits, and before [`AgentEvent::Exited`] is reported.
///   If something else reaped it first, as a host ignoring SIGCHLD does,
///   it's reported as `Exit::Code(-1)`.
/// - **[`AgentEvent::OutputClosed`]** is also reported when a process has
///   exited and something it started holds its output open with nothing
///   coming through for [`AgentLimits::drain`]: its output isn't waited
///   for any more. Something that goes on writing is read until the core
///   forgets the process.
/// - **Writes after [`AgentDirective::CloseInput`]**, or after
///   [`AgentEvent::WriteFailed`], are dropped without a word: nothing more
///   is written.
/// - **Memory:** each process may hold up to
///   [`AgentLimits::biggest_line`] of a line not yet ended, and
///   [`AgentLimits::parsing`] of lines waiting for the core, so 128 MiB by
///   default, besides its unsent input, which is held to four times
///   [`AgentLimits::unsent`].
/// - **It needs tokio's real clock:** under a paused clock
///   (`tokio::time::pause`) its drain and stuck timers would jump ahead
///   whenever the runtime is idle, while its processes run in real time.
/// - **[`AgentDirective::Forget`]** of a process that hasn't exited kills
///   its group and reaps it. Dropping the driver kills every group it
///   started, and reaps what's left. If the tokio runtime shuts down
///   first, a thread of its own reaps each process left.
/// - **Installs in use:** a process started with a `hold` folder, CAF's
///   install of an agent, holds a shared `flock` on that folder from
///   before it starts until it's reaped, so no CAF program removes the
///   install meanwhile
///   ([`SetupDirective::Remove`](caf_core::directive::SetupDirective::Remove)). If the folder can't
///   be opened, or a removal holds it for over two seconds, the start
///   fails. Something the process started that outlives it doesn't hold
///   the install.
/// - **SIGPIPE:** writing to a process that has gone raises it. Rust
///   programs ignore it; a host whose `main` isn't Rust's must too.
/// - **A process another thread starts** holds every pipe open in this
///   one for a moment, until it starts its program. So the first write to
///   a process that just closed its input can still succeed, and only the
///   next one fails.
pub struct AgentDriver {
    limits: AgentLimits,
    /// Each process's orders, by its ticket, until it's forgotten.
    processes: HashMap<Ticket, mpsc::UnboundedSender<Order>>,
    events: mpsc::Receiver<(Ticket, AgentEvent)>,
    /// Kept so `events` never ends, and given to each process's task.
    sender: mpsc::Sender<(Ticket, AgentEvent)>,
}

/// How many events may wait between the processes' tasks and
/// [`AgentDriver::next`]. Each task keeps more of its own.
const EVENTS: usize = 256;

/// How many things a process may have waiting to be handed on before
/// reading it pauses, however small they are: a flood of empty lines is
/// no bytes, but each is a frame.
const WAITING: usize = 4096;

/// How many times [`AgentLimits::unsent`] may wait unsent, moving or not,
/// before writing fails.
const UNSENT_CEILING: usize = 4;

/// How much is read from a pipe at once.
const READ: usize = 64 << 10;

/// Writes up to this size are joined to the one before, so a burst of
/// small messages takes few writes.
const JOIN: usize = 64 << 10;

/// What a process's `PATH` is when none of its directories is absolute:
/// where programs are installed on any Linux. An empty `PATH` would mean
/// the current folder.
const SAFE_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

impl AgentDriver {
    /// A driver with these limits.
    #[must_use]
    pub fn new(limits: AgentLimits) -> Self {
        let (sender, events) = mpsc::channel(EVENTS);
        Self {
            limits,
            processes: HashMap::new(),
            events,
            sender,
        }
    }

    /// Does one directive. Never blocks: starting, writing and signalling
    /// all return at once, and what they lead to is reported by
    /// [`AgentDriver::next`].
    ///
    /// A directive for a process that's been forgotten, or never started,
    /// does nothing.
    ///
    /// # Panics
    ///
    /// If it starts a process outside a tokio runtime, which must have
    /// its I/O and time drivers enabled.
    pub fn apply(&mut self, directive: AgentDirective) {
        let (process, order) = match directive {
            AgentDirective::Start {
                process,
                command,
                hold,
            } => {
                debug_assert!(
                    !self.processes.contains_key(&process),
                    "a ticket was used twice"
                );
                if self.processes.contains_key(&process) {
                    return;
                }
                let (orders, inbox) = mpsc::unbounded_channel();
                self.processes.insert(process, orders);
                tokio::spawn(run(
                    process,
                    command,
                    hold,
                    self.limits.clone(),
                    inbox,
                    self.sender.clone(),
                ));
                return;
            }
            AgentDirective::Write { process, bytes } => (process, Order::Write(bytes)),
            AgentDirective::CloseInput { process } => (process, Order::CloseInput),
            AgentDirective::Signal { process, signal } => (process, Order::Signal(signal)),
            AgentDirective::CheckGroup { process } => (process, Order::CheckGroup),
            AgentDirective::Forget { process } => {
                if let Some(orders) = self.processes.remove(&process) {
                    let _ = orders.send(Order::Forget);
                }
                return;
            }
            _ => return,
        };
        // One that couldn't start keeps its ticket until it's forgotten,
        // so its `StartFailed` is still handed on; orders to it do nothing.
        if let Some(orders) = self.processes.get(&process) {
            let _ = orders.send(order);
        }
    }

    /// The next thing that happened to any process, with the process's
    /// ticket. Pending forever while there are no processes.
    ///
    /// Cancel-safe: an event is never lost by dropping this future, so it
    /// can be a branch of `select!`.
    pub async fn next(&mut self) -> (Ticket, AgentEvent) {
        loop {
            let Some((process, event)) = self.events.recv().await else {
                // Never: the driver keeps a sender itself.
                return std::future::pending().await;
            };
            // What a forgotten process said last is dropped with it.
            if self.processes.contains_key(&process) {
                return (process, event);
            }
        }
    }
}

/// A directive for one process, sent to its task.
#[derive(Debug)]
enum Order {
    Write(Vec<u8>),
    CloseInput,
    Signal(Signal),
    CheckGroup,
    Forget,
}

/// A process's task: starts it, then does its orders and reports what
/// happens to it until it's forgotten or the driver is dropped.
async fn run(
    ticket: Ticket,
    command: SpawnCommand,
    hold: Option<PathBuf>,
    limits: AgentLimits,
    mut orders: mpsc::UnboundedReceiver<Order>,
    events: mpsc::Sender<(Ticket, AgentEvent)>,
) {
    let held = match hold {
        Some(folder) => hold_folder(&folder).await.map(Some),
        None => Ok(None),
    };
    let mut process = match held.and_then(|held| Process::start(&command, held, limits)) {
        Ok(process) => process,
        Err(error) => {
            let event = AgentEvent::StartFailed {
                error: error.to_string(),
            };
            let _ = events.send((ticket, event)).await;
            return;
        }
    };
    loop {
        let deliver = matches!(process.queue.front(), Some(Queued::Ready(..)));
        let room = process.room();
        if !room && process.drain.is_some() {
            // Waiting for the core isn't waiting for the pipes.
            process.drain = Some(later(process.limits.drain));
        }
        let mut stepped = false;
        tokio::select! {
            order = orders.recv() => match order {
                Some(Order::Forget) => return process.forget().await,
                Some(order) => process.order(order),
                None => return process.abandon().await,
            },
            permit = events.reserve(), if deliver => match permit {
                Ok(permit) => process.deliver(ticket, permit),
                Err(_) => return process.abandon().await,
            },
            parsed = parsed(process.parsing.as_mut()) => process.parsed(parsed),
            writable = writable(process.stdin.as_ref()), if !process.unsent.is_empty() => match writable {
                Ok(()) => process.flush(),
                Err(error) => process.write_failed(&error),
            },
            readable = readable(process.stdout.as_ref()), if room => {
                stepped = match readable {
                    Ok(()) => process.read_output(),
                    Err(_) => process.close_output(),
                };
            },
            readable = readable(process.stderr.as_ref()), if room => {
                stepped = match readable {
                    Ok(()) => process.read_errors(),
                    Err(_) => process.close_errors(),
                };
            },
            status = exit(&process.leader.pidfd), if process.leader.watched() => process.exited(status),
            () = until(process.drain), if room => process.stop_draining(),
            () = until(process.stuck_since.map(|since| since + process.limits.stuck.min(LONGEST))) => process.stuck(),
        }
        if stepped {
            tokio::task::yield_now().await;
        }
    }
}

/// A running process: its pipes, and what's on its way to and from it.
struct Process {
    limits: AgentLimits,
    leader: Leader,
    /// Its input, until it's closed or writing to it fails.
    stdin: Option<pipe::Sender>,
    /// What's waiting to be written to it.
    unsent: Unsent,
    /// Its input is to be closed once `unsent` is written.
    closing: bool,
    /// Its output, until it ends.
    stdout: Option<pipe::Receiver>,
    /// Its error output, until it ends.
    stderr: Option<pipe::Receiver>,
    framer: Framer,
    /// Lines just framed, on their way into `queue`.
    lines: Vec<Line>,
    /// What's waiting to be handed on, in order.
    queue: VecDeque<Queued>,
    /// About how many bytes `queue` holds.
    held: usize,
    /// The one line being parsed elsewhere, if any: the first
    /// [`Queued::Parsing`] in `queue`.
    parsing: Option<JoinHandle<Frame>>,
    /// When to stop waiting for its pipes to close, once it has exited:
    /// pushed back whenever something is read, or reading is paused.
    drain: Option<Instant>,
    /// Since when more than the most unsent input has waited with none of
    /// it taken.
    stuck_since: Option<Instant>,
    buffer: Box<[u8]>,
}

/// Something waiting to be handed on.
enum Queued {
    /// Ready, and how many bytes it counts for.
    Ready(AgentEvent, usize),
    /// A big line, waiting its turn to be parsed elsewhere.
    Big(Line),
    /// The big line being parsed elsewhere, and how long it is.
    Parsing(usize),
}

impl Process {
    /// Starts `command`, holding `held`'s lock until it's reaped.
    fn start(command: &SpawnCommand, held: Option<File>, limits: AgentLimits) -> io::Result<Self> {
        let (mut leader, stdin, stdout, stderr) = spawn(command)?;
        leader.held = held;
        let mut process = Self {
            framer: Framer::new(limits.biggest_line),
            limits,
            leader,
            stdin: Some(stdin),
            unsent: Unsent::default(),
            closing: false,
            stdout: Some(stdout),
            stderr: Some(stderr),
            lines: Vec::new(),
            queue: VecDeque::new(),
            held: 0,
            parsing: None,
            drain: None,
            stuck_since: None,
            buffer: vec![0; READ].into_boxed_slice(),
        };
        let pid = process.leader.pid;
        process.push(AgentEvent::Started { pid }, 0);
        Ok(process)
    }

    /// Whether there's room to read more.
    fn room(&self) -> bool {
        self.held < self.limits.parsing && self.queue.len() < WAITING
    }

    /// Adds `event`, counting for `bytes`, to what's to be handed on.
    fn push(&mut self, event: AgentEvent, bytes: usize) {
        self.held += bytes;
        self.queue.push_back(Queued::Ready(event, bytes));
    }

    /// Hands on the first thing waiting, which is ready.
    fn deliver(&mut self, ticket: Ticket, permit: mpsc::Permit<'_, (Ticket, AgentEvent)>) {
        if let Some(Queued::Ready(event, bytes)) = self.queue.pop_front() {
            self.held -= bytes;
            permit.send((ticket, event));
        }
    }

    fn order(&mut self, order: Order) {
        match order {
            Order::Write(bytes) => self.write(bytes),
            Order::CloseInput => {
                self.closing = true;
                self.flush();
            }
            Order::Signal(signal) => match signal {
                Signal::Term => {
                    let _ = self.leader.signal(libc::SIGTERM);
                }
                Signal::Kill => self.leader.kill(),
                _ => {}
            },
            Order::CheckGroup => {
                let empty = self.leader.group_empty();
                self.push(AgentEvent::Group { empty }, 0);
            }
            Order::Forget => {}
        }
    }

    /// Writes `bytes`, keeping what the pipe won't take yet.
    fn write(&mut self, bytes: Vec<u8>) {
        if self.stdin.is_none() || self.closing || bytes.is_empty() {
            return;
        }
        self.unsent.push(bytes);
        self.flush();
        let ceiling = self.limits.unsent.saturating_mul(UNSENT_CEILING);
        if self.unsent.len > ceiling {
            let error = io::Error::other(format!(
                "it can't take its input fast enough: {} bytes are waiting, more than the most, {ceiling}",
                self.unsent.len
            ));
            self.write_failed(&error);
        }
    }

    /// Writes what's waiting until the pipe is full, then closes the pipe
    /// if that was asked for and all is written.
    fn flush(&mut self) {
        let Some(stdin) = &self.stdin else {
            return;
        };
        let mut moved = false;
        while let Some(bytes) = self.unsent.front() {
            match stdin.try_write(bytes) {
                Ok(written) => {
                    self.unsent.sent(written);
                    moved = true;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return self.write_failed(&error),
            }
        }
        if self.closing && self.unsent.is_empty() {
            self.stdin = None;
        }
        // Past the most, the clock starts, and starts again whenever the
        // pipe takes some: only input that isn't moving counts.
        self.stuck_since = match self.stuck_since {
            _ if self.unsent.len <= self.limits.unsent => None,
            Some(since) if !moved => Some(since),
            _ => Some(Instant::now()),
        };
    }

    /// More than the most unsent input has waited too long with none of
    /// it taken: it isn't reading, so it's taken as gone.
    fn stuck(&mut self) {
        let error = io::Error::other(format!(
            "it isn't reading its input: {} bytes have waited {:?} with none taken",
            self.unsent.len, self.limits.stuck
        ));
        self.write_failed(&error);
    }

    /// Writing failed: what's waiting is thrown away, nothing more is
    /// written, and reading goes on.
    fn write_failed(&mut self, error: &io::Error) {
        self.unsent = Unsent::default();
        self.stuck_since = None;
        self.stdin = None;
        self.push(
            AgentEvent::WriteFailed {
                error: error.to_string(),
            },
            0,
        );
    }

    /// Reads its output, up to a step's worth or until there's no room.
    /// Returns whether it read a whole step, so more may be waiting.
    fn read_output(&mut self) -> bool {
        let step = self.limits.read_step.max(1);
        let mut read = 0;
        while read < step && self.room() {
            let Some(stdout) = &self.stdout else {
                return false;
            };
            let most = READ.min(step - read);
            match stdout.try_read(&mut self.buffer[..most]) {
                Ok(0) => return self.close_output(),
                Ok(n) => {
                    read += n;
                    self.heard();
                    self.framer.push(&self.buffer[..n], &mut self.lines);
                    self.queue_lines();
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return false,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return self.close_output(),
            }
        }
        read >= step
    }

    /// Its output has ended, or isn't waited for any more: the last line,
    /// if unfinished, is handed on, then that it closed. Returns false, as
    /// there's nothing more to read.
    fn close_output(&mut self) -> bool {
        self.framer.finish(&mut self.lines);
        self.queue_lines();
        self.stdout = None;
        self.push(AgentEvent::OutputClosed, 0);
        self.drained();
        false
    }

    /// Reads its error output, up to a step's worth or until there's no
    /// room, and hands it on as one piece. Returns whether it read a whole
    /// step.
    fn read_errors(&mut self) -> bool {
        let Some(stderr) = &self.stderr else {
            return false;
        };
        let step = self.limits.read_step.max(1);
        let mut bytes = Vec::new();
        let mut ended = false;
        while bytes.len() < step && self.held + bytes.len() < self.limits.parsing {
            let most = READ.min(step - bytes.len());
            match stderr.try_read(&mut self.buffer[..most]) {
                Ok(0) => {
                    ended = true;
                    break;
                }
                Ok(n) => bytes.extend_from_slice(&self.buffer[..n]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    ended = true;
                    break;
                }
            }
        }
        let full = bytes.len() >= step;
        if !bytes.is_empty() {
            self.heard();
            let len = bytes.len();
            self.push(AgentEvent::Stderr(bytes), len);
        }
        if ended {
            self.close_errors();
        }
        full
    }

    /// Its error output has ended, or isn't waited for any more. Returns
    /// false, as there's nothing more to read.
    fn close_errors(&mut self) -> bool {
        self.stderr = None;
        self.drained();
        false
    }

    /// Something came through its pipes: once it has exited, they're
    /// waited for a while longer.
    fn heard(&mut self) {
        if self.drain.is_some() {
            self.drain = Some(later(self.limits.drain));
        }
    }

    /// Stops waiting for its pipes once both have closed.
    fn drained(&mut self) {
        if self.stdout.is_none() && self.stderr.is_none() {
            self.drain = None;
        }
    }

    /// It has exited, and been reaped. What's left in its pipes is read,
    /// a step's worth, before that's told: all it wrote itself is there
    /// by now. That's best effort: nothing is read if there's no room, or
    /// if the runtime hasn't yet seen that the pipes can be read. Something
    /// it started may hold its pipes open, so they're waited for only
    /// while something comes through.
    fn exited(&mut self, status: io::Result<Option<WaitIdStatus>>) {
        let Ok(status) = status else {
            // It can't be watched, as the runtime is going: it's left for
            // the leader to kill and reap as it's dropped.
            self.leader.unwatchable = true;
            return;
        };
        self.leader.reaped = true;
        self.leader.held = None;
        self.read_output();
        self.read_errors();
        let exit = match status.map(|status| (status.exit_status(), status.terminating_signal())) {
            Some((Some(code), _)) => Exit::Code(code),
            Some((None, Some(signal))) => Exit::Signal(signal),
            // Something else reaped it, so how it ended isn't known.
            Some((None, None)) | None => Exit::Code(-1),
        };
        self.push(AgentEvent::Exited(exit), 0);
        if self.stdout.is_some() || self.stderr.is_some() {
            self.drain = Some(later(self.limits.drain));
        }
    }

    /// Nothing has come through its pipes for long enough since it exited:
    /// they're held open by something it started, and aren't waited for any
    /// more.
    fn stop_draining(&mut self) {
        self.drain = None;
        if self.stdout.is_some() {
            self.close_output();
        }
        self.close_errors();
    }

    /// Moves the lines just framed into the queue: small ones parsed here
    /// and now, big ones to wait their turn to be parsed elsewhere.
    fn queue_lines(&mut self) {
        for line in self.lines.drain(..) {
            let len = line_len(&line);
            if len >= self.limits.parse_elsewhere {
                self.held += len;
                self.queue.push_back(Queued::Big(line));
            } else {
                self.held += len;
                self.queue
                    .push_back(Queued::Ready(AgentEvent::Frame(acp::parse(line)), len));
            }
        }
        self.parse_next();
    }

    /// Starts parsing the first big line waiting, unless one is being
    /// parsed already.
    fn parse_next(&mut self) {
        if self.parsing.is_some() {
            return;
        }
        let Some(queued) = (self.queue.iter_mut()).find(|queued| matches!(queued, Queued::Big(_)))
        else {
            return;
        };
        let Queued::Big(line) = std::mem::replace(queued, Queued::Parsing(0)) else {
            unreachable!("found as a big line");
        };
        let len = line_len(&line);
        *queued = Queued::Parsing(len);
        self.parsing = Some(tokio::task::spawn_blocking(move || acp::parse(line)));
    }

    /// The big line being parsed elsewhere has been: it takes its place.
    fn parsed(&mut self, parsed: Result<Frame, tokio::task::JoinError>) {
        self.parsing = None;
        let Some(queued) =
            (self.queue.iter_mut()).find(|queued| matches!(queued, Queued::Parsing(_)))
        else {
            return;
        };
        let Queued::Parsing(len) = *queued else {
            unreachable!("found as being parsed");
        };
        // Parsing can't panic; if it did, the line is junk.
        let frame = parsed.unwrap_or(Frame::Junk { len });
        *queued = Queued::Ready(AgentEvent::Frame(frame), len);
        self.parse_next();
    }

    /// Forgotten: nothing more is read or reported. If it hasn't exited
    /// yet, it's killed, with its group, and reaped.
    async fn forget(mut self) {
        self.stdin = None;
        self.stdout = None;
        self.stderr = None;
        self.queue.clear();
        if self.leader.watched() {
            self.leader.kill();
            // If it can't be waited for, the leader reaps it as it's
            // dropped.
            if exit(&self.leader.pidfd).await.is_ok() {
                self.leader.reaped = true;
                self.leader.held = None;
            }
        }
    }

    /// The driver has gone: its group is killed, and it's reaped.
    async fn abandon(self) {
        self.leader.kill();
        self.forget().await;
    }
}

/// How many bytes a line counts for.
fn line_len(line: &Line) -> usize {
    match line {
        Line::Whole(bytes) => bytes.len(),
        Line::TooBig { head, .. } => head.len(),
        _ => 0,
    }
}

/// What's waiting to be written to a process, in pieces as it was given,
/// so writing part of it copies nothing.
#[derive(Default)]
struct Unsent {
    pieces: VecDeque<Vec<u8>>,
    /// How much of the first piece has been written.
    written: usize,
    /// How many bytes are waiting.
    len: usize,
}

impl Unsent {
    fn push(&mut self, bytes: Vec<u8>) {
        self.len += bytes.len();
        match self.pieces.back_mut() {
            Some(last) if last.len() + bytes.len() <= JOIN => last.extend_from_slice(&bytes),
            _ => self.pieces.push_back(bytes),
        }
    }

    /// What's to be written next.
    fn front(&self) -> Option<&[u8]> {
        self.pieces.front().map(|first| &first[self.written..])
    }

    /// `written` more bytes have been written.
    fn sent(&mut self, written: usize) {
        self.len -= written;
        self.written += written;
        if self
            .pieces
            .front()
            .is_some_and(|first| self.written == first.len())
        {
            self.pieces.pop_front();
            self.written = 0;
        }
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// A process this driver started, leading a process group of its own,
/// known by its pidfd.
struct Leader {
    pid: u32,
    pidfd: AsyncFd<OwnedFd>,
    /// Whether it has been reaped. Its pidfd still reaches its group.
    reaped: bool,
    /// Whether waiting for its exit failed, as when the runtime is going.
    unwatchable: bool,
    /// The shared lock on the install it runs from, if it's CAF's install
    /// of an agent: let go of once it's reaped, and not before, so the
    /// install isn't removed while it runs.
    held: Option<File>,
}

/// `pidfd_send_signal`'s flag to signal the pidfd's process group, from
/// `linux/pidfd.h`, which the libc crate doesn't have.
const PIDFD_SIGNAL_PROCESS_GROUP: libc::c_uint = 4;

impl Leader {
    /// Whether its exit is still to be waited for.
    fn watched(&self) -> bool {
        !self.reaped && !self.unwatchable
    }

    /// Sends signal number `signal` to its group. With nothing left in the
    /// group, or a kernel too old to signal a group through a pidfd
    /// (before Linux 6.9), it alone is sent it.
    fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        let pidfd = self.pidfd.get_ref().as_fd();
        match pidfd_send_signal(pidfd, signal, PIDFD_SIGNAL_PROCESS_GROUP) {
            Err(error) if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ESRCH)) => {
                pidfd_send_signal(pidfd, signal, 0)
            }
            sent => sent,
        }
    }

    /// Sends SIGKILL to its group, and to it too, in case it left the
    /// group and left something in it.
    fn kill(&self) {
        let _ = self.signal(libc::SIGKILL);
        let _ = pidfd_send_signal(self.pidfd.get_ref().as_fd(), libc::SIGKILL, 0);
    }

    /// Whether everything in its group has gone, it included. One not yet
    /// reaped counts, even once it has exited.
    fn group_empty(&self) -> bool {
        matches!(self.signal(0), Err(error) if error.raw_os_error() == Some(libc::ESRCH))
    }
}

impl Drop for Leader {
    /// A task dropped with its process not reaped, as when the runtime
    /// shuts down: the group is killed, and a thread of its own reaps it,
    /// so no zombie is left and the runtime isn't held up.
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        self.kill();
        let reap = |pidfd: BorrowedFd<'_>| {
            let _ = rustix::process::waitid(WaitId::PidFd(pidfd), WaitIdOptions::EXITED);
        };
        match self.pidfd.get_ref().try_clone() {
            Ok(pidfd) => {
                // The lock goes with the thread, and is let go of once it
                // has reaped the process. If the thread can't be started,
                // it's let go of as this is dropped, after the reaping
                // below.
                let held = self.held.take();
                let spawned = std::thread::Builder::new()
                    .name("caf-reap".to_owned())
                    .spawn(move || {
                        reap(pidfd.as_fd());
                        drop(held);
                    });
                if spawned.is_err() {
                    reap(self.pidfd.get_ref().as_fd());
                }
            }
            Err(_) => reap(self.pidfd.get_ref().as_fd()),
        }
    }
}

/// The `pidfd_send_signal` system call, with `flags`, which rustix's
/// doesn't take, and signal 0, which its `Signal` can't be.
#[allow(unsafe_code)]
fn pidfd_send_signal(
    pidfd: BorrowedFd<'_>,
    signal: libc::c_int,
    flags: libc::c_uint,
) -> io::Result<()> {
    // SAFETY: the system call takes an fd, borrowed for the call, a signal
    // number, a pointer to a `siginfo_t`, which may be null, and flags. It
    // reads no memory of ours, as the pointer is null, and writes none.
    let sent = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            flags,
        )
    };
    if sent == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Starts `command` in a process group of its own, with its standard
/// streams piped. Returns it, by its pidfd, and its pipes.
fn spawn(
    command: &SpawnCommand,
) -> io::Result<(Leader, pipe::Sender, pipe::Receiver, pipe::Receiver)> {
    if !command.program.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} isn't an absolute path", command.program.display()),
        ));
    }
    let mut child = command_for(command, std::env::var_os("PATH")).spawn()?;
    // Taken first, so failing below closes them, which ends most programs.
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        unreachable!("all three are piped");
    };
    let pid = child.id();
    // Opened before anything waits for it: until then it can't be reaped,
    // so its number still names it.
    let pidfd = match rustix::process::pidfd_open(Pid::from_child(&child), PidfdFlags::empty()) {
        Ok(pidfd) => pidfd,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
    };
    // From here on it's reaped through its pidfd, and the leader kills
    // and reaps it if anything below fails.
    drop(child);
    // SAFETY: an `OwnedFd` stays open, and is the same fd, for as long as
    // it's owned.
    #[allow(unsafe_code)]
    let registered = unsafe { AsyncFd::register_with_interest(pidfd, Interest::READABLE) };
    let pidfd = match registered {
        Ok(pidfd) => pidfd,
        Err(error) => {
            let (pidfd, error) = error.into_parts();
            pidfd_send_signal(pidfd.as_fd(), libc::SIGKILL, PIDFD_SIGNAL_PROCESS_GROUP).ok();
            pidfd_send_signal(pidfd.as_fd(), libc::SIGKILL, 0).ok();
            let _ = rustix::process::waitid(WaitId::PidFd(pidfd.as_fd()), WaitIdOptions::EXITED);
            return Err(error);
        }
    };
    let leader = Leader {
        pid,
        pidfd,
        reaped: false,
        unwatchable: false,
        held: None,
    };
    let stdin = pipe::Sender::from_owned_fd(OwnedFd::from(stdin))?;
    let stdout = pipe::Receiver::from_owned_fd(OwnedFd::from(stdout))?;
    let stderr = pipe::Receiver::from_owned_fd(OwnedFd::from(stderr))?;
    Ok((leader, stdin, stdout, stderr))
}

/// How long starting a process waits for a shared lock on its install,
/// while a removal looks whether anything runs from it.
const HOLD_WAIT: Duration = Duration::from_secs(2);

/// Takes a shared lock on `folder`, an install, for a process about to run
/// from it. A removal holds it exclusively only for a moment, while it
/// moves the folder away, so that's waited for, briefly.
async fn hold_folder(folder: &Path) -> io::Result<File> {
    let give_up = Instant::now() + HOLD_WAIT;
    loop {
        let file = File::open(folder).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("its install, {}, can't be held: {error}", folder.display()),
            )
        })?;
        if let Some(held) = lock_file(file, Lock::SharedNow)? {
            return Ok(held);
        }
        if Instant::now() >= give_up {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("its install, {}, is being removed", folder.display()),
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What runs `command`, with `path` as the `PATH` it inherits. Its
/// environment changes go on top of the inherited one, then `PATH` keeps
/// only its absolute directories, so a project folder can't put programs
/// on it. No `pre_exec` hook, so it can be started with `posix_spawn`.
pub(crate) fn command_for(command: &SpawnCommand, mut path: Option<OsString>) -> Command {
    let mut process = Command::new(&command.program);
    process.args(&command.args);
    for change in &command.env {
        match change {
            EnvChange::Set(name, value) if name == "PATH" => path = Some(value.clone()),
            EnvChange::Remove(name) if name == "PATH" => path = None,
            EnvChange::Set(name, value) => {
                process.env(name, value);
            }
            EnvChange::Remove(name) => {
                process.env_remove(name);
            }
            _ => {}
        }
    }
    match path {
        Some(path) => process.env("PATH", absolute_only(&path)),
        None => process.env_remove("PATH"),
    };
    if let Some(cwd) = &command.cwd {
        process.current_dir(cwd);
    }
    process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    process
}

/// The absolute directories of `path`, a `PATH`, or [`SAFE_PATH`] if
/// there are none.
pub(crate) fn absolute_only(path: &OsStr) -> OsString {
    let kept: Vec<&[u8]> = (path.as_bytes().split(|&byte| byte == b':'))
        .filter(|dir| dir.starts_with(b"/"))
        .collect();
    if kept.is_empty() {
        return OsString::from(SAFE_PATH);
    }
    OsString::from_vec(kept.join(&b':'))
}

/// Resolves once the process whose pidfd is `pidfd` has exited, reaping
/// it, with how it exited: `None` if something else reaped it first. An
/// error means it can't be waited for, and wasn't reaped. Cancel-safe:
/// it's reaped only as this resolves.
async fn exit(pidfd: &AsyncFd<OwnedFd>) -> io::Result<Option<WaitIdStatus>> {
    loop {
        let mut ready = pidfd.readable().await?;
        let waited = rustix::process::waitid(
            WaitId::PidFd(pidfd.get_ref().as_fd()),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
        );
        match waited {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => ready.clear_ready(),
            Err(rustix::io::Errno::CHILD) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
}

/// Resolves when `pipe` may be read, or never if there's none.
async fn readable(pipe: Option<&pipe::Receiver>) -> io::Result<()> {
    match pipe {
        Some(pipe) => pipe.readable().await,
        None => std::future::pending().await,
    }
}

/// Resolves when `pipe` may be written, or never if there's none.
async fn writable(pipe: Option<&pipe::Sender>) -> io::Result<()> {
    match pipe {
        Some(pipe) => pipe.writable().await,
        None => std::future::pending().await,
    }
}

/// Resolves when the line being parsed elsewhere is, or never if there's
/// none.
async fn parsed(parsing: Option<&mut JoinHandle<Frame>>) -> Result<Frame, tokio::task::JoinError> {
    match parsing {
        Some(parsing) => parsing.await,
        None => std::future::pending().await,
    }
}

/// The longest any limit waits: longer is as good as never, and an
/// instant past the end of the clock can't be made.
const LONGEST: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// The instant `by` from now.
fn later(by: Duration) -> Instant {
    Instant::now() + by.min(LONGEST)
}

/// Resolves at `at`, or never.
async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_keeps_only_absolute_directories() {
        assert_eq!(
            absolute_only(OsStr::new("/usr/bin:bin:.::/bin:./x")),
            "/usr/bin:/bin"
        );
        assert_eq!(absolute_only(OsStr::new("")), SAFE_PATH);
        assert_eq!(absolute_only(OsStr::new("node_modules/.bin:.")), SAFE_PATH);
    }

    fn env_of(command: &Command) -> Vec<(OsString, Option<OsString>)> {
        let mut env: Vec<_> = (command.get_envs())
            .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
            .collect();
        env.sort();
        env
    }

    #[test]
    fn environment_changes_go_on_top_and_path_is_filtered_after() {
        let mut spawn = SpawnCommand::new("/bin/true");
        spawn.env = vec![
            EnvChange::Set("A".into(), "1".into()),
            EnvChange::Remove("B".into()),
            EnvChange::Set("PATH".into(), "rel:/opt/bin".into()),
        ];
        let command = command_for(&spawn, Some("/usr/bin".into()));
        assert_eq!(
            env_of(&command),
            [
                ("A".into(), Some("1".into())),
                ("B".into(), None),
                ("PATH".into(), Some("/opt/bin".into())),
            ]
        );
        assert_eq!(
            env_of(&command_for(
                &SpawnCommand::new("/bin/true"),
                Some("./bin:/usr/bin".into())
            )),
            [("PATH".into(), Some("/usr/bin".into()))]
        );
        spawn.env = vec![EnvChange::Remove("PATH".into())];
        assert_eq!(
            env_of(&command_for(&spawn, Some("/usr/bin".into()))),
            [("PATH".into(), None)]
        );
    }
}
