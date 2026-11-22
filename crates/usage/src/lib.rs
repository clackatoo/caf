//! CAF's usage ledger, which every CAF app on a machine shares, the
//! summaries read from it, and the prices cost is estimated with. No async
//! runtime, so any runtime's usage driver uses it: `caf-driver-tokio` runs
//! it on a blocking thread, and a host on plain threads on one of its own.
//! Unix only, macOS included, as caf-save is: it locks with `flock`.
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
//! caf-core's docs), in its app's file for the month the turn ended in,
//! in UTC. An app writes only in its own folder, under that folder's lock
//! (`.lock`). A turn that ends in a month already compressed, as one
//! recorded just after midnight may, goes in that month's `.jsonl` all the
//! same: a reader reads both files, and the next compression joins them.
//!
//! What it never holds: what was said, and the folder a chat works in, so
//! the ledger can't tell which projects you work on.
//!
//! # Guarantees
//!
//! Each of these has a test.
//!
//! 1. **Every line is whole.** One is added under the app's lock, in one
//!    write, so two copies of an app never mix theirs. Only a line with
//!    its line end counts: one without may be cut off by a crash, or still
//!    being written, so it's never counted, and the next line added is
//!    written over it. A line that doesn't read names its file.
//! 2. **Lines are only ever added**, and every line ever written is read by
//!    every later CAF: fields are only added, and readers ignore those they
//!    don't know.
//! 3. **A turn counts once**, however many times its line was written: the
//!    first line for each `(chat, turn)` counts.
//! 4. **A summary is never quietly short:** every file in its period it
//!    couldn't read is named in it ([`Summary::unreadable`]).
//! 5. **A month is compressed only once it has ended**: each app
//!    compresses its own folder's, when it first writes in a later month,
//!    and the file is replaced in one step, so a reader sees one or the
//!    other whole.
//! 6. **Every count is below 2^53**, as everything CAF writes is: a line
//!    with one that isn't is refused, or read as unreadable, and a sum
//!    stops at 2^53 - 1.
//!
//! [`UsageDirective`]: caf_core::directive::UsageDirective
//! [`LedgerLine`]: caf_core::usage::LedgerLine
//! [`Summary::unreadable`]: caf_core::usage::Summary::unreadable

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use caf_core::ChatId;
use caf_core::ids::TurnId;
use caf_core::time::Wall;
use caf_core::usage::{
    BadQuery, Bucket, LedgerLine, PriceTable, PricedModel, Resolution, Summary, Tokens, TurnUsage,
    UsageQuery,
};
use jiff::Timestamp;
use jiff::tz::TimeZone;

mod files;
pub mod prices;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

/// The folder prices are kept in, which no app may be named.
const PRICES: &str = "prices";
/// The longest period a summary covers: 400 days, in milliseconds.
const LONGEST: i64 = 400 * 24 * 60 * 60 * 1000;
/// How hard a finished month is compressed: well, and still quickly, as
/// it's done under the app's lock.
const LEVEL: i32 = 12;
/// The most any count may be: 2^53 - 1, the most JSON's numbers hold
/// exactly everywhere.
const MOST: u64 = (1 << 53) - 1;

/// The ledger's folder every CAF program shares, by default:
/// `$XDG_DATA_HOME/caf/usage`, or `~/.local/share/caf/usage` without it,
/// on every Unix, macOS included, beside the agents CAF installs. Each is
/// taken only if it's an absolute path, as the XDG rules say. `None` if
/// there's no home folder to find it from.
#[must_use]
pub fn default_folder() -> Option<PathBuf> {
    folder_from(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

/// The default folder, from `$XDG_DATA_HOME` and `$HOME`.
fn folder_from(data: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let data = absolute(data).or_else(|| absolute(home).map(|home| home.join(".local/share")))?;
    Some(data.join("caf/usage"))
}

/// A path from the environment, if it's absolute.
fn absolute(path: Option<OsString>) -> Option<PathBuf> {
    path.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// One app's side of the ledger: where it adds its lines.
#[derive(Debug)]
pub struct Ledger {
    folder: PathBuf,
    app: String,
    /// The months whose compression failed: not tried again by this
    /// ledger, so a month that can't be compressed isn't read again with
    /// every line. The next ledger, as after a restart, tries again.
    failed: Mutex<HashSet<String>>,
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
            failed: Mutex::new(HashSet::new()),
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
    /// left uncompressed, that file is compressed first; if that fails,
    /// it's left as it is, and the line is added all the same. A month
    /// that failed is tried again by the next ledger made, as after a
    /// restart.
    ///
    /// # Errors
    ///
    /// What the system said, if it couldn't: the line isn't written. The
    /// usage driver keeps it and tries again with the next.
    /// [`InvalidInput`](io::ErrorKind::InvalidInput) if the app's name
    /// can't be a folder's (empty, `prices`, starting with a dot, or with
    /// a slash, backslash or nothing character in it; `prices` in any
    /// case); if the line is another app's, has a count of 2^53 or more or
    /// a cost that isn't a number; or if the turn ended outside the years
    /// 0 to 9999.
    pub fn record(&self, line: &LedgerLine) -> io::Result<()> {
        if !is_app(&self.app) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{:?} can't name an app's folder", self.app),
            ));
        }
        let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, why.to_owned());
        if line.app != self.app {
            return Err(invalid("the line is another app's"));
        }
        if !within(&line.usage) {
            return Err(invalid("the line has a count of 2^53 or more"));
        }
        if line
            .usage
            .cost
            .as_ref()
            .is_some_and(|cost| !cost.amount.is_finite())
        {
            return Err(invalid("the line's cost isn't a number"));
        }
        let month = month_of(line.at).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the turn ended outside 0 to 9999",
            )
        })?;
        let mut bytes = serde_json::to_vec(line).map_err(io::Error::other)?;
        bytes.push(b'\n');

        let folder = self.folder.join(&self.app);
        files::make_folder(&folder)?;
        let _lock = files::lock(&folder)?;
        let mut failed = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
        for earlier in uncompressed(&folder)? {
            if earlier < month && !failed.contains(&earlier) && compress(&folder, &earlier).is_err()
            {
                failed.insert(earlier);
            }
        }
        drop(failed);
        append(&folder.join(format!("{month}.jsonl")), &bytes)
    }
}

/// Whether every count in a turn's usage is below 2^53.
fn within(usage: &TurnUsage) -> bool {
    usage.models.iter().all(|row| {
        let tokens = &row.tokens;
        [
            tokens.input,
            tokens.cache_read,
            tokens.cache_write,
            tokens.output,
            tokens.thinking.unwrap_or(0),
        ]
        .into_iter()
        .all(|count| count <= MOST)
    })
}

/// Whether `app` can name an app's folder in the ledger's.
fn is_app(app: &str) -> bool {
    // Not `Prices` either: a file system that ignores case would make
    // them one.
    !app.is_empty()
        && !app.eq_ignore_ascii_case(PRICES)
        && !app.starts_with('.')
        && !app.contains(['/', '\\', '\0'])
}

/// The month `at` is in, in UTC, as its files are named: `2026-11`.
fn month_of(at: Wall) -> Option<String> {
    let date = Timestamp::from_millisecond(at.0)
        .ok()?
        .to_zoned(TimeZone::UTC)
        .date();
    (date.year() >= 0).then(|| format!("{:04}-{:02}", date.year(), date.month()))
}

/// Whether `name` is a month, as files are named.
fn is_month(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 7
        && bytes[4] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(at, byte)| at == 4 || byte.is_ascii_digit())
}

/// The months an app's folder has a `.jsonl` for: those not compressed.
fn uncompressed(folder: &Path) -> io::Result<Vec<String>> {
    let mut months = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let name = entry?.file_name();
        if let Some(month) = name.to_str().and_then(|name| name.strip_suffix(".jsonl"))
            && is_month(month)
        {
            months.push(month.to_owned());
        }
    }
    Ok(months)
}

/// Where a file's whole lines end: after its last line end.
fn whole(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1)
}

/// Adds `bytes`, a whole line, to the end of the file at `path`, in one
/// write, synced. A line a crash cut off at its end is written over first,
/// so it never runs into the new one: it was never whole, so never
/// counted.
fn append(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    let size = file.metadata()?.len();
    if size == 0 {
        file.write_all(bytes)?;
        file.sync_data()?;
        // Made just now, perhaps: its name is synced too.
        return files::sync_folder(path.parent().unwrap_or(Path::new(".")));
    }
    let mut last = [0];
    file.read_exact_at(&mut last, size - 1)?;
    if last[0] != b'\n' {
        file.set_len(whole_end(&file, size)?)?;
    }
    file.write_all(bytes)?;
    file.sync_data()
}

/// Where the whole lines of a file `size` long end, read from its end
/// back.
fn whole_end(file: &File, size: u64) -> io::Result<u64> {
    const CHUNK: u64 = 64 * 1024;
    let mut end = size;
    let mut chunk = vec![0; CHUNK as usize];
    while end > 0 {
        let start = end.saturating_sub(CHUNK);
        let read = &mut chunk[..(end - start) as usize];
        file.read_exact_at(read, start)?;
        if let Some(at) = read.iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + at as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// Compresses an app's file for a month that has ended: written beside it
/// with what's already compressed for the month, if anything, replacing
/// that in one step, then the `.jsonl` removed. Only under the app's lock.
fn compress(folder: &Path, month: &str) -> io::Result<()> {
    let plain = folder.join(format!("{month}.jsonl"));
    let name = format!("{month}.jsonl.zst");
    let Some(mut lines) = files::read(&plain)? else {
        return Ok(());
    };
    lines.truncate(whole(&lines));
    if let Some(packed) = files::read(&folder.join(&name))? {
        // The `.jsonl` holds turns that ended in the month after it was
        // compressed, or, after a crash between the last compression's
        // rename and removal, what it compressed: only lines it doesn't
        // hold already, by their text or their turn, are added.
        let mut before = zstd::decode_all(packed.as_slice())?;
        before.truncate(whole(&before));
        let added = {
            let held: HashSet<&[u8]> = before.split(|byte| *byte == b'\n').collect();
            let turns: HashSet<(ChatId, TurnId)> =
                held.iter().filter_map(|text| turn(text)).collect();
            let mut added = Vec::new();
            for text in lines.split(|byte| *byte == b'\n') {
                if text.iter().all(u8::is_ascii_whitespace)
                    || held.contains(text)
                    || turn(text).is_some_and(|turn| turns.contains(&turn))
                {
                    continue;
                }
                added.extend_from_slice(text);
                added.push(b'\n');
            }
            added
        };
        before.extend_from_slice(&added);
        lines = before;
    }
    let packed = zstd::encode_all(lines.as_slice(), LEVEL)?;
    files::replace(folder, &name, &packed)?;
    std::fs::remove_file(&plain)?;
    files::sync_folder(folder)
}

/// The turn a line is of, if it reads as one.
fn turn(text: &[u8]) -> Option<(ChatId, TurnId)> {
    /// Only what tells a turn: the rest is skipped.
    #[derive(serde::Deserialize)]
    struct Turn {
        chat: ChatId,
        turn: TurnId,
    }
    let line: Turn = serde_json::from_slice(text).ok()?;
    Some((line.chat, line.turn))
}

/// Usage over a period, from every app's lines in `folder`, with cost
/// estimated with `prices` for turns that reported none. Only the months
/// the period touches are read.
///
/// # Errors
///
/// [`BadQuery`] if the query's time zone isn't known, or its period is
/// empty or longer than 400 days. Files that can't be read don't fail it:
/// they're named in the summary, and the folder itself, if it's there but
/// can't be listed, as `.`.
pub fn summarise(
    folder: &Path,
    query: &UsageQuery,
    prices: &PriceTable,
) -> Result<Summary, BadQuery> {
    let zone = TimeZone::get(&query.zone).map_err(|_| BadQuery::Zone(query.zone.clone()))?;
    let (since, until) = (query.since, query.until);
    if until <= since || until.0.saturating_sub(since.0) > LONGEST {
        return Err(BadQuery::Period);
    }
    let months = months(since, Wall(until.0 - 1)).ok_or(BadQuery::Period)?;

    let mut summary = Summary::new(query.clone());
    summary.fetched = prices.fetched;
    let mut read = Lines::default();
    let asked = |app: &String| query.apps.is_empty() || query.apps.contains(app);
    for app in apps(folder, &mut summary.unreadable)
        .into_iter()
        .filter(asked)
    {
        for month in &months {
            read.month(folder, &app, month, &mut summary.unreadable);
        }
    }

    let mut sum = Sum::new(prices);
    for line in read.lines {
        if line.at < since || line.at >= until {
            continue;
        }
        let agent = line.profile.clone().unwrap_or_else(|| line.agent.0.clone());
        if !query.apps.is_empty() && !query.apps.contains(&line.app) {
            continue;
        }
        if !query.agents.is_empty() && !query.agents.contains(&agent) {
            continue;
        }
        let Some(start) = start(line.at, &zone, query.by) else {
            continue;
        };
        sum.add(&line, start, agent);
    }
    sum.into(&mut summary);
    Ok(summary)
}

/// The months, as files are named, from `first`'s to `last`'s in UTC.
fn months(first: Wall, last: Wall) -> Option<Vec<String>> {
    let first = Timestamp::from_millisecond(first.0)
        .ok()?
        .to_zoned(TimeZone::UTC)
        .date();
    let last = Timestamp::from_millisecond(last.0)
        .ok()?
        .to_zoned(TimeZone::UTC)
        .date();
    let (mut year, mut month) = (first.year(), first.month());
    let mut months = Vec::new();
    while (year, month) <= (last.year(), last.month()) {
        if year >= 0 {
            months.push(format!("{year:04}-{month:02}"));
        }
        (year, month) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
    }
    Some(months)
}

/// The apps with folders in the ledger's, sorted. If the ledger's can't be
/// listed, it's named unreadable, as `.`.
fn apps(folder: &Path, unreadable: &mut Vec<String>) -> Vec<String> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => {
            unreadable.push(".".into());
            return Vec::new();
        }
    };
    let mut apps: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_app(name))
        .collect();
    apps.sort();
    apps
}

/// The lines read so far, each turn's first.
#[derive(Default)]
struct Lines {
    lines: Vec<LedgerLine>,
    seen: HashSet<(ChatId, TurnId)>,
}

impl Lines {
    /// Reads an app's files for a month: the compressed one, then any
    /// lines added since. Both are opened before either is read, the
    /// `.jsonl` first, so a compression meanwhile is never missed.
    fn month(&mut self, folder: &Path, app: &str, month: &str, unreadable: &mut Vec<String>) {
        let plain_name = format!("{app}/{month}.jsonl");
        let packed_name = format!("{plain_name}.zst");
        let plain = open(&folder.join(&plain_name));
        let packed = open(&folder.join(&packed_name));
        let packed = packed.and_then(|file| file.map(zstd::decode_all).transpose());
        let plain = plain.and_then(|file| {
            file.map(|mut file| {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).map(|_| bytes)
            })
            .transpose()
        });
        for (name, bytes) in [(packed_name, packed), (plain_name, plain)] {
            match bytes {
                Ok(Some(bytes)) => {
                    if !self.lines(&bytes) {
                        unreadable.push(name);
                    }
                }
                Ok(None) => {}
                Err(_) => unreadable.push(name),
            }
        }
    }

    /// Reads a file's lines, keeping each turn's first. Only lines with
    /// their line end count: what's after the last may be a line a crash
    /// cut off, or one still being written. Whether every line that counts
    /// read.
    fn lines(&mut self, bytes: &[u8]) -> bool {
        let mut readable = true;
        for text in bytes[..whole(bytes)].split(|byte| *byte == b'\n') {
            if text.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let line = serde_json::from_slice::<LedgerLine>(text)
                .ok()
                .filter(|line| within(&line.usage));
            match line {
                Some(line) => {
                    if self.seen.insert((line.chat, line.turn)) {
                        self.lines.push(line);
                    }
                }
                None => readable = false,
            }
        }
        readable
    }
}

/// A file, opened to read: `None` if it isn't there.
fn open(path: &Path) -> io::Result<Option<File>> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// When the hour or day `at` is in starts, in `zone`.
fn start(at: Wall, zone: &TimeZone, by: Resolution) -> Option<Wall> {
    let zoned = Timestamp::from_millisecond(at.0)
        .ok()?
        .to_zoned(zone.clone());
    if by == Resolution::Hour {
        // The hour's start under the offset in force at `at`: in an hour
        // the clocks go back, the first and second times through are two.
        let into = i64::from(zoned.minute()) * 60_000
            + i64::from(zoned.second()) * 1000
            + i64::from(zoned.millisecond());
        return Some(Wall(at.0 - into));
    }
    let start = zoned.start_of_day().ok()?;
    Some(Wall(start.timestamp().as_millisecond()))
}

/// Buckets, and the chats in them, as they're added up.
struct Sum<'a> {
    prices: &'a PriceTable,
    /// Each model's price, found once.
    found: HashMap<String, Option<&'a PricedModel>>,
    buckets: BTreeMap<(Wall, String, String, String, bool), Bucket>,
    chats: HashSet<ChatId>,
    by_app: BTreeMap<String, HashSet<ChatId>>,
    by_agent: BTreeMap<String, HashSet<ChatId>>,
}

impl<'a> Sum<'a> {
    fn new(prices: &'a PriceTable) -> Self {
        Self {
            prices,
            found: HashMap::new(),
            buckets: BTreeMap::new(),
            chats: HashSet::new(),
            by_app: BTreeMap::new(),
            by_agent: BTreeMap::new(),
        }
    }

    /// A model's price.
    fn price(&mut self, model: &str) -> Option<&'a PricedModel> {
        if let Some(found) = self.found.get(model) {
            return *found;
        }
        let found = self.prices.find(model);
        self.found.insert(model.to_owned(), found);
        found
    }

    /// Adds a turn's line to the buckets for `start` and `agent`.
    fn add(&mut self, line: &LedgerLine, start: Wall, agent: String) {
        let usage = &line.usage;
        if usage.models.is_empty() {
            return;
        }
        self.chats.insert(line.chat);
        self.by_app
            .entry(line.app.clone())
            .or_default()
            .insert(line.chat);
        self.by_agent
            .entry(agent.clone())
            .or_default()
            .insert(line.chat);

        let estimates: Vec<Option<f64>> = usage
            .models
            .iter()
            .map(|row| {
                self.price(&row.model)
                    .map(|priced| priced.price.cost(&row.tokens, usage.fast))
            })
            .collect();
        // Only dollars: CAF converts no currency, so another is estimated.
        let reported = usage
            .cost
            .as_ref()
            .filter(|cost| cost.currency.eq_ignore_ascii_case("USD"))
            .map(|cost| cost.amount);
        let shares = reported.map(|amount| shares(amount, usage, &estimates));

        let mut counted = BTreeSet::new();
        for (at, row) in usage.models.iter().enumerate() {
            let key = (
                start,
                line.app.clone(),
                agent.clone(),
                row.model.clone(),
                usage.fast,
            );
            let bucket = self
                .buckets
                .entry(key.clone())
                .or_insert_with(|| Bucket::new(start, &line.app, &agent, &row.model, usage.fast));
            bucket.tokens.add(&row.tokens);
            match (&shares, estimates[at]) {
                (Some(shares), _) => bucket.reported += shares[at],
                (None, Some(estimate)) => bucket.estimated += estimate,
                (None, None) => {
                    bucket.unpriced = bucket.unpriced.saturating_add(row.tokens.total());
                }
            }
            // A turn with two rows for one model is one turn in its bucket.
            if counted.insert(key) {
                bucket.turns += 1;
            }
            clamp(&mut bucket.tokens);
            bucket.unpriced = bucket.unpriced.min(MOST);
            bucket.turns = bucket.turns.min(MOST);
        }
    }

    /// Puts what's added up into `summary`.
    fn into(mut self, summary: &mut Summary) {
        let mut prices: BTreeMap<String, PricedModel> = BTreeMap::new();
        let models: BTreeSet<String> = self
            .buckets
            .values()
            .map(|bucket| bucket.model.clone())
            .collect();
        for model in models {
            if let Some(priced) = self.price(&model) {
                prices
                    .entry(priced.model.clone())
                    .or_insert_with(|| priced.clone());
            }
        }
        summary.prices = prices.into_values().collect();
        summary.buckets = self.buckets.into_values().collect();
        let count = |chats: HashSet<ChatId>| chats.len() as u64;
        summary.chats.all = count(self.chats);
        summary.chats.by_app = self
            .by_app
            .into_iter()
            .map(|(app, chats)| (app, count(chats)))
            .collect();
        summary.chats.by_agent = self
            .by_agent
            .into_iter()
            .map(|(agent, chats)| (agent, count(chats)))
            .collect();
    }
}

/// Stops every count at 2^53 - 1.
fn clamp(tokens: &mut Tokens) {
    for count in [
        &mut tokens.input,
        &mut tokens.cache_read,
        &mut tokens.cache_write,
        &mut tokens.output,
    ] {
        *count = (*count).min(MOST);
    }
    tokens.thinking = tokens.thinking.map(|thinking| thinking.min(MOST));
}

/// A reported cost shared between a turn's models: by their estimates, or
/// by their tokens if none has a price, or evenly if they have none.
fn shares(amount: f64, usage: &TurnUsage, estimates: &[Option<f64>]) -> Vec<f64> {
    let estimated: Vec<f64> = estimates
        .iter()
        .map(|estimate| estimate.unwrap_or(0.0))
        .collect();
    let tokens: Vec<f64> = usage
        .models
        .iter()
        .map(|row| row.tokens.total() as f64)
        .collect();
    let weights = [estimated, tokens]
        .into_iter()
        .find(|weights| weights.iter().sum::<f64>() > 0.0)
        .unwrap_or_else(|| vec![1.0; usage.models.len()]);
    let total: f64 = weights.iter().sum();
    weights
        .iter()
        .map(|weight| amount * weight / total)
        .collect()
}
