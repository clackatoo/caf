//! The wiring: one task owning the core and its drivers, and handles for
//! the app.
//!
//! # How it works
//!
//! [`Caf::start`] spawns one task that owns the [`Core`], the
//! [`AgentDriver`], the [`TimerDriver`] and the [`Policy`]. Handles talk to
//! it through a queue, and it waits on four things at once: a handle's
//! command, an adapter's event, the timer, and a policy's answer. Each
//! goes into the core with the time from [`TimerDriver::now`], and every
//! directive that comes out goes to its driver:
//!
//! | Directive | Where it goes |
//! |---|---|
//! | `Agent` | [`AgentDriver::apply`] |
//! | `Timer(WakeAt)` | [`TimerDriver::wake_at`] |
//! | `Policy(Decide)` | [`Policy::decide`], whose future runs as a task of its own; its answer comes back as [`Event::Policy`] |
//! | `Policy(Withdrawn)` | Stops that decision's task. An answer it gave already is still handed in, and the core ignores it |
//! | `Viewer(Changed)` | Wakes the chat's watches, which read with [`Core::read`] |
//! | `Report` | The [`log`] facade, target `caf` |
//!
//! A [`Steps`] given to [`Caf::start_watching`] sees each event once the
//! core has it, with its directives, before they go to their drivers.
//!
//! Watches pull rather than being pushed to: a watch asks for what
//! changed since it last read, so the only changes held for it are the
//! core's own list, which is bounded
//! ([`Limits::kept_changes`](caf_core::host::Limits::kept_changes)). A
//! watch further behind than that gets the whole chat again.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use caf_core::acp::{Frame, Message};
use caf_core::agent::{AgentSpec, EnvChange, McpServer};
use caf_core::chat::{ChatState, UserMessage};
use caf_core::directive::{Level, PolicyDirective, TimerDirective, ViewerDirective};
use caf_core::event::{AgentEvent, HostCommand, PolicyAnswer};
use caf_core::ids::{AgentName, ChangeNo};
use caf_core::question::Answer;
use caf_core::viewer::ChatSummary;
use caf_core::{
    AppItem, Chat, ChatId, Core, Directive, Event, Host, NoAppItems, Now, Numbered, OpenChat,
    QuestionId, Read, Refused, Report, Ticket, TurnId,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle, JoinSet};

use crate::agent::{AgentDriver, AgentLimits};
use crate::policy::{AlwaysAsk, Policy};
use crate::steps::{Step, Steps};
use crate::timer::TimerDriver;

/// What to start CAF with.
#[non_exhaustive]
pub struct Config {
    /// The host's description of itself.
    pub host: Host,
    /// The agents it offers.
    pub agents: Vec<AgentSpec>,
    /// Who answers permission requests. [`AlwaysAsk`] by default.
    pub policy: Box<dyn Policy>,
    /// Limits for the agent driver.
    pub agent_limits: AgentLimits,
    /// A file to record the run in, so it can be replayed into a fresh
    /// [`Core`] as a test. `None` by default. The file must not exist yet.
    ///
    /// It's JSON lines, of version 2. The first holds the host and the
    /// agents: `{"caf_recording": 2, "host": …, "agents": […]}`; a reader
    /// checks the version before reading the rest. Each line
    /// after it is one event the core was given, with its time, in order:
    /// `{"now": …, "event": …}`. Everything is in CAF's one shape (see
    /// caf-core's docs), such as `{"type": "Woken"}`, with each `type`
    /// written before its `value`. Calls such as [`ChatHandle::send`] are
    /// written as the [`Event::Host`] they stand for, with the id the core
    /// gave out, and only if the core took them. Read each line's event
    /// back as an [`Event`]. A recording cut short ends with
    /// `{"stopped": …}`, saying why: a replay stops there.
    ///
    /// **What's hidden.** None of it changes what the core does:
    /// - Claude's sign-in status (`_auth/status_update`) holds the
    ///   account's email: its params are written as `null`.
    /// - Each agent's arguments are left out, and its environment
    ///   variables are written with their names but not their values.
    /// - Each MCP server's environment variables and HTTP headers are
    ///   written with their names but not their values.
    ///
    /// **What's kept**, as it came: the host, each agent's name, program,
    /// folder and profile; what the user sent; everything the agent said;
    /// its error output, exactly, as text where a piece of it is UTF-8 and
    /// as bytes where it isn't, such as one that ends partway through a
    /// character; folders; and each MCP server's name, program, arguments
    /// and URL. So don't put secrets in MCP arguments or URLs, and treat a
    /// recording as private: read it through before sharing it. Names of
    /// variables and arguments are written as arrays of bytes, as Rust
    /// writes them.
    ///
    /// The file is made new, readable only by its owner: if anything is
    /// there already, even a link, CAF won't record. It's written on a
    /// thread of its own, so the disk never holds CAF up, and it's complete
    /// once [`Caf::shutdown`] returns. If it can't be made, or a write
    /// fails, CAF says so in a warning and stops recording. If more than
    /// 256 MiB is waiting to be written because the disk has fallen
    /// behind, it stops too, and once what came before is written, ends
    /// the file with `{"stopped": …}`. Either way, what's in the file still
    /// replays, as far as it goes.
    pub record: Option<PathBuf>,
}

impl Config {
    /// The defaults for `host`, offering these agents. Set any field to
    /// change it.
    #[must_use]
    pub fn new(host: Host, agents: Vec<AgentSpec>) -> Self {
        Self {
            host,
            agents,
            policy: Box::new(AlwaysAsk),
            agent_limits: AgentLimits::default(),
            record: None,
        }
    }
}

/// CAF, running on tokio. Cheap to clone: every clone talks to the same
/// task.
///
/// Once every clone and every [`ChatHandle`] is dropped, CAF shuts down by
/// itself, as if [`Caf::shutdown`] had been called. A [`Watch`] doesn't
/// keep it running.
pub struct Caf<A: AppItem = NoAppItems> {
    commands: mpsc::Sender<Command>,
    /// Watches' reads, apart from the commands, so a watch never waits
    /// for room among them: see [`Reading`].
    reads: mpsc::UnboundedSender<Reading<A>>,
    shared: Common<A>,
}

impl<A: AppItem> Clone for Caf<A> {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            reads: self.reads.clone(),
            shared: self.shared.clone(),
        }
    }
}

/// What every handle shares, strong or weak.
struct Common<A: AppItem> {
    last: LastWords<A>,
    /// Closed once the task has ended and `last` is filled in.
    ended: watch::Receiver<()>,
    /// The agents' names, in order.
    agents: Arc<[AgentName]>,
    /// The open chats, in the order they were opened.
    chats: watch::Receiver<Vec<ChatSummary>>,
    /// See [`Caf::run`].
    run: Arc<str>,
}

impl<A: AppItem> Clone for Common<A> {
    fn clone(&self) -> Self {
        Self {
            last: Arc::clone(&self.last),
            ended: self.ended.clone(),
            agents: Arc::clone(&self.agents),
            chats: self.chats.clone(),
            run: Arc::clone(&self.run),
        }
    }
}

/// A handle on CAF that doesn't keep it running, as a [`Watch`] holds.
pub(crate) struct WeakCaf<A: AppItem> {
    commands: mpsc::WeakSender<Command>,
    reads: mpsc::WeakUnboundedSender<Reading<A>>,
    shared: Common<A>,
}

impl<A: AppItem> WeakCaf<A> {
    /// A handle that keeps CAF running while it's held, unless CAF has
    /// shut down.
    pub(crate) fn upgrade(&self) -> Option<Caf<A>> {
        Some(Caf {
            commands: self.commands.upgrade()?,
            reads: self.reads.upgrade()?,
            shared: self.shared.clone(),
        })
    }

    /// See [`Caf::agents`].
    pub(crate) fn agents(&self) -> &[AgentName] {
        &self.shared.agents
    }

    /// See [`Caf::run`].
    pub(crate) fn run(&self) -> &str {
        &self.shared.run
    }

    /// See [`Caf::watch_chats`].
    pub(crate) fn watch_chats(&self) -> ChatsWatch {
        ChatsWatch {
            list: self.shared.chats.clone(),
            first: true,
        }
    }

    /// A watch on `chat`, from `since` if the watcher has that already.
    pub(crate) fn watch(&self, chat: ChatId, since: Option<ChangeNo>) -> Watch<A> {
        let mut last = (self.shared.last.lock()).unwrap_or_else(PoisonError::into_inner);
        // Once the task has ended, the last words are written: there's
        // nothing to keep for a new watch.
        let counted = !last.ended;
        if counted {
            *last.watched.entry(chat).or_default() += 1;
        }
        drop(last);
        Watch {
            reads: self.reads.clone(),
            last: Arc::clone(&self.shared.last),
            caf_ended: self.shared.ended.clone(),
            chat,
            since,
            wake: None,
            ended: false,
            counted,
        }
    }

    /// Resolves once CAF's task has ended.
    pub(crate) async fn ended(&self) {
        let mut ended = self.shared.ended.clone();
        while ended.changed().await.is_ok() {}
    }
}

/// The task CAF runs in. Await it after [`Caf::shutdown`] to know every
/// adapter has stopped.
///
/// It resolves when the task ends: once shut down, every adapter has
/// exited and been reaped, what it started has been sent SIGKILL, and a
/// recording, if asked for, is complete.
///
/// # Panics
///
/// Awaiting it panics if the task did, with the task's panic.
pub struct Running {
    task: JoinHandle<()>,
}

impl std::future::Future for Running {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        match std::pin::Pin::new(&mut self.get_mut().task).poll(cx) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(()),
            std::task::Poll::Ready(Err(error)) => match error.try_into_panic() {
                Ok(panic) => std::panic::resume_unwind(panic),
                // Cancelled: the runtime is shutting down.
                Err(_) => std::task::Poll::Ready(()),
            },
        }
    }
}

/// Why something asked of CAF wasn't done.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The core refused it, and changed nothing.
    Refused(Refused),
    /// CAF has shut down, or is shutting down, so nothing can be asked of
    /// it.
    Gone,
}

impl From<Refused> for Error {
    fn from(refused: Refused) -> Self {
        Self::Refused(refused)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refused) => write!(f, "CAF refused: {refused:?}"),
            Self::Gone => f.write_str("CAF has shut down"),
        }
    }
}

impl std::error::Error for Error {}

/// How many commands may wait for the task before a handle waits to
/// send one.
const COMMANDS: usize = 64;

impl<A: AppItem> Caf<A> {
    /// Starts CAF in a task of its own, on the current tokio runtime.
    ///
    /// Reports go to the [`log`] facade, with target `caf`, at the level
    /// the core gave them.
    ///
    /// # Panics
    ///
    /// If `config.agent_limits.biggest_line` isn't the host's
    /// `limits.biggest_message`: the driver and the core must agree on what
    /// a too-big line is. Also if called outside a tokio runtime, which
    /// must have its I/O and time drivers enabled.
    #[must_use]
    pub fn start(config: Config) -> (Self, Running) {
        Self::start_with(config, None)
    }

    /// Starts CAF as [`Caf::start`] does, with `steps` seeing every step
    /// it takes: for debuggers and tests.
    ///
    /// Steps hold secrets, such as the values of MCP servers' variables
    /// and headers: treat them as private, and never write them out as
    /// they are. See [`Steps`](crate::Steps).
    ///
    /// # Panics
    ///
    /// As [`Caf::start`].
    #[must_use]
    pub fn start_watching(config: Config, steps: impl Steps<A>) -> (Self, Running) {
        Self::start_with(config, Some(Box::new(steps)))
    }

    /// Starts CAF, with `steps` seeing every step if there are any.
    fn start_with(config: Config, steps: Option<Box<dyn Steps<A>>>) -> (Self, Running) {
        assert_eq!(
            config.agent_limits.biggest_line, config.host.limits.biggest_message,
            "the agent driver's biggest line must be the host's biggest message"
        );
        let recorder = (config.record.as_ref())
            .and_then(|path| Recorder::start(path, &config.host, &config.agents));
        let (commands, inbox) = mpsc::channel(COMMANDS);
        let (reads, reading) = mpsc::unbounded_channel();
        let last = LastWords::default();
        let (ending, ended) = watch::channel(());
        let agents = (config.agents.iter())
            .map(|agent| agent.name.clone())
            .collect();
        let (open, chats) = watch::channel(Vec::new());
        let task = Task {
            core: Core::new(config.host, config.agents),
            agents: AgentDriver::new(config.agent_limits),
            timers: TimerDriver::new(),
            policy: config.policy,
            deciding: JoinSet::new(),
            tickets: HashMap::new(),
            decisions: HashMap::new(),
            feeds: HashMap::new(),
            last: Arc::clone(&last),
            ending: Some(ending),
            recorder,
            steps,
            open,
            inbox,
            listening: true,
            reads: reading,
            reading: true,
            shutting_down: false,
            waiting: Vec::new(),
            out: Vec::new(),
        };
        let task = tokio::spawn(task.run());
        let caf = Self {
            commands,
            reads,
            shared: Common {
                last,
                ended,
                agents,
                chats,
                run: new_run().into(),
            },
        };
        (caf, Running { task })
    }

    /// The names of the agents CAF was started with, in order.
    #[must_use]
    pub fn agents(&self) -> &[AgentName] {
        &self.shared.agents
    }

    /// This run of CAF: different every time CAF starts, so a viewer can
    /// tell a restarted CAF from the one it was watching. A change's
    /// number means something only within one run.
    #[must_use]
    pub fn run(&self) -> &str {
        &self.shared.run
    }

    /// Follows which chats are open, with each one's agent and folder.
    #[must_use]
    pub fn watch_chats(&self) -> ChatsWatch {
        self.downgrade().watch_chats()
    }

    /// Answers a question, whichever chat it's in. See
    /// [`ChatHandle::answer`].
    pub(crate) async fn answer(&self, question: QuestionId, answer: Answer) -> Result<(), Error> {
        let answering = |reply| Command::Answer {
            question,
            answer,
            reply,
        };
        self.ask(answering).await
    }

    /// A handle that doesn't keep CAF running.
    pub(crate) fn downgrade(&self) -> WeakCaf<A> {
        WeakCaf {
            commands: self.commands.downgrade(),
            reads: self.reads.downgrade(),
            shared: self.shared.clone(),
        }
    }

    /// Opens a chat. It's ready to take messages at once: they wait until
    /// its session is open.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn open_chat(&self, open: OpenChat) -> Result<ChatHandle<A>, Error> {
        let chat = self.ask(|reply| Command::Open { open, reply }).await?;
        Ok(self.chat(chat))
    }

    /// A handle on a chat that's already open.
    #[must_use]
    pub fn chat(&self, chat: ChatId) -> ChatHandle<A> {
        ChatHandle {
            id: chat,
            caf: self.clone(),
        }
    }

    /// Stops everything: every session closed, every adapter stopped.
    ///
    /// Returns once every adapter has exited and been reaped, and what it
    /// started has been sent SIGKILL; or at once if CAF has already shut
    /// down. From the moment it's called, anything else asked of CAF is
    /// [`Error::Gone`], but watches go on getting what changes.
    pub async fn shutdown(&self) {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::Shutdown { reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }

    /// Sends the task a command that takes a reply, and waits for it.
    async fn ask<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, Refused>>) -> Command,
    ) -> Result<T, Error> {
        let (reply, answer) = oneshot::channel();
        (self.commands.send(command(reply)).await).map_err(|_| Error::Gone)?;
        // The task drops the reply when it's shutting down.
        Ok(answer.await.map_err(|_| Error::Gone)??)
    }
}

/// A handle on one chat.
pub struct ChatHandle<A: AppItem = NoAppItems> {
    /// The chat's id.
    pub id: ChatId,
    caf: Caf<A>,
}

impl<A: AppItem> ChatHandle<A> {
    /// Sends a message. See [`caf_core::Core::send`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn send(&self, message: UserMessage) -> Result<TurnId, Error> {
        let chat = self.id;
        (self.caf)
            .ask(|reply| Command::Send {
                chat,
                message,
                reply,
            })
            .await
    }

    /// Stops the running turn. See [`caf_core::Core::stop`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn stop(&self) -> Result<(), Error> {
        let chat = self.id;
        self.caf.ask(|reply| Command::Stop { chat, reply }).await
    }

    /// Answers a question. See [`caf_core::Core::answer`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn answer(&self, question: QuestionId, answer: Answer) -> Result<(), Error> {
        self.caf.answer(question, answer).await
    }

    /// Closes the chat for good.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn close(self) -> Result<(), Error> {
        let chat = self.id;
        self.caf.ask(|reply| Command::Close { chat, reply }).await
    }

    /// Watches the chat: first the whole chat, then its changes as they
    /// happen. A watcher that falls behind gets the whole chat again
    /// rather than holding changes without end.
    #[must_use]
    pub fn watch(&self) -> Watch<A> {
        self.caf.downgrade().watch(self.id, None)
    }

    /// Watches the chat from a change the watcher already has, such as a
    /// viewer reconnecting with its copy: the changes after `since` come
    /// first, or the whole chat if CAF no longer has them all.
    #[must_use]
    pub fn watch_from(&self, since: ChangeNo) -> Watch<A> {
        self.caf.downgrade().watch(self.id, Some(since))
    }
}

/// A watch on a chat.
///
/// It reads what changed only when asked, so nothing piles up for a
/// watch that isn't read: how far behind it may fall before it gets the
/// whole chat again is
/// [`Limits::kept_changes`](caf_core::host::Limits::kept_changes).
///
/// It doesn't keep CAF running: once every [`Caf`] and [`ChatHandle`] is
/// dropped, CAF shuts down, and the watch ends.
pub struct Watch<A: AppItem = NoAppItems> {
    reads: mpsc::WeakUnboundedSender<Reading<A>>,
    last: LastWords<A>,
    /// Closed once CAF's task has ended.
    caf_ended: watch::Receiver<()>,
    chat: ChatId,
    /// The last change it has had.
    since: Option<ChangeNo>,
    /// How it hears of changes, once it has read the open chat.
    wake: Option<watch::Receiver<()>>,
    /// It has had its last update.
    ended: bool,
    /// It's counted in [`Last::watched`].
    counted: bool,
}

impl<A: AppItem> Drop for Watch<A> {
    fn drop(&mut self) {
        if !self.counted {
            return;
        }
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = last.watched.get_mut(&self.chat) {
            *count -= 1;
            if *count == 0 {
                last.watched.remove(&self.chat);
            }
        }
    }
}

/// Follows which chats are open: opened by the host, by any viewer, or
/// closed. Like [`Watch`], it doesn't keep CAF running.
pub struct ChatsWatch {
    list: watch::Receiver<Vec<ChatSummary>>,
    /// It hasn't given the list yet.
    first: bool,
}

impl ChatsWatch {
    /// The open chats, in the order they were opened: at once the first
    /// time, then each time the list changes. `None` once CAF has shut
    /// down.
    ///
    /// If the list changes more than once between calls, only the latest
    /// is given.
    ///
    /// Cancel-safe: dropped before it resolves, it loses nothing.
    pub async fn next(&mut self) -> Option<Vec<ChatSummary>> {
        if self.first {
            // The task drops the list's sender as it ends.
            self.list.has_changed().ok()?;
            self.first = false;
        } else {
            self.list.changed().await.ok()?;
        }
        Some(self.list.borrow_and_update().clone())
    }
}

/// What a watch gets.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Update<A: AppItem = NoAppItems> {
    /// The whole chat, as it is: at the start, and after falling behind.
    Picture(Chat<A>),
    /// What changed since the last update, oldest first.
    Changes(Vec<Numbered<A>>),
}

impl<A: AppItem> Watch<A> {
    /// The next update, or `None` once the chat is closed or CAF has shut
    /// down.
    ///
    /// When CAF shuts down, a watch that hasn't seen how it left the chat
    /// gets that as one last [`Update::Picture`], even if it never read
    /// before.
    ///
    /// Cancel-safe: dropped before it resolves, it loses nothing.
    pub async fn next(&mut self) -> Option<Update<A>> {
        loop {
            if self.ended {
                return None;
            }
            // Marked seen before reading, so a change after the read
            // wakes the wait below.
            if let Some(wake) = &mut self.wake {
                wake.borrow_and_update();
            }
            let seen = match self.read().await {
                Got::Seen(seen) => *seen,
                Got::NoChat => {
                    self.ended = true;
                    return None;
                }
                Got::Gone => {
                    // CAF may still be shutting down: its last words come
                    // once it has.
                    while self.caf_ended.changed().await.is_ok() {}
                    return self.last_words();
                }
            };
            if let Some(wake) = seen.wake {
                self.wake = Some(wake);
            }
            self.ended = seen.closed;
            if let Some(update) = seen.update {
                self.since = seen.latest;
                return Some(update);
            }
            if self.ended {
                return None;
            }
            // A read of a chat that's open always gives a way to wait.
            let wake = self.wake.as_mut()?;
            if wake.changed().await.is_err() {
                // The chat was closed or CAF has ended: the next read says
                // which.
                self.wake = None;
            }
        }
    }

    /// What changed since this watch last read, and a way to hear of more
    /// if it has none.
    async fn read(&self) -> Got<A> {
        let Some(reads) = self.reads.upgrade() else {
            return Got::Gone;
        };
        let (reply, answer) = oneshot::channel();
        let read = Reading {
            chat: self.chat,
            since: self.since,
            subscribe: self.wake.is_none(),
            reply,
        };
        // Sent at once, with no waiting, so a watch left unpolled partway
        // through never keeps CAF running or holds anything of its.
        let sent = reads.send(read);
        drop(reads);
        if sent.is_err() {
            return Got::Gone;
        }
        let mut ended = self.caf_ended.clone();
        tokio::select! {
            biased;
            answer = answer => match answer {
                Ok(Some(seen)) => Got::Seen(Box::new(seen)),
                Ok(None) => Got::NoChat,
                Err(_) => Got::Gone,
            },
            // A read sent just as the task ends may never be answered.
            () = async { while ended.changed().await.is_ok() {} } => Got::Gone,
        }
    }

    /// The chat as CAF left it, if this watch hasn't seen it yet: once.
    fn last_words(&mut self) -> Option<Update<A>> {
        self.ended = true;
        let last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let (chat, latest) = last.chats.get(&self.chat)?;
        if self.since.is_some_and(|since| since >= *latest) {
            return None;
        }
        self.since = Some(*latest);
        Some(Update::Picture(chat.clone()))
    }

    /// The last change it has had, if any.
    pub(crate) fn latest(&self) -> Option<ChangeNo> {
        self.since
    }
}

/// What a handle asks of the task.
enum Command {
    Open {
        open: OpenChat,
        reply: oneshot::Sender<Result<ChatId, Refused>>,
    },
    Send {
        chat: ChatId,
        message: UserMessage,
        reply: oneshot::Sender<Result<TurnId, Refused>>,
    },
    Stop {
        chat: ChatId,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    Answer {
        question: QuestionId,
        answer: Answer,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    Close {
        chat: ChatId,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

/// A watch's read: what changed in `chat` since `since`, and a way to
/// hear of more if `subscribe`.
///
/// Reads have a channel of their own, without a bound, so a watch never
/// waits to send one. Each watch waits on at most one, and the task
/// never waits to answer it.
struct Reading<A: AppItem> {
    chat: ChatId,
    since: Option<ChangeNo>,
    subscribe: bool,
    reply: oneshot::Sender<Option<Seen<A>>>,
}

/// What a watch's read got.
enum Got<A: AppItem> {
    /// What it found.
    Seen(Box<Seen<A>>),
    /// There's no such chat.
    NoChat,
    /// CAF has gone, or is going: every handle on it was dropped.
    Gone,
}

/// What a watch's read found.
struct Seen<A: AppItem> {
    /// What it hadn't seen, if anything.
    update: Option<Update<A>>,
    /// The last change it has now seen.
    latest: Option<ChangeNo>,
    /// The chat is closed: nothing more will change.
    closed: bool,
    /// How to hear of changes, if it asked and the chat is open.
    wake: Option<watch::Receiver<()>>,
}

/// The chats some watch is following, and each as CAF left it when its
/// task ended, with its last change's number.
type LastWords<A> = Arc<Mutex<Last<A>>>;

/// See [`LastWords`].
struct Last<A: AppItem> {
    /// The chats some watch is following, each with how many: a watch
    /// counts from when it's made until it's dropped, so this never holds
    /// more than the watches there are.
    watched: HashMap<ChatId, usize>,
    /// Each of them as CAF left it. Empty until the task ends.
    chats: HashMap<ChatId, (Chat<A>, ChangeNo)>,
    /// The task has ended, and `chats` is filled in.
    ended: bool,
}

impl<A: AppItem> Default for Last<A> {
    fn default() -> Self {
        Self {
            watched: HashMap::new(),
            chats: HashMap::new(),
            ended: false,
        }
    }
}

/// The task: the core and its drivers.
struct Task<A: AppItem> {
    core: Core<A>,
    agents: AgentDriver,
    timers: TimerDriver,
    policy: Box<dyn Policy>,
    /// The policy's decisions under way.
    deciding: JoinSet<PolicyAnswer>,
    /// Each decision's ticket, by its task.
    tickets: HashMap<tokio::task::Id, Ticket>,
    /// Each decision's task, by its ticket, to stop it if it's withdrawn.
    decisions: HashMap<Ticket, AbortHandle>,
    /// Each open chat's watches, woken when it changes.
    feeds: HashMap<ChatId, watch::Sender<()>>,
    last: LastWords<A>,
    /// Dropped once `last` is filled in, to tell the watches.
    ending: Option<watch::Sender<()>>,
    recorder: Option<Recorder>,
    /// Who sees every step, if anyone.
    steps: Option<Box<dyn Steps<A>>>,
    /// The open chats, for [`ChatsWatch`]es. Dropped as the task ends.
    open: watch::Sender<Vec<ChatSummary>>,
    inbox: mpsc::Receiver<Command>,
    /// Whether any handle is left to send commands.
    listening: bool,
    reads: mpsc::UnboundedReceiver<Reading<A>>,
    /// Whether any handle is left to send reads.
    reading: bool,
    shutting_down: bool,
    /// Those waiting for shutting down to be done.
    waiting: Vec<oneshot::Sender<()>>,
    /// The core's directives, kept to save allocating.
    out: Vec<Directive>,
}

impl<A: AppItem> Task<A> {
    async fn run(mut self) {
        while !(self.shutting_down && self.core.is_idle()) {
            tokio::select! {
                command = self.inbox.recv(), if self.listening => {
                    self.answer_waiting();
                    match command {
                        Some(command) => self.command(command),
                        None => {
                            self.listening = false;
                            self.shutdown_if_unheld();
                        }
                    }
                }
                read = self.reads.recv(), if self.reading => match read {
                    Some(read) => self.answer(read),
                    None => {
                        self.reading = false;
                        self.shutdown_if_unheld();
                    }
                },
                (process, event) = self.agents.next() => self.give(Event::Agent { process, event }),
                () = self.timers.woken() => self.give(Event::Woken),
                Some(decided) = self.deciding.join_next_with_id() => self.decided(decided),
            }
        }
        {
            let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
            let Last {
                watched,
                chats,
                ended,
            } = &mut *last;
            *ended = true;
            for &chat in watched.keys() {
                if let Some(Read::Picture {
                    chat: whole,
                    latest,
                }) = self.core.read(chat, None)
                {
                    chats.insert(chat, (whole.clone(), latest));
                }
            }
        }
        // Nothing more is taken, so no watch waits on a read sent from
        // now on. Then closing every watch's wake-up channel tells them
        // it's over.
        self.inbox.close();
        self.reads.close();
        self.feeds.clear();
        self.ending = None;
        if let Some(recorder) = self.recorder.take() {
            recorder.finish().await;
        }
        for waiting in self.waiting.drain(..) {
            let _ = waiting.send(());
        }
    }

    /// Carries out a handle's command.
    fn command(&mut self, command: Command) {
        // Once shutting down, none is carried out: dropping the reply tells
        // the handle CAF is gone. Reads are still answered.
        match command {
            Command::Shutdown { reply } => {
                self.waiting.push(reply);
                self.shutdown();
            }
            _ if self.shutting_down => {}
            Command::Open { open, reply } => {
                let recorded = self.keeping().then(|| open.clone());
                let result = self.host(
                    |core, now, out| core.open_chat(now, open, out),
                    |&chat| recorded.map(|open| HostCommand::OpenChat { chat, open }),
                );
                // Before the reply, so the list has the chat by the time
                // the host hears its id.
                if result.is_ok() {
                    self.tell_chats();
                }
                let _ = reply.send(result);
            }
            Command::Send {
                chat,
                message,
                reply,
            } => {
                let recorded = self.keeping().then(|| message.clone());
                let result = self.host(
                    |core, now, out| core.send(now, chat, message, out),
                    |&turn| {
                        recorded.map(|message| HostCommand::Send {
                            chat,
                            turn,
                            message,
                        })
                    },
                );
                let _ = reply.send(result);
            }
            Command::Stop { chat, reply } => {
                let result = self.host(
                    |core, now, out| core.stop(now, chat, out),
                    |()| Some(HostCommand::Stop { chat }),
                );
                let _ = reply.send(result);
            }
            Command::Answer {
                question,
                answer,
                reply,
            } => {
                let recorded = self.keeping().then(|| answer.clone());
                let result = self.host(
                    |core, now, out| core.answer(now, question, answer, out),
                    |()| recorded.map(|answer| HostCommand::Answer { question, answer }),
                );
                let _ = reply.send(result);
            }
            Command::Close { chat, reply } => {
                let result = self.host(
                    |core, now, out| core.close_chat(now, chat, out),
                    |()| Some(HostCommand::CloseChat { chat }),
                );
                if result.is_ok() {
                    self.tell_chats();
                }
                let _ = reply.send(result);
            }
        }
    }

    /// Calls one of the core's host methods, recording it as `recorded`
    /// says if the core took it.
    fn host<T>(
        &mut self,
        call: impl FnOnce(&mut Core<A>, Now, &mut Vec<Directive>) -> Result<T, Refused>,
        recorded: impl FnOnce(&T) -> Option<HostCommand<A>>,
    ) -> Result<T, Refused> {
        let now = self.timers.now();
        let result = call(&mut self.core, now, &mut self.out);
        let step = (result.as_ref().ok())
            .and_then(recorded)
            .and_then(|command| self.keep(now, &Event::Host(command)));
        self.kept(step);
        self.route();
        result
    }

    /// Starts shutting down once every handle has gone: once both
    /// channels have closed, so whatever was sent on either before then
    /// has been taken. A watch's read holds its channel open only while it
    /// sends.
    fn shutdown_if_unheld(&mut self) {
        if !self.listening && !self.reading {
            self.shutdown();
        }
    }

    /// Starts shutting down, once.
    fn shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        let now = self.timers.now();
        let step = self.keep(now, &Event::Host(HostCommand::<A>::Shutdown));
        self.core.shutdown(now, &mut self.out);
        self.kept(step);
        self.route();
    }

    /// Gives the core what a driver reported.
    fn give(&mut self, event: Event<A>) {
        let now = self.timers.now();
        let step = self.keep(now, &event);
        self.core.handle(now, event, &mut self.out);
        self.kept(step);
        self.route();
    }

    /// Whether anyone keeps the steps: a recording or a watcher.
    fn keeping(&self) -> bool {
        self.steps.is_some() || (self.recorder.as_ref()).is_some_and(|recorder| !recorder.stopped)
    }

    /// Writes `event`, about to be given to the core at `now`, to the
    /// recording with its secrets hidden, and reads it back as it is for
    /// the watcher, if there are either.
    ///
    /// A driver's event is written before the core takes it, so one the
    /// core panics on is in the recording. A host's call is written after,
    /// since the core gives it its id, such as a new chat's, and isn't
    /// written if the core panics on it.
    fn keep(&mut self, now: Now, event: &Event<A>) -> Option<Recorded<A>> {
        if let Some(recorder) = (self.recorder.as_mut()).filter(|recorder| !recorder.stopped) {
            match line(now, event) {
                Ok(line) => recorder.write(line),
                Err(error) => recorder.stop(&format!("an event can't be written: {error}")),
            }
        }
        self.steps.as_ref()?;
        // Read back, since an event can't be copied: so it's exactly what
        // the core was given, and a fresh core given it does the same.
        let watched = serde_json::to_string(&Line { now, event })
            .and_then(|line| serde_json::from_str(&line));
        match watched {
            Ok(watched) => Some(watched),
            Err(error) => {
                log::warn!(target: "caf", "a step can't be watched: its event doesn't read back: {error}");
                None
            }
        }
    }

    /// Shows the watcher the step [`Task::keep`] read back, once the core
    /// has been given its event and before its directives are carried
    /// out.
    fn kept(&mut self, recorded: Option<Recorded<A>>) {
        if let (Some(steps), Some(Recorded { now, event })) = (&mut self.steps, recorded) {
            steps.step(Step {
                now,
                event: &event,
                directives: &self.out,
            });
        }
    }

    /// Answers the reads already waiting, before a command: so a read sent
    /// before a command is answered before it, as if they shared a
    /// channel.
    fn answer_waiting(&mut self) {
        for _ in 0..self.reads.len() {
            let Ok(read) = self.reads.try_recv() else {
                return;
            };
            self.answer(read);
        }
    }

    /// Answers a watch's read, unless the watch has stopped waiting for
    /// it, as a viewer's does when it watches the chat over again: then
    /// there's no need to copy what it would have been given.
    fn answer(&mut self, read: Reading<A>) {
        if read.reply.is_closed() {
            return;
        }
        let seen = self.read(read.chat, read.since, read.subscribe);
        let _ = read.reply.send(seen);
    }

    /// Tells the [`ChatsWatch`]es which chats are open, if that changed.
    fn tell_chats(&self) {
        let open: Vec<ChatSummary> = (self.core.chats())
            .filter(|chat| chat.state != ChatState::Closed)
            .map(ChatSummary::of)
            .collect();
        self.open.send_if_modified(|was| {
            let changed = *was != open;
            if changed {
                *was = open;
            }
            changed
        });
    }

    /// The policy decided, or its decision panicked, which counts as a
    /// refusal so the request is still answered.
    fn decided(
        &mut self,
        decided: Result<(tokio::task::Id, PolicyAnswer), tokio::task::JoinError>,
    ) {
        let (id, answer) = match decided {
            Ok(decided) => decided,
            Err(error) if error.is_panic() => {
                log::warn!(target: "caf", "the policy panicked deciding a permission request: refused");
                (error.id(), PolicyAnswer::Refuse)
            }
            // Stopped, because the request was withdrawn.
            Err(error) => {
                if let Some(ticket) = self.tickets.remove(&error.id()) {
                    self.decisions.remove(&ticket);
                }
                return;
            }
        };
        if let Some(ticket) = self.tickets.remove(&id) {
            self.decisions.remove(&ticket);
            self.give(Event::Policy { ticket, answer });
        }
    }

    /// Starts deciding a permission request. A policy that panics, in
    /// `decide` itself or in the future it gives, refuses.
    fn decide(
        &mut self,
        ticket: Ticket,
        chat: ChatId,
        request: caf_core::directive::PermissionRequest,
    ) {
        let policy = &mut self.policy;
        let deciding = std::panic::catch_unwind(AssertUnwindSafe(|| policy.decide(chat, request)))
            .unwrap_or_else(|_| {
                log::warn!(target: "caf", "the policy panicked deciding a permission request: refused");
                Box::pin(std::future::ready(PolicyAnswer::Refuse))
            });
        let handle = self.deciding.spawn(deciding);
        self.tickets.insert(handle.id(), ticket);
        self.decisions.insert(ticket, handle);
    }

    /// Hands each of the core's directives to its driver.
    fn route(&mut self) {
        let mut out = std::mem::take(&mut self.out);
        for directive in out.drain(..) {
            match directive {
                Directive::Agent(agent) => self.agents.apply(agent),
                Directive::Timer(TimerDirective::WakeAt(at)) => self.timers.wake_at(at),
                Directive::Policy(PolicyDirective::Decide {
                    ticket,
                    chat,
                    request,
                }) => self.decide(ticket, chat, request),
                Directive::Policy(PolicyDirective::Withdrawn { ticket }) => {
                    if let Some(decision) = self.decisions.remove(&ticket) {
                        decision.abort();
                    }
                }
                Directive::Viewer(ViewerDirective::Changed { chat, .. }) => {
                    if let Some(feed) = self.feeds.get(&chat) {
                        feed.send_replace(());
                    }
                    // A closed chat changes no more: its watches read the
                    // last of it and end.
                    if self.closed(chat) {
                        self.feeds.remove(&chat);
                    }
                }
                Directive::Report(report) => log_report(&report),
                // Storage comes in a later feature, and none are made yet.
                _ => {}
            }
        }
        self.out = out;
    }

    /// Whether `chat` is closed, or was never there.
    fn closed(&self, chat: ChatId) -> bool {
        self.core
            .chat(chat)
            .is_none_or(|whole| whole.state == ChatState::Closed)
    }

    /// What a watch that has seen up to `since` hasn't seen of `chat`.
    fn read(&mut self, chat: ChatId, since: Option<ChangeNo>, subscribe: bool) -> Option<Seen<A>> {
        let closed = self.closed(chat);
        let read = self.core.read(chat, since)?;
        let (update, latest) = match read {
            Read::Changes([]) => (None, since),
            Read::Changes(changes) => (
                Some(Update::Changes(changes.to_vec())),
                changes.last().map(|last| last.no),
            ),
            Read::Picture { chat, latest } => (Some(Update::Picture(chat.clone())), Some(latest)),
            // No other kind of read is given yet. One added later needs
            // handling here, or its watches end.
            _ => return None,
        };
        let wake = (subscribe && !closed).then(|| {
            let feed = (self.feeds.entry(chat)).or_insert_with(|| watch::Sender::new(()));
            feed.subscribe()
        });
        Some(Seen {
            update,
            latest,
            closed,
            wake,
        })
    }
}

/// A new run's name: 128 random bits, as hex.
fn new_run() -> String {
    // Each `RandomState` has keys of its own, random for the process, so
    // what each hashes differs from every other's.
    let half = || RandomState::new().build_hasher().finish();
    format!("{:016x}{:016x}", half(), half())
}

/// What's written in a recording for "redacted".
const HIDDEN: &str = "[redacted]";

/// `open`, with its MCP servers' variables' and headers' values hidden.
fn hide_mcp_secrets(open: &OpenChat) -> OpenChat {
    let mut open = open.clone();
    for server in &mut open.mcp_servers {
        match server {
            McpServer::Stdio { command, .. } => hide_values(&mut command.env),
            McpServer::Http { headers, .. } => {
                for (_, value) in headers {
                    HIDDEN.clone_into(value);
                }
            }
            // No other kind is given yet. One added later needs its secrets
            // hidden here.
            _ => {}
        }
    }
    open
}

/// Hides the values of the variables `env` sets, keeping their names.
fn hide_values(env: &mut [EnvChange]) {
    for change in env {
        if let EnvChange::Set(_, value) = change {
            *value = HIDDEN.into();
        }
    }
}

/// Hands a report to the `log` facade.
fn log_report(report: &Report) {
    let level = match report.level {
        Level::Debug => log::Level::Debug,
        Level::Info => log::Level::Info,
        _ => log::Level::Warn,
    };
    log::log!(target: "caf", level, "{}", report.message);
}

/// How much a recording may have waiting to be written before CAF stops
/// recording.
const RECORDING_BEHIND: usize = 256 << 20;

/// Claude's sign-in status, which holds the account's email.
const SIGN_IN_STATUS: &str = "_auth/status_update";

/// Writes a recording, on a thread of its own.
struct Recorder {
    lines: std::sync::mpsc::Sender<String>,
    shared: Arc<Shared>,
    /// How much may wait to be written.
    most: usize,
    /// It has stopped recording.
    stopped: bool,
    thread: std::thread::JoinHandle<()>,
    /// Where it writes, for warnings.
    path: String,
}

/// What a recorder and its thread both see.
#[derive(Default)]
struct Shared {
    /// Bytes waiting to be written.
    waiting: AtomicUsize,
    /// A write failed: nothing more is written.
    failed: AtomicBool,
}

/// A recording's first line.
#[derive(Serialize)]
struct Header<'a> {
    caf_recording: u32,
    host: &'a Host,
    agents: Vec<AgentSpec>,
}

/// One of a recording's events.
#[derive(Serialize)]
#[serde(bound = "A: AppItem")]
struct Line<'a, A: AppItem> {
    now: Now,
    event: &'a Event<A>,
}

/// An event at the time it was given, read back from its JSON: a
/// recording's line, or a watched step's event.
#[derive(Deserialize)]
#[serde(bound = "A: AppItem")]
struct Recorded<A: AppItem> {
    now: Now,
    event: Event<A>,
}

impl Recorder {
    /// Makes the file and writes its first line, or says why it couldn't.
    fn start(path: &PathBuf, host: &Host, agents: &[AgentSpec]) -> Option<Self> {
        let agents = (agents.iter())
            .map(|agent| {
                let mut agent = agent.clone();
                agent.command.args.clear();
                hide_values(&mut agent.command.env);
                agent
            })
            .collect();
        let header = Header {
            caf_recording: 2,
            host,
            agents,
        };
        let header = match serde_json::to_string(&header) {
            Ok(header) => header,
            Err(error) => {
                log::warn!(target: "caf", "can't record: the host or agents can't be written: {error}");
                return None;
            }
        };
        // New, so it's never a link, a pipe or someone else's file.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path);
        match file {
            Ok(file) => Some(Self::spawn(
                path.display().to_string(),
                BufWriter::new(file),
                header,
                RECORDING_BEHIND,
            )),
            Err(error) => {
                log::warn!(target: "caf", "can't record to {}: {error}", path.display());
                None
            }
        }
    }

    /// Writes `header` then each line recorded to `writer`, on a thread,
    /// stopping if more than `most` bytes are waiting.
    fn spawn(
        path: String,
        writer: impl Write + Send + 'static,
        header: String,
        most: usize,
    ) -> Self {
        let (lines, inbox) = std::sync::mpsc::channel();
        let shared = Arc::new(Shared::default());
        let mut recorder = Self {
            lines,
            shared: Arc::clone(&shared),
            most,
            stopped: false,
            thread: std::thread::spawn({
                let path = path.clone();
                move || write_lines(&path, writer, &inbox, &shared)
            }),
            path,
        };
        recorder.send(header);
        recorder
    }

    /// Writes one event's line, made by [`line`].
    fn write(&mut self, line: String) {
        if !self.stopped {
            self.send(line);
        }
    }

    /// Hands a line to the thread, unless too much is waiting already.
    fn send(&mut self, line: String) {
        if self.shared.failed.load(Ordering::Relaxed) {
            // The thread said why.
            self.stopped = true;
            return;
        }
        let waiting = self.shared.waiting.fetch_add(line.len(), Ordering::Relaxed);
        if waiting + line.len() > self.most {
            self.shared.waiting.fetch_sub(line.len(), Ordering::Relaxed);
            self.stop("the disk has fallen behind");
            return;
        }
        let _ = self.lines.send(line);
    }

    /// Stops recording, saying why: in a warning, and in a last line
    /// written once everything before it has been.
    fn stop(&mut self, why: &str) {
        log::warn!(target: "caf", "stopped recording to {}: {why}", self.path);
        self.stopped = true;
        let last = serde_json::json!({ "stopped": why }).to_string();
        self.shared.waiting.fetch_add(last.len(), Ordering::Relaxed);
        let _ = self.lines.send(last);
    }

    /// Waits for everything to be written.
    async fn finish(self) {
        drop(self.lines);
        let thread = self.thread;
        let _ = tokio::task::spawn_blocking(move || thread.join()).await;
    }
}

/// `event` at `now`, as a recording's line: with MCP servers' secrets
/// and Claude's sign-in status hidden, and error output as text where
/// it's UTF-8.
fn line<A: AppItem>(now: Now, event: &Event<A>) -> serde_json::Result<String> {
    let (process, agent) = match event {
        Event::Agent { process, event } => (process, event),
        Event::Host(HostCommand::OpenChat { chat, open }) => {
            let event = Event::<A>::Host(HostCommand::OpenChat {
                chat: *chat,
                open: hide_mcp_secrets(open),
            });
            return serde_json::to_string(&Line { now, event: &event });
        }
        _ => return serde_json::to_string(&Line { now, event }),
    };
    let hidden = match agent {
        // As text where it's UTF-8, so a person can read it. A chunk can
        // end partway through a character, and is then written as bytes,
        // so the core is given exactly what it was given before.
        AgentEvent::Stderr(bytes) => {
            let Ok(text) = std::str::from_utf8(bytes) else {
                return serde_json::to_string(&Line { now, event });
            };
            let event = serde_json::json!({
                "type": "Agent",
                "value": { "process": process, "event": { "type": "Stderr", "value": text } },
            });
            return serde_json::to_string(&serde_json::json!({ "now": now, "event": event }));
        }
        AgentEvent::Frame(Frame::Message(Message::Notification { method, .. }))
            if method == SIGN_IN_STATUS =>
        {
            Message::Notification {
                method: method.clone(),
                params: None,
            }
        }
        AgentEvent::Frame(Frame::Message(Message::Request { id, method, .. }))
            if method == SIGN_IN_STATUS =>
        {
            Message::Request {
                id: id.clone(),
                method: method.clone(),
                params: None,
            }
        }
        _ => return serde_json::to_string(&Line { now, event }),
    };
    let event = Event::<A>::Agent {
        process: *process,
        event: AgentEvent::Frame(Frame::Message(hidden)),
    };
    serde_json::to_string(&Line { now, event: &event })
}

/// Writes lines as they come until there are no more, flushing whenever
/// none are waiting. After a failure, lines are read and dropped, so CAF
/// never waits on them.
fn write_lines(
    path: &str,
    mut writer: impl Write,
    inbox: &std::sync::mpsc::Receiver<String>,
    shared: &Shared,
) {
    let fail = |error: std::io::Error| {
        log::warn!(target: "caf", "stopped recording to {path}: {error}");
        shared.failed.store(true, Ordering::Relaxed);
    };
    while let Ok(first) = inbox.recv() {
        let mut line = Some(first);
        while let Some(each) = line {
            shared.waiting.fetch_sub(each.len(), Ordering::Relaxed);
            if !shared.failed.load(Ordering::Relaxed)
                && let Err(error) =
                    (writer.write_all(each.as_bytes())).and_then(|()| writer.write_all(b"\n"))
            {
                fail(error);
            }
            line = inbox.try_recv().ok();
        }
        if !shared.failed.load(Ordering::Relaxed)
            && let Err(error) = writer.flush()
        {
            fail(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use caf_core::time::{Mono, Wall};

    use super::*;

    /// A writer that keeps what it's given, and waits to be let go before
    /// taking anything.
    struct Stalled {
        go: mpsc::Receiver<()>,
        kept: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for Stalled {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let _ = self.go.recv_timeout(Duration::from_secs(60));
            self.kept.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_recording_stops_when_the_disk_falls_behind() {
        let (go, wait) = mpsc::channel();
        let kept = Arc::new(Mutex::new(Vec::new()));
        let writer = Stalled {
            go: wait,
            kept: Arc::clone(&kept),
        };
        let mut recorder = Recorder::spawn("test".into(), writer, "{}".into(), 300);
        let now = Now {
            wall: Wall(0),
            mono: Mono(0),
        };
        for _ in 0..20 {
            recorder.write(line(now, &Event::<NoAppItems>::Woken).unwrap());
        }
        assert!(recorder.stopped, "it stops rather than holding more");
        drop(go);
        recorder.finish().await;
        let kept = String::from_utf8(kept.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = kept.lines().collect();
        assert!(lines.len() > 1 && lines.len() < 20, "{lines:?}");
        assert_eq!(lines[0], "{}");
        // What it did write replays, as far as it goes, and then it says
        // why it stopped.
        let (stopped, steps) = lines[1..].split_last().unwrap();
        for line in steps {
            let step: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(step["event"], serde_json::json!({ "type": "Woken" }));
        }
        assert_eq!(*stopped, r#"{"stopped":"the disk has fallen behind"}"#);
    }

    #[test]
    fn error_output_split_inside_a_character_replays_exactly() {
        use caf_core::agent::SpawnCommand;
        use caf_core::directive::AgentDirective;
        use caf_core::profiles::ProfileChoice;
        let agents = vec![AgentSpec::new(
            "plain",
            SpawnCommand::new("/opt/plain"),
            ProfileChoice::Cautious,
        )];
        let host = Host::new("test", "1.0.0");
        let now = Now {
            wall: Wall(0),
            mono: Mono(0),
        };
        let mut live = Core::<NoAppItems>::new(host.clone(), agents.clone());
        let mut out = Vec::new();
        let chat = live
            .open_chat(now, OpenChat::new("plain", "/work"), &mut out)
            .unwrap();
        let Some(process) = out.iter().find_map(|directive| match directive {
            Directive::Agent(AgentDirective::Start { process, .. }) => Some(*process),
            _ => None,
        }) else {
            panic!("an adapter started: {out:?}")
        };
        let mut lines = vec![
            serde_json::to_string(&Line {
                now,
                event: &Event::<NoAppItems>::Host(HostCommand::OpenChat {
                    chat,
                    open: OpenChat::new("plain", "/work"),
                }),
            })
            .unwrap(),
        ];
        // "café" and "naïve", each cut inside its accented letter.
        for chunk in [&b"caf\xc3"[..], b"\xa9 na\xc3", b"\xafve\n"] {
            let event = Event::Agent {
                process,
                event: AgentEvent::Stderr(chunk.to_vec()),
            };
            lines.push(line(now, &event).unwrap());
            live.handle(now, event, &mut out);
        }
        let mut replayed = Core::<NoAppItems>::new(host, agents);
        for each in &lines {
            #[derive(serde::Deserialize)]
            struct Step {
                now: Now,
                event: Event,
            }
            let step: Step = serde_json::from_str(each).unwrap();
            replayed.handle(step.now, step.event, &mut out);
        }
        assert_eq!(live.connections()[0].stderr, ["café naïve"]);
        assert_eq!(replayed.connections(), live.connections());
        // Whole characters are still written as text.
        let whole = Event::<NoAppItems>::Agent {
            process,
            event: AgentEvent::Stderr("café\n".into()),
        };
        assert!(
            line(now, &whole)
                .unwrap()
                .contains(r#"{"type":"Stderr","value":"café\n"}"#)
        );
    }

    #[tokio::test]
    async fn watching_a_chat_that_isnt_there_leaves_nothing_behind() {
        let (caf, running) =
            Caf::<NoAppItems>::start(Config::new(Host::new("test", "1.0.0"), vec![]));
        for chat in 1000..1100 {
            let mut watch = caf.chat(ChatId(chat)).watch();
            assert!(watch.next().await.is_none());
        }
        let watched = caf.shared.last.lock().unwrap().watched.len();
        assert_eq!(watched, 0, "a watch for no chat is forgotten once it ends");
        caf.shutdown().await;
        running.await;
    }

    #[tokio::test]
    async fn a_watch_made_after_caf_has_ended_leaves_nothing_behind() {
        let (caf, running) =
            Caf::<NoAppItems>::start(Config::new(Host::new("test", "1.0.0"), vec![]));
        caf.shutdown().await;
        running.await;
        let mut watch = caf.chat(ChatId(1000)).watch();
        assert!(watch.next().await.is_none());
        let watched = caf.shared.last.lock().unwrap().watched.len();
        assert_eq!(watched, 0, "nothing is kept for a watch once CAF has ended");
    }

    #[tokio::test]
    async fn a_read_caf_never_answers_ends_once_caf_has_ended() {
        // A read taken just as CAF's task ended: it's held where nothing
        // will ever answer it, while a handle on CAF is still held.
        let (reads, inbox) = tokio::sync::mpsc::unbounded_channel();
        let (ending, ended) = watch::channel(());
        let mut watch = Watch::<NoAppItems> {
            reads: reads.downgrade(),
            last: LastWords::default(),
            caf_ended: ended,
            chat: ChatId(1),
            since: None,
            wake: None,
            ended: false,
            counted: false,
        };
        drop(ending);
        let next = tokio::time::timeout(Duration::from_secs(5), watch.next()).await;
        assert!(matches!(next, Ok(None)), "the watch ends with CAF");
        drop((reads, inbox));
    }

    #[tokio::test]
    async fn watches_dropped_unread_leave_nothing_behind() {
        let (caf, running) =
            Caf::<NoAppItems>::start(Config::new(Host::new("test", "1.0.0"), vec![]));
        for chat in 1000..2000 {
            drop(caf.chat(ChatId(chat)).watch());
        }
        let kept = caf.shared.last.lock().unwrap().watched.len();
        assert_eq!(kept, 0, "nothing is kept for a watch once it's dropped");
        caf.shutdown().await;
        running.await;
    }
}
