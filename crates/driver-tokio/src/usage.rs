//! The usage driver: the usage ledger and the prices, on a thread of
//! their own, and LiteLLM's price table, fetched on another.
//!
//! Each line is written with [`Ledger::record`], in the order it's given.
//! A line that can't be written is kept, and tried again before the next
//! and every hour, so a ledger that fails for a while loses nothing; the
//! first failure in a row is reported to the [`log`] facade, target
//! `caf`, and so is the ledger taking lines again. It never answers the
//! core, and nothing it does holds up a chat. A lock another program
//! holds costs five seconds a try ([`Ledger::record`]), so once one is
//! given up on, lines are kept without being tried for
//! [`LOCK_COOLDOWN`], or until the next hourly look, so reloads, prices
//! and shutting down aren't held up behind them.
//!
//! The prices are read as the thread starts, and again when they may have
//! changed: a price of the user's own set ([`prices::set_own`]),
//! LiteLLM's table fetched, every hour, and before each summary, so a
//! price another app set is taken too. The whole table goes to the task,
//! for the core ([`Event::Prices`](caf_core::Event::Prices)), as they're
//! first read and whenever they differ from what it was last given. Only
//! one reading for a summary is under way at a time: those asked for
//! meanwhile wait for one more, after it, so a price set while it was
//! under way is never missed.
//!
//! Looking is another thread's, so a slow download never holds a line up:
//! it looks as it starts and then every hour, fetching first, if it's
//! asked to, when the table every CAF app shares
//! ([`prices::fetched_at`]) is a day old, or was never fetched. A failed
//! fetch is reported, and tried again in an hour.
//!
//! # A thread that's stuck
//!
//! The ledger's thread may be stuck, on a disk that has hung, say. So
//! nothing waits for it for ever:
//! - if the first prices haven't come in [`PRICES_WITHIN`], the task is
//!   given the bundled snapshot, with a report, and the prices read come
//!   when they come;
//! - a reading for a summary that hasn't come in that time is answered
//!   with the prices the core has, with a report;
//! - a price set that hasn't been in that time is answered that the
//!   ledger didn't answer ([`Error::Price`]), and one waiting as the
//!   driver finishes is let go, which its asker hears as CAF gone;
//! - finishing waits for it for at most [`FINISH_WITHIN`], then reports
//!   how many lines may be lost, and leaves it.
//!
//! Account limits are kept on the ledger's thread too
//! ([`caf_usage::limits`]): read as it starts, after the prices, and
//! reported, for the core; each save merged with the file, the merged
//! limits reported if the file had news. Each profile's latest limits not
//! yet kept, those given since merged in, are saved before the thread
//! does anything else, so saves given one after another are one. Those
//! that fail are kept, and tried again with the next save, at the hourly
//! look and as the thread ends; the first failure in a row is reported to
//! the log's warnings. One whose save panics is dropped, as the next has
//! news as fresh. Once another program's lock on them is given up on,
//! nothing is tried for [`LOCK_COOLDOWN`], as for lines, so nothing else
//! waits behind them, and what's not kept as the thread ends is reported
//! lost. With the thread stuck, the limits kept come when it isn't: the
//! core has those it hears meanwhile.
//!
//! With no ledger, the prices are the bundled snapshot's, and lines and
//! limits are dropped.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use caf_core::limits::{AccountLimits, ProfileLimits};
use caf_core::time::Wall;
use caf_core::usage::{LedgerLine, ModelPrice, PriceTable};
use caf_usage::{Ledger, prices};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Sleep;

use crate::caf::{Error, UsageConfig};

/// How often the looking thread looks at the prices.
const CHECK_EVERY: Duration = Duration::from_hours(1);

/// How long the prices are waited for, first and for a summary, before
/// going on without them.
pub(crate) const PRICES_WITHIN: Duration = Duration::from_secs(10);

/// How long finishing waits for the ledger's thread.
pub(crate) const FINISH_WITHIN: Duration = Duration::from_secs(30);

/// How long lines are kept without being tried once another program's
/// lock was given up on.
const LOCK_COOLDOWN: Duration = Duration::from_secs(60);

/// The most LiteLLM's table may be, in bytes: past that, the fetch fails.
/// It's about 1.5 MB.
const MOST: u64 = 32 << 20;

/// The most lines kept while the ledger can't be written: past that, the
/// oldest are dropped, with a report.
const MOST_KEPT: usize = 10_000;

/// Fetches LiteLLM's table, or says why it couldn't.
pub(crate) type Fetcher = Box<dyn FnMut() -> Result<Vec<u8>, String> + Send>;

/// What the task hears from the driver.
pub(crate) enum Report {
    /// The prices now, whole.
    Prices(PriceTable),
    /// A price of the user's own was set, or why it couldn't be: after
    /// the prices it changed, so whoever asked is answered once the core
    /// has them.
    PriceSet {
        reply: oneshot::Sender<Result<(), Error>>,
        result: Result<(), String>,
    },
    /// The prices were read again for those waiting: after them, if they
    /// changed, so each is answered once the core has them. Or they
    /// weren't in time, and the core's are as good as it gets.
    Reloaded(Vec<oneshot::Sender<crate::caf::Priced>>),
    /// Account limits kept: every profile's, as the thread starts, or
    /// one's a save found news in.
    Limits(Vec<ProfileLimits>),
}

/// What the ledger's thread tells the driver.
enum Told {
    Report(Report),
    /// The price set with this number was, or why not.
    PriceSet {
        set: u64,
        result: Result<(), String>,
    },
    /// The prices were read again, as asked with this number.
    Reloaded(u64),
}

/// Whoever set a price, waiting to hear if it was.
type SetReply = oneshot::Sender<Result<(), Error>>;

/// What a price set the ledger didn't answer in time is answered.
const NO_ANSWER: &str = "the usage ledger didn't answer";

/// Work for the ledger's thread.
enum Work {
    Record(Box<LedgerLine>),
    SetPrice {
        set: u64,
        model: String,
        price: Option<ModelPrice>,
    },
    /// The prices may have changed on disk: read them again, and say so if
    /// asked to, with this number. Every hour, also try the lines kept.
    Reload(Option<u64>),
    SaveLimits(Box<ProfileLimits>),
}

/// How the driver runs: what's not the defaults is for tests.
pub(crate) struct Options {
    /// How often the prices are looked at.
    pub(crate) every: Duration,
    /// [`PRICES_WITHIN`].
    pub(crate) prices_within: Duration,
    /// [`FINISH_WITHIN`].
    pub(crate) finish_within: Duration,
    /// [`LOCK_COOLDOWN`].
    pub(crate) cooldown: Duration,
    /// For tests: the ledger's thread is stuck until this is dropped.
    pub(crate) stuck: Option<std::sync::mpsc::Receiver<()>>,
    /// [`caf_usage::limits::save`], but in tests.
    pub(crate) save_limits: Saver,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            every: CHECK_EVERY,
            prices_within: PRICES_WITHIN,
            finish_within: FINISH_WITHIN,
            cooldown: LOCK_COOLDOWN,
            stuck: None,
            save_limits: caf_usage::limits::save,
        }
    }
}

/// Keeps the ledger and the prices on a thread of its own, and looks at
/// the prices every hour on another, fetching them if asked to.
pub(crate) struct UsageDriver {
    /// The ledger's folder, if there is one.
    folder: Option<PathBuf>,
    /// Work for the thread. Dropped to end it.
    work: Option<std::sync::mpsc::Sender<Work>>,
    /// What the thread tells: the driver keeps a sender, so it never
    /// closes.
    told: mpsc::UnboundedReceiver<Told>,
    _tell: mpsc::UnboundedSender<Told>,
    /// Each look at the prices, from the looking thread.
    looked: mpsc::UnboundedReceiver<()>,
    /// Dropped to stop the looking thread.
    looking: Option<std::sync::mpsc::Sender<()>>,
    /// Whether the looking thread may keep a table it fetched: no, once
    /// finishing. It's held while one is kept, so none is kept after.
    keeping: Arc<Mutex<bool>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Lines given the thread, not yet written, dropped or reported lost.
    unwritten: Arc<AtomicUsize>,
    /// Until the first prices come: when to go on without them.
    first: Option<Pin<Box<Sleep>>>,
    /// The reading asked for a summary, by its number, and when to go on
    /// without it.
    reloading: Option<(u64, Instant)>,
    /// The last reading's number.
    reloads: u64,
    /// How many looks at the prices were handed to the thread: so a test
    /// knows the first, as the driver starts, is behind it.
    #[cfg(test)]
    pub(crate) looks: u64,
    /// Those waiting for the reading under way.
    reading: Vec<oneshot::Sender<crate::caf::Priced>>,
    /// Those who asked while it was under way: they wait for the next.
    after: Vec<oneshot::Sender<crate::caf::Priced>>,
    /// The prices set, by number, oldest first, each with whoever asked
    /// and when to stop waiting.
    setting: VecDeque<(u64, SetReply, Instant)>,
    /// The last price set's number.
    sets: u64,
    prices_within: Duration,
    finish_within: Duration,
}

impl UsageDriver {
    /// A driver for the ledger `usage` says, or none, writing lines as the
    /// app named `app`.
    pub(crate) fn new(usage: Option<UsageConfig>, app: String) -> Self {
        let fetcher = (usage.as_ref())
            .filter(|usage| usage.fetch_prices)
            .map(|_| Box::new(|| fetch(prices::LITELLM, true)) as Fetcher);
        let folder = usage.map(|usage| usage.folder);
        Self::launch(folder, app, fetcher, Options::default())
    }

    /// A driver for the ledger in `folder`, or none, looking at the prices
    /// every `every`, and fetching them with `fetcher` if given.
    #[cfg(test)]
    pub(crate) fn start(
        folder: Option<PathBuf>,
        app: String,
        fetcher: Option<Fetcher>,
        every: Duration,
    ) -> Self {
        let options = Options {
            every,
            ..Options::default()
        };
        Self::launch(folder, app, fetcher, options)
    }

    /// A driver for the ledger in `folder`, or none, fetching prices with
    /// `fetcher` if given, running as `options` say.
    pub(crate) fn launch(
        folder: Option<PathBuf>,
        app: String,
        fetcher: Option<Fetcher>,
        options: Options,
    ) -> Self {
        let (tell, told) = mpsc::unbounded_channel();
        let (work, inbox) = std::sync::mpsc::channel();
        let ledger = (folder.clone()).map(|folder| Ledger::new(folder, app));
        let telling = tell.clone();
        let unwritten = Arc::new(AtomicUsize::new(0));
        let counting = Arc::clone(&unwritten);
        let stuck = options.stuck;
        let cooldown = options.cooldown;
        let limits = Limits::new(options.cooldown, options.save_limits);
        let thread = std::thread::Builder::new()
            .name("caf-usage".into())
            .spawn(move || {
                if let Some(stuck) = stuck {
                    let _ = stuck.recv();
                }
                keep(
                    ledger.as_ref(),
                    &inbox,
                    &telling,
                    &counting,
                    cooldown,
                    limits,
                );
            })
            .expect("a thread for the usage ledger");
        let keeping = Arc::new(Mutex::new(true));
        let (looks, looked) = mpsc::unbounded_channel();
        let looking = folder.as_ref().map(|folder| {
            let (stop, stopped) = std::sync::mpsc::channel();
            let folder = folder.clone();
            let keeping = Arc::clone(&keeping);
            let every = options.every;
            std::thread::Builder::new()
                .name("caf-prices".into())
                .spawn(move || {
                    look_now_and_then(&folder, fetcher, &keeping, &looks, &stopped, every);
                })
                .expect("a thread for the prices");
            stop
        });
        Self {
            folder,
            work: Some(work),
            told,
            _tell: tell,
            looked,
            looking,
            keeping,
            thread: Some(thread),
            unwritten,
            first: Some(Box::pin(tokio::time::sleep(options.prices_within))),
            reloading: None,
            reloads: 0,
            #[cfg(test)]
            looks: 0,
            reading: Vec::new(),
            after: Vec::new(),
            setting: VecDeque::new(),
            sets: 0,
            prices_within: options.prices_within,
            finish_within: options.finish_within,
        }
    }

    /// The ledger's folder, if there is one.
    pub(crate) fn folder(&self) -> Option<&PathBuf> {
        self.folder.as_ref()
    }

    /// Keeps a profile's account limits beside the ledger, merged with
    /// what's there, or drops them with none. If what was kept had news,
    /// the merged limits are reported, for the core
    /// ([`Event::Limits`](caf_core::Event::Limits)). If they can't be
    /// kept, they're tried again, with the next.
    pub(crate) fn save_limits(&mut self, limits: Box<ProfileLimits>) {
        if self.folder.is_some() {
            self.send(Work::SaveLimits(limits));
        }
    }

    /// Writes a turn's line to the ledger, or drops it with none.
    pub(crate) fn record(&mut self, line: Box<LedgerLine>) {
        if self.folder.is_some() {
            self.unwritten.fetch_add(1, Ordering::Relaxed);
            self.send(Work::Record(line));
        }
    }

    /// Sets a price of the user's own, answering `reply` once the prices
    /// it changed are reported, or if the ledger doesn't answer in
    /// [`PRICES_WITHIN`], [`Error::Price`]. With no ledger, it's
    /// [`Error::NoLedger`].
    pub(crate) fn set_price(
        &mut self,
        model: String,
        price: Option<ModelPrice>,
        reply: oneshot::Sender<Result<(), Error>>,
    ) {
        if self.folder.is_none() {
            let _ = reply.send(Err(Error::NoLedger));
            return;
        }
        self.sets += 1;
        let set = self.sets;
        self.setting
            .push_back((set, reply, Instant::now() + self.prices_within));
        self.send(Work::SetPrice { set, model, price });
    }

    /// Reads the prices again, as they may have changed on disk, for
    /// `reply`: [`Report::Reloaded`] gives it back once the core has them,
    /// if they changed, or in [`PRICES_WITHIN`] if they aren't read by
    /// then. One asked for while a reading is under way waits for another
    /// after it, which those asked for meanwhile share.
    pub(crate) fn reload(&mut self, reply: oneshot::Sender<crate::caf::Priced>) {
        if self.reloading.is_some() {
            self.after.push(reply);
            return;
        }
        self.reading.push(reply);
        self.read_again();
    }

    /// Asks the thread to read the prices again, for those waiting.
    fn read_again(&mut self) {
        self.reloads += 1;
        self.reloading = Some((self.reloads, Instant::now() + self.prices_within));
        self.send(Work::Reload(Some(self.reloads)));
    }

    /// The reading under way is over: those waiting for it, and the next
    /// started for those waiting after it, if any.
    fn read(&mut self) -> Report {
        self.reloading = None;
        let read = std::mem::take(&mut self.reading);
        if !self.after.is_empty() {
            self.reading = std::mem::take(&mut self.after);
            self.read_again();
        }
        Report::Reloaded(read)
    }

    /// Hands the thread work. Once it has gone, which it does only as the
    /// driver finishes, there's nobody to do it: a reply in it is dropped,
    /// which its asker hears as CAF gone.
    fn send(&self, work: Work) {
        if let Some(sender) = &self.work {
            let _ = sender.send(work);
        }
    }

    /// The driver's next report. Cancel-safe.
    pub(crate) async fn next(&mut self) -> Report {
        loop {
            let reload_late = at(self.reloading.as_ref().map(|(_, at)| *at));
            let set_late = at(self.setting.front().map(|(_, _, at)| *at));
            let Self {
                told,
                looked,
                first,
                ..
            } = self;
            let first_late = async {
                match first {
                    Some(first) => first.as_mut().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                Some(told) = told.recv() => match told {
                    Told::Report(report) => {
                        if matches!(report, Report::Prices(_)) {
                            self.first = None;
                        }
                        return report;
                    }
                    // One asked for before, and given up on, is let be.
                    Told::Reloaded(reload) => {
                        if self.reloading.as_ref().is_some_and(|(asked, _)| *asked == reload) {
                            return self.read();
                        }
                    }
                    Told::PriceSet { set, result } => {
                        let at = self.setting.iter().position(|(asked, ..)| *asked == set);
                        if let Some((_, reply, _)) = at.and_then(|at| self.setting.remove(at)) {
                            return Report::PriceSet { reply, result };
                        }
                    }
                },
                Some(()) = looked.recv() => {
                    #[cfg(test)]
                    {
                        self.looks += 1;
                    }
                    self.send(Work::Reload(None));
                }
                () = first_late => {
                    self.first = None;
                    log::warn!(
                        target: "caf",
                        "the prices weren't read in {} s: the bundled snapshot is used until they are",
                        self.prices_within.as_secs()
                    );
                    return Report::Prices(prices::bundled());
                }
                () = reload_late => {
                    log::warn!(
                        target: "caf",
                        "the prices weren't read again in {} s: a summary is priced as they were",
                        self.prices_within.as_secs()
                    );
                    return self.read();
                }
                () = set_late => {
                    if let Some((_, reply, _)) = self.setting.pop_front() {
                        log::warn!(
                            target: "caf",
                            "the usage ledger didn't set a price in {} s",
                            self.prices_within.as_secs()
                        );
                        return Report::PriceSet { reply, result: Err(NO_ANSWER.to_owned()) };
                    }
                }
            }
        }
    }

    /// Stops looking at the prices, and ends the ledger's thread once it
    /// has written what it was given, waiting for it for at most
    /// [`FINISH_WITHIN`]: past that, it reports how many lines may be lost,
    /// and leaves it. A fetch under way isn't waited for, and keeps
    /// nothing from now on.
    pub(crate) async fn finish(mut self) {
        // Those waiting hear CAF has gone, not after the thread.
        self.setting.clear();
        self.reading.clear();
        self.after.clear();
        self.looking = None;
        self.work = None;
        let Some(thread) = self.thread.take() else {
            return;
        };
        let keeping = Arc::clone(&self.keeping);
        let (done, ended) = oneshot::channel();
        // On a thread of its own, so a stuck thread is left, not waited
        // for, even by the runtime as it shuts down.
        std::thread::Builder::new()
            .name("caf-usage-end".into())
            .spawn(move || {
                // Waits for a table being kept now, so none is after.
                *keeping.lock().unwrap_or_else(PoisonError::into_inner) = false;
                let _ = thread.join();
                let _ = done.send(());
            })
            .expect("a thread to end the usage ledger's");
        if tokio::time::timeout(self.finish_within, ended)
            .await
            .is_err()
        {
            log::warn!(
                target: "caf",
                "the usage ledger hasn't finished writing in {} s: {} turns' usage may be lost",
                self.finish_within.as_secs(),
                self.unwritten.load(Ordering::Relaxed)
            );
        }
    }
}

/// Resolves at `at`, or never with none.
async fn at(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// The prices in force: from the ledger's folder, or the bundled snapshot
/// with none. Reading them is never let panic the thread: a panic gives
/// none, with a report.
fn read_prices(ledger: Option<&Ledger>) -> PriceTable {
    let read = std::panic::catch_unwind(|| match ledger {
        Some(ledger) => prices::load(ledger.folder()),
        None => prices::bundled(),
    });
    read.unwrap_or_else(|_| {
        log::warn!(target: "caf", "reading the prices panicked: no cost is estimated until they're read again");
        PriceTable::default()
    })
}

/// Does each piece of work, in order, until there's no more.
fn keep(
    ledger: Option<&Ledger>,
    inbox: &std::sync::mpsc::Receiver<Work>,
    tell: &mpsc::UnboundedSender<Told>,
    unwritten: &AtomicUsize,
    cooldown: Duration,
    mut limits: Limits,
) {
    // Once the task has gone, nobody needs a report.
    let report = |report: Report| {
        let _ = tell.send(Told::Report(report));
    };
    let mut given = read_prices(ledger);
    report(Report::Prices(given.clone()));
    // Gives the task the prices if they changed.
    let mut give = |now: PriceTable| {
        if now != given {
            given = now.clone();
            report(Report::Prices(now));
        }
    };
    if let Some(ledger) = ledger {
        load_limits(ledger, &report);
    }
    let mut kept = Kept::new(unwritten, cooldown);
    let mut next = inbox.recv().ok();
    while let Some(work) = next.take() {
        // Limits given are saved before anything else is done, so those
        // given one after another are one save.
        let mut tried = false;
        if limits.given
            && !matches!(work, Work::SaveLimits(_))
            && let Some(ledger) = ledger
        {
            limits.try_saving(ledger, &report);
            tried = true;
        }
        match work {
            Work::Record(line) => {
                if let Some(ledger) = ledger {
                    kept.add(*line);
                    kept.try_writing(ledger);
                }
            }
            // Never given without one.
            Work::SetPrice { set, model, price } => {
                if let Some(ledger) = ledger {
                    let written = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        prices::set_own(ledger.folder(), &model, price)
                    }));
                    let result = match written {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("setting it panicked".to_owned()),
                    };
                    if result.is_ok() {
                        give(read_prices(Some(ledger)));
                    }
                    let _ = tell.send(Told::PriceSet { set, result });
                }
            }
            Work::Reload(asked) => {
                if let Some(ledger) = ledger {
                    // Every hour, whatever the lock did.
                    if asked.is_none() && !kept.lines.is_empty() {
                        kept.cooling = None;
                        kept.try_writing(ledger);
                    }
                    // Not again if they were just now.
                    if asked.is_none() && !tried {
                        limits.try_saving(ledger, &report);
                    }
                    give(read_prices(Some(ledger)));
                }
                if let Some(asked) = asked {
                    let _ = tell.send(Told::Reloaded(asked));
                }
            }
            Work::SaveLimits(given) => limits.give(*given),
        }
        next = match inbox.try_recv() {
            Ok(work) => Some(work),
            // Or once there's nothing else to do.
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if limits.given
                    && let Some(ledger) = ledger
                {
                    limits.try_saving(ledger, &report);
                }
                inbox.recv().ok()
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
    }
    // Once, whatever the lock did.
    if let Some(ledger) = ledger
        && let Err(error) = kept.write(ledger)
    {
        log::warn!(
            target: "caf",
            "{} turns' usage couldn't be written to the usage ledger in {} as CAF stopped, and is lost: {error}",
            kept.lines.len(),
            ledger.folder().display()
        );
        kept.lose_all();
    }
    if let Some(ledger) = ledger {
        limits.finish(ledger, &report);
    }
}

/// Reads the account limits kept, and reports them if there are any. A
/// file that can't be read is reported to the log, and left out: its
/// limits are heard again as agents say them. Reading them is never let
/// panic the thread.
fn load_limits(ledger: &Ledger, report: &impl Fn(Report)) {
    let read = std::panic::catch_unwind(|| caf_usage::limits::load(ledger.folder(), wall_now()));
    let Ok((kept, errors)) = read else {
        log::warn!(target: "caf", "reading the account limits kept panicked: they're heard again as agents say them");
        return;
    };
    for error in errors {
        log::warn!(
            target: "caf",
            "account limits kept beside the usage ledger in {} can't be read: {error}. They're heard again as agents say them",
            ledger.folder().display()
        );
    }
    if !kept.is_empty() {
        report(Report::Limits(kept));
    }
}

/// Saves a profile's account limits: [`caf_usage::limits::save`].
type Saver = fn(&std::path::Path, &ProfileLimits, Wall) -> std::io::Result<Option<AccountLimits>>;

/// Keeps account limits beside the ledger: each profile's latest not yet
/// kept, tried with the next save, at the hourly look and as the thread
/// ends.
struct Limits {
    /// Each profile's limits not yet kept, all those given merged.
    unsaved: Vec<ProfileLimits>,
    /// Limits were given since they were last tried.
    given: bool,
    /// The last save failed, and that was reported.
    failing: bool,
    /// Another program's lock was given up on: nothing is tried until
    /// then.
    cooling: Option<Instant>,
    /// [`LOCK_COOLDOWN`].
    cooldown: Duration,
    /// [`caf_usage::limits::save`], but in tests.
    save: Saver,
}

impl Limits {
    fn new(cooldown: Duration, save: Saver) -> Self {
        Self {
            unsaved: Vec::new(),
            given: false,
            failing: false,
            cooling: None,
            cooldown,
            save,
        }
    }

    /// Takes in a profile's limits to keep, merged with any of its not
    /// yet kept: so several given at once are one save, with the latest
    /// news of each.
    fn give(&mut self, limits: ProfileLimits) {
        let unsaved = self.unsaved.iter_mut();
        match unsaved
            .into_iter()
            .find(|unsaved| unsaved.profile == limits.profile)
        {
            Some(unsaved) => {
                unsaved.limits.merge(&limits.limits);
            }
            None => self.unsaved.push(limits),
        }
        self.given = true;
    }

    /// Merges each profile's limits not yet kept into what's kept,
    /// reporting the merged limits if what was kept had news; unless
    /// another program's lock was given up on less than the cooldown ago.
    /// Those that fail are kept, to be tried again, and the first failure
    /// in a row is reported to the log's warnings; those whose save
    /// panics are dropped, as the next has news as fresh.
    fn try_saving(&mut self, ledger: &Ledger, report: &impl Fn(Report)) {
        self.given = false;
        if self.unsaved.is_empty() || self.cooling.is_some_and(|until| Instant::now() < until) {
            return;
        }
        self.cooling = None;
        let folder = ledger.folder();
        let mut at = 0;
        while let Some(limits) = self.unsaved.get(at) {
            let save = self.save;
            let saved = std::panic::catch_unwind(|| save(folder, limits, wall_now()));
            match saved {
                Ok(Ok(merged)) => {
                    let limits = self.unsaved.remove(at);
                    if self.failing {
                        log::info!(target: "caf", "account limits are kept beside the usage ledger in {} again", folder.display());
                    }
                    self.failing = false;
                    if let Some(merged) = merged {
                        report(Report::Limits(vec![ProfileLimits::new(
                            limits.profile,
                            merged,
                        )]));
                    }
                }
                Ok(Err(error)) => {
                    let timed_out = error.kind() == std::io::ErrorKind::TimedOut;
                    let again = if timed_out {
                        format!(
                            "not tried again for {} s, then with the next save, every hour and as CAF shuts down",
                            self.cooldown.as_secs()
                        )
                    } else {
                        "tried again with the next save, every hour and as CAF shuts down"
                            .to_owned()
                    };
                    self.failed(&format!(
                        "account limits can't be kept beside the usage ledger in {}: {error}. They're kept while CAF runs, and {again}",
                        folder.display()
                    ));
                    if timed_out {
                        // Every other profile's would wait as long.
                        self.cooling = Some(Instant::now() + self.cooldown);
                        return;
                    }
                    at += 1;
                }
                Err(_) => {
                    self.unsaved.remove(at);
                    self.failed(&format!(
                        "keeping account limits beside the usage ledger in {} panicked: they're dropped, and saved again as they change",
                        folder.display()
                    ));
                }
            }
        }
    }

    /// Reports a failure: the first in a row as a warning, as limits are
    /// saved with every change.
    fn failed(&mut self, why: &str) {
        let level = if self.failing {
            log::Level::Debug
        } else {
            log::Level::Warn
        };
        log::log!(target: "caf", level, "{why}");
        self.failing = true;
    }

    /// As the thread ends: tries what's not yet kept once more, unless
    /// cooling, and reports what's lost.
    fn finish(&mut self, ledger: &Ledger, report: &impl Fn(Report)) {
        self.try_saving(ledger, report);
        if !self.unsaved.is_empty() {
            log::warn!(
                target: "caf",
                "{} profiles' account limits couldn't be kept beside the usage ledger in {} as CAF stopped: they're heard again as agents say them",
                self.unsaved.len(),
                ledger.folder().display()
            );
        }
    }
}

/// The lines waiting to be written, oldest first.
struct Kept<'a> {
    lines: VecDeque<LedgerLine>,
    /// The driver's count of lines not yet written, dropped or lost.
    unwritten: &'a AtomicUsize,
    /// The last write failed, and that was reported.
    failing: bool,
    /// Lines were dropped since the ledger last took one, and that was
    /// reported.
    dropping: bool,
    /// Another program's lock was given up on: lines aren't tried again
    /// until then.
    cooling: Option<Instant>,
    /// [`LOCK_COOLDOWN`].
    cooldown: Duration,
}

impl<'a> Kept<'a> {
    fn new(unwritten: &'a AtomicUsize, cooldown: Duration) -> Self {
        Self {
            lines: VecDeque::new(),
            unwritten,
            failing: false,
            dropping: false,
            cooling: None,
            cooldown,
        }
    }

    /// Keeps `line`, dropping the oldest if too many are waiting.
    fn add(&mut self, line: LedgerLine) {
        if self.lines.len() >= MOST_KEPT {
            self.lines.pop_front();
            self.unwritten.fetch_sub(1, Ordering::Relaxed);
            if !self.dropping {
                log::warn!(
                    target: "caf",
                    "{MOST_KEPT} turns' usage is waiting for the usage ledger: the oldest are dropped"
                );
                self.dropping = true;
            }
        }
        self.lines.push_back(line);
    }

    /// Writes every line waiting, as [`Kept::write`] does, reporting the
    /// first failure in a row, and the ledger taking lines again after;
    /// unless another program's lock was given up on less than the
    /// cooldown ago.
    fn try_writing(&mut self, ledger: &Ledger) {
        if self.cooling.is_some_and(|until| Instant::now() < until) {
            return;
        }
        self.cooling = None;
        match self.write(ledger) {
            Ok(()) => {
                if self.failing {
                    log::info!(target: "caf", "the usage ledger takes lines again: every turn's usage kept is written");
                }
                self.failing = false;
                self.dropping = false;
            }
            Err(error) => {
                if error.kind() == std::io::ErrorKind::TimedOut {
                    self.cooling = Some(Instant::now() + self.cooldown);
                }
                if !self.failing {
                    log::warn!(
                        target: "caf",
                        "a turn's usage can't be written to the usage ledger in {}: {error}. \
                         It's kept, and tried again with the next, and every hour",
                        ledger.folder().display()
                    );
                    self.failing = true;
                }
            }
        }
    }

    /// Writes every line waiting, in order, stopping at the first that
    /// fails, which stays first. A line whose write panics is dropped, so
    /// it never holds up the rest.
    fn write(&mut self, ledger: &Ledger) -> std::io::Result<()> {
        while let Some(line) = self.lines.front() {
            match std::panic::catch_unwind(AssertUnwindSafe(|| ledger.record(line))) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    log::warn!(target: "caf", "writing a turn's usage to the usage ledger panicked: it's dropped");
                }
            }
            self.lines.pop_front();
            self.unwritten.fetch_sub(1, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Lets go of every line, as lost, once that's reported.
    fn lose_all(&mut self) {
        self.unwritten
            .fetch_sub(self.lines.len(), Ordering::Relaxed);
        self.lines.clear();
    }
}

/// Looks at the prices now and then every `every`, until `stopped` is
/// dropped, telling `looks` each time, so they're read again: another app
/// may have changed them. Each time, first fetches LiteLLM's table with
/// `fetcher`, if given and it's due, and keeps it, if `keeping` still
/// says to: it's held while one is kept, so none is once it says not to.
fn look_now_and_then(
    folder: &std::path::Path,
    mut fetcher: Option<Fetcher>,
    keeping: &Mutex<bool>,
    looks: &mpsc::UnboundedSender<()>,
    stopped: &std::sync::mpsc::Receiver<()>,
    every: Duration,
) {
    let mut failing = false;
    loop {
        if let Some(fetcher) = &mut fetcher
            && due(prices::fetched_at(folder), wall_now())
        {
            let fetched = std::panic::catch_unwind(AssertUnwindSafe(fetcher))
                .unwrap_or_else(|_| Err("fetching it panicked".to_owned()));
            let taken = {
                let keeping = keeping.lock().unwrap_or_else(PoisonError::into_inner);
                // Nothing is kept once the driver is finishing.
                if !*keeping {
                    return;
                }
                fetched.and_then(|json| {
                    prices::take_fetched(folder, &json, wall_now())
                        .map_err(|why| format!("it isn't kept: {why}"))
                })
            };
            match taken {
                Ok(()) => {
                    if failing {
                        log::info!(target: "caf", "LiteLLM's price table was fetched again");
                    }
                    failing = false;
                }
                Err(why) => {
                    // Once in a row, so a machine offline isn't told every
                    // hour.
                    let level = if failing {
                        log::Level::Debug
                    } else {
                        log::Level::Warn
                    };
                    log::log!(
                        target: "caf",
                        level,
                        "LiteLLM's price table couldn't be fetched: {why}. The prices stay as they were, and it's tried again in an hour"
                    );
                    failing = true;
                }
            }
        }
        let _ = looks.send(());
        match stopped.recv_timeout(every) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => return,
        }
    }
}

/// Whether the table, last fetched at `fetched` if ever, is due to be
/// fetched at `now`: if it's never been, or a day has passed, or it says
/// it was fetched later than now, as when the clock has gone back.
fn due(fetched: Option<Wall>, now: Wall) -> bool {
    let every = i64::try_from(prices::FETCH_EVERY.as_millis()).unwrap_or(i64::MAX);
    fetched.is_none_or(|at| at > now || now.0.saturating_sub(at.0) >= every)
}

/// The wall clock now.
fn wall_now() -> Wall {
    let since = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    Wall(i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// Fetches `url`, up to [`MOST`] bytes, through the proxy the environment
/// names, if any: over `https` only, unless `https_only` is false, as it
/// is for tests on this machine.
fn fetch(url: &str, https_only: bool) -> Result<Vec<u8>, String> {
    let proxy = crate::setup::proxy_for(url, |name| std::env::var(name).ok());
    // Never the proxy's value in what's said: it may hold a password.
    let proxy = proxy
        .map(|(name, proxy)| {
            ureq::Proxy::new(&proxy).map_err(|_| format!("the proxy {name} names can't be used"))
        })
        .transpose()?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(proxy)
        .https_only(https_only)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .timeout_global(Some(Duration::from_mins(5)))
        .http_status_as_error(true)
        .build()
        .into();
    let response = agent.get(url).call().map_err(|error| failure(&error))?;
    (response.into_body().with_config().limit(MOST).read_to_vec()).map_err(|error| failure(&error))
}

/// Why a fetch failed, by the kind of failure alone: what ureq says may
/// hold a URL a redirect gave.
fn failure(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(code) => return format!("it answered HTTP {code}"),
        ureq::Error::Io(error) => {
            use std::io::ErrorKind::{
                ConnectionAborted, ConnectionRefused, ConnectionReset, HostUnreachable,
                NetworkDown, NetworkUnreachable, TimedOut,
            };
            let offline = [
                ConnectionAborted,
                ConnectionRefused,
                ConnectionReset,
                HostUnreachable,
                NetworkDown,
                NetworkUnreachable,
                TimedOut,
            ];
            if !offline.contains(&error.kind()) {
                return error.kind().to_string();
            }
        }
        _ => {}
    }
    match error {
        ureq::Error::Io(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Timeout(_)
        | ureq::Error::BodyStalled => "the machine seems to be offline",
        ureq::Error::Tls(_)
        | ureq::Error::Rustls(_)
        | ureq::Error::Pem(_)
        | ureq::Error::TlsRequired => "its site's certificate couldn't be checked",
        ureq::Error::RequireHttpsOnly(_) => "it was sent somewhere that isn't https",
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            "it was sent elsewhere too often, or somewhere it can't go"
        }
        ureq::Error::InvalidProxyUrl | ureq::Error::ConnectProxyFailed(_) => {
            "its proxy wouldn't take it"
        }
        ureq::Error::BodyExceedsLimit(_) => "it was far bigger than it should be",
        _ => "something went wrong",
    }
    .to_owned()
}

#[cfg(test)]
mod tests;
