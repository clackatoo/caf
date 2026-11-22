//! Account limits, kept across restarts for every CAF app on the machine
//! to share: a file per profile, beside the ledger.
//!
//! ```text
//! limits/claude-agent-acp.json    ProfileLimits' limits, as JSON in CAF's one shape
//! ```
//!
//! Named after the profile's [slug](caf_core::profiles::Builtin::slug), as
//! every agent with a profile shares one account. A file whose name is no
//! profile's is left alone.
//!
//! # Guarantees
//!
//! Each of these has a test.
//!
//! 1. **A save never undoes later news.** It's merged with what's kept
//!    ([`AccountLimits::merge`]), under the folder's lock
//!    (`limits/.lock`), and the file replaced in one step, so a reader
//!    sees one whole file or the other, and two apps saving at once both
//!    count. A file kept that doesn't read is written over: it holds only
//!    what an agent says again with its next turn.
//! 2. **What's kept never holds a window past its reset**, nor one heard
//!    before it was cleared: both are dropped as it's read and saved.
//! 3. **Nothing waits for ever**, as for the ledger: files are opened
//!    without waiting, only plain files used, and the lock given up on
//!    after five seconds.
//! 4. **A later CAF's news is never lost to an earlier one.** A window,
//!    or extra usage, this CAF can't read, of a kind or state it doesn't
//!    know, is left out of what's loaded and written back as it was after
//!    those it read, until it goes as one it read would: heard before a
//!    clearing, past its reset, or one of its kind heard later. Extra usage
//!    this CAF can't read is kept over that it can if heard later; if its
//!    `heard` isn't a number, so it can't be timed, it gives way to one
//!    this CAF can read.
//! 5. **No clock set ahead wins for long:** a window or extra usage heard
//!    later than a minute from now, whether this CAF can read it or not,
//!    is taken as heard a minute from now, and a clearing later than ten
//!    seconds from now as ten seconds from now, and kept so: so a clearing
//!    from a clock set ahead never drops news heard since.
//!
//! Guarantees 2, 4 and 5 hold for a window, or extra usage, this CAF
//! can't read only as far as its `heard` and `resets` read as numbers, as
//! that's all of it this CAF reads: one with neither is kept as it was
//! until a CAF that can read it writes the file.
//!
//! So a field a later CAF adds, to the file or to a window this CAF can
//! read, must be safe for an earlier one to lose as it writes the file
//! again: only what it can't read of a window is kept as it was. A file
//! bigger than a mebibyte doesn't read.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use caf_core::limits::{AccountLimits, ExtraUsage, LimitWindow, ProfileLimits};
use caf_core::profiles::Builtin;
use caf_core::time::Wall;
use serde::Deserialize;
use serde_json::Value;

use crate::{LIMITS, files};

#[cfg(test)]
mod tests;

/// How far past now a window or extra usage read or saved may be heard,
/// in milliseconds: past it, it's taken as now and this.
const SLACK: i64 = 60_000;

/// How far past now a clearing read or saved may be, in milliseconds:
/// past it, it's taken as now and this. Less than [`SLACK`], as a later
/// clearing drops news heard before it, and one taken earlier only keeps
/// more. But not none: a save's `now` is taken before it waits for the
/// lock, for up to [`LOCK_WAIT`](files::LOCK_WAIT), and another app may
/// save a clearing meanwhile, which must stay as it was, with a few
/// seconds more for the two apps' clocks.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "five seconds"
)]
const CLEARED_SLACK: i64 = files::LOCK_WAIT.as_millis() as i64 + 5_000;

/// The most a profile's file may be, in bytes: past that, it doesn't
/// read. A few windows are a few hundred.
const MOST: u64 = 1 << 20;

/// Every profile's limits kept in `folder`, the ledger's, as of `now`: a
/// file that doesn't read is left out, with its error.
#[must_use]
pub fn load(folder: &Path, now: Wall) -> (Vec<ProfileLimits>, Vec<io::Error>) {
    let folder = folder.join(LIMITS);
    let mut kept = Vec::new();
    let mut errors = Vec::new();
    let entries = match std::fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (kept, errors),
        Err(error) => {
            errors.push(io::Error::new(
                error.kind(),
                format!("{LIMITS} can't be listed: {error}"),
            ));
            return (kept, errors);
        }
    };
    let mut profiles: Vec<Builtin> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| Builtin::from_slug(name.strip_suffix(".json")?))
        .collect();
    // In the order of their names, so loading is the same every time.
    profiles.sort_by_key(|profile| profile.slug());
    for profile in profiles {
        match read(&folder, profile, now) {
            Ok(Some(file)) => kept.push(ProfileLimits::new(profile, file.limits)),
            // Gone meanwhile.
            Ok(None) => {}
            Err(error) => errors.push(error),
        }
    }
    (kept, errors)
}

/// Merge `limits` into what `folder` keeps of its profile's, as of `now`,
/// and keep the result. The merged limits if what was kept had news
/// `limits` didn't: the core is to be told them.
///
/// A file kept that doesn't read is written over, as it holds only what
/// an agent says again with its next turn, and that's reported to the
/// [`log`] facade, target `caf`.
///
/// # Errors
///
/// What the system said, if it couldn't: what's kept is as it was.
/// [`TimedOut`](io::ErrorKind::TimedOut) if another program held the
/// limits' lock for five seconds.
pub fn save(folder: &Path, limits: &ProfileLimits, now: Wall) -> io::Result<Option<AccountLimits>> {
    let folder = folder.join(LIMITS);
    files::make_folder(&folder)?;
    let _lock = files::lock(&folder)?;
    let was = match read(&folder, limits.profile, now) {
        Ok(was) => was,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            log::log!(
                target: "caf",
                once_then_debug(&WRITTEN_OVER),
                "{error}: it's written over with the limits saved"
            );
            None
        }
        Err(error) => return Err(error),
    };
    let given = tidy(limits.limits.clone(), now);
    let mut merged = given.clone();
    let mut file = serde_json::to_value(&merged).map_err(io::Error::other)?;
    if let Some(was) = &was {
        merged.merge(&was.limits);
        merged.forget_passed(now);
        file = serde_json::to_value(&merged).map_err(io::Error::other)?;
        let cleared = merged.cleared.map_or(i64::MIN, |cleared| cleared.0);
        let unread = (was.unread.iter())
            .filter(|window| {
                let kind = &window["kind"];
                let heard = time(&window["heard"]);
                let later = (merged.windows.iter()).any(|read| {
                    heard.is_some_and(|heard| read.heard.0 > heard)
                        && serde_json::to_value(&read.kind).is_ok_and(|read| read == *kind)
                });
                !later
                    && heard.is_none_or(|heard| heard >= cleared)
                    && time(&window["resets"]).is_none_or(|resets| resets > now.0)
            })
            .cloned();
        if let Value::Array(windows) = &mut file["windows"] {
            windows.extend(unread);
        }
        if let Some(extra) = &was.unread_extra {
            let heard = time(&extra["heard"]);
            // Heard later than any read, as far as can be told.
            let later = (merged.extra_usage)
                .is_none_or(|read| heard.is_some_and(|heard| heard > read.heard.0));
            if later && heard.is_none_or(|heard| heard >= cleared) {
                file["extra_usage"] = extra.clone();
            }
        }
    }
    if was.as_ref().map(|was| &was.json) != Some(&file) {
        let mut bytes = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
        bytes.push(b'\n');
        files::replace(&folder, &file_name(limits.profile), &bytes)?;
    }
    Ok((merged != given).then_some(merged))
}

/// Whether a file of limits was written over yet, as it didn't read: the
/// first is a warning, the rest only debugging, as each save would say it
/// again.
static WRITTEN_OVER: AtomicBool = AtomicBool::new(false);

/// [`Warn`](log::Level::Warn) the first time, as `said` says, and
/// [`Debug`](log::Level::Debug) after.
fn once_then_debug(said: &AtomicBool) -> log::Level {
    if said.swap(true, Ordering::Relaxed) {
        log::Level::Debug
    } else {
        log::Level::Warn
    }
}

/// The file a profile's limits are kept in.
fn file_name(profile: Builtin) -> String {
    format!("{}.json", profile.slug())
}

/// A profile's file, as it's read before anything in it is: each window,
/// and extra usage, read alone, so one this CAF can't read leaves the
/// rest.
#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    windows: Vec<Value>,
    #[serde(default)]
    extra_usage: Option<Value>,
    #[serde(default)]
    cleared: Option<Wall>,
}

/// A profile's file, read.
struct Kept {
    /// What reads, [tidied](tidy).
    limits: AccountLimits,
    /// The windows that don't read, as they were.
    unread: Vec<Value>,
    /// Extra usage, if it doesn't read, as it was.
    unread_extra: Option<Value>,
    /// The whole file, as it was.
    json: Value,
}

/// A profile's limits as kept in `folder`, the limits', as of `now`:
/// `None` if there are none, [`InvalidData`](io::ErrorKind::InvalidData)
/// if the file doesn't read, or is bigger than [`MOST`].
fn read(folder: &Path, profile: Builtin, now: Wall) -> io::Result<Option<Kept>> {
    let path = folder.join(file_name(profile));
    let Some(bytes) = files::read(&path, MOST)? else {
        return Ok(None);
    };
    let unreadable = |error: serde_json::Error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} doesn't read: {error}", files::name(&path)),
        )
    };
    let json: Value = serde_json::from_slice(&bytes).map_err(unreadable)?;
    let raw = Raw::deserialize(&json).map_err(unreadable)?;
    let mut limits = AccountLimits::default();
    let mut unread = Vec::new();
    for window in raw.windows {
        match LimitWindow::deserialize(&window) {
            Ok(window) => limits.windows.push(window),
            Err(_) => unread.push(window),
        }
    }
    let mut unread_extra = None;
    if let Some(extra) = raw.extra_usage.filter(|extra| !extra.is_null()) {
        match ExtraUsage::deserialize(&extra) {
            Ok(extra) => limits.extra_usage = Some(extra),
            Err(_) => unread_extra = Some(extra),
        }
    }
    limits.cleared = raw.cleared;
    // Held to now, as what reads is.
    let latest = now.0.saturating_add(SLACK);
    for value in unread.iter_mut().chain(&mut unread_extra) {
        if time(&value["heard"]).is_some_and(|heard| heard > latest) {
            value["heard"] = latest.into();
        }
    }
    Ok(Some(Kept {
        limits: tidy(limits, now),
        unread,
        unread_extra,
        json,
    }))
}

/// A time in a window, or extra usage, this CAF can't read, if it's one.
fn time(value: &Value) -> Option<i64> {
    value.as_i64()
}

/// `limits` as they're kept: heard no later than [`SLACK`] from `now`,
/// cleared no later than [`CLEARED_SLACK`] from it,
/// each window once, in order, with none past its reset at `now`, nor
/// heard before they were cleared.
fn tidy(mut limits: AccountLimits, now: Wall) -> AccountLimits {
    let latest = Wall(now.0.saturating_add(SLACK));
    for window in &mut limits.windows {
        window.heard = window.heard.min(latest);
    }
    if let Some(extra) = &mut limits.extra_usage {
        extra.heard = extra.heard.min(latest);
    }
    let cleared = Wall(now.0.saturating_add(CLEARED_SLACK));
    limits.cleared = limits.cleared.map(|held| held.min(cleared));
    let mut tidy = AccountLimits::default();
    tidy.merge(&limits);
    tidy.forget_passed(now);
    tidy
}
