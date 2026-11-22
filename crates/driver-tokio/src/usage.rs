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
//! limits reported if the file had news. A save that fails is reported,
//! and not tried again: the next has news as fresh.
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

use caf_core::limits::ProfileLimits;
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
}

impl Default for Options {
    fn default() -> Self {
        Self {
            every: CHECK_EVERY,
            prices_within: PRICES_WITHIN,
            finish_within: FINISH_WITHIN,
            cooldown: LOCK_COOLDOWN,
            stuck: None,
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
        let thread = std::thread::Builder::new()
            .name("caf-usage".into())
            .spawn(move || {
                if let Some(stuck) = stuck {
                    let _ = stuck.recv();
                }
                keep(ledger.as_ref(), &inbox, &telling, &counting, cooldown);
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
    /// ([`Event::Limits`](caf_core::Event::Limits)). Not tried again: the
    /// next save has news as fresh.
    #[allow(clippy::boxed_local, reason = "drafted: the builder sends it on to the thread")]
    pub(crate) fn save_limits(&mut self, limits: Box<ProfileLimits>) {
        let _ = limits;
        todo!()
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
                Some(()) = looked.recv() => self.send(Work::Reload(None)),
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
    let mut kept = Kept::new(unwritten, cooldown);
    while let Ok(work) = inbox.recv() {
        match work {
            Work::Record(line) => {
                if let Some(ledger) = ledger {
                    kept.add(*line);
                    kept.try_writing(ledger);
                }
            }
            Work::SetPrice { set, model, price } => {
                // Never given without one.
                let Some(ledger) = ledger else {
                    continue;
                };
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
            Work::Reload(asked) => {
                if let Some(ledger) = ledger {
                    // Every hour, whatever the lock did.
                    if asked.is_none() && !kept.lines.is_empty() {
                        kept.cooling = None;
                        kept.try_writing(ledger);
                    }
                    give(read_prices(Some(ledger)));
                }
                if let Some(asked) = asked {
                    let _ = tell.send(Told::Reloaded(asked));
                }
            }
        }
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
