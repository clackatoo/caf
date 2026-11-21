//! Where the debugger keeps its files: recordings of every live run, the
//! fake agent's script, and the folder its chats are kept in.

use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The debugger's folder: `$XDG_STATE_HOME/caf/debugger`, or
/// `~/.local/state/caf/debugger`. `None` if there's no home to put it in.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            home.is_absolute().then(|| home.join(".local/state"))
        })?;
    Some(state.join("caf/debugger"))
}

/// Makes the folder, readable only by its owner, as recordings are
/// private: one made before is made so too.
///
/// # Errors
///
/// If it can't be made, or made private.
pub fn make(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Checks chats can be kept in `dir`: it's made, readable only by its
/// owner, with any folders it's in, if it isn't there, and something is
/// written in it and removed. One that's there keeps its permissions.
/// Checked as the debugger starts, as CAF tries a chat it can't write
/// again until it can, and so would never finish shutting down.
///
/// # Errors
///
/// If it can't be made, or written in.
pub fn check_store(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let probe = dir.join(format!(".caf-debugger-check-{}", std::process::id()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&probe)?;
    std::fs::remove_file(&probe)
}

/// A new recording's path in `dir`, named for `now` in UTC and this
/// process, such as `2026-11-20T14-03-22Z-4182.jsonl`, with `-2` and so
/// on added if that's taken. The process's id keeps two debuggers that
/// start in the same second from choosing the same name.
#[must_use]
pub fn new_recording(dir: &Path, now: SystemTime) -> PathBuf {
    let stamp = format!("{}-{}", stamp(now), std::process::id());
    (1..)
        .map(|n| {
            let name = if n == 1 {
                format!("{stamp}.jsonl")
            } else {
                format!("{stamp}-{n}.jsonl")
            };
            dir.join(name)
        })
        .find(|path| std::fs::symlink_metadata(path).is_err())
        .expect("a free name")
}

/// A time as `2026-11-20T14-03-22Z`: ISO 8601, with `-` for `:`, which
/// some systems don't allow in names.
fn stamp(now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    let (year, month, day) = civil(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// The date `days` after 1970-01-01, in the proleptic Gregorian
/// calendar: Howard Hinnant's `civil_from_days`.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime};

    use super::{check_store, make, new_recording, stamp};

    #[test]
    fn recordings_are_named_for_when_they_start_and_the_process() {
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(stamp(at(0)), "1970-01-01T00-00-00Z");
        assert_eq!(stamp(at(951_782_400)), "2000-02-29T00-00-00Z");
        assert_eq!(stamp(at(1_795_100_602)), "2026-11-19T15-03-22Z");
        let dir = std::env::temp_dir().join(format!("caf-debugger-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid = std::process::id();
        let first = new_recording(&dir, at(0));
        assert_eq!(first, dir.join(format!("1970-01-01T00-00-00Z-{pid}.jsonl")));
        std::fs::write(&first, "").unwrap();
        assert_eq!(
            new_recording(&dir, at(0)),
            dir.join(format!("1970-01-01T00-00-00Z-{pid}-2.jsonl"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_folder_is_made_private_even_if_it_was_there() {
        let dir = std::env::temp_dir().join(format!("caf-debugger-private-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        make(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let inner = dir.join("a/b");
        make(&inner).unwrap();
        assert_eq!(
            std::fs::metadata(&inner).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_store_is_made_private_if_it_isnt_there_and_must_take_writes() {
        let dir = std::env::temp_dir().join(format!("caf-debugger-store-{}", std::process::id()));
        let store = dir.join("data/chats");
        check_store(&store).unwrap();
        let mode = std::fs::metadata(&store).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(
            std::fs::read_dir(&store).unwrap().count(),
            0,
            "nothing left"
        );
        // One that's there keeps its permissions, and must take writes.
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o500)).unwrap();
        let writable = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(store.join("root-writes-anywhere"))
            .is_ok();
        if !writable {
            assert!(check_store(&store).is_err(), "read-only");
        }
        let mode = std::fs::metadata(&store).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o500);
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
