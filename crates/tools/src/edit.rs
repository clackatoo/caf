//! Edit and Write: a file's new bytes, worked out from what it was and
//! what the model asked, or why it can't be changed.

use std::borrow::Cow;
use std::fmt;
use std::iter::Peekable;
use std::ops::Range;
use std::path::Path;
use std::str::CharIndices;

use caf_core::tool::Diff;

use crate::backend::{FileRead, FileText, Version};
use crate::read::missing;
use crate::state::{Check, NOT_READ, ReadState, STALE};
use crate::text::{BOM, Ending, decode, dominant, format_size};

/// The biggest file Edit changes, in bytes.
pub const MOST_EDIT: u64 = 16 * 1024 * 1024;

/// What Claude Code adds to the answer to a change: the model needn't read
/// the file back.
const CURRENT: &str = " (file state is current in your context \u{2014} no need to Read it back)";

/// What Claude Code adds to the answer to an edit made to a file that
/// changed since it was read, in place of [`CURRENT`].
const RECOVERED: &str = " (note: the file had been modified on disk since you last read it \u{2014} the edit applied cleanly, but the file contains other changes not in your context. Read it before edits that depend on surrounding content.)";

/// Why a file can't be changed as the model asked. Its words are Claude
/// Code's, where it has them.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// It wasn't read: [`NOT_READ`].
    NotRead,
    /// It changed since it was read, and the edit doesn't apply to it as it
    /// is now: [`STALE`].
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
        /// The string, as the model wrote it.
        old_string: String,
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
            Self::Several { count, old_string } => write!(
                f,
                "Found {count} matches of the string to replace, but replace_all is false. To replace all occurrences, set replace_all to true. To replace only one occurrence, please provide more context to uniquely identify the instance.\nString: {old_string}"
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

/// Edit's words for a file bigger than [`MOST_EDIT`], as Claude Code's.
pub(crate) fn too_big(size: u64) -> String {
    format!(
        "File is too large to edit ({}). Maximum editable file size is {}.",
        format_size(size),
        format_size(MOST_EDIT)
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
    /// How the model is answered.
    says: Says,
}

/// How the model is answered of a change, as Claude Code's tools word it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Says {
    /// Edit's, of one place, or all.
    Edited { all: bool, recovered: bool },
    /// Write's.
    Wrote,
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

    /// What the model is answered once it's written to the file at
    /// `path`, as it gave it.
    #[must_use]
    pub fn answer(&self, path: &Path) -> String {
        let path = path.display();
        match self.says {
            Says::Edited { all, recovered } => {
                let note = if recovered { RECOVERED } else { CURRENT };
                if all {
                    format!(
                        "The file {path} has been updated. All occurrences were successfully replaced.{note}"
                    )
                } else {
                    format!("The file {path} has been updated successfully.{note}")
                }
            }
            Says::Wrote if self.creates() => {
                format!("File created successfully at: {path}{CURRENT}")
            }
            Says::Wrote => format!("The file {path} has been updated successfully.{CURRENT}"),
        }
    }

    /// The text to write.
    #[must_use]
    pub fn text(&self) -> FileText {
        FileText::new(self.bytes.clone())
    }
}

/// Edit: the file at `path`, as it is now (`None` if there's none),
/// with `old_string` replaced by `new_string`, once, or everywhere if
/// `replace_all`, as Claude Code's Edit does it, checking `state` that
/// the model read it.
///
/// - **Checks,** in Claude Code's order: the strings are different; the
///   file is at most [`MOST_EDIT`]; an empty `old_string` makes a file,
///   so there mustn't be one with text (an empty file is fine, read or
///   not); the file was read; `old_string` is in it, and just once unless
///   `replace_all`.
/// - **A file changed since it was read** is still edited if the edit
///   applies to it as it is now, as Claude Code 2.1.310 does, and the
///   answer says so; otherwise it's [`EditError::Stale`].
/// - **Matching:** exactly first, then with curly quotes (‘’“”) taken as
///   straight ones, both with a `\r` before a `\n` ignored, as Read
///   doesn't show it. A match through curly quotes keeps them: straight
///   quotes in `new_string` are curled as the file's are.
/// - **Text:** UTF-8 only, with a byte order mark kept. Each line keeps its
///   ending; those `new_string` puts in end as most of the file's do (in
///   its first 4096 characters).
/// - **Deleting:** an empty `new_string` takes the line ending right after
///   each match too, as Claude Code does, unless `old_string` ends with
///   one.
pub fn edit(
    path: &Path,
    file: Option<&FileRead>,
    state: &ReadState,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<Edited, EditError> {
    if old_string == new_string {
        return Err(EditError::Identical);
    }
    let says = |recovered| Says::Edited {
        all: replace_all,
        recovered,
    };
    let Some(file) = file else {
        if old_string.is_empty() {
            return Ok(Edited {
                bytes: new_string.as_bytes().to_vec(),
                expected: None,
                before: None,
                says: says(false),
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
    let (after, recovered) = if old_string.is_empty() {
        if !text.trim().is_empty() {
            return Err(EditError::Exists);
        }
        (new_string.to_owned(), false)
    } else {
        match state.check(path, file) {
            Check::NotRead => return Err(EditError::NotRead),
            Check::Fresh => (replace(text, old_string, new_string, replace_all)?, false),
            Check::Stale => {
                let after = replace(text, old_string, new_string, replace_all)
                    .map_err(|_| EditError::Stale)?;
                (after, true)
            }
        }
    };
    let mut bytes = Vec::with_capacity(after.len() + BOM.len());
    if decoded.bom {
        bytes.extend_from_slice(BOM);
    }
    bytes.extend_from_slice(after.as_bytes());
    Ok(Edited {
        bytes,
        expected: Some(file.version.clone()),
        before: Some(text.to_owned()),
        says: says(recovered),
    })
}

/// Write: the file at `path`, as it is now (`None` if there's none),
/// replaced by `content`, as given, line endings and all, checking
/// `state` that the model read it, if there was one, and that it hasn't
/// changed since.
pub fn write(
    path: &Path,
    file: Option<&FileRead>,
    state: &ReadState,
    content: &str,
) -> Result<Edited, EditError> {
    let before = match file {
        Some(file) => {
            match state.check(path, file) {
                Check::Fresh => {}
                Check::NotRead => return Err(EditError::NotRead),
                Check::Stale => return Err(EditError::Stale),
            }
            let bytes = file.bytes.strip_prefix(BOM).unwrap_or(&file.bytes);
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        None => None,
    };
    Ok(Edited {
        bytes: content.as_bytes().to_vec(),
        expected: file.map(|file| file.version.clone()),
        before,
        says: Says::Wrote,
    })
}

/// `text` with `old` replaced by `new`, as [`edit`] says.
fn replace(text: &str, old: &str, new: &str, replace_all: bool) -> Result<String, EditError> {
    let old = old.replace("\r\n", "\n");
    let mut new = new.replace("\r\n", "\n");
    if dominant(text) == Ending::CrLf {
        new = new.replace('\n', "\r\n");
    }
    // Exactly, then with curly quotes straight.
    for quotes in [false, true] {
        let seen = normal(text, quotes);
        let wanted = if quotes {
            straighten(&old)
        } else {
            old.clone()
        };
        let mut matches = Matches::new(text, &seen, &wanted, quotes);
        let Some(first) = matches.next() else {
            continue;
        };
        if !replace_all && matches.next().is_some() {
            return Err(EditError::Several {
                count: 2 + matches.count(),
                old_string: old,
            });
        }
        let new = if quotes {
            curl(&new, &text[first])
        } else {
            new
        };
        let deleting = new.is_empty() && !old.ends_with('\n');
        let mut matches = Matches::new(text, &seen, &wanted, quotes).peekable();
        let mut after = String::with_capacity(text.len() + new.len());
        let mut at = 0;
        while let Some(range) = matches.next() {
            let mut end = range.end;
            if deleting {
                let rest = &text[end..];
                let ending = if rest.starts_with("\r\n") {
                    2
                } else {
                    usize::from(rest.starts_with('\n'))
                };
                // Not into the next match.
                if matches.peek().is_none_or(|next| next.start >= end + ending) {
                    end += ending;
                }
            }
            after.push_str(&text[at..range.start]);
            after.push_str(&new);
            at = end;
            if !replace_all {
                break;
            }
        }
        after.push_str(&text[at..]);
        return Ok(after);
    }
    Err(EditError::NotFound { old_string: old })
}

/// `text` as it's matched: with the `\r` of every `\r\n` dropped, and its
/// curly quotes straight if `quotes`.
fn normal(text: &str, quotes: bool) -> Cow<'_, str> {
    if !(text.contains("\r\n") || quotes && text.contains(is_curly)) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            c if quotes && is_curly(c) => out.push(straight(c)),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Where `wanted` is in `text`, found in `seen`, `text` made [`normal`]:
/// each match as `text`'s bytes, in order, worked out as they're asked
/// for, so counting them keeps none.
struct Matches<'a> {
    found: std::str::MatchIndices<'a, &'a str>,
    wanted: usize,
    text: &'a str,
    chars: Peekable<CharIndices<'a>>,
    /// How many bytes shorter `seen` is than `text` so far.
    shrunk: usize,
    quotes: bool,
}

impl<'a> Matches<'a> {
    fn new(text: &'a str, seen: &'a str, wanted: &'a str, quotes: bool) -> Self {
        Self {
            found: seen.match_indices(wanted),
            wanted: wanted.len(),
            text,
            chars: text.char_indices().peekable(),
            shrunk: 0,
            quotes,
        }
    }

    /// Where `at`, in `seen`, is in `text`: before any `\r` dropped there,
    /// so a match starting at a line's end takes its `\r`, and one ending
    /// there leaves it.
    fn in_text(&mut self, at: usize) -> usize {
        while let Some(&(i, c)) = self.chars.peek() {
            if i - self.shrunk >= at {
                break;
            }
            if c == '\r' && self.text[i + 1..].starts_with('\n') {
                self.shrunk += 1;
            } else if self.quotes && is_curly(c) {
                self.shrunk += c.len_utf8() - 1;
            }
            self.chars.next();
        }
        at + self.shrunk
    }
}

impl Iterator for Matches<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let (at, _) = self.found.next()?;
        let start = self.in_text(at);
        let end = self.in_text(at + self.wanted);
        Some(start..end)
    }

    fn count(self) -> usize {
        self.found.count()
    }
}

/// Whether `c` is a curly quote, which [`straight`] makes straight.
fn is_curly(c: char) -> bool {
    matches!(c, '‘' | '’' | '“' | '”')
}

/// `c`, straight if it's a curly quote.
fn straight(c: char) -> char {
    match c {
        '‘' | '’' => '\'',
        '“' | '”' => '"',
        c => c,
    }
}

/// `text` with its curly quotes straight.
fn straighten(text: &str) -> String {
    text.chars().map(straight).collect()
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
