//! The usage driver: the usage ledger and the prices, on a thread of
//! their own, and LiteLLM's price table, fetched on another.
//!
//! Each line is written with [`Ledger::record`], in the order it's given.
//! A line that can't be written is kept, and tried again before the next,
//! so a ledger that fails for a while loses nothing; the first failure in
//! a row is reported to the [`log`] facade, target `caf`, and so is the
//! ledger taking lines again. It never answers the core, and nothing it
//! does holds up a chat. Once it's finishing, what's still kept is tried
//! once more, and any left is reported lost.
//!
//! The prices are read as the thread starts, and again whenever they
//! change: a price of the user's own set ([`prices::set_own`]), or
//! LiteLLM's table fetched. Each time, the whole table goes to the task,
//! for the core ([`Event::Prices`](caf_core::Event::Prices)). Fetching is
//! another thread's, so a slow download never holds a line up: it looks
//! as it starts and then every hour, and fetches if the table every CAF
//! app shares ([`prices::fetched_at`]) is a day old, or was never
//! fetched. A failed fetch is reported, and tried again in an hour.
//!
//! With no ledger, the prices are the bundled snapshot's, and lines are
//! dropped.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, SystemTime};

use caf_core::time::Wall;
use caf_core::usage::{LedgerLine, ModelPrice, PriceTable};
use caf_usage::{Ledger, prices};
use tokio::sync::{mpsc, oneshot};

use crate::caf::{Error, UsageConfig};

/// How often the fetching thread looks at how old the prices are.
const CHECK_EVERY: Duration = Duration::from_hours(1);

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
}

/// Work for the ledger's thread.
enum Work {
    Record(Box<LedgerLine>),
    SetPrice {
        model: String,
        price: Option<ModelPrice>,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    /// The prices changed on disk: read them again.
    Reload,
}

/// Keeps the ledger and the prices on a thread of its own, and fetches
/// prices on another, if asked to.
pub(crate) struct UsageDriver {
    /// The ledger's folder, if there is one.
    folder: Option<PathBuf>,
    /// Work for the thread. Dropped to end it.
    work: Option<std::sync::mpsc::Sender<Work>>,
    /// Where reports come from: the driver keeps a sender, so it never
    /// closes.
    reports: mpsc::UnboundedReceiver<Report>,
    _tell: mpsc::UnboundedSender<Report>,
    /// Each fetch kept, from the fetching thread.
    fetched: mpsc::UnboundedReceiver<()>,
    /// Dropped to stop the fetching thread.
    fetching: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl UsageDriver {
    /// A driver for the ledger `usage` says, or none, writing lines as the
    /// app named `app`.
    pub(crate) fn new(usage: Option<UsageConfig>, app: String) -> Self {
        let fetcher = (usage.as_ref())
            .filter(|usage| usage.fetch_prices)
            .map(|_| Box::new(|| fetch(prices::LITELLM, true)) as Fetcher);
        Self::start(usage.map(|usage| usage.folder), app, fetcher, CHECK_EVERY)
    }

    /// A driver for the ledger in `folder`, or none, fetching prices with
    /// `fetcher` if given, looking every `every`.
    pub(crate) fn start(
        folder: Option<PathBuf>,
        app: String,
        fetcher: Option<Fetcher>,
        every: Duration,
    ) -> Self {
        let (tell, reports) = mpsc::unbounded_channel();
        let (work, inbox) = std::sync::mpsc::channel();
        let ledger = (folder.clone()).map(|folder| Ledger::new(folder, app));
        let telling = tell.clone();
        let thread = std::thread::Builder::new()
            .name("caf-usage".into())
            .spawn(move || keep(ledger.as_ref(), &inbox, &telling))
            .expect("a thread for the usage ledger");
        let (kept, fetched) = mpsc::unbounded_channel();
        let fetching = match (&folder, fetcher) {
            (Some(folder), Some(fetcher)) => {
                let (stop, stopped) = std::sync::mpsc::channel();
                let folder = folder.clone();
                std::thread::Builder::new()
                    .name("caf-prices".into())
                    .spawn(move || fetch_now_and_then(&folder, fetcher, &kept, &stopped, every))
                    .expect("a thread for fetching prices");
                Some(stop)
            }
            _ => None,
        };
        Self {
            folder,
            work: Some(work),
            reports,
            _tell: tell,
            fetched,
            fetching,
            thread: Some(thread),
        }
    }

    /// The ledger's folder, if there is one.
    pub(crate) fn folder(&self) -> Option<&PathBuf> {
        self.folder.as_ref()
    }

    /// Writes a turn's line to the ledger, or drops it with none.
    pub(crate) fn record(&mut self, line: Box<LedgerLine>) {
        if self.folder.is_some() {
            self.send(Work::Record(line));
        }
    }

    /// Sets a price of the user's own, answering `reply` once the prices
    /// it changed are reported. With no ledger, it's
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
        self.send(Work::SetPrice {
            model,
            price,
            reply,
        });
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
            tokio::select! {
                Some(report) = self.reports.recv() => return report,
                Some(()) = self.fetched.recv() => self.send(Work::Reload),
                else => std::future::pending().await,
            }
        }
    }

    /// Stops fetching, and ends the ledger's thread once it has written
    /// what it was given, and waits for it. A fetch under way isn't waited
    /// for: it keeps nothing once it's stopped.
    pub(crate) async fn finish(mut self) {
        self.fetching = None;
        self.work = None;
        if let Some(thread) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
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
    tell: &mpsc::UnboundedSender<Report>,
) {
    // Once the task has gone, nobody needs a report.
    let _ = tell.send(Report::Prices(read_prices(ledger)));
    let mut kept = Kept::default();
    while let Ok(work) = inbox.recv() {
        match work {
            Work::Record(line) => {
                if let Some(ledger) = ledger {
                    kept.add(*line);
                    kept.write(ledger);
                }
            }
            Work::SetPrice {
                model,
                price,
                reply,
            } => {
                let Some(ledger) = ledger else {
                    let _ = reply.send(Err(Error::NoLedger));
                    continue;
                };
                let set = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    prices::set_own(ledger.folder(), &model, price)
                }));
                let result = match set {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(_) => Err("setting it panicked".to_owned()),
                };
                if result.is_ok() {
                    let _ = tell.send(Report::Prices(read_prices(Some(ledger))));
                }
                let _ = tell.send(Report::PriceSet { reply, result });
            }
            Work::Reload => {
                let _ = tell.send(Report::Prices(read_prices(ledger)));
            }
        }
    }
    if let Some(ledger) = ledger {
        kept.write(ledger);
        if !kept.lines.is_empty() {
            log::warn!(
                target: "caf",
                "{} turns' usage couldn't be written to the usage ledger in {}, and is lost",
                kept.lines.len(),
                ledger.folder().display()
            );
        }
    }
}

/// The lines waiting to be written, oldest first.
#[derive(Default)]
struct Kept {
    lines: VecDeque<LedgerLine>,
    /// The last write failed, and that was reported.
    failing: bool,
    /// Lines were dropped since the ledger last took one, and that was
    /// reported.
    dropping: bool,
}

impl Kept {
    /// Keeps `line`, dropping the oldest if too many are waiting.
    fn add(&mut self, line: LedgerLine) {
        if self.lines.len() >= MOST_KEPT {
            self.lines.pop_front();
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

    /// Writes every line waiting, in order, stopping at the first that
    /// fails, which stays first. A line whose write panics is dropped, so
    /// it never holds up the rest.
    fn write(&mut self, ledger: &Ledger) {
        while let Some(line) = self.lines.front() {
            match std::panic::catch_unwind(AssertUnwindSafe(|| ledger.record(line))) {
                Ok(Ok(())) => {
                    self.lines.pop_front();
                }
                Ok(Err(error)) => {
                    if !self.failing {
                        log::warn!(
                            target: "caf",
                            "a turn's usage can't be written to the usage ledger in {}: {error}. \
                             It's kept, and tried again with the next",
                            ledger.folder().display()
                        );
                        self.failing = true;
                    }
                    return;
                }
                Err(_) => {
                    log::warn!(target: "caf", "writing a turn's usage to the usage ledger panicked: it's dropped");
                    self.lines.pop_front();
                }
            }
        }
        if self.failing {
            log::info!(target: "caf", "the usage ledger takes lines again: every turn's usage kept is written");
        }
        self.failing = false;
        self.dropping = false;
    }
}

/// Fetches LiteLLM's table now, if it's due, and then every `every`,
/// until `stopped` is dropped, telling `kept` each time one is kept.
fn fetch_now_and_then(
    folder: &std::path::Path,
    mut fetcher: Fetcher,
    kept: &mpsc::UnboundedSender<()>,
    stopped: &std::sync::mpsc::Receiver<()>,
    every: Duration,
) {
    let mut failing = false;
    loop {
        let now = wall_now();
        if due(prices::fetched_at(folder), now) {
            let fetched = std::panic::catch_unwind(AssertUnwindSafe(&mut fetcher))
                .unwrap_or_else(|_| Err("fetching it panicked".to_owned()));
            // Nothing is kept once the driver has finished.
            if !matches!(
                stopped.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ) {
                return;
            }
            let taken = fetched.and_then(|json| {
                prices::take_fetched(folder, &json, wall_now())
                    .map_err(|why| format!("it isn't kept: {why:?}"))
            });
            match taken {
                Ok(()) => {
                    if failing {
                        log::info!(target: "caf", "LiteLLM's price table was fetched again");
                    }
                    failing = false;
                    let _ = kept.send(());
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
