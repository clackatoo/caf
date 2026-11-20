//! The setup driver: installing agents and signing in to them, as the core
//! directs ([`SetupDirective`]), on tokio.
//!
//! It keeps the contract written on [`SetupDirective`]: installs built in a
//! hidden folder and renamed into place whole; one install of a folder at
//! a time across every CAF program on the machine; nothing removed while a
//! process runs from it; `https` downloads checked before they're unpacked;
//! and nothing a sign-in or status command prints reaches the core except
//! what its recipe names.
//!
//! npm installs run `npm ci --ignore-scripts --no-audit --no-fund` from the
//! profile's lockfile. Downloads are fetched by the driver itself, not by
//! `curl`. Sign-in commands that open a browser are given a `BROWSER` of
//! the driver's own, which hands their link back instead of opening
//! anything.
//!
//! # How it works
//!
//! Each directive that does something runs as a task of its own, so
//! nothing it does holds up the core, or any other. Work that touches
//! the disk for long (writing, unpacking, measuring, deleting) runs on
//! tokio's blocking threads, and downloads on one of those too. Every
//! task reports through one queue, which [`SetupDriver::next`] reads.
//!
//! The folder installs live in, the *root*, holds:
//!
//! | Name | What it is |
//! |---|---|
//! | `<agent>-<version>/` | An install, complete: only ever renamed into place whole |
//! | `<agent>-private/` | An agent's private folder, which outlives its versions: made, readable only by the user, by an install whose directive names it; never listed, never removed. One that's a link is refused |
//! | `.lock` | The lock every install and removal takes, so only one runs at a time on the machine |
//! | `.build-…/` | An install being built. Any left when nothing holds `.lock` is a cut-off one, removed |
//! | `.trash-…/` | An install being removed. Any left when nothing holds `.lock` is removed |
//!
//! **Locks** are `flock`s, which the system lets go of when the process
//! holding them ends, however it ends. Waiting for one is a poll every
//! tenth of a second, so it never blocks a thread and can be stopped.
//! - An install holds `.lock` from before it looks whether the folder is
//!   there until it's renamed into place, so two programs installing one
//!   folder at once install it once: the second finds it there.
//! - The agent driver holds a shared lock on an install's folder while a
//!   process runs from it ([`AgentDirective::Start`]'s `hold`). A removal
//!   holds `.lock`, then takes the folder's lock exclusively without
//!   waiting: if it can't, something runs from it, and it's kept. If it
//!   can, the folder is renamed to `.trash-…` before the lock is let go,
//!   and deleted after.
//! - A survey removes `.build-…` and `.trash-…` folders only if it can
//!   take `.lock` at once: while something holds it, they may be in use.
//! - A process another thread of the program starts holds every file
//!   open in it for a moment, until it starts its program, and so every
//!   lock too. So a removal just after a process ends may find its install
//!   still held, and keep it: never the other way round.
//!
//! **Commands** (sign-in, sign-out, status) run in a process group of
//! their own, with the environment the core gave, `PATH` cut to its
//! absolute folders, as for adapters. Stopping one kills its group. What
//! each prints is read only for what its recipe names: a sign-in's link
//! and code, or a status command's fields. Nothing else of it reaches the
//! core, or the log: a failure is told in the driver's own words.
//!
//! **Sign-in links** come one of two ways ([`LinkFrom`]):
//! - `Browser`: the command is given `BROWSER`, a small shell script in a
//!   folder of the driver's own under the system's temporary folder,
//!   readable only by the user. Run with a link, it writes the link to a
//!   file beside it and exits; the driver looks for such files every
//!   tenth of a second, and reports each link once. The folder goes when
//!   the command ends.
//! - `Output(marker)`: each line the command prints, on either output, is
//!   looked at for the marker, and what follows it, up to the next space,
//!   is the link.
//!
//! Either way, a line holding the recipe's `code_after` gives the code
//! after it the same way. Terminal colours and other escape sequences are
//! taken out of lines first, and lines longer than 64 KiB are skipped.
//!
//! [`AgentDirective::Start`]: caf_core::directive::AgentDirective::Start
//! [`LinkFrom`]: caf_core::install::LinkFrom

mod command;
mod install;
#[cfg(test)]
mod tests;

pub(crate) use install::{Lock, lock_file};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use caf_core::directive::SetupDirective;
use caf_core::event::SetupEvent;
use caf_core::ids::Ticket;
use tokio::sync::{mpsc, oneshot};

/// Installs agents and runs their sign-in commands.
///
/// What a host should know, besides the contract on [`SetupDirective`]:
///
/// - **It needs `/bin/sh`** for the `BROWSER` it gives sign-in commands,
///   and Node and npm on `PATH` for npm installs.
/// - **Stopping** an install, sign-in or command kills what it runs and
///   reports it stopped within about a second, as
///   [`SetupDirective::Stop`] says. A download waiting on the network
///   can't be woken: it's left to end by itself, within a minute, writing
///   only to a file that went with the half-built folder.
/// - **Dropping the driver** stops everything it's doing, as stopping
///   would, as far as the runtime lets it: a half-built install the runtime
///   shut down too soon to remove is removed by the next survey.
/// - **A pasted text** goes to the sign-in's standard input as one line: up
///   to its first line break, without spaces around it.
/// - **Timeouts:** sign-out and other commands have 2 minutes, status
///   commands 1 minute, and finding Node's version or npm 30 seconds. A
///   sign-in has none, as it waits for the user: the core stops it.
///   Downloads give up when connecting takes over 30 seconds, or the
///   answer doesn't start within a minute.
/// - **Proxies:** a download goes through the proxy npm would use:
///   `HTTPS_PROXY`, `https_proxy`, `HTTP_PROXY` or `http_proxy`, the first
///   that's set, unless `NO_PROXY` (or `no_proxy`), a list split by commas,
///   names its host or a domain the host is in (`example.com` and
///   `.example.com` both cover `dl.example.com`). npm installs are npm's
///   own, and read the same. A download asks the proxy for a tunnel
///   (`CONNECT`), and its `https` is checked end to end through it.
/// - **A download unpacked as `Unpack::No`** is saved under the last part
///   of its URL's path, such as `agent` for `https://example.com/v1/agent`,
///   and made runnable.
pub struct SetupDriver {
    root: PathBuf,
    /// What's under way, by ticket, until its last report is read.
    jobs: HashMap<Ticket, Job>,
    reports: mpsc::UnboundedReceiver<Report>,
    /// Kept so `reports` never ends, and given to each job.
    sender: mpsc::UnboundedSender<Report>,
    options: Options,
}

/// A job under way: how to stop it, and give it what the user pasted.
struct Job {
    stop: Option<oneshot::Sender<()>>,
    paste: Option<mpsc::UnboundedSender<String>>,
}

/// One report from a job: whether it's the job's last.
struct Report {
    ticket: Ticket,
    event: SetupEvent,
    last: bool,
}

/// How a job reports.
#[derive(Clone)]
pub(crate) struct Reporter {
    ticket: Ticket,
    sender: mpsc::UnboundedSender<Report>,
}

impl Reporter {
    /// Reports `event`, with more to come.
    pub(crate) fn send(&self, event: SetupEvent) {
        let _ = self.sender.send(Report {
            ticket: self.ticket,
            event,
            last: false,
        });
    }

    /// Reports `event`, the job's last.
    pub(crate) fn last(self, event: SetupEvent) {
        let _ = self.sender.send(Report {
            ticket: self.ticket,
            event,
            last: true,
        });
    }
}

/// Settings tests change. None, outside tests.
#[derive(Clone, Debug, Default)]
pub(crate) struct Options {
    /// Downloads may be `http`, to this machine only.
    #[cfg(test)]
    pub(crate) http_to_this_machine: bool,
    /// npm installs run `--offline`, with this cache.
    #[cfg(test)]
    pub(crate) npm_offline_cache: Option<PathBuf>,
    /// Where a sign-in's `BROWSER` folder is made, instead of the
    /// system's temporary folder.
    #[cfg(test)]
    pub(crate) temp: Option<PathBuf>,
    /// The environment variables a download reads, such as `HTTPS_PROXY`,
    /// instead of this program's.
    #[cfg(test)]
    pub(crate) env: Option<std::collections::HashMap<String, String>>,
}

impl Options {
    /// Where a sign-in's `BROWSER` folder is made.
    fn temp(&self) -> PathBuf {
        #[cfg(test)]
        if let Some(temp) = &self.temp {
            return temp.clone();
        }
        std::env::temp_dir()
    }
}

/// A number for each hidden folder this process makes, so no two drivers
/// in it ever pick the same name.
static HIDDEN: AtomicU64 = AtomicU64::new(0);

/// A name for a hidden folder, `.{kind}-{folder}-{pid}-{n}`, that no other
/// in this process, or any other running one, has.
pub(crate) fn hidden_name(kind: &str, folder: &str) -> String {
    let n = HIDDEN.fetch_add(1, Ordering::Relaxed);
    format!(".{kind}-{folder}-{}-{n}", std::process::id())
}

impl SetupDriver {
    /// A driver keeping installs under `root`, which it makes, readable
    /// only by the user, when it first installs something. Until then it
    /// makes nothing: a survey of a root that isn't there finds nothing
    /// installed.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self::with_options(root, Options::default())
    }

    /// A driver with `options`.
    pub(crate) fn with_options(root: PathBuf, options: Options) -> Self {
        let (sender, reports) = mpsc::unbounded_channel();
        Self {
            root,
            jobs: HashMap::new(),
            reports,
            sender,
            options,
        }
    }

    /// The folder installs go in by default: `$XDG_DATA_HOME/caf/agents`,
    /// or `~/.local/share/caf/agents`. `None` if neither can be found.
    ///
    /// Either is taken only if it's an absolute path, as the XDG rules
    /// say: a relative one would name a different folder for each working
    /// directory.
    #[must_use]
    pub fn default_root() -> Option<PathBuf> {
        let absolute = |name| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let data = absolute("XDG_DATA_HOME")
            .or_else(|| absolute("HOME").map(|home| home.join(".local/share")))?;
        Some(data.join("caf/agents"))
    }

    /// Starts carrying out a directive. It never blocks: the work runs on
    /// tasks and threads of the driver's own, and reports come from
    /// [`SetupDriver::next`].
    ///
    /// A `Paste` or `Stop` for a ticket that's done, or never was, does
    /// nothing.
    ///
    /// # Panics
    ///
    /// If it starts work outside a tokio runtime.
    pub fn apply(&mut self, directive: SetupDirective) {
        match directive {
            SetupDirective::Survey { ticket } => {
                let reporter = self.job(ticket);
                tokio::spawn(install::survey(self.root.clone(), reporter));
            }
            SetupDirective::Install {
                ticket,
                folder,
                install,
                private,
            } => {
                let (reporter, stop) = self.stoppable(ticket);
                tokio::spawn(install::install(
                    self.root.clone(),
                    install::Folders { folder, private },
                    install,
                    self.options.clone(),
                    reporter,
                    stop,
                ));
            }
            SetupDirective::Remove { ticket, folder } => {
                let reporter = self.job(ticket);
                tokio::spawn(install::remove(self.root.clone(), folder, reporter));
            }
            SetupDirective::SignIn {
                ticket,
                command,
                link,
                code_after,
                paste,
            } => {
                let (reporter, stop) = self.stoppable(ticket);
                let pasted = paste.then(|| self.pasted(ticket));
                tokio::spawn(command::sign_in(
                    command::SignIn {
                        command,
                        link,
                        code_after,
                        pasted,
                        temp: self.options.temp(),
                    },
                    reporter,
                    stop,
                ));
            }
            SetupDirective::Paste { ticket, text } => {
                if let Some(paste) = (self.jobs.get(&ticket)).and_then(|job| job.paste.as_ref()) {
                    let _ = paste.send(text);
                }
            }
            SetupDirective::Run { ticket, command } => {
                let (reporter, stop) = self.stoppable(ticket);
                tokio::spawn(command::run(command, reporter, stop));
            }
            SetupDirective::Status {
                ticket,
                command,
                signed_in,
                how,
            } => {
                let (reporter, stop) = self.stoppable(ticket);
                tokio::spawn(command::status(command, signed_in, how, reporter, stop));
            }
            SetupDirective::Stop { ticket } => {
                if let Some(stop) = (self.jobs.get_mut(&ticket)).and_then(|job| job.stop.take()) {
                    let _ = stop.send(());
                }
            }
            // No other kind is given yet.
            _ => {}
        }
    }

    /// The next report, quoting the ticket of the directive it's about.
    ///
    /// Cancel-safe: dropped before it resolves, it loses nothing.
    pub async fn next(&mut self) -> (Ticket, SetupEvent) {
        let Some(report) = self.reports.recv().await else {
            // Never: the driver keeps a sender itself.
            return std::future::pending().await;
        };
        if report.last {
            self.jobs.remove(&report.ticket);
        }
        (report.ticket, report.event)
    }

    /// Notes a new job under `ticket`, and gives its reporter.
    fn job(&mut self, ticket: Ticket) -> Reporter {
        debug_assert!(!self.jobs.contains_key(&ticket), "a ticket was used twice");
        self.jobs.insert(
            ticket,
            Job {
                stop: None,
                paste: None,
            },
        );
        Reporter {
            ticket,
            sender: self.sender.clone(),
        }
    }

    /// Notes a new job that can be stopped, and gives its reporter and
    /// what tells it to stop. Dropping the driver tells it too.
    fn stoppable(&mut self, ticket: Ticket) -> (Reporter, oneshot::Receiver<()>) {
        let reporter = self.job(ticket);
        let (stop, stopped) = oneshot::channel();
        if let Some(job) = self.jobs.get_mut(&ticket) {
            job.stop = Some(stop);
        }
        (reporter, stopped)
    }

    /// A way to give the sign-in under `ticket` what's pasted.
    fn pasted(&mut self, ticket: Ticket) -> mpsc::UnboundedReceiver<String> {
        let (paste, pasted) = mpsc::unbounded_channel();
        if let Some(job) = self.jobs.get_mut(&ticket) {
            job.paste = Some(paste);
        }
        pasted
    }
}
