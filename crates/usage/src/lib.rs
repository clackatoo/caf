//! CAF's usage ledger, which every CAF app on a machine shares, the
//! summaries read from it, and the prices cost is estimated with. No async
//! runtime, so any runtime's usage driver uses it: `caf-driver-tokio` runs
//! it on a blocking thread, and a host on plain threads on one of its own.
//!
//! The core says what to write ([`UsageDirective`]); [`Ledger::record`]
//! writes it. A summary ([`summarise`]) reads the ledger without the core,
//! as it touches no chat. Prices ([`prices`]) are read here, from the
//! user's own, LiteLLM's table as last fetched, and the snapshot bundled
//! with CAF, and the driver hands them to the core.
//!
//! # The folder
//!
//! One per user and machine, shared by every CAF app on it
//! ([`default_folder`]):
//!
//! ```text
//! usage/
//!   clackatoo/2026-10.jsonl.zst    a month that has ended, compressed
//!   clackatoo/2026-11.jsonl        this month, a line per turn
//!   eyespy/2026-11.jsonl
//!   prices/own.json                the user's own prices
//!   prices/litellm.json            LiteLLM's table, as last fetched
//! ```
//!
//! Each line is one [`LedgerLine`], as JSON in CAF's one shape (see
//! caf-core's docs). What it never holds: what was said, and the folder a
//! chat works in, so the ledger can't tell which projects you work on.
//!
//! # Guarantees
//!
//! Each of these has a test.
//!
//! 1. **Every line is whole.** One is added under the app's lock, in one
//!    write, so two copies of an app never mix theirs; a line cut off by a
//!    crash is the file's last, and is skipped as it's read.
//! 2. **Lines are only ever added**, and every line ever written is read by
//!    every later CAF: fields are only added, and readers ignore those they
//!    don't know.
//! 3. **A turn counts once**, however many times its line was written: the
//!    first line for each `(chat, turn)` counts.
//! 4. **A summary is never quietly short:** every file in its period it
//!    couldn't read is named in it ([`Summary::unreadable`]).
//! 5. **A month is compressed only once it has ended**, by whichever app
//!    first writes in a later one, and the file is replaced in one step, so
//!    a reader sees one or the other whole.
//!
//! [`UsageDirective`]: caf_core::directive::UsageDirective
//! [`LedgerLine`]: caf_core::usage::LedgerLine
//! [`Summary::unreadable`]: caf_core::usage::Summary::unreadable

use std::io;
use std::path::{Path, PathBuf};

use caf_core::usage::{BadQuery, LedgerLine, PriceTable, Summary, UsageQuery};

pub mod prices;

/// The ledger's folder every CAF program shares, by default:
/// `$XDG_DATA_HOME/caf/usage`, or `~/.local/share/caf/usage` without it;
/// `~/Library/Application Support/caf/usage` on macOS; and
/// `%APPDATA%\caf\usage` on Windows. `None` if there's no home folder to
/// find it from.
#[must_use]
pub fn default_folder() -> Option<PathBuf> {
    todo!()
}

/// One app's side of the ledger: where it adds its lines.
#[derive(Debug)]
pub struct Ledger {
    folder: PathBuf,
    app: String,
}

impl Ledger {
    /// The ledger in `folder`, for the app named `app`: its host's
    /// [`name`](caf_core::Host::name). Nothing is touched until a line is
    /// written.
    #[must_use]
    pub fn new(folder: PathBuf, app: impl Into<String>) -> Self {
        Self {
            folder,
            app: app.into(),
        }
    }

    /// The folder.
    #[must_use]
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// The app.
    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }

    /// Adds a line to the app's file for the month it ended in, in UTC,
    /// making the folders if need be. If that's a later month than a file
    /// left uncompressed, that file is compressed first.
    ///
    /// # Errors
    ///
    /// What the system said, if it couldn't: the line isn't written. The
    /// usage driver keeps it and tries again with the next.
    pub fn record(&self, line: &LedgerLine) -> io::Result<()> {
        let _ = line;
        todo!()
    }
}

/// Usage over a period, from every app's lines in `folder`, with cost
/// estimated with `prices` for turns that reported none. Only the months
/// the period touches are read.
///
/// # Errors
///
/// [`BadQuery`] if the query's time zone isn't known, or its period is
/// empty or longer than 400 days. Files that can't be read don't fail it:
/// they're named in the summary.
pub fn summarise(
    folder: &Path,
    query: &UsageQuery,
    prices: &PriceTable,
) -> Result<Summary, BadQuery> {
    let _ = (folder, query, prices);
    todo!()
}
