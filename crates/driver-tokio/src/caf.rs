//! The wiring: one task owning the core and its drivers, and handles for
//! the app.
//!
//! # How it works
//!
//! [`Caf::start`] spawns one task that owns the [`Core`], the
//! [`AgentDriver`], the [`TimerDriver`], the [`SetupDriver`], the store
//! ([`Config::store`]), the usage driver ([`Config::usage`]) and the
//! [`Policy`]. Handles talk to it through a queue, and it waits on seven
//! things at once: a handle's command, an adapter's event, the timer, the
//! setup driver's report, the store's answer, the usage driver's prices,
//! and a policy's answer. Each
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
//! | `Setup` | [`SetupDriver::apply`]; its reports come back as [`Event::Setup`] |
//! | `Storage` | [`caf_save::carry_out`] with [`Config::store`], on a thread of its own, in order; its reports come back as [`Event::Storage`] |
//! | `Usage` | [`caf_usage::Ledger::record`] and [`caf_usage::limits::save`] with [`Config::usage`], on a thread of its own, in order. The prices, read on the same thread as it starts and whenever they change, come back as [`Event::Prices`]; the limits kept, read after them, and those a save found news in, as [`Event::Limits`] |
//! | `Report` | The [`log`] facade, target `caf` |
//!
//! A [`Steps`] given to [`Caf::start_watching`] sees each event once the
//! core has it, with its directives, before they go to their drivers.
//!
//! As it starts, before the task runs, CAF calls [`Core::check_agents`],
//! recorded as [`HostCommand::CheckAgents`], so the setup driver's survey
//! is under way by the time anything else happens. After each event, the
//! [`AgentsWatch`]es are told if any agent's status changed: they read
//! [`Core::agents`] through a `watch` channel, so a slow one only ever
//! misses statuses that were replaced. [`ChatsWatch`]es and
//! [`SavedWatch`]es are told the same way, after each host's call and
//! each answer from the store: from [`Core::summaries`], and from
//! [`Core::saved`] for each shelf listed.
//!
//! A restore is answered once [`Core::restoring`] says it's over: loaded,
//! as [`Core::chat`] then says, or failed, saying why as the store did.
//!
//! Asking for the prices ([`Caf::prices`], and [`Caf::usage`], which
//! prices a summary with them) waits for the usage driver's first table,
//! so a summary asked for as CAF starts is never priced with none: for at
//! most ten seconds, after which the core is given the bundled snapshot.
//! Before a summary, the usage driver reads the prices again, as another
//! app may have set one, and the core is given them if they changed; those
//! asked for while a reading is under way wait for one more after it, so
//! a price set meanwhile is never missed. A reading is waited for for at
//! most ten seconds too. The summary is then read on a blocking thread, apart
//! from the task. A price set ([`Caf::set_price`]) is answered once the
//! core has the prices it changed, or, if the usage ledger doesn't answer
//! in ten seconds, that it didn't. Shutting down waits for the usage
//! ledger to be written for at most thirty seconds.
//!
//! Watches pull rather than being pushed to: a watch asks for what
//! changed since it last read, so the only changes held for it are the
//! core's own list, which is bounded
//! ([`Limits::kept_changes`](caf_core::host::Limits::kept_changes)). A
//! watch further behind than that gets the whole chat again.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use caf_core::Shelf;
use caf_core::acp::{Frame, Message};
use caf_core::agent::AgentStatus;
use caf_core::agent::{AgentSpec, EnvChange, Launch, McpServer};
use caf_core::chat::{ChatState, UserMessage};
use caf_core::directive::{
    Level, PolicyDirective, StorageDirective, TimerDirective, UsageDirective, ViewerDirective,
};
use caf_core::event::{AgentEvent, HostCommand, PolicyAnswer, StorageEvent};
use caf_core::ids::{AgentName, ChangeNo};
use caf_core::question::Answer;
use caf_core::save::{CannotRestore, RestoreChat, SavedChat, SavedState};
use caf_core::settings::SettingChoice;
use caf_core::usage::{BadQuery, ModelPrice, PriceTable, Summary, UsageQuery};
use caf_core::viewer::ChatSummary;
use caf_core::{
    AppData, AppItem, Chat, ChatId, Core, Directive, Event, Host, NoAppData, NoAppItems, Now,
    Numbered, OpenChat, QuestionId, Read, Refused, Report, Ticket, TurnId,
};
use caf_save::Store;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle, JoinSet};

use crate::agent::{AgentDriver, AgentLimits};
use crate::policy::{AlwaysAsk, Policy};
use crate::setup::SetupDriver;
use crate::steps::{Step, Steps};
use crate::storage::StorageDriver;
use crate::timer::TimerDriver;
use crate::usage::{Report as UsageReport, UsageDriver};

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
    ///   account's email: its params are written as the kind of sign-in
    ///   alone, `{"authStatus": {"kind": "account"}}`, which is all the
    ///   core reads, or `{}` if they have none.
    /// - What the user pastes into a sign-in ([`Caf::paste`]) is a
    ///   one-time code: it's written as `{"agent": …, "text": null}`. A
    ///   reader replaying the file takes that `null` as text it doesn't
    ///   have, such as an empty string.
    /// - Each agent's arguments are left out, and its environment
    ///   variables are written with their names but not their values.
    /// - Each MCP server's environment variables and HTTP headers, for a
    ///   chat opened or restored, are written with their names but not
    ///   their values.
    ///
    /// **What's kept**, as it came: the host, each agent's name, program,
    /// folder and profile; what the user sent; everything the agent said;
    /// its error output, exactly, as text where a piece of it is UTF-8 and
    /// as bytes where it isn't, such as one that ends partway through a
    /// character; folders; each MCP server's name, program, arguments and
    /// URL; and what the setup driver reported, with the folder installs
    /// live in, and a sign-in's link and code (each good for one sign-in
    /// only, and of no use without the command that asked for it); every
    /// chat restored, whole, as the store read it; and the app's data on
    /// each chat, each time it's set. So don't put secrets in MCP
    /// arguments or URLs, or in the app's data, and treat a recording as
    /// private: read it through before sharing it. Names of
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
    /// The folder agents CAF installs live in, shared by every CAF program
    /// on the machine. `None` (the default) means
    /// [`SetupDriver::default_root`](crate::SetupDriver::default_root).
    pub installs: Option<PathBuf>,
    /// Where chats are kept, such as a
    /// [`FileStore`](caf_save::FileStore) in
    /// [`FileStore::default_root`](caf_save::FileStore::default_root), or
    /// the app's own. `None` (the default) keeps none: a chat opened with
    /// a shelf is refused ([`Refused::NotSaved`]), and so is restoring,
    /// listing, deleting and moving a shelf.
    ///
    /// It runs on a thread of its own, so the disk never holds CAF up.
    /// Once [`Caf::shutdown`] returns, every saved chat has been written
    /// whole and released.
    pub store: Option<Box<dyn Store>>,
    /// The usage ledger every CAF app on the machine shares, and where
    /// prices are kept: [`UsageConfig::for_user`] by default, so usage can
    /// be summed across apps without each app opting in. `None` keeps no
    /// ledger: chats still record their usage and spend, but there's no
    /// summary ([`Error::NoLedger`]), and prices are the bundled snapshot's.
    ///
    /// Tests set it to a folder of their own, or `None`, so they never
    /// write to the user's.
    pub usage: Option<UsageConfig>,
}

/// Where the usage ledger is, and whether prices are fetched.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageConfig {
    /// The ledger's folder, shared by every CAF app on the machine.
    pub folder: PathBuf,
    /// Whether to fetch LiteLLM's price table
    /// ([`caf_usage::prices::LITELLM`]) once a day, across every CAF app
    /// using `folder`. `true` by default. Without it, prices are the
    /// user's own and the snapshot bundled with CAF.
    pub fetch_prices: bool,
}

impl UsageConfig {
    /// The ledger in `folder`, fetching prices.
    #[must_use]
    pub fn new(folder: PathBuf) -> Self {
        Self {
            folder,
            fetch_prices: true,
        }
    }

    /// The ledger in the folder every CAF program shares
    /// ([`caf_usage::default_folder`]), fetching prices; `None` if there's
    /// no home folder to find it from.
    #[must_use]
    pub fn for_user() -> Option<Self> {
        caf_usage::default_folder().map(Self::new)
    }
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
            installs: None,
            store: None,
            usage: UsageConfig::for_user(),
        }
    }
}

/// CAF, running on tokio. Cheap to clone: every clone talks to the same
/// task.
///
/// Once every clone and every [`ChatHandle`] is dropped, CAF shuts down by
/// itself, as if [`Caf::shutdown`] had been called. A [`Watch`] doesn't
/// keep it running.
pub struct Caf<A: AppItem = NoAppItems, D: AppData = NoAppData> {
    commands: mpsc::Sender<Command<D>>,
    /// Watches' reads, apart from the commands, so a watch never waits
    /// for room among them: see [`Reading`].
    reads: mpsc::UnboundedSender<Reading<A>>,
    shared: Common<A>,
}

impl<A: AppItem, D: AppData> Clone for Caf<A, D> {
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
    /// Closed once the task has ended: `last` is filled in, the store let
    /// go and the recording written.
    ended: watch::Receiver<()>,
    /// The agents' names, in order.
    agents: Arc<[AgentName]>,
    /// The open chats, in the order they were opened.
    chats: watch::Receiver<Vec<ChatSummary>>,
    /// Where every agent stands, in the order they were offered.
    statuses: watch::Receiver<Vec<AgentStatus>>,
    /// The saved chats on every shelf listed: `None` until one is.
    saved: watch::Receiver<Option<Vec<SavedChat>>>,
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
            statuses: self.statuses.clone(),
            saved: self.saved.clone(),
            run: Arc::clone(&self.run),
        }
    }
}

/// A handle on CAF that doesn't keep it running, as a [`Watch`] holds.
pub(crate) struct WeakCaf<A: AppItem, D: AppData> {
    commands: mpsc::WeakSender<Command<D>>,
    reads: mpsc::WeakUnboundedSender<Reading<A>>,
    shared: Common<A>,
}

impl<A: AppItem, D: AppData> WeakCaf<A, D> {
    /// A handle that keeps CAF running while it's held, unless CAF has
    /// shut down.
    pub(crate) fn upgrade(&self) -> Option<Caf<A, D>> {
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

    /// See [`Caf::watch_agents`].
    pub(crate) fn watch_agents(&self) -> AgentsWatch {
        AgentsWatch {
            list: self.shared.statuses.clone(),
            first: true,
        }
    }

    /// See [`Caf::watch_saved`].
    pub(crate) fn watch_saved(&self) -> SavedWatch {
        SavedWatch {
            list: self.shared.saved.clone(),
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
        let serial = last.made;
        last.made += 1;
        drop(last);
        Watch {
            reads: self.reads.clone(),
            last: Arc::clone(&self.shared.last),
            caf_ended: self.shared.ended.clone(),
            chat,
            serial,
            since,
            wake: None,
            ended: false,
            counted,
        }
    }

    /// Restores a saved chat, as [`Caf::restore`] does, keeping CAF
    /// running only while it asks: so a viewer's restore doesn't keep it
    /// running while the store reads.
    pub(crate) fn restore(
        &self,
        restore: RestoreChat,
    ) -> impl Future<Output = Result<ChatId, Error>> + Send + 'static {
        let commands = self.commands.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            let commands = commands.upgrade().ok_or(Error::Gone)?;
            let sent = commands.send(Command::Restore { restore, reply }).await;
            drop(commands);
            sent.map_err(|_| Error::Gone)?;
            // The task drops the reply when it's shutting down.
            match answer.await.map_err(|_| Error::Gone)?? {
                Ok(chat) => Ok(chat),
                Err(why) => Err(Error::CannotRestore(why)),
            }
        }
    }

    /// The ledger's folder, if there is one, and the prices in force,
    /// once the core has the usage driver's first, and if `fresh`, read
    /// again: keeping CAF running only while it asks.
    pub(crate) fn priced(
        &self,
        fresh: bool,
    ) -> impl Future<Output = Result<Priced, Error>> + Send + 'static {
        let commands = self.commands.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            let commands = commands.upgrade().ok_or(Error::Gone)?;
            let sent = commands.send(Command::Prices { fresh, reply }).await;
            drop(commands);
            sent.map_err(|_| Error::Gone)?;
            // The task drops the reply when it's shutting down.
            answer.await.map_err(|_| Error::Gone)
        }
    }

    /// A usage summary, as [`Caf::usage`] makes one, keeping CAF running
    /// only while it asks for the prices: so a viewer's summary doesn't
    /// keep it running while the ledger is read.
    pub(crate) fn usage(
        &self,
        query: UsageQuery,
    ) -> impl Future<Output = Result<Summary, Error>> + Send + 'static {
        // Read again, so a price another app set is taken.
        let priced = self.priced(true);
        async move {
            let (folder, prices) = priced.await?;
            let folder = folder.ok_or(Error::NoLedger)?;
            let read =
                tokio::task::spawn_blocking(move || caf_usage::summarise(&folder, &query, &prices));
            match read.await {
                Ok(summary) => summary.map_err(Error::BadQuery),
                // Shutting down, or a bug.
                Err(error) => {
                    if error.is_panic() {
                        log::error!(target: "caf", "reading a usage summary panicked");
                    }
                    Err(Error::Gone)
                }
            }
        }
    }

    /// Sets a price of the user's own, as [`Caf::set_price`] does, keeping
    /// CAF running only while it asks.
    pub(crate) fn set_price(
        &self,
        model: String,
        price: Option<ModelPrice>,
    ) -> impl Future<Output = Result<(), Error>> + Send + 'static {
        let commands = self.commands.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            let commands = commands.upgrade().ok_or(Error::Gone)?;
            let setting = Command::SetPrice {
                model,
                price,
                reply,
            };
            let sent = commands.send(setting).await;
            drop(commands);
            sent.map_err(|_| Error::Gone)?;
            // Dropped when the task is shutting down.
            answer.await.map_err(|_| Error::Gone)?
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
    /// A saved chat couldn't be restored. Its save is left as it was.
    CannotRestore(CannotRestore),
    /// CAF keeps no usage ledger ([`Config::usage`] is `None`), so there's
    /// no summary, and no price of the user's own can be set.
    NoLedger,
    /// A usage summary can't be made for this query.
    BadQuery(BadQuery),
    /// The user's own price couldn't be written, as the system said.
    Price(String),
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
            Self::CannotRestore(why) => write!(f, "CAF couldn't restore the chat: {why:?}"),
            Self::NoLedger => f.write_str("CAF keeps no usage ledger"),
            Self::BadQuery(why) => write!(f, "no usage summary for that query: {why:?}"),
            Self::Price(why) => write!(f, "CAF couldn't set the price: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// How many commands may wait for the task before a handle waits to
/// send one.
const COMMANDS: usize = 64;

impl<A: AppItem, D: AppData> Caf<A, D> {
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
    /// and headers, and what the user pastes into a sign-in: treat them as
    /// private, and never write them out as they are. See
    /// [`Steps`](crate::Steps).
    ///
    /// # Panics
    ///
    /// As [`Caf::start`].
    #[must_use]
    pub fn start_watching(config: Config, steps: impl Steps<A, D>) -> (Self, Running) {
        Self::start_with(config, Some(Box::new(steps)))
    }

    /// Starts CAF, with `steps` seeing every step if there are any.
    fn start_with(config: Config, steps: Option<Box<dyn Steps<A, D>>>) -> (Self, Running) {
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
        let root = config
            .installs
            .or_else(SetupDriver::default_root)
            .unwrap_or_else(|| {
                let root = std::env::temp_dir().join("caf/agents");
                log::warn!(
                    target: "caf",
                    "there's no home folder for installs: using {}, which may not last",
                    root.display()
                );
                root
            });
        let usage = UsageDriver::new(config.usage, config.host.name.clone());
        let core = Core::new(config.host, config.agents);
        let (told, statuses) = watch::channel(core.agents());
        let (listed, saved) = watch::channel(None);
        let mut task = Task {
            core,
            agents: AgentDriver::new(config.agent_limits),
            timers: TimerDriver::new(),
            setup: SetupDriver::new(root),
            storage: StorageDriver::new(config.store),
            usage,
            priced: false,
            unpriced: Vec::new(),
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
            statuses: told,
            listed,
            shelves: Vec::new(),
            restores: Restores::default(),
            put_away: HashMap::new(),
            numbered: HashMap::new(),
            lists_due: false,
            inbox,
            listening: true,
            reads: reading,
            reading: true,
            shutting_down: false,
            waiting: Vec::new(),
            out: Vec::new(),
        };
        task.check_agents();
        let task = tokio::spawn(task.run());
        let caf = Self {
            commands,
            reads,
            shared: Common {
                last,
                ended,
                agents,
                chats,
                statuses,
                saved,
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

    /// Follows where every agent stands, installed or not and signed in or
    /// not: the whole list at once, then again each time any of it
    /// changes. CAF looks at what's installed as it starts.
    #[must_use]
    pub fn watch_agents(&self) -> AgentsWatch {
        self.downgrade().watch_agents()
    }

    /// Looks again at what's installed and what the machine has, such as
    /// after the user installed Node. See [`Core::check_agents`].
    ///
    /// # Errors
    ///
    /// [`Error::Gone`].
    ///
    /// [`Core::check_agents`]: caf_core::Core::check_agents
    pub async fn check_agents(&self) -> Result<(), Error> {
        self.ask(|reply| Command::CheckAgents { reply }).await
    }

    /// Installs an agent. See [`Core::install`]; progress shows in
    /// [`Caf::watch_agents`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    ///
    /// [`Core::install`]: caf_core::Core::install
    pub async fn install(&self, agent: &AgentName) -> Result<(), Error> {
        self.about(agent, AgentCall::Install).await
    }

    /// Stops installing an agent. See [`Core::stop_install`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    ///
    /// [`Core::stop_install`]: caf_core::Core::stop_install
    pub async fn stop_install(&self, agent: &AgentName) -> Result<(), Error> {
        self.about(agent, AgentCall::StopInstall).await
    }

    /// Starts signing in to an agent. See [`Core::sign_in`]; the link to
    /// show shows in [`Caf::watch_agents`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    ///
    /// [`Core::sign_in`]: caf_core::Core::sign_in
    pub async fn sign_in(&self, agent: &AgentName) -> Result<(), Error> {
        self.about(agent, AgentCall::SignIn).await
    }

    /// Gives a sign-in what the user pasted. See [`Core::paste`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    ///
    /// [`Core::paste`]: caf_core::Core::paste
    pub async fn paste(&self, agent: &AgentName, text: String) -> Result<(), Error> {
        self.about(agent, AgentCall::Paste(text)).await
    }

    /// Asks the task to do `call` about `agent`.
    async fn about(&self, agent: &AgentName, call: AgentCall) -> Result<(), Error> {
        let agent = agent.clone();
        (self.ask(|reply| Command::Agent { agent, call, reply })).await
    }

    /// Stops a sign-in. See [`Core::stop_sign_in`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    ///
    /// [`Core::stop_sign_in`]: caf_core::Core::stop_sign_in
    pub async fn stop_sign_in(&self, agent: &AgentName) -> Result<(), Error> {
        self.about(agent, AgentCall::StopSignIn).await
    }

    /// Signs out of an agent. See [`Core::sign_out`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    ///
    /// [`Core::sign_out`]: caf_core::Core::sign_out
    pub async fn sign_out(&self, agent: &AgentName) -> Result<(), Error> {
        self.about(agent, AgentCall::SignOut).await
    }

    /// Learns what settings an agent offers, such as its models, to offer
    /// them before any chat with it; they show in [`Caf::watch_agents`].
    /// See [`Core::learn_settings`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    ///
    /// [`Core::learn_settings`]: caf_core::Core::learn_settings
    pub async fn learn_settings(&self, agent: &AgentName) -> Result<(), Error> {
        self.about(agent, AgentCall::LearnSettings).await
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
    pub(crate) fn downgrade(&self) -> WeakCaf<A, D> {
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
    /// [`Error::Refused`] with the core's reason, or [`Refused::NotSaved`]
    /// for a chat with a shelf and no [`Config::store`]; or
    /// [`Error::Gone`].
    pub async fn open_chat(&self, open: OpenChat) -> Result<ChatHandle<A, D>, Error> {
        let chat = self.ask(|reply| Command::Open { open, reply }).await?;
        Ok(self.chat(chat))
    }

    /// A handle on a chat that's already open.
    #[must_use]
    pub fn chat(&self, chat: ChatId) -> ChatHandle<A, D> {
        ChatHandle {
            id: chat,
            caf: self.clone(),
        }
    }

    /// Lists a shelf's saved chats. They come in [`Caf::watch_saved`]. See
    /// [`Core::list_saved`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Refused::NotSaved`]
    /// with no [`Config::store`]; or [`Error::Gone`].
    ///
    /// [`Core::list_saved`]: caf_core::Core::list_saved
    pub async fn list_saved(&self, shelf: Shelf) -> Result<(), Error> {
        self.ask(|reply| Command::ListSaved { shelf, reply }).await
    }

    /// Follows the saved chats on every shelf listed, loaded or not: the
    /// whole list at once, then again each time it changes.
    #[must_use]
    pub fn watch_saved(&self) -> SavedWatch {
        self.downgrade().watch_saved()
    }

    /// Restores a saved chat, and returns its handle once it's loaded.
    /// See [`Core::restore`].
    ///
    /// # Errors
    ///
    /// [`Error::CannotRestore`] saying why; [`Error::Refused`] with the
    /// core's reason, or [`Refused::NotSaved`] with no [`Config::store`];
    /// or [`Error::Gone`].
    ///
    /// [`Core::restore`]: caf_core::Core::restore
    pub async fn restore(&self, restore: RestoreChat) -> Result<ChatHandle<A, D>, Error> {
        let chat = self.downgrade().restore(restore).await?;
        Ok(self.chat(chat))
    }

    /// Deletes a chat for good, loaded or not. See [`Core::delete`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Refused::NotSaved`]
    /// with no [`Config::store`]; or [`Error::Gone`].
    ///
    /// [`Core::delete`]: caf_core::Core::delete
    pub async fn delete(&self, chat: ChatId, shelf: Shelf) -> Result<(), Error> {
        self.ask(|reply| Command::Delete { chat, shelf, reply })
            .await
    }

    /// Moves every chat on a shelf to another. See [`Core::move_shelf`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Refused::NotSaved`]
    /// with no [`Config::store`]; or [`Error::Gone`].
    ///
    /// [`Core::move_shelf`]: caf_core::Core::move_shelf
    pub async fn move_shelf(&self, from: Shelf, to: Shelf) -> Result<(), Error> {
        self.ask(|reply| Command::MoveShelf { from, to, reply })
            .await
    }

    /// Usage over a period, from every CAF app's lines in the ledger: what
    /// a usage page is drawn from. Read on a blocking thread, apart from
    /// CAF's task, so a summary never holds up a chat; turns that ended a
    /// moment ago may not be in it yet. It's priced with the prices as
    /// they are on disk, so a price another app set counts, and every
    /// chat's spend is worked out again with them if they changed. See
    /// [`caf_usage::summarise`].
    ///
    /// # Errors
    ///
    /// [`Error::NoLedger`] with no [`Config::usage`];
    /// [`Error::BadQuery`]; or [`Error::Gone`], which also covers reading
    /// the summary failing unexpectedly, a bug, which is logged.
    pub async fn usage(&self, query: UsageQuery) -> Result<Summary, Error> {
        self.downgrade().usage(query).await
    }

    /// Sets the user's own price for a model, by the agent's id for it,
    /// or with `None` clears it, for every CAF app using the same ledger.
    /// Every chat's spend is worked out again with it. See
    /// [`caf_usage::prices::set_own`].
    ///
    /// # Errors
    ///
    /// [`Error::NoLedger`] with no [`Config::usage`]; [`Error::Price`] if
    /// it couldn't be written, or the usage ledger didn't answer in ten
    /// seconds; or [`Error::Gone`]. One asked for as CAF shuts down is
    /// [`Error::Gone`] even if the price was written.
    pub async fn set_price(&self, model: String, price: Option<ModelPrice>) -> Result<(), Error> {
        self.downgrade().set_price(model, price).await
    }

    /// The prices CAF estimates cost with now: the user's own, then
    /// LiteLLM's as last fetched, then the bundled snapshot.
    ///
    /// # Errors
    ///
    /// [`Error::Gone`].
    pub async fn prices(&self) -> Result<PriceTable, Error> {
        let (_, prices) = self.downgrade().priced(false).await?;
        Ok(prices)
    }

    /// Stops everything: every session closed, every adapter stopped,
    /// every saved chat written whole and let go in the store.
    ///
    /// Returns once every adapter has exited and been reaped, and what it
    /// started has been sent SIGKILL, and every saved chat has been
    /// written whole and released: called again, or after CAF has begun to
    /// end by itself, once all that is done. From the moment it's called, anything else asked of CAF is
    /// [`Error::Gone`], but watches go on getting what changes.
    pub async fn shutdown(&self) {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::Shutdown { reply })
            .await
            .is_ok()
            && done.await.is_ok()
        {
            return;
        }
        // The task no longer takes commands: it's ending, and may still be
        // letting the store go.
        self.downgrade().ended().await;
    }

    /// Sends the task a command that takes a reply, and waits for it.
    async fn ask<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, Refused>>) -> Command<D>,
    ) -> Result<T, Error> {
        let (reply, answer) = oneshot::channel();
        (self.commands.send(command(reply)).await).map_err(|_| Error::Gone)?;
        // The task drops the reply when it's shutting down.
        Ok(answer.await.map_err(|_| Error::Gone)??)
    }
}

/// A handle on one chat.
pub struct ChatHandle<A: AppItem = NoAppItems, D: AppData = NoAppData> {
    /// The chat's id.
    pub id: ChatId,
    caf: Caf<A, D>,
}

impl<A: AppItem, D: AppData> ChatHandle<A, D> {
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

    /// Closes the chat: the user is done with it. See
    /// [`caf_core::Core::close_chat`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn close(self) -> Result<(), Error> {
        let chat = self.id;
        self.caf.ask(|reply| Command::Close { chat, reply }).await
    }

    /// Unarchives the chat: it's no longer archived, and its agent starts
    /// when a message is sent. See [`caf_core::Core::unarchive`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn unarchive(&self) -> Result<(), Error> {
        let chat = self.id;
        self.caf
            .ask(|reply| Command::Unarchive { chat, reply })
            .await
    }

    /// Chooses a setting of the chat's agent, such as its model. See
    /// [`caf_core::Core::set_setting`]; the chat's settings show it
    /// waiting until the agent answers.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn set_setting(&self, choice: SettingChoice) -> Result<(), Error> {
        let chat = self.id;
        (self.caf)
            .ask(|reply| Command::SetSetting {
                chat,
                choice,
                reply,
            })
            .await
    }

    /// Saves the chat whole and lets it go from memory, to be restored
    /// later. See [`caf_core::Core::put_away`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn put_away(self) -> Result<(), Error> {
        let chat = self.id;
        self.caf.ask(|reply| Command::PutAway { chat, reply }).await
    }

    /// Sets the chat's data slot, whole. See
    /// [`caf_core::Core::set_data`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn set_data(&self, data: D) -> Result<(), Error> {
        let chat = self.id;
        (self.caf)
            .ask(|reply| Command::SetData { chat, data, reply })
            .await
    }

    /// The chat's data slot. See [`caf_core::Core::data`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with [`Refused::NoSuchChat`], or [`Error::Gone`].
    pub async fn data(&self) -> Result<D, Error> {
        let chat = self.id;
        self.caf.ask(|reply| Command::Data { chat, reply }).await
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
    /// Which watch it is, counting from the first made: so a chat put
    /// away is shown, as it was left, only to watches made before.
    serial: u64,
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
                // Once CAF has ended, nothing else is kept for it.
                if last.ended {
                    last.chats.remove(&self.chat);
                    last.left.remove(&self.chat);
                }
            }
        }
    }
}

/// Follows where every agent stands. Like [`Watch`], it doesn't keep CAF
/// running.
pub struct AgentsWatch {
    list: watch::Receiver<Vec<AgentStatus>>,
    /// It hasn't given the list yet.
    first: bool,
}

impl AgentsWatch {
    /// Every agent's status, in the order they were offered: at once the
    /// first time, then each time any changes. If they change more than
    /// once between calls, only the latest is given. `None` once CAF has
    /// shut down.
    ///
    /// Cancel-safe: dropped before it resolves, it loses nothing.
    pub async fn next(&mut self) -> Option<Vec<AgentStatus>> {
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

/// Follows the saved chats on every shelf listed. Like [`Watch`], it
/// doesn't keep CAF running.
pub struct SavedWatch {
    list: watch::Receiver<Option<Vec<SavedChat>>>,
    /// It hasn't given the list yet.
    first: bool,
}

impl SavedWatch {
    /// The saved chats on every shelf listed, in the order they were
    /// made, whatever shelf each is on: at once the first time, then each time it
    /// changes. If it changes more than once between calls, only the
    /// latest is given. `None` once CAF has shut down.
    ///
    /// Cancel-safe: dropped before it resolves, it loses nothing.
    pub async fn next(&mut self) -> Option<Vec<SavedChat>> {
        Some(self.next_listed().await?.unwrap_or_default())
    }

    /// As [`SavedWatch::next`], but `None` inside until any shelf is
    /// listed, rather than an empty list: so a viewer is sent the list
    /// only once there is one.
    pub(crate) async fn next_listed(&mut self) -> Option<Option<Vec<SavedChat>>> {
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

/// Follows which chats are open: opened by the host, by any viewer, or
/// closed, which archives them. An archived chat isn't open, until it's
/// unarchived. Like [`Watch`], it doesn't keep CAF running.
pub struct ChatsWatch {
    list: watch::Receiver<Vec<ChatSummary>>,
    /// It hasn't given the list yet.
    first: bool,
}

impl ChatsWatch {
    /// Has the next call give the list again, even if it hasn't changed:
    /// so a viewer told a chat is open hears a list with it after.
    pub(crate) fn again(&mut self) {
        self.list.mark_changed();
    }

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
// A picture is the chat itself, as a watcher reads it: boxing it would
// change the frozen API for a few hundred bytes.
#[allow(clippy::large_enum_variant)]
pub enum Update<A: AppItem = NoAppItems> {
    /// The whole chat, as it is: at the start, and after falling behind.
    Picture(Chat<A>),
    /// What changed since the last update, oldest first.
    Changes(Vec<Numbered<A>>),
}

impl<A: AppItem> Watch<A> {
    /// The next update, or `None` once the chat is put away or deleted,
    /// a saved chat closed and so put away included, or CAF has shut
    /// down. Closing a chat that isn't saved archives it, and its watch
    /// goes on, as it may be unarchived ([`ChatHandle::unarchive`]); so
    /// does a watch of an archived chat restored.
    ///
    /// When CAF shuts down, a watch that hasn't seen how it left the chat
    /// gets that as one last [`Update::Picture`], even if it never read
    /// before. So does a watch of a chat put away, or closed and so put
    /// away, as it's let go from memory: the chat as it was written whole.
    /// That copy is kept while any watch of the chat is, so drop a watch
    /// once it has ended.
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
            // A read of a chat the core has always gives a way to wait.
            let wake = self.wake.as_mut()?;
            if wake.changed().await.is_err() {
                // The chat is gone or CAF has ended: the next read says
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
            serial: self.serial,
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
        let Some((chat, latest)) = last.chats.get(&self.chat) else {
            // Put away before CAF ended, and not yet seen.
            let left = (last.left.get(&self.chat)).filter(|left| self.serial < left.before)?;
            self.since = left.latest.max(self.since);
            return Some(Update::Picture(left.chat.clone()));
        };
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
enum Command<D: AppData> {
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
    Unarchive {
        chat: ChatId,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
    CheckAgents {
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    Agent {
        agent: AgentName,
        call: AgentCall,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    ListSaved {
        shelf: Shelf,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    /// Answered once the chat is loaded or has failed to be.
    Restore {
        restore: RestoreChat,
        reply: RestoreReply,
    },
    PutAway {
        chat: ChatId,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    Delete {
        chat: ChatId,
        shelf: Shelf,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    MoveShelf {
        from: Shelf,
        to: Shelf,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    SetSetting {
        chat: ChatId,
        choice: SettingChoice,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    SetData {
        chat: ChatId,
        data: D,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    Data {
        chat: ChatId,
        reply: oneshot::Sender<Result<D, Refused>>,
    },
    /// Answered once the usage driver has given the core its first
    /// prices; if `fresh`, once it has read them again, as another app
    /// may have changed them, and given them to the core if they changed.
    Prices {
        fresh: bool,
        reply: oneshot::Sender<Priced>,
    },
    /// Answered once the core has the prices it changed.
    SetPrice {
        model: String,
        price: Option<ModelPrice>,
        reply: oneshot::Sender<Result<(), Error>>,
    },
}

/// The ledger's folder, if there is one, and the prices in force: what a
/// summary is read with.
pub(crate) type Priced = (Option<PathBuf>, PriceTable);

/// Something a handle asks the task to do about an agent.
enum AgentCall {
    Install,
    StopInstall,
    SignIn,
    Paste(String),
    StopSignIn,
    SignOut,
    LearnSettings,
}

/// A watch's read: what changed in `chat` since `since`, and a way to
/// hear of more if `subscribe`.
///
/// Reads have a channel of their own, without a bound, so a watch never
/// waits to send one. Each watch waits on at most one, and the task
/// never waits to answer it.
struct Reading<A: AppItem> {
    chat: ChatId,
    /// The watch's [`Watch::serial`].
    serial: u64,
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
    /// The chat is gone from the core: nothing more will change.
    closed: bool,
    /// How to hear of changes, if it asked and the core has the chat.
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
    /// Each of them put away, as it was left, and not loaded again, while
    /// watched. Empty until the task ends. Only watches made before it
    /// was put away are shown it, as each reads it.
    left: HashMap<ChatId, Left<A>>,
    /// The task has ended, and `chats` is filled in.
    ended: bool,
    /// How many watches have been made: the next one's serial.
    made: u64,
}

impl<A: AppItem> Default for Last<A> {
    fn default() -> Self {
        Self {
            watched: HashMap::new(),
            chats: HashMap::new(),
            left: HashMap::new(),
            ended: false,
            made: 0,
        }
    }
}

/// Where a restore is answered: once it's over, with the chat loaded, or
/// why not; or at once, refused.
type RestoreReply = oneshot::Sender<Result<Result<ChatId, CannotRestore>, Refused>>;

/// A chat put away, as it was left, for the watches of it there were.
struct Left<A: AppItem> {
    chat: Chat<A>,
    /// The number of its last change viewers were told of. What it was
    /// left with after that, such as being closed, has no number.
    latest: Option<ChangeNo>,
    /// The serial of the first watch made after it was put away, which
    /// isn't shown it.
    before: u64,
}

/// The restores the host waits for, and what the store found for each.
#[derive(Default)]
struct Restores {
    /// Each restore the host waits for, by chat: one at a time for a
    /// chat, as the core refuses another while one is under way.
    waiting: HashMap<ChatId, Waiting>,
    /// Each load given to the store and not yet answered: its chat, and
    /// the shelf it's read from.
    loads: HashMap<Ticket, (ChatId, Shelf)>,
    /// The loads given out while the host's restore is taken, before it
    /// waits: one of them may be its.
    fresh: Vec<Ticket>,
    /// A host's restore is being taken: loads go in `fresh`.
    taking: bool,
}

/// A restore the host waits for.
struct Waiting {
    reply: RestoreReply,
    /// Its load, once it's given to the store: it may wait for a move of
    /// its shelf. An answer to any other load, such as one of a restore
    /// of the chat dropped before, isn't this one's.
    load: Option<Ticket>,
    /// What the store found, once its load is answered: its shelf, and
    /// why it couldn't be read, or `None` if it was.
    found: Option<(Shelf, Option<CannotRestore>)>,
}

impl Restores {
    /// Notes a load given to the store.
    fn loading(&mut self, ticket: Ticket, chat: ChatId, shelf: Shelf) {
        self.loads.insert(ticket, (chat, shelf));
        if let Some(waiting) = (self.waiting.get_mut(&chat)).filter(|each| each.load.is_none()) {
            waiting.load = Some(ticket);
        } else if self.taking {
            self.fresh.push(ticket);
        }
    }

    /// Waits for a restore of `chat`, just taken.
    fn wait(&mut self, chat: ChatId, reply: RestoreReply) {
        let fresh = std::mem::take(&mut self.fresh);
        let load = (fresh.into_iter())
            .find(|ticket| (self.loads.get(ticket)).is_some_and(|(loading, _)| *loading == chat));
        let waiting = Waiting {
            reply,
            load,
            found: None,
        };
        self.waiting.insert(chat, waiting);
    }

    /// Notes what the store found, if `ticket` is the load of a restore
    /// the host waits for.
    fn answered<A: AppItem, D: AppData>(&mut self, ticket: Ticket, event: &StorageEvent<A, D>) {
        let Some((chat, shelf)) = self.loads.remove(&ticket) else {
            return;
        };
        let Some(waiting) = (self.waiting.get_mut(&chat)).filter(|each| each.load == Some(ticket))
        else {
            return;
        };
        let why = match event {
            StorageEvent::Loaded(_) => None,
            StorageEvent::NotLoaded(why) => Some(why.clone()),
            StorageEvent::Failed(why) => Some(CannotRestore::Store(why.clone())),
            // As the core takes it.
            other => {
                let what = match other {
                    StorageEvent::Listed { .. } => "a list",
                    StorageEvent::Done { .. } => "done",
                    _ => "something else",
                };
                Some(CannotRestore::Damaged(format!(
                    "the store answered with {what}"
                )))
            }
        };
        waiting.found = Some((shelf, why));
    }
}

/// Why a restore that's over didn't load its chat, from what the store
/// found for it.
fn why_not_restored<A: AppItem, D: AppData>(
    core: &Core<A, D>,
    chat: ChatId,
    found: Option<(Shelf, Option<CannotRestore>)>,
) -> CannotRestore {
    match found {
        Some((_, Some(why))) => why,
        // The store read it, and the core found it damaged: why is in the
        // shelf's list, if it's listed, and in the core's report.
        Some((shelf, None)) => (core.saved(&shelf).into_iter().flatten())
            .find(|each| each.header.chat == chat)
            .and_then(|each| match each.state {
                SavedState::Failed(why) => Some(why),
                _ => None,
            })
            .unwrap_or_else(|| {
                CannotRestore::Damaged(
                    "CAF can't use the chat the store read: its log says why".into(),
                )
            }),
        // Over before the store answered: the chat was deleted.
        None => CannotRestore::NotFound,
    }
}

/// Refuses something that needs a store, with none.
fn not_saved<T>(reply: oneshot::Sender<Result<T, Refused>>) {
    let _ = reply.send(Err(Refused::NotSaved));
}

/// The task: the core and its drivers.
struct Task<A: AppItem, D: AppData> {
    core: Core<A, D>,
    agents: AgentDriver,
    timers: TimerDriver,
    setup: SetupDriver,
    storage: StorageDriver<A, D>,
    usage: UsageDriver,
    /// The core has been given the usage driver's first prices.
    priced: bool,
    /// Those waiting for the first prices.
    unpriced: Vec<oneshot::Sender<Priced>>,
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
    /// Dropped once the task is done, `last` filled in, the store let go
    /// and the recording written, to tell the watches and anyone waiting
    /// for it to end.
    ending: Option<watch::Sender<()>>,
    recorder: Option<Recorder>,
    /// Who sees every step, if anyone.
    steps: Option<Box<dyn Steps<A, D>>>,
    /// The open chats, for [`ChatsWatch`]es. Dropped as the task ends.
    open: watch::Sender<Vec<ChatSummary>>,
    /// Every agent's status, for [`AgentsWatch`]es. Dropped as the task
    /// ends.
    statuses: watch::Sender<Vec<AgentStatus>>,
    /// The saved chats on every shelf listed, for [`SavedWatch`]es.
    /// Dropped as the task ends.
    listed: watch::Sender<Option<Vec<SavedChat>>>,
    /// The shelves the host has listed, in the order it first did.
    shelves: Vec<Shelf>,
    /// The restores the host waits for.
    restores: Restores,
    /// Each chat some watch follows that was put away, as it was left,
    /// for its watches' last picture: kept until it's loaded again or
    /// deleted, or the step after its last watch is dropped.
    put_away: HashMap<ChatId, Left<A>>,
    /// The number of each loaded chat's latest change, as viewers were
    /// last told: what a chat put away was shown up to. Every loaded chat
    /// has one, from when it was opened or restored, so it also says which
    /// chats were loaded as the last step ended.
    numbered: HashMap<ChatId, ChangeNo>,
    /// The lists of chats, open and saved, may have changed since the
    /// watches were last told: after a host's call or the store's answer,
    /// and when the core gives the store work, as a loaded chat written is
    /// listed as it is now.
    lists_due: bool,
    inbox: mpsc::Receiver<Command<D>>,
    /// Whether any handle is left to send commands.
    listening: bool,
    reads: mpsc::UnboundedReceiver<Reading<A>>,
    /// Whether any handle is left to send reads.
    reading: bool,
    shutting_down: bool,
    /// Those waiting for shutting down to be done.
    waiting: Vec<oneshot::Sender<()>>,
    /// The core's directives, kept to save allocating.
    out: Vec<Directive<A, D>>,
}

impl<A: AppItem, D: AppData> Task<A, D> {
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
                (ticket, event) = self.setup.next() => self.give(Event::Setup { ticket, event }),
                (ticket, event) = self.storage.next() => self.stored(ticket, event),
                report = self.usage.next() => self.usage_report(report),
                () = self.timers.woken() => self.give(Event::Woken),
                Some(decided) = self.deciding.join_next_with_id() => self.decided(decided),
            }
        }
        {
            // No watch is counted from now on: the chats watched are known.
            let watched: Vec<ChatId> = {
                let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
                last.ended = true;
                last.watched.keys().copied().collect()
            };
            // Copied with the lock let go, as a chat may be big.
            let mut chats = HashMap::new();
            let mut left = HashMap::new();
            for chat in watched {
                if let Some(Read::Picture {
                    chat: whole,
                    latest,
                }) = self.core.read(chat, None)
                {
                    chats.insert(chat, (whole.clone(), latest));
                } else if let Some(away) = self.put_away.remove(&chat) {
                    left.insert(chat, away);
                }
            }
            // Only for those still watched: a watch dropped meanwhile was
            // the last of its chat's.
            let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
            let Last {
                watched,
                chats: kept,
                left: kept_left,
                ..
            } = &mut *last;
            kept.extend(
                chats
                    .into_iter()
                    .filter(|(chat, _)| watched.contains_key(chat)),
            );
            kept_left.extend(
                left.into_iter()
                    .filter(|(chat, _)| watched.contains_key(chat)),
            );
        }
        // Nothing more is taken, so no watch waits on a read sent from
        // now on. Then closing every watch's wake-up channel tells them
        // it's over.
        self.inbox.close();
        self.reads.close();
        self.feeds.clear();
        // The store lets go of the chats the core released last before
        // its thread ends, so once shutting down returns, they're free.
        let storage = std::mem::replace(&mut self.storage, StorageDriver::new(None));
        storage.finish().await;
        // Every line the core gave it is written, or reported lost.
        self.usage.finish().await;
        if let Some(recorder) = self.recorder.take() {
            recorder.finish().await;
        }
        // Last, so a shutdown that finds the task ending waits for all of
        // that too.
        self.ending = None;
        for waiting in self.waiting.drain(..) {
            let _ = waiting.send(());
        }
    }

    /// Carries out a handle's command.
    fn command(&mut self, command: Command<D>) {
        // Once shutting down, none is carried out: dropping the reply tells
        // the handle CAF is gone. Reads are still answered.
        match command {
            Command::Shutdown { reply } => {
                self.waiting.push(reply);
                self.shutdown();
            }
            _ if self.shutting_down => {}
            Command::Open { open, reply } if open.shelf.is_some() && !self.storage.has_store() => {
                not_saved(reply);
            }
            Command::Open { open, reply } => {
                let recorded = self.keeping().then(|| open.clone());
                // The list has the chat by the time the host hears its id:
                // `host` tells it.
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
                let _ = reply.send(result);
            }
            Command::Unarchive { chat, reply } => {
                let result = self.host(
                    |core, now, out| core.unarchive(now, chat, out),
                    |()| Some(HostCommand::Unarchive { chat }),
                );
                let _ = reply.send(result);
            }
            Command::CheckAgents { reply } => {
                self.check_agents();
                let _ = reply.send(Ok(()));
            }
            Command::Agent { agent, call, reply } => {
                let result = self.agent(&agent, call);
                let _ = reply.send(result);
            }
            Command::SetSetting {
                chat,
                choice,
                reply,
            } => {
                let recorded = self.keeping().then(|| choice.clone());
                let result = self.host(
                    |core, now, out| core.set_setting(now, chat, choice, out),
                    |()| recorded.map(|choice| HostCommand::SetSetting { chat, choice }),
                );
                let _ = reply.send(result);
            }
            // The first prices are read fresh.
            Command::Prices { reply, .. } if !self.priced => self.unpriced.push(reply),
            Command::Prices { fresh: true, reply } => self.usage.reload(reply),
            Command::Prices { reply, .. } => {
                let _ = reply.send(self.priced_now());
            }
            Command::SetPrice {
                model,
                price,
                reply,
            } => self.usage.set_price(model, price, reply),
            command @ (Command::ListSaved { .. }
            | Command::Restore { .. }
            | Command::PutAway { .. }
            | Command::Delete { .. }
            | Command::MoveShelf { .. }
            | Command::SetData { .. }
            | Command::Data { .. }) => self.saving(command),
        }
    }

    /// Carries out a handle's command about saved chats. With no store,
    /// those that need one are refused, so the core never gives the store
    /// work.
    fn saving(&mut self, command: Command<D>) {
        let stored = self.storage.has_store();
        match command {
            Command::ListSaved { reply, .. } if !stored => not_saved(reply),
            Command::ListSaved { shelf, reply } => {
                let listing = shelf.clone();
                let result = self.host(
                    |core, now, out| core.list_saved(now, listing, out),
                    |()| Some(HostCommand::ListSaved(shelf.clone())),
                );
                if result.is_ok() && !self.shelves.contains(&shelf) {
                    self.shelves.push(shelf);
                    self.tell_saved();
                }
                let _ = reply.send(result);
            }
            Command::Restore { reply, .. } if !stored => not_saved(reply),
            Command::Restore { restore, reply } => {
                let chat = restore.chat;
                let recorded = self.keeping().then(|| restore.clone());
                self.restores.taking = true;
                let result = self.host(
                    |core, now, out| core.restore(now, restore, out),
                    |()| recorded.map(HostCommand::Restore),
                );
                self.restores.taking = false;
                match result {
                    Ok(()) => {
                        self.restores.wait(chat, reply);
                        // It may be over already, if the store is quick.
                        self.settle_restores();
                    }
                    Err(refused) => {
                        self.restores.fresh.clear();
                        let _ = reply.send(Err(refused));
                    }
                }
            }
            Command::PutAway { chat, reply } => {
                let result = self.host(
                    |core, now, out| core.put_away(now, chat, out),
                    |()| Some(HostCommand::PutAway { chat }),
                );
                let _ = reply.send(result);
            }
            Command::Delete { reply, .. } if !stored => not_saved(reply),
            Command::Delete { chat, shelf, reply } => {
                let deleting = shelf.clone();
                let result = self.host(
                    |core, now, out| core.delete(now, chat, deleting, out),
                    |()| Some(HostCommand::Delete { chat, shelf }),
                );
                // Deleted, it's shown to no watch as it was left.
                if result.is_ok() {
                    self.put_away.remove(&chat);
                }
                let _ = reply.send(result);
            }
            Command::MoveShelf { reply, .. } if !stored => not_saved(reply),
            Command::MoveShelf { from, to, reply } => {
                let (moving, to_shelf) = (from.clone(), to.clone());
                let result = self.host(
                    |core, now, out| core.move_shelf(now, moving, to_shelf, out),
                    |()| Some(HostCommand::MoveShelf { from, to }),
                );
                let _ = reply.send(result);
            }
            Command::SetData { chat, data, reply } => {
                let recorded = self.keeping().then(|| data.clone());
                let result = self.host(
                    |core, now, out| core.set_data(now, chat, data, out),
                    |()| recorded.map(|data| HostCommand::SetData { chat, data }),
                );
                let _ = reply.send(result);
            }
            Command::Data { chat, reply } => {
                let data = self.core.data(chat).cloned().ok_or(Refused::NoSuchChat);
                let _ = reply.send(data);
            }
            // The rest aren't about saving.
            _ => {}
        }
    }

    /// Looks at what's installed and what the machine has.
    fn check_agents(&mut self) {
        self.host(
            |core, now, out| {
                core.check_agents(now, out);
                Ok(())
            },
            |()| Some(HostCommand::CheckAgents),
        )
        .unwrap_or_default();
    }

    /// Does `call` about `agent`.
    fn agent(&mut self, agent: &AgentName, call: AgentCall) -> Result<(), Refused> {
        let named = || agent.clone();
        match call {
            AgentCall::Install => self.host(
                |core, now, out| core.install(now, agent, out),
                |()| Some(HostCommand::Install(named())),
            ),
            AgentCall::StopInstall => self.host(
                |core, now, out| core.stop_install(now, agent, out),
                |()| Some(HostCommand::StopInstall(named())),
            ),
            AgentCall::SignIn => self.host(
                |core, now, out| core.sign_in(now, agent, out),
                |()| Some(HostCommand::SignIn(named())),
            ),
            AgentCall::Paste(text) => {
                let recorded = self.keeping().then(|| text.clone());
                self.host(
                    |core, now, out| core.paste(now, agent, text, out),
                    |()| {
                        recorded.map(|text| HostCommand::Paste {
                            agent: named(),
                            text,
                        })
                    },
                )
            }
            AgentCall::StopSignIn => self.host(
                |core, now, out| core.stop_sign_in(now, agent, out),
                |()| Some(HostCommand::StopSignIn(named())),
            ),
            AgentCall::SignOut => self.host(
                |core, now, out| core.sign_out(now, agent, out),
                |()| Some(HostCommand::SignOut(named())),
            ),
            AgentCall::LearnSettings => self.host(
                |core, now, out| core.learn_settings(now, agent, out),
                |()| Some(HostCommand::LearnSettings(named())),
            ),
        }
    }

    /// Calls one of the core's host methods, recording it as `recorded`
    /// says if the core took it.
    fn host<T>(
        &mut self,
        call: impl FnOnce(&mut Core<A, D>, Now, &mut Vec<Directive<A, D>>) -> Result<T, Refused>,
        recorded: impl FnOnce(&T) -> Option<HostCommand<A, D>>,
    ) -> Result<T, Refused> {
        let now = self.timers.now();
        let result = call(&mut self.core, now, &mut self.out);
        let step = (result.as_ref().ok())
            .and_then(recorded)
            .and_then(|command| self.keep(now, &Event::Host(command)));
        self.kept(step);
        self.lists_due = true;
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
        let step = self.keep(now, &Event::Host(HostCommand::<A, D>::Shutdown));
        self.core.shutdown(now, &mut self.out);
        self.kept(step);
        self.lists_due = true;
        self.route();
    }

    /// Gives the core the store's answer.
    fn stored(&mut self, ticket: Ticket, event: StorageEvent<A, D>) {
        self.restores.answered(ticket, &event);
        self.lists_due = true;
        self.give(Event::Storage { ticket, event });
    }

    /// Gives the core the usage driver's prices, answering those waiting
    /// for them, or the account limits it keeps; or answers whoever set a
    /// price, once the core has the prices it changed.
    fn usage_report(&mut self, report: UsageReport) {
        match report {
            UsageReport::Prices(prices) => {
                self.give(Event::Prices(prices));
                self.priced = true;
                for waiting in std::mem::take(&mut self.unpriced) {
                    let _ = waiting.send(self.priced_now());
                }
            }
            UsageReport::PriceSet { reply, result } => {
                let _ = reply.send(result.map_err(Error::Price));
            }
            UsageReport::Reloaded(waiting) => {
                for reply in waiting {
                    let _ = reply.send(self.priced_now());
                }
            }
            UsageReport::Limits(limits) => self.give(Event::Limits(limits)),
        }
    }

    /// The ledger's folder and the prices in force.
    fn priced_now(&self) -> Priced {
        (self.usage.folder().cloned(), self.core.prices().clone())
    }

    /// Gives the core what a driver reported.
    fn give(&mut self, event: Event<A, D>) {
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
    /// the watcher, if there are either. The watcher is also given the
    /// line, if it was handed to the recording.
    ///
    /// A driver's event is written before the core takes it, so one the
    /// core panics on is in the recording. A host's call is written after,
    /// since the core gives it its id, such as a new chat's, and isn't
    /// written if the core panics on it.
    fn keep(&mut self, now: Now, event: &Event<A, D>) -> Option<Kept<A, D>> {
        let watching = self.steps.is_some();
        let mut written = None;
        if let Some(recorder) = (self.recorder.as_mut()).filter(|recorder| !recorder.stopped) {
            match line(now, event) {
                Ok(line) => {
                    let copy = watching.then(|| line.clone());
                    recorder.write(line);
                    // Not if that write stopped the recording.
                    if !recorder.stopped {
                        written = copy;
                    }
                }
                Err(error) => recorder.stop(&format!("an event can't be written: {error}")),
            }
        }
        if !watching {
            return None;
        }
        // Read back, since an event can't be copied: so it's exactly what
        // the core was given, and a fresh core given it does the same.
        let watched = serde_json::to_string(&Line { now, event })
            .and_then(|line| serde_json::from_str(&line));
        match watched {
            Ok(recorded) => Some(Kept {
                recorded,
                line: written,
            }),
            Err(error) => {
                log::warn!(target: "caf", "a step can't be watched: its event doesn't read back: {error}");
                None
            }
        }
    }

    /// Shows the watcher the step [`Task::keep`] read back, once the core
    /// has been given its event and before its directives are carried
    /// out.
    fn kept(&mut self, kept: Option<Kept<A, D>>) {
        if let (Some(steps), Some(Kept { recorded, line })) = (&mut self.steps, kept) {
            steps.step(Step {
                now: recorded.now,
                event: &recorded.event,
                directives: &self.out,
                line: line.as_deref(),
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
        let seen = self.read(read.chat, read.serial, read.since, read.subscribe);
        let _ = read.reply.send(seen);
    }

    /// After a host's call or the store's answer: tells the watches of
    /// lists what changed, ends the watches of chats no longer loaded, and
    /// answers the restores that are over. Open chats are told first, so
    /// a watcher who sees a chat loaded in the saved list finds it open,
    /// as does a host told a restore is done.
    fn tell_lists(&mut self) {
        self.lists_due = false;
        self.tell_chats();
        self.tell_saved();
        // A chat put away or deleted is gone from the core, with no change
        // to say so: its watches are woken, find it gone, and end, with how
        // it was left if it was put away. That's taken from the core now,
        // before its write is done, which may wait for a move of its shelf.
        let core = &self.core;
        let (left, before) = {
            let last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
            (self.put_away)
                .retain(|chat, _| last.watched.contains_key(chat) && core.chat(*chat).is_none());
            // Only chats loaded as the last step ended, so put away in this
            // one: a watch made since doesn't count it as there.
            let left: Vec<ChatId> = (last.watched.keys())
                .filter(|chat| {
                    self.numbered.contains_key(chat)
                        && !self.put_away.contains_key(chat)
                        && core.chat(**chat).is_none()
                })
                .copied()
                .collect();
            (left, last.made)
        };
        // Copied with the lock let go, as a chat may be big.
        for chat in left {
            if let Some(away) = core.away(chat) {
                let left = Left {
                    chat: away.clone(),
                    latest: self.numbered.get(&chat).copied(),
                    before,
                };
                self.put_away.insert(chat, left);
            }
        }
        self.feeds.retain(|chat, _| core.chat(*chat).is_some());
        self.numbered.retain(|chat, _| core.chat(*chat).is_some());
        self.settle_restores();
    }

    /// Answers each restore the host waits for that's over: loaded, or
    /// failed, saying why.
    fn settle_restores(&mut self) {
        let core = &self.core;
        let over: Vec<ChatId> = (self.restores.waiting.keys())
            .filter(|&&chat| !core.restoring(chat))
            .copied()
            .collect();
        for chat in over {
            let Some(Waiting { reply, found, .. }) = self.restores.waiting.remove(&chat) else {
                continue;
            };
            let outcome = if core.chat(chat).is_some() {
                Ok(chat)
            } else {
                Err(why_not_restored(core, chat, found))
            };
            let _ = reply.send(Ok(outcome));
        }
    }

    /// Tells the [`SavedWatch`]es the saved chats on every shelf listed,
    /// if that changed.
    fn tell_saved(&self) {
        let mut listed = None;
        for shelf in &self.shelves {
            if let Some(chats) = self.core.saved(shelf) {
                listed.get_or_insert_with(Vec::new).extend(chats);
            }
        }
        // Whatever shelf each is on, in the order they were made.
        if let Some(listed) = &mut listed {
            listed.sort_by_key(|each: &SavedChat| (each.header.made, each.header.chat));
        }
        self.listed.send_if_modified(|was| {
            let changed = *was != listed;
            if changed {
                *was = listed;
            }
            changed
        });
    }

    /// Tells the [`ChatsWatch`]es which chats are open, with each one's
    /// label and shelf, if that changed.
    fn tell_chats(&self) {
        let open: Vec<ChatSummary> = (self.core.summaries().into_iter())
            .filter(|summary| !self.closed(summary.id))
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
                Directive::Viewer(ViewerDirective::Changed { chat, latest }) => {
                    self.numbered.insert(chat, latest);
                    if let Some(feed) = self.feeds.get(&chat) {
                        feed.send_replace(());
                    }
                    // A chat gone from the core changes no more: its
                    // watches read the last of it and end. An archived one
                    // goes on, as it may be unarchived.
                    if self.core.chat(chat).is_none() {
                        self.feeds.remove(&chat);
                    }
                }
                Directive::Setup(setup) => self.setup.apply(setup),
                Directive::Storage(storage) => {
                    if let StorageDirective::Load {
                        ticket,
                        shelf,
                        chat,
                    } = &storage
                    {
                        (self.restores).loading(*ticket, *chat, shelf.clone());
                    }
                    self.lists_due = true;
                    self.storage.apply(storage);
                }
                Directive::Report(report) => log_report(&report),
                Directive::Usage(UsageDirective::Record(line)) => self.usage.record(line),
                Directive::Usage(UsageDirective::SaveLimits(limits)) => {
                    self.usage.save_limits(limits);
                }
                // No other kind is given yet.
                Directive::Usage(_) => {}
                // No other kind is given yet. One added later needs a
                // driver here.
                _ => {}
            }
        }
        self.out = out;
        self.tell_agents();
        if self.lists_due {
            self.tell_lists();
        }
    }

    /// Tells the [`AgentsWatch`]es where every agent stands, if that
    /// changed.
    fn tell_agents(&self) {
        let now = self.core.agents();
        self.statuses.send_if_modified(|was| {
            let changed = *was != now;
            if changed {
                *was = now;
            }
            changed
        });
    }

    /// Whether `chat` isn't open: archived, or not there.
    fn closed(&self, chat: ChatId) -> bool {
        self.core
            .chat(chat)
            .is_none_or(|whole| whole.state == ChatState::Closed)
    }

    /// What a watch that has seen up to `since` hasn't seen of `chat`.
    fn read(
        &mut self,
        chat: ChatId,
        serial: u64,
        since: Option<ChangeNo>,
        subscribe: bool,
    ) -> Option<Seen<A>> {
        let Some(read) = self.core.read(chat, since) else {
            // Put away while it was watched: as it was left, and nothing
            // more.
            let left = (self.put_away.get(&chat)).filter(|left| serial < left.before)?;
            return Some(Seen {
                update: Some(Update::Picture(left.chat.clone())),
                latest: left.latest.max(since),
                closed: true,
                wake: None,
            });
        };
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
        // A chat the core has may change, even archived: it may be
        // unarchived.
        let wake = subscribe.then(|| {
            let feed = (self.feeds.entry(chat)).or_insert_with(|| watch::Sender::new(()));
            feed.subscribe()
        });
        Some(Seen {
            update,
            latest,
            closed: false,
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
    hide_servers(&mut open.mcp_servers);
    open
}

/// Hides the values of MCP servers' variables and headers, keeping their
/// names.
fn hide_servers(servers: &mut [McpServer]) {
    for server in servers {
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

/// Where the kind of sign-in is in [`SIGN_IN_STATUS`]'s params, as Claude's
/// adapter sends them: `{"authStatus": {"kind": "account", …}}`.
const SIGN_IN_KIND: &str = "/authStatus/kind";

/// [`SIGN_IN_STATUS`]'s params, as recorded: the kind of sign-in alone,
/// where the adapter put it, or nothing. Never the email, or anything else
/// beside it.
fn sign_in_status_params(params: Option<&RawValue>) -> Option<Box<RawValue>> {
    let kind = params
        .and_then(|params| serde_json::from_str::<serde_json::Value>(params.get()).ok())
        .and_then(|params| {
            params
                .pointer(SIGN_IN_KIND)
                .and_then(|kind| kind.as_str().map(str::to_owned))
        });
    let kept = match kind {
        Some(kind) => serde_json::json!({ "authStatus": { "kind": kind } }),
        None => serde_json::json!({}),
    };
    serde_json::value::to_raw_value(&kept).ok()
}

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
struct Line<'a, A: AppItem, D: AppData> {
    now: Now,
    event: &'a Event<A, D>,
}

/// A step kept for the watcher: its event read back, and its line of the
/// recording, if it went into one.
struct Kept<A: AppItem, D: AppData> {
    recorded: Recorded<A, D>,
    line: Option<String>,
}

/// An event at the time it was given, read back from its JSON: a
/// recording's line, or a watched step's event.
#[derive(Deserialize)]
#[serde(bound = "A: AppItem")]
struct Recorded<A: AppItem, D: AppData> {
    now: Now,
    event: Event<A, D>,
}

impl Recorder {
    /// Makes the file and writes its first line, or says why it couldn't.
    fn start(path: &PathBuf, host: &Host, agents: &[AgentSpec]) -> Option<Self> {
        let agents = (agents.iter())
            .map(|agent| {
                let mut agent = agent.clone();
                if let Launch::Command(command) = &mut agent.launch {
                    command.args.clear();
                    hide_values(&mut command.env);
                }
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

/// `event` at `now`, as a recording's line: with MCP servers' secrets,
/// pasted codes and all of Claude's sign-in status but its kind hidden,
/// and error output as text where it's UTF-8.
fn line<A: AppItem, D: AppData>(now: Now, event: &Event<A, D>) -> serde_json::Result<String> {
    let (process, agent) = match event {
        Event::Agent { process, event } => (process, event),
        Event::Host(HostCommand::OpenChat { chat, open }) => {
            let event = Event::<A>::Host(HostCommand::OpenChat {
                chat: *chat,
                open: hide_mcp_secrets(open),
            });
            return serde_json::to_string(&Line { now, event: &event });
        }
        Event::Host(HostCommand::Restore(restore)) => {
            let mut restore = restore.clone();
            hide_servers(&mut restore.mcp_servers);
            let event = Event::<A>::Host(HostCommand::Restore(restore));
            return serde_json::to_string(&Line { now, event: &event });
        }
        // A one-time code: it's never written, so a replay can't use it.
        Event::Host(HostCommand::Paste { agent, .. }) => {
            let event = serde_json::json!({
                "type": "Host",
                "value": { "type": "Paste", "value": { "agent": agent, "text": null } },
            });
            return serde_json::to_string(&serde_json::json!({ "now": now, "event": event }));
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
        AgentEvent::Frame(Frame::Message(Message::Notification { method, params }))
            if method == SIGN_IN_STATUS =>
        {
            Message::Notification {
                method: method.clone(),
                params: sign_in_status_params(params.as_deref()),
            }
        }
        AgentEvent::Frame(Frame::Message(Message::Request { id, method, params }))
            if method == SIGN_IN_STATUS =>
        {
            Message::Request {
                id: id.clone(),
                method: method.clone(),
                params: sign_in_status_params(params.as_deref()),
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

    /// A config for these tests, offering `agents`, keeping no usage
    /// ledger.
    fn test_config(agents: Vec<AgentSpec>) -> Config {
        let mut config = Config::new(Host::new("test", "1.0.0"), agents);
        config.usage = None;
        config
    }

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
        use caf_core::profiles::{Builtin, ProfileChoice};
        let agents = vec![AgentSpec::command(
            "plain",
            SpawnCommand::new("/opt/plain"),
            ProfileChoice::Builtin(Builtin::Fake),
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

    /// The event `line` writes, read back as JSON.
    fn written(event: &Event<NoAppItems>) -> serde_json::Value {
        let now = Now {
            wall: Wall(0),
            mono: Mono(0),
        };
        let line = line(now, event).unwrap();
        let read: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(read["now"], serde_json::json!({ "wall": 0, "mono": 0 }));
        read["event"].clone()
    }

    #[test]
    fn a_pasted_code_is_never_recorded() {
        let event = Event::<NoAppItems>::Host(HostCommand::Paste {
            agent: AgentName("claude".into()),
            text: "one-time-code-1234".into(),
        });
        assert_eq!(
            written(&event),
            serde_json::json!({
                "type": "Host",
                "value": { "type": "Paste", "value": { "agent": "claude", "text": null } },
            })
        );
    }

    #[test]
    fn only_the_kind_of_a_sign_in_status_is_recorded() {
        use caf_core::acp::RequestId;
        let raw = |json: &str| Some(serde_json::value::RawValue::from_string(json.into()).unwrap());
        let notification = |params| Event::<NoAppItems>::Agent {
            process: Ticket(1000),
            event: AgentEvent::Frame(Frame::Message(Message::Notification {
                method: SIGN_IN_STATUS.into(),
                params,
            })),
        };
        let params = |event: serde_json::Value| {
            event["value"]["event"]["value"]["value"]["value"]["params"].clone()
        };
        let claude = r#"{"authStatus": {"kind": "account", "label": "Claude Max", "email": "me@example.com", "organization": "Me"}}"#;
        assert_eq!(
            params(written(&notification(raw(claude)))),
            serde_json::json!({ "authStatus": { "kind": "account" } })
        );
        // Anything without a kind where Claude's adapter puts it keeps
        // nothing.
        for other in [
            r#"{"email": "me@example.com"}"#,
            r#"{"authStatus": {"kind": 7}}"#,
            "[1]",
            "null",
        ] {
            assert_eq!(
                params(written(&notification(raw(other)))),
                serde_json::json!({}),
                "{other}"
            );
        }
        assert_eq!(params(written(&notification(None))), serde_json::json!({}));
        let request = Event::<NoAppItems>::Agent {
            process: Ticket(1000),
            event: AgentEvent::Frame(Frame::Message(Message::Request {
                id: RequestId::Number(5),
                method: SIGN_IN_STATUS.into(),
                params: raw(claude),
            })),
        };
        assert_eq!(
            params(written(&request)),
            serde_json::json!({ "authStatus": { "kind": "account" } })
        );
    }

    #[test]
    fn a_sign_ins_link_and_code_are_recorded_as_they_came() {
        use caf_core::event::SetupEvent;
        for event in [
            SetupEvent::Link("https://claude.ai/oauth?state=abc".into()),
            SetupEvent::Code("ABCD-1234".into()),
        ] {
            let expected = serde_json::to_value(&event).unwrap();
            let event = Event::<NoAppItems>::Setup {
                ticket: Ticket(1000),
                event,
            };
            assert_eq!(written(&event)["value"]["event"], expected);
        }
    }

    #[tokio::test]
    async fn watching_a_chat_that_isnt_there_leaves_nothing_behind() {
        let (caf, running) = Caf::<NoAppItems>::start(test_config(vec![]));
        for chat in 1000..1100 {
            let mut watch = caf.chat(ChatId::from_number(chat)).watch();
            assert!(watch.next().await.is_none());
        }
        let watched = caf.shared.last.lock().unwrap().watched.len();
        assert_eq!(watched, 0, "a watch for no chat is forgotten once it ends");
        caf.shutdown().await;
        running.await;
    }

    #[tokio::test]
    async fn a_watch_made_after_caf_has_ended_leaves_nothing_behind() {
        let (caf, running) = Caf::<NoAppItems>::start(test_config(vec![]));
        caf.shutdown().await;
        running.await;
        let mut watch = caf.chat(ChatId::from_number(1000)).watch();
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
            chat: ChatId::from_number(1),
            since: None,
            wake: None,
            ended: false,
            counted: false,
            serial: 0,
        };
        drop(ending);
        let next = tokio::time::timeout(Duration::from_secs(5), watch.next()).await;
        assert!(matches!(next, Ok(None)), "the watch ends with CAF");
        drop((reads, inbox));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn what_caf_kept_for_a_watch_once_it_ended_goes_with_its_last_watch() {
        use caf_core::agent::{EnvChange, SpawnCommand};
        use caf_core::profiles::ProfileChoice;
        let dir = std::env::temp_dir().join(format!("caf-last-words-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("script.jsonl");
        std::fs::write(
            &script,
            [
                r#"{"wait": {"method": "initialize"}}"#,
                r#"{"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {}, "agentInfo": {"name": "fake", "version": "1.0.0"}}}}"#,
                r#"{"wait": {"method": "session/new"}}"#,
                r#"{"respond": {"result": {"sessionId": "s1"}}}"#,
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        let mut command = SpawnCommand::new(caf_fake_agent::binary());
        command.env = vec![EnvChange::Set(
            caf_fake_agent::SCRIPT_VAR.into(),
            script.into(),
        )];
        let fake = AgentSpec::command(
            "fake",
            command,
            ProfileChoice::Builtin(caf_core::profiles::Builtin::Fake),
        );
        let (caf, running) = Caf::<NoAppItems>::start(test_config(vec![fake]));
        let chat = caf.open_chat(OpenChat::new("fake", &dir)).await.unwrap();
        let first = chat.watch();
        let second = chat.watch();
        caf.shutdown().await;
        running.await;
        let kept = |caf: &Caf| caf.shared.last.lock().unwrap().chats.len();
        assert_eq!(kept(&caf), 1, "the chat as CAF left it");
        drop(first);
        assert_eq!(kept(&caf), 1, "a watch of it is left");
        drop(second);
        assert_eq!(kept(&caf), 0, "nothing, with its last watch gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn watches_dropped_unread_leave_nothing_behind() {
        let (caf, running) = Caf::<NoAppItems>::start(test_config(vec![]));
        for chat in 1000..2000 {
            drop(caf.chat(ChatId::from_number(chat)).watch());
        }
        let kept = caf.shared.last.lock().unwrap().watched.len();
        assert_eq!(kept, 0, "nothing is kept for a watch once it's dropped");
        caf.shutdown().await;
        running.await;
    }
}
