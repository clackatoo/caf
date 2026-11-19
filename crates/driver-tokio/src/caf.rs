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
//! | `Policy(Withdrawn)` | Nowhere: the decision runs on, and the core ignores its answer |
//! | `Viewer(Changed)` | Wakes the chat's watches, which read with [`Core::read`] |
//! | `Report` | The [`log`] facade, target `caf` |
//!
//! Watches pull rather than being pushed to: a watch asks for what
//! changed since it last read, so the only changes held for it are the
//! core's own list, which is bounded
//! ([`Limits::kept_changes`](caf_core::host::Limits::kept_changes)). A
//! watch further behind than that gets the whole chat again.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use caf_core::acp::{Frame, Message};
use caf_core::agent::AgentSpec;
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
use tokio::task::{JoinHandle, JoinSet};

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
    /// [`Core`] as a test. `None` by default.
    ///
    /// It's JSON lines. The first holds the host and the agents:
    /// `{"caf_recording": 1, "host": …, "agents": […]}`. Each line after
    /// it is one event the core was given, with its time, in order:
    /// `{"now": …, "event": …}`. Calls such as [`ChatHandle::send`] are
    /// written as the [`Event::Host`] they stand for, with the id the core
    /// gave out, and only if the core took them.
    ///
    /// **What's left out:** Claude's sign-in status
    /// (`_auth/status_update`), which holds the account's email, and the
    /// agents' environment changes, which may hold keys. The core changes
    /// nothing for the first, and needs neither to end in the same state.
    /// Everything else goes in as it came: what the user sent, what the
    /// agent said, its error output, folders and MCP servers. Treat a
    /// recording as private.
    ///
    /// The file is made afresh, readable only by its owner, and never
    /// through a symbolic link. It's written on a thread of its own, so
    /// the disk never holds CAF up, and it's complete once
    /// [`Caf::shutdown`] returns. If it can't be made, or a write fails,
    /// CAF says so in a warning and runs on without recording.
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
/// Once every clone, every [`ChatHandle`] and every [`Watch`] is dropped,
/// CAF shuts down by itself, as if [`Caf::shutdown`] had been called.
pub struct Caf<A: AppItem = NoAppItems> {
    commands: mpsc::Sender<Command<A>>,
}

impl<A: AppItem> Clone for Caf<A> {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
        }
    }
}

/// The task CAF runs in. Await it after [`Caf::shutdown`] to know every
/// adapter has stopped.
///
/// It resolves when the task ends: once shut down, every adapter has
/// stopped, and a recording, if asked for, is complete.
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
        let task = Task {
            core: Core::new(config.host, config.agents),
            agents: AgentDriver::new(config.agent_limits),
            timers: TimerDriver::new(),
            policy: config.policy,
            deciding: JoinSet::new(),
            tickets: HashMap::new(),
            feeds: HashMap::new(),
            recorder,
            inbox,
            listening: true,
            shutting_down: false,
            waiting: Vec::new(),
            out: Vec::new(),
        };
        let task = tokio::spawn(task.run());
        (Self { commands }, Running { task })
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
    /// Returns once that's done, or at once if CAF has already shut down.
    /// From the moment it's called, anything else asked of CAF is
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

    /// What changed in `chat` since `since`, and a way to hear of more if
    /// `subscribe`. `None` if there's no such chat, or the task has gone.
    async fn read(
        &self,
        chat: ChatId,
        since: Option<ChangeNo>,
        subscribe: bool,
    ) -> Option<Seen<A>> {
        let (reply, answer) = oneshot::channel();
        let read = Command::Read {
            chat,
            since,
            subscribe,
            reply,
        };
        self.commands.send(read).await.ok()?;
        answer.await.ok()?
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
            caf: self.caf.clone(),
            chat: self.id,
            since: None,
            feed: None,
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
pub struct Watch<A: AppItem = NoAppItems> {
    caf: Caf<A>,
    chat: ChatId,
    /// The last change it has had.
    since: Option<ChangeNo>,
    /// How it hears of changes, once it has read once.
    feed: Option<Subscription<A>>,
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
    /// When CAF shuts down, a watch that hasn't seen the chat as shutting
    /// down left it gets it as one last [`Update::Picture`].
    ///
    /// Cancel-safe: dropped before it resolves, it loses nothing.
    pub async fn next(&mut self) -> Option<Update<A>> {
        loop {
            if self.ended {
                return None;
            }
            // Marked seen before reading, so a change after the read
            // wakes the wait below.
            if let Some(feed) = &mut self.feed {
                feed.wake.borrow_and_update();
            }
            let subscribe = self.feed.is_none();
            let Some(seen) = self.caf.read(self.chat, self.since, subscribe).await else {
                return self.last_words();
            };
            if let Some(feed) = seen.feed {
                self.feed = Some(feed);
            }
            self.ended = seen.closed;
            if let Some(update) = seen.update {
                self.since = seen.latest;
                return Some(update);
            }
            if self.ended {
                return None;
            }
            let feed = self.feed.as_mut()?;
            if feed.wake.changed().await.is_err() {
                return self.last_words();
            }
        }
    }

    /// The chat as CAF left it, if this watch hasn't seen it yet: once.
    fn last_words(&mut self) -> Option<Update<A>> {
        self.ended = true;
        let feed = self.feed.as_ref()?;
        let last = feed.last.lock().unwrap_or_else(PoisonError::into_inner);
        let (chat, latest) = last.as_ref()?;
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

/// What a watch's read found.
struct Seen<A: AppItem> {
    /// What it hadn't seen, if anything.
    update: Option<Update<A>>,
    /// The last change it has now seen.
    latest: Option<ChangeNo>,
    /// The chat is closed: nothing more will change.
    closed: bool,
    /// How to hear of changes, if it asked.
    feed: Option<Subscription<A>>,
}

/// The chat as CAF left it when it ended, with its last change's number.
type LastWords<A> = Arc<Mutex<Option<(Chat<A>, ChangeNo)>>>;

/// A watch's way of hearing of a chat's changes.
struct Subscription<A: AppItem> {
    /// Marked changed whenever the chat changes; closed when the task
    /// ends.
    wake: watch::Receiver<()>,
    /// Filled in just before the task ends.
    last: LastWords<A>,
}

/// The task's side of a chat's subscriptions.
struct Feed<A: AppItem> {
    wake: watch::Sender<()>,
    last: LastWords<A>,
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
    /// The chats being watched.
    feeds: HashMap<ChatId, Feed<A>>,
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
        for (&chat, feed) in &self.feeds {
            if let (Some(whole), Some(Read::Picture { latest, .. })) =
                (self.core.chat(chat), self.core.read(chat, None))
            {
                *feed.last.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((whole.clone(), latest));
            }
        }
        // Closing every watch's wake-up channel tells them it's over.
        self.feeds.clear();
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
                let recorded = self.recorder.is_some().then(|| open.clone());
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
        if let Some(recorder) = &mut self.recorder
            && !private(&event)
        {
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
            Err(error) => {
                if !error.is_panic() {
                    // Cancelled: only as the runtime shuts down.
                    self.tickets.remove(&error.id());
                    return;
                }
                log::warn!(target: "caf", "the policy panicked deciding a permission request: refused");
                (error.id(), PolicyAnswer::Refuse)
            }
        };
        if let Some(ticket) = self.tickets.remove(&id) {
            self.give(Event::Policy { ticket, answer });
        }
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
                }) => {
                    let deciding = self.deciding.spawn(self.policy.decide(chat, request));
                    self.tickets.insert(deciding.id(), ticket);
                }
                // A withdrawn decision runs on: the core ignores its
                // answer, as it does any stale ticket's.
                Directive::Policy(_) => {}
                Directive::Viewer(ViewerDirective::Changed { chat, .. }) => {
                    if let Some(feed) = self.feeds.get(&chat) {
                        feed.wake.send_replace(());
                    }
                }
                Directive::Report(report) => log_report(&report),
                // Storage comes in a later feature, and none are made yet.
                _ => {}
            }
        }
        self.out = out;
    }

    /// What a watch that has seen up to `since` hasn't seen of `chat`.
    fn read(&mut self, chat: ChatId, since: Option<ChangeNo>, subscribe: bool) -> Option<Seen<A>> {
        let closed = self.core.chat(chat)?.state == ChatState::Closed;
        let (update, latest) = match self.core.read(chat, since)? {
            Read::Changes([]) => (None, since),
            Read::Changes(changes) => (
                Some(Update::Changes(changes.to_vec())),
                changes.last().map(|last| last.no),
            ),
            Read::Picture { chat, latest } => (Some(Update::Picture(chat.clone())), Some(latest)),
            // No other kind of read is given yet.
            _ => return None,
        };
        let feed = subscribe.then(|| {
            let feed = self.feeds.entry(chat).or_insert_with(|| Feed {
                wake: watch::Sender::new(()),
                last: Arc::default(),
            });
            Subscription {
                wake: feed.wake.subscribe(),
                last: Arc::clone(&feed.last),
            }
        });
        Some(Seen {
            update,
            latest,
            closed,
            feed,
        })
    }
}

/// Whether `event` holds something a recording must never have: Claude's
/// sign-in status, which holds the account's email.
fn private<A: AppItem>(event: &Event<A>) -> bool {
    let Event::Agent {
        event: AgentEvent::Frame(Frame::Message(message)),
        ..
    } = event
    else {
        return false;
    };
    matches!(message, Message::Notification { method, .. } | Message::Request { method, .. } if method == "_auth/status_update")
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

/// Writes a recording, on a thread of its own.
struct Recorder {
    lines: std::sync::mpsc::Sender<String>,
    thread: std::thread::JoinHandle<()>,
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
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path);
        let file = match file {
            Ok(file) => file,
            Err(error) => {
                log::warn!(target: "caf", "can't record to {}: {error}", path.display());
                return None;
            }
        };
        let agents = (agents.iter())
            .map(|agent| {
                let mut agent = agent.clone();
                agent.command.env.clear();
                agent
            })
            .collect();
        let header = Header {
            caf_recording: 1,
            host,
            agents,
        };
        let header = serde_json::to_string(&header).unwrap_or_default();
        let (lines, inbox) = std::sync::mpsc::channel();
        let _ = lines.send(header);
        let path = path.clone();
        let thread = std::thread::spawn(move || write_lines(&path, file, &inbox));
        Some(Self { lines, thread })
    }

    /// Writes one event.
    fn record<A: AppItem>(&mut self, now: Now, event: &Event<A>) {
        match serde_json::to_string(&Step { now, event }) {
            Ok(line) => {
                let _ = self.lines.send(line);
            }
            Err(error) => log::warn!(target: "caf", "can't record an event: {error}"),
        }
    }

    /// Waits for everything to be written.
    async fn finish(self) {
        drop(self.lines);
        let thread = self.thread;
        let _ = tokio::task::spawn_blocking(move || thread.join()).await;
    }
}

/// Writes lines as they come until there are no more, flushing whenever
/// none are waiting. After a failure, lines are read and dropped, so CAF
/// never waits on them.
fn write_lines(path: &std::path::Path, file: File, inbox: &std::sync::mpsc::Receiver<String>) {
    fn line(file: &mut BufWriter<File>, line: &str) -> std::io::Result<()> {
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")
    }
    let mut file = Some(BufWriter::new(file));
    while let Ok(first) = inbox.recv() {
        let Some(writing) = &mut file else { continue };
        let mut result = line(writing, &first);
        while result.is_ok()
            && let Ok(next) = inbox.try_recv()
        {
            result = line(writing, &next);
        }
        if let Err(error) = result.and_then(|()| writing.flush()) {
            log::warn!(target: "caf", "stopped recording to {}: {error}", path.display());
            file = None;
        }
    }
}
