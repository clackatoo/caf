//! Where the debugger keeps its files: recordings of every live run, and
//! the fake agent's script.

use std::os::unix::fs::DirBuilderExt;
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
/// private.
///
/// # Errors
///
/// If it can't be made.
pub fn make(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// A new recording's path in `dir`, named for `now` in UTC, such as
/// `2026-11-20T14-03-22Z.jsonl`, with `-2` and so on added if that's
/// taken.
#[must_use]
pub fn new_recording(dir: &Path, now: SystemTime) -> PathBuf {
    let stamp = stamp(now);
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

/// What's in a recording, up to its last whole line: CAF writes it as
/// it runs, so its end may be half a line.
#[must_use]
pub fn whole_lines(mut bytes: Vec<u8>) -> Vec<u8> {
    let end = bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |at| at + 1);
    bytes.truncate(end);
    bytes
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{new_recording, stamp, whole_lines};

    #[test]
    fn recordings_are_named_for_when_they_start() {
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(stamp(at(0)), "1970-01-01T00-00-00Z");
        assert_eq!(stamp(at(951_782_400)), "2000-02-29T00-00-00Z");
        assert_eq!(stamp(at(1_795_100_602)), "2026-11-19T15-03-22Z");
        let dir = std::env::temp_dir().join(format!("caf-debugger-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = new_recording(&dir, at(0));
        assert_eq!(first, dir.join("1970-01-01T00-00-00Z.jsonl"));
        std::fs::write(&first, "").unwrap();
        assert_eq!(
            new_recording(&dir, at(0)),
            dir.join("1970-01-01T00-00-00Z-2.jsonl")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_download_ends_at_a_whole_line() {
        assert_eq!(whole_lines(b"{}\n{\"a\"".to_vec()), b"{}\n");
        assert_eq!(whole_lines(b"{}\n".to_vec()), b"{}\n");
        assert_eq!(whole_lines(b"{".to_vec()), b"");
    }
}
