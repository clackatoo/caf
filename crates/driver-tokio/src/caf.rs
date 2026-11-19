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
//! Watches pull rather than being pushed to: a watch asks for what
//! changed since it last read, so the only changes held for it are the
//! core's own list, which is bounded
//! ([`Limits::kept_changes`](caf_core::host::Limits::kept_changes)). A
//! watch further behind than that gets the whole chat again.

use std::collections::HashMap;
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
use caf_core::ids::ChangeNo;
use caf_core::question::Answer;
use caf_core::{
    AppItem, Chat, ChatId, Core, Directive, Event, Host, NoAppItems, Now, Numbered, OpenChat,
    QuestionId, Read, Refused, Report, Ticket, TurnId,
};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle, JoinSet};

use crate::agent::{AgentDriver, AgentLimits};
use crate::policy::{AlwaysAsk, Policy};
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
    /// It's JSON lines. The first holds the host and the agents:
    /// `{"caf_recording": 1, "host": …, "agents": […]}`. Each line after
    /// it is one event the core was given, with its time, in order:
    /// `{"now": …, "event": …}`. Calls such as [`ChatHandle::send`] are
    /// written as the [`Event::Host`] they stand for, with the id the core
    /// gave out, and only if the core took them. Read each line's event
    /// back as an [`Event`].
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
    /// its error output, as text, with any bytes that aren't UTF-8
    /// replaced; folders; and each MCP server's name, program, arguments
    /// and URL. So don't put secrets in MCP arguments or URLs, and treat a
    /// recording as private: read it through before sharing it. Names of
    /// variables and arguments are written as arrays of bytes, as Rust
    /// writes them.
    ///
    /// The file is made new, readable only by its owner: if anything is
    /// there already, even a link, CAF won't record. It's written on a
    /// thread of its own, so the disk never holds CAF up, and it's complete
    /// once [`Caf::shutdown`] returns. If it can't be made, if a write
    /// fails, or if more than 256 MiB is waiting to be written because the
    /// disk has fallen behind, CAF says so in a warning and stops
    /// recording: what's in the file still replays, as far as it goes.
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
    commands: mpsc::Sender<Command<A>>,
    last: LastWords<A>,
    /// Closed once the task has ended and `last` is filled in.
    ended: watch::Receiver<()>,
}

impl<A: AppItem> Clone for Caf<A> {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            last: Arc::clone(&self.last),
            ended: self.ended.clone(),
        }
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
        assert_eq!(
            config.agent_limits.biggest_line, config.host.limits.biggest_message,
            "the agent driver's biggest line must be the host's biggest message"
        );
        let recorder = (config.record.as_ref())
            .and_then(|path| Recorder::start(path, &config.host, &config.agents));
        let (commands, inbox) = mpsc::channel(COMMANDS);
        let last = LastWords::default();
        let (ending, ended) = watch::channel(());
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
            inbox,
            listening: true,
            shutting_down: false,
            waiting: Vec::new(),
            out: Vec::new(),
        };
        let task = tokio::spawn(task.run());
        let caf = Self {
            commands,
            last,
            ended,
        };
        (caf, Running { task })
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
        command: impl FnOnce(oneshot::Sender<Result<T, Refused>>) -> Command<A>,
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
        let answering = |reply| Command::Answer {
            question,
            answer,
            reply,
        };
        self.caf.ask(answering).await
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
        Watch {
            commands: self.caf.commands.downgrade(),
            last: Arc::clone(&self.caf.last),
            caf_ended: self.caf.ended.clone(),
            chat: self.id,
            since: None,
            wake: None,
            ended: false,
        }
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
    commands: mpsc::WeakSender<Command<A>>,
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
        let Some(commands) = self.commands.upgrade() else {
            return Got::Gone;
        };
        let (reply, answer) = oneshot::channel();
        let read = Command::Read {
            chat: self.chat,
            since: self.since,
            subscribe: self.wake.is_none(),
            reply,
        };
        if commands.send(read).await.is_err() {
            return Got::Gone;
        }
        drop(commands);
        match answer.await {
            Ok(Some(seen)) => Got::Seen(Box::new(seen)),
            Ok(None) => Got::NoChat,
            Err(_) => Got::Gone,
        }
    }

    /// The chat as CAF left it, if this watch hasn't seen it yet: once.
    fn last_words(&mut self) -> Option<Update<A>> {
        self.ended = true;
        let last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let (chat, latest) = last.get(&self.chat)?;
        let behind = self.since.is_none_or(|since| since < *latest);
        behind.then(|| Update::Picture(chat.clone()))
    }
}

/// What a handle asks of the task.
enum Command<A: AppItem> {
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
    Read {
        chat: ChatId,
        since: Option<ChangeNo>,
        subscribe: bool,
        reply: oneshot::Sender<Option<Seen<A>>>,
    },
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

/// Every chat as CAF left it when its task ended, with its last change's
/// number. Empty until then.
type LastWords<A> = Arc<Mutex<HashMap<ChatId, (Chat<A>, ChangeNo)>>>;

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
    inbox: mpsc::Receiver<Command<A>>,
    /// Whether any handle is left to send commands.
    listening: bool,
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
                command = self.inbox.recv(), if self.listening => match command {
                    Some(command) => self.command(command),
                    None => {
                        self.listening = false;
                        self.shutdown();
                    }
                },
                (process, event) = self.agents.next() => self.give(Event::Agent { process, event }),
                () = self.timers.woken() => self.give(Event::Woken),
                Some(decided) = self.deciding.join_next_with_id() => self.decided(decided),
            }
        }
        {
            let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
            for whole in self.core.chats() {
                if let Some(Read::Picture { latest, .. }) = self.core.read(whole.id, None) {
                    last.insert(whole.id, (whole.clone(), latest));
                }
            }
        }
        // Closing every watch's wake-up channel tells them it's over.
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
    fn command(&mut self, command: Command<A>) {
        // Once shutting down, only reads are answered: dropping the reply
        // tells the handle CAF is gone.
        match command {
            Command::Read {
                chat,
                since,
                subscribe,
                reply,
            } => {
                let _ = reply.send(self.read(chat, since, subscribe));
            }
            Command::Shutdown { reply } => {
                self.waiting.push(reply);
                self.shutdown();
            }
            _ if self.shutting_down => {}
            Command::Open { open, reply } => {
                let recorded = self.recorder.is_some().then(|| hide_mcp_secrets(&open));
                let result = self.host(
                    |core, now, out| core.open_chat(now, open, out),
                    |&chat| recorded.map(|open| HostCommand::OpenChat { chat, open }),
                );
                let _ = reply.send(result);
            }
            Command::Send {
                chat,
                message,
                reply,
            } => {
                let recorded = self.recorder.is_some().then(|| message.clone());
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
                let recorded = self.recorder.is_some().then(|| answer.clone());
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
        if let (Ok(done), Some(recorder)) = (&result, &mut self.recorder)
            && let Some(command) = recorded(done)
        {
            recorder.record(now, &Event::Host(command));
        }
        self.route();
        result
    }

    /// Starts shutting down, once.
    fn shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        let now = self.timers.now();
        if let Some(recorder) = &mut self.recorder {
            recorder.record(now, &Event::Host(HostCommand::<A>::Shutdown));
        }
        self.core.shutdown(now, &mut self.out);
        self.route();
    }

    /// Gives the core what a driver reported.
    fn give(&mut self, event: Event<A>) {
        let now = self.timers.now();
        if let Some(recorder) = &mut self.recorder {
            recorder.record(now, &event);
        }
        self.core.handle(now, event, &mut self.out);
        self.route();
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
        let (update, latest) = match self.core.read(chat, since)? {
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
struct Step<'a, A: AppItem> {
    now: Now,
    event: &'a Event<A>,
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
            caf_recording: 1,
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

    /// Writes one event.
    fn record<A: AppItem>(&mut self, now: Now, event: &Event<A>) {
        if self.stopped {
            return;
        }
        match line(now, event) {
            Ok(line) => self.send(line),
            Err(error) => self.stop(&format!("an event can't be written: {error}")),
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

    /// Stops recording, saying why.
    fn stop(&mut self, why: &str) {
        log::warn!(target: "caf", "stopped recording to {}: {why}", self.path);
        self.stopped = true;
    }

    /// Waits for everything to be written.
    async fn finish(self) {
        drop(self.lines);
        let thread = self.thread;
        let _ = tokio::task::spawn_blocking(move || thread.join()).await;
    }
}

/// `event` at `now`, as a recording's line: with Claude's sign-in status
/// emptied, and error output as text.
fn line<A: AppItem>(now: Now, event: &Event<A>) -> serde_json::Result<String> {
    let Event::Agent {
        process,
        event: agent,
    } = event
    else {
        return serde_json::to_string(&Step { now, event });
    };
    let hidden = match agent {
        AgentEvent::Stderr(bytes) => {
            let text = String::from_utf8_lossy(bytes);
            let event =
                serde_json::json!({ "Agent": { "process": process, "event": { "Stderr": text } } });
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
        _ => return serde_json::to_string(&Step { now, event }),
    };
    let event = Event::<A>::Agent {
        process: *process,
        event: AgentEvent::Frame(Frame::Message(hidden)),
    };
    serde_json::to_string(&Step { now, event: &event })
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
            recorder.record(now, &Event::<NoAppItems>::Woken);
        }
        assert!(recorder.stopped, "it stops rather than holding more");
        drop(go);
        recorder.finish().await;
        let kept = String::from_utf8(kept.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = kept.lines().collect();
        assert!(lines.len() > 1 && lines.len() < 20, "{lines:?}");
        assert_eq!(lines[0], "{}");
        // What it did write replays, as far as it goes.
        for line in &lines[1..] {
            let step: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(step["event"], "Woken");
        }
    }
}
