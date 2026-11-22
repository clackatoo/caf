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
//!    ([`AccountLimits::merge`]), under the folder's lock, and the file
//!    replaced in one step, so a reader sees one whole file or the other,
//!    and two apps saving at once both count.
//! 2. **What's kept never holds a window past its reset**, nor one heard
//!    before it was cleared: both are dropped as it's read and saved.
//! 3. **Nothing waits for ever**, as for the ledger: files are opened
//!    without waiting, only plain files used, and the lock given up on
//!    after five seconds.

use std::io;
use std::path::Path;

use caf_core::limits::{AccountLimits, ProfileLimits};
use caf_core::profiles::Builtin;
use caf_core::time::Wall;

use crate::{LIMITS, files};

#[cfg(test)]
mod tests;

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
        match read(&folder, profile) {
            Ok(Some(limits)) => kept.push(ProfileLimits::new(profile, tidy(&limits, now))),
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
/// A file kept that doesn't read is written over: it holds only what an
/// agent says again with its next turn.
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
    let was = match read(&folder, limits.profile) {
        Ok(was) => was,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => None,
        Err(error) => return Err(error),
    };
    let given = tidy(&limits.limits, now);
    let mut merged = given.clone();
    if let Some(was) = &was {
        merged.merge(was);
        merged.forget_passed(now);
    }
    if was.as_ref() != Some(&merged) {
        let mut bytes = serde_json::to_vec_pretty(&merged).map_err(io::Error::other)?;
        bytes.push(b'\n');
        files::replace(&folder, &file_name(limits.profile), &bytes)?;
    }
    Ok((merged != given).then_some(merged))
}

/// The file a profile's limits are kept in.
fn file_name(profile: Builtin) -> String {
    format!("{}.json", profile.slug())
}

/// A profile's limits as kept in `folder`, the limits': `None` if there
/// are none, [`InvalidData`](io::ErrorKind::InvalidData) if they don't
/// read.
fn read(folder: &Path, profile: Builtin) -> io::Result<Option<AccountLimits>> {
    let path = folder.join(file_name(profile));
    let Some(bytes) = files::read(&path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} doesn't read: {error}", files::name(&path)),
        )
    })
}

/// `limits` as they're kept: each window once, in order, with none past
/// its reset at `now`, nor heard before they were cleared.
fn tidy(limits: &AccountLimits, now: Wall) -> AccountLimits {
    let mut tidy = AccountLimits::default();
    tidy.merge(limits);
    tidy.forget_passed(now);
    tidy
}
