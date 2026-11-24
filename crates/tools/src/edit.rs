//! Edit and Write: a file's new bytes, worked out from what it was and
//! what the model asked, or why it can't be changed.

use std::fmt;
use std::path::Path;

use caf_core::tool::Diff;

use crate::backend::{FileText, Version};
use crate::read::missing;
use crate::state::{Check, NOT_READ, ReadState, STALE};
use crate::text::{BOM, Ending, decode, dominant, format_size};

/// The biggest file Edit changes, in bytes.
pub const MOST_EDIT: u64 = 16 * 1024 * 1024;

/// Why a file can't be changed as the model asked. Its words are Claude
/// Code's, where it has them.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// It wasn't read whole: [`NOT_READ`].
    NotRead,
    /// It changed since it was read: [`STALE`].
    Stale,
    /// The strings are the same.
    Identical,
    /// The string isn't in it.
    NotFound {
        /// The string, as the model wrote it.
        old_string: String,
    },
    /// The string is in it this many times, and the model didn't ask for
    /// them all.
    Several {
        /// How many times.
        count: usize,
    },
    /// An empty `old_string`, which makes a file, for one that has text.
    Exists,
    /// It's bigger than [`MOST_EDIT`], this many bytes.
    TooBig {
        /// Its size.
        size: u64,
    },
    /// It isn't UTF-8 text.
    NotText,
    /// There's no file, and `old_string` isn't empty: say it with
    /// [`EditError::message`], which names the folder.
    Missing,
}

impl EditError {
    /// What the model is answered, `cwd` being the folder its paths are
    /// taken from: as [`Display`](fmt::Display), but naming `cwd` for a
    /// missing file, as Claude Code does.
    #[must_use]
    pub fn message(&self, cwd: &Path) -> String {
        match self {
            Self::Missing => missing(cwd),
            other => other.to_string(),
        }
    }
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRead => f.write_str(NOT_READ),
            Self::Stale => f.write_str(STALE),
            Self::Identical => {
                f.write_str("No changes to make: old_string and new_string are exactly the same.")
            }
            Self::NotFound { old_string } => {
                write!(
                    f,
                    "String to replace not found in file.\nString: {old_string}"
                )
            }
            Self::Several { count } => write!(
                f,
                "Found {count} matches of the string to replace, but replace_all is false. To replace all occurrences, set replace_all to true. To replace only one occurrence, please provide more context to uniquely identify the instance."
            ),
            Self::Exists => f.write_str("Cannot create new file - file already exists."),
            Self::TooBig { size } => f.write_str(&too_big(*size)),
            Self::NotText => {
                f.write_str("This tool cannot edit binary files. The file is not UTF-8 text.")
            }
            Self::Missing => f.write_str("File does not exist."),
        }
    }
}

impl std::error::Error for EditError {}

/// Edit's words for a file bigger than [`MOST_EDIT`].
pub(crate) fn too_big(size: u64) -> String {
    format!(
        "File is too large to edit ({}). Maximum editable file size is 16 MiB.",
        format_size(size)
    )
}

/// A file's change, worked out: to write, and to show.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edited {
    /// The file's bytes after it.
    pub bytes: Vec<u8>,
    /// The version to write it expecting: the file's as it is now, or
    /// `None` if there's none and it's made.
    pub expected: Option<Version>,
    /// The file's text before it, without a byte order mark: `None` if
    /// there was no file.
    before: Option<String>,
}

impl Edited {
    /// Whether it makes the file.
    #[must_use]
    pub fn creates(&self) -> bool {
        self.before.is_none()
    }

    /// The change, for the record, of the file at `path`.
    #[must_use]
    pub fn diff(&self, path: impl Into<String>) -> Diff {
        let after = String::from_utf8_lossy(self.bytes.strip_prefix(BOM).unwrap_or(&self.bytes));
        Diff::new(path, self.before.as_deref(), &after)
    }
}

/// What the model is answered when a file it changed was there before.
#[must_use]
pub fn updated(path: &Path) -> String {
    format!("The file {} has been updated successfully.", path.display())
}

/// What the model is answered when Write made a file.
#[must_use]
pub fn created(path: &Path) -> String {
    format!("File created successfully at: {}", path.display())
}

/// Edit: the file at `path`, as it is now (`None` if there's none),
/// with `old_string` replaced by `new_string`, once, or everywhere if
/// `replace_all`, as Claude Code's Edit does it, checking `state` that
/// the model read it.
///
/// - **Checks,** in Claude Code's order: the strings are different; the
///   file is at most [`MOST_EDIT`]; an empty `old_string` makes a file,
///   so there mustn't be one with text (an empty file is fine); the file
///   was read whole and hasn't changed since; `old_string` is in it, and
///   just once unless `replace_all`.
/// - **Matching:** exactly first, then with curly quotes (‘’“”) taken as
///   straight ones. A match through curly quotes keeps them: straight
///   quotes in `new_string` are curled as the file's are.
/// - **Text:** UTF-8 only, with a byte order mark kept. A file whose lines
///   mostly end `\r\n` (in its first 4096 characters) keeps them: the
///   model's `\n`s are `\r\n`s in it.
/// - **Deleting:** an empty `new_string` takes the line ending right after
///   each match too, as Claude Code does, unless `old_string` ends with
///   one.
pub fn edit(
    path: &Path,
    file: Option<&FileText>,
    state: &ReadState,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<Edited, EditError> {
    if old_string == new_string {
        return Err(EditError::Identical);
    }
    let Some(file) = file else {
        if old_string.is_empty() {
            return Ok(Edited {
                bytes: new_string.as_bytes().to_vec(),
                expected: None,
                before: None,
            });
        }
        return Err(EditError::Missing);
    };
    let size = file.bytes.len() as u64;
    if size > MOST_EDIT {
        return Err(EditError::TooBig { size });
    }
    let decoded = decode(&file.bytes).ok_or(EditError::NotText)?;
    let text = decoded.text;
    if old_string.is_empty() && !text.trim().is_empty() {
        return Err(EditError::Exists);
    }
    allowed(state, path, file)?;
    let after = if old_string.is_empty() {
        new_string.to_owned()
    } else {
        replace(text, old_string, new_string, replace_all)?
    };
    let mut bytes = Vec::with_capacity(after.len() + BOM.len());
    if decoded.bom {
        bytes.extend_from_slice(BOM);
    }
    bytes.extend_from_slice(after.as_bytes());
    Ok(Edited {
        bytes,
        expected: file.version.clone(),
        before: Some(text.to_owned()),
    })
}

/// Write: the file at `path`, as it is now (`None` if there's none),
/// replaced by `content`, as given, line endings and all, checking
/// `state` that the model read it whole, if there was one.
pub fn write(
    path: &Path,
    file: Option<&FileText>,
    state: &ReadState,
    content: &str,
) -> Result<Edited, EditError> {
    let before = match file {
        Some(file) => {
            allowed(state, path, file)?;
            let bytes = file.bytes.strip_prefix(BOM).unwrap_or(&file.bytes);
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        None => None,
    };
    Ok(Edited {
        bytes: content.as_bytes().to_vec(),
        expected: file.and_then(|file| file.version.clone()),
        before,
    })
}

/// Whether `state` allows the file at `path`, as it is now, to be changed.
fn allowed(state: &ReadState, path: &Path, file: &FileText) -> Result<(), EditError> {
    match state.check(path, file) {
        Check::Fresh => Ok(()),
        Check::NotRead => Err(EditError::NotRead),
        Check::Stale => Err(EditError::Stale),
    }
}

/// `text` with `old` replaced by `new`, as [`edit`] says.
fn replace(text: &str, old: &str, new: &str, replace_all: bool) -> Result<String, EditError> {
    let crlf = dominant(text) == Ending::CrLf;
    let (old_crlf, new_crlf) = if crlf {
        (to_crlf(old), to_crlf(new))
    } else {
        (old.to_owned(), new.to_owned())
    };
    // As the file's endings first, then as the model wrote them.
    let mut tries = vec![(old_crlf.as_str(), new_crlf.as_str())];
    if crlf && old_crlf != old {
        tries.push((old, new));
    }
    let mut found = None;
    for &(old, new) in &tries {
        let ranges: Vec<_> = text
            .match_indices(old)
            .map(|(at, _)| at..at + old.len())
            .collect();
        if !ranges.is_empty() {
            found = Some((ranges, new.to_owned()));
            break;
        }
    }
    if found.is_none() {
        let straight = straighten(text);
        for &(old, new) in &tries {
            let wanted = straighten(old);
            let ranges = matches_straightened(text, &straight, &wanted);
            if let Some(first) = ranges.first() {
                let found_text = &text[first.clone()];
                found = Some((ranges, curl(new, found_text)));
                break;
            }
        }
    }
    let Some((ranges, new)) = found else {
        return Err(EditError::NotFound {
            old_string: old.to_owned(),
        });
    };
    if ranges.len() > 1 && !replace_all {
        return Err(EditError::Several {
            count: ranges.len(),
        });
    }
    let mut after = String::with_capacity(text.len() + new.len());
    let mut at = 0;
    for (i, range) in ranges.iter().enumerate() {
        let found = &text[range.clone()];
        let mut end = range.end;
        if new.is_empty() && !found.ends_with('\n') {
            let rest = &text[end..];
            let ending = if crlf && rest.starts_with("\r\n") {
                2
            } else {
                usize::from(rest.starts_with('\n'))
            };
            // Not into the next match.
            if ranges
                .get(i + 1)
                .is_none_or(|next| next.start >= end + ending)
            {
                end += ending;
            }
        }
        after.push_str(&text[at..range.start]);
        after.push_str(&new);
        at = end;
    }
    after.push_str(&text[at..]);
    Ok(after)
}

/// `text` with every line ending `\r\n`.
fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Whether `c` is a curly quote, which [`straighten`] makes straight.
fn is_curly(c: char) -> bool {
    matches!(c, '‘' | '’' | '“' | '”')
}

/// `text` with its curly quotes straight.
fn straighten(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '‘' | '’' => '\'',
            '“' | '”' => '"',
            c => c,
        })
        .collect()
}

/// Where `wanted` is in `text`, by `straight`, `text` straightened: each
/// match as `text`'s bytes.
fn matches_straightened(text: &str, straight: &str, wanted: &str) -> Vec<std::ops::Range<usize>> {
    if wanted.is_empty() {
        return Vec::new();
    }
    // Every quote straightened is 2 bytes shorter: count them to map back.
    let mut chars = text.char_indices().peekable();
    let mut shrunk = 0;
    let mut to_text = |at: usize| {
        while let Some(&(i, c)) = chars.peek() {
            if i - shrunk >= at {
                break;
            }
            if is_curly(c) {
                shrunk += c.len_utf8() - 1;
            }
            chars.next();
        }
        at + shrunk
    };
    let mut ranges = Vec::new();
    for (at, _) in straight.match_indices(wanted) {
        let start = to_text(at);
        let end = to_text(at + wanted.len());
        ranges.push(start..end);
    }
    ranges
}

/// `new` with its straight quotes curled as those in `found`, the file's
/// text it replaces, are: double quotes if it has curly ones, single
/// quotes likewise, opening after a space or bracket, and an apostrophe
/// between letters closing.
fn curl(new: &str, found: &str) -> String {
    let doubles = found.contains(['“', '”']);
    let singles = found.contains(['‘', '’']);
    if !doubles && !singles {
        return new.to_owned();
    }
    let chars: Vec<char> = new.chars().collect();
    let opens = |i: usize| {
        i == 0 || {
            let before = chars[i - 1];
            before.is_whitespace() || matches!(before, '(' | '[' | '{' | '—' | '–')
        }
    };
    let mut out = String::with_capacity(new.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '"' if doubles => out.push(if opens(i) { '“' } else { '”' }),
            '\'' if singles => {
                let between_letters = i > 0
                    && chars[i - 1].is_alphabetic()
                    && chars.get(i + 1).is_some_and(|c| c.is_alphabetic());
                out.push(if !between_letters && opens(i) {
                    '‘'
                } else {
                    '’'
                });
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
