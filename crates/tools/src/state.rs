//! Read state: which files a chat's model has read, and as they were, so
//! that it never changes one it hasn't seen, or has seen only as it was
//! before someone else changed it.
//!
//! As Claude Code's: a read of any part of a file counts, so a file too big
//! to read whole can still be changed. Whether it was read whole matters
//! only when its version moved since: then its bytes being the same as
//! those read lets it be changed only if they were all read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::backend::{FileRead, Version};

/// What the model is answered when it changes a file it hasn't read, as
/// Claude Code says it.
pub const NOT_READ: &str = "File has not been read yet. Read it first before writing to it.";
/// What the model is answered when it changes a file that changed since
/// it read it, as Claude Code says it.
pub const STALE: &str = "File has been modified since read, either by the user or by a linter. Read it again before attempting to write it.";

/// The files one chat's model has read, in one run: lost on restart,
/// after which the model just reads them again.
#[derive(Clone, Debug, Default)]
pub struct ReadState {
    seen: HashMap<PathBuf, Seen>,
}

/// A file as it was read.
#[derive(Clone, Debug)]
struct Seen {
    version: Version,
    hash: [u8; 32],
    partial: bool,
}

/// Whether a file may be changed, as it is now.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// It was read, and hasn't changed since.
    Fresh,
    /// It wasn't read: [`NOT_READ`].
    NotRead,
    /// It changed since it was read: [`STALE`].
    Stale,
}

impl Check {
    /// What the model is answered, if it may not change the file.
    #[must_use]
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Fresh => None,
            Self::NotRead => Some(NOT_READ),
            Self::Stale => Some(STALE),
        }
    }
}

impl ReadState {
    /// No files read.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The model read the file at `path`, of `bytes` at `version`, whole
    /// or `partial`ly. Also what a change the model made does: it's seen
    /// what it wrote, whole.
    ///
    /// A partial read of a file read whole, unchanged since, leaves it
    /// read whole.
    pub fn record(&mut self, path: PathBuf, version: Version, bytes: &[u8], partial: bool) {
        let hash = hash(bytes);
        if partial
            && let Some(seen) = self.seen.get_mut(&path)
            && !seen.partial
            && seen.hash == hash
        {
            seen.version = version;
            return;
        }
        self.seen.insert(
            path,
            Seen {
                version,
                hash,
                partial,
            },
        );
    }

    /// Whether the file at `path` may be changed, as it is now, `current`:
    /// only if it was read, whole or in part, and either its version is the
    /// same as then, or it was read whole and its bytes are.
    ///
    /// If only its bytes are, it's [`Check::Fresh`], but its version has
    /// moved on: the change must be written expecting `current`'s, not the
    /// one read.
    #[must_use]
    pub fn check(&self, path: &Path, current: &FileRead) -> Check {
        let Some(seen) = self.seen.get(path) else {
            return Check::NotRead;
        };
        if current.version == seen.version || (!seen.partial && hash(&current.bytes) == seen.hash) {
            Check::Fresh
        } else {
            Check::Stale
        }
    }

    /// Forgets the file at `path`, as if it were never read.
    pub fn forget(&mut self, path: &Path) {
        self.seen.remove(path);
    }
}

/// `bytes`' sha256.
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(bytes: &[u8], version: &str) -> FileRead {
        FileRead::new(bytes.to_vec(), Version::new(version))
    }

    #[test]
    fn a_file_never_read_may_not_be_changed() {
        let state = ReadState::new();
        let check = state.check(Path::new("/a"), &file(b"x", "1"));
        assert_eq!(check, Check::NotRead);
        assert_eq!(check.refusal(), Some(NOT_READ));
    }

    #[test]
    fn a_file_read_whole_and_unchanged_may_be() {
        let mut state = ReadState::new();
        state.record("/a".into(), Version::new("1"), b"x", false);
        let check = state.check(Path::new("/a"), &file(b"x", "1"));
        assert_eq!(check, Check::Fresh);
        assert_eq!(check.refusal(), None);
        // Another file wasn't read.
        assert_eq!(
            state.check(Path::new("/b"), &file(b"x", "1")),
            Check::NotRead
        );
    }

    #[test]
    fn a_file_read_in_part_may_be_changed_as_claude_code_allows() {
        // A file too big to read whole can still be edited.
        let mut state = ReadState::new();
        state.record("/a".into(), Version::new("1"), b"x", true);
        assert_eq!(state.check(Path::new("/a"), &file(b"x", "1")), Check::Fresh);
    }

    #[test]
    fn a_file_read_in_part_whose_version_moved_is_stale_even_if_its_bytes_didnt() {
        // Only a whole read saw all the bytes the hash vouches for.
        let mut state = ReadState::new();
        state.record("/a".into(), Version::new("1"), b"x", true);
        assert_eq!(state.check(Path::new("/a"), &file(b"x", "2")), Check::Stale);
    }

    #[test]
    fn a_file_changed_since_it_was_read_is_stale() {
        let mut state = ReadState::new();
        state.record("/a".into(), Version::new("1"), b"x", false);
        let check = state.check(Path::new("/a"), &file(b"y", "2"));
        assert_eq!(check, Check::Stale);
        assert_eq!(check.refusal(), Some(STALE));
    }

    #[test]
    fn a_new_version_of_the_same_bytes_is_fresh() {
        // Touched, or saved unchanged: the version moved, the content didn't.
        let mut state = ReadState::new();
        state.record("/a".into(), Version::new("1"), b"x", false);
        assert_eq!(state.check(Path::new("/a"), &file(b"x", "2")), Check::Fresh);
    }

    #[test]
    fn the_same_version_is_fresh_whatever_the_bytes() {
        // The backend says it's the same: its version is the truth.
        let mut state = ReadState::new();
        state.record("/a".into(), Version::new("1"), b"x", false);
        assert_eq!(state.check(Path::new("/a"), &file(b"y", "1")), Check::Fresh);
    }

    #[test]
    fn the_bom_counts_as_a_change() {
        let mut state = ReadState::new();
        state.record("/a".into(), Version::new("1"), b"x", false);
        assert_eq!(
            state.check(Path::new("/a"), &file(b"\xEF\xBB\xBFx", "2")),
            Check::Stale
        );
    }

    #[test]
    fn a_partial_read_of_a_file_read_whole_and_unchanged_keeps_it_whole() {
        let mut state = ReadState::new();
        state.record("/a".into(), Version::new("1"), b"x", false);
        state.record("/a".into(), Version::new("2"), b"x", true);
        // Still whole: a later touch with the same bytes is fresh.
        assert_eq!(state.check(Path::new("/a"), &file(b"x", "3")), Check::Fresh);
        // A partial read of a file that changed is partial.
        state.record("/a".into(), Version::new("4"), b"y", true);
        assert_eq!(state.check(Path::new("/a"), &file(b"y", "5")), Check::Stale);
        // And a whole read makes it whole again.
        state.record("/a".into(), Version::new("4"), b"y", false);
        assert_eq!(state.check(Path::new("/a"), &file(b"y", "5")), Check::Fresh);
    }

    #[test]
    fn a_file_forgotten_is_unread() {
        let mut state = ReadState::new();
        state.record("/a".into(), Version::new("1"), b"x", false);
        state.forget(Path::new("/a"));
        assert_eq!(
            state.check(Path::new("/a"), &file(b"x", "1")),
            Check::NotRead
        );
    }
}
