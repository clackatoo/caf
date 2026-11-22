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
use caf_core::time::Wall;

/// Every profile's limits kept in `folder`, the ledger's, as of `now`: a
/// file that doesn't read is left out, with its error.
#[must_use]
pub fn load(folder: &Path, now: Wall) -> (Vec<ProfileLimits>, Vec<io::Error>) {
    let _ = (folder, now);
    todo!()
}

/// Merge `limits` into what `folder` keeps of its profile's, as of `now`,
/// and keep the result. The merged limits if what was kept had news
/// `limits` didn't: the core is to be told them.
pub fn save(folder: &Path, limits: &ProfileLimits, now: Wall) -> io::Result<Option<AccountLimits>> {
    let _ = (folder, limits, now);
    todo!()
}
