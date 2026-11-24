//! Read: a file's lines, numbered, as the model is answered them.

use std::path::Path;

use caf_core::tool::MOST_IMAGE;

use crate::backend::FileError;
use crate::text::{decode, format_size};

/// The most lines a read gives, unless it's given a limit.
pub const DEFAULT_LIMIT: u64 = 2000;
/// The most tokens a read gives, estimated as a quarter of its bytes.
pub const MOST_TOKENS: u64 = 25_000;
/// The most tokens a read given `allow_large` gives.
pub const MOST_TOKENS_LARGE: u64 = 100_000;

/// What a read of a file comes to.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// Its lines.
    Text(TextRead),
    /// A picture: the model is answered it as an image, and the record
    /// keeps it as one.
    Image(ImageRead),
    /// It isn't read, for this reason, which the model is answered.
    Refused(String),
}

/// The lines of a file that were read.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRead {
    /// What the model is answered: each line as `{n}\t{line}`, numbered
    /// from 1, or a reminder if there were none to give.
    pub answer: String,
    /// What was read, for the record: the lines, without their numbers,
    /// each without its ending, joined by `\n`.
    pub text: String,
    /// The first line read, numbered from 1.
    pub first: u64,
    /// Whether it left out any of the file: it may not be edited until
    /// it's read whole.
    pub partial: bool,
}

/// A picture that was read.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRead {
    /// Its type, from its first bytes, such as `image/png`.
    pub mime: &'static str,
}

/// Reads a file of `bytes`, at `path`, as Claude Code's Read does: from
/// line `offset` (numbered from 1: 0 is read as 1), `limit` lines (2000
/// unless given; 0 is read as 1).
///
/// - Its lines are split at `\n`, and a `\r` before it, or at the very
///   end, is dropped. A last line ending in `\n` is followed by none.
/// - A UTF-8 byte order mark is skipped.
/// - Over [`MOST_TOKENS`], estimated as a quarter of the bytes of the
///   lines read, it's refused, as Claude Code refuses it, unless
///   `allow_large` raises that to [`MOST_TOKENS_LARGE`].
/// - A `.png`, `.jpg`, `.jpeg`, `.gif` or `.webp` whose first bytes are a
///   picture's is read as one, up to [`MOST_IMAGE`]. A `.pdf` is refused,
///   as is anything that isn't UTF-8 text (see Claude Code's words for
///   both below).
/// - A notebook (`.ipynb`) is read as its JSON.
#[must_use]
pub fn read(
    path: &Path,
    bytes: &[u8],
    offset: Option<u64>,
    limit: Option<u64>,
    allow_large: bool,
) -> Reading {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase());
    let ext = ext.as_deref();
    if let Some(mime) = image_type(ext, bytes) {
        if bytes.len() > MOST_IMAGE {
            return Reading::Refused(format!(
                "This image is too large to read ({}). The most CAF reads is {}.",
                format_size(bytes.len() as u64),
                format_size(MOST_IMAGE as u64),
            ));
        }
        return Reading::Image(ImageRead { mime });
    }
    if ext == Some("pdf") {
        return Reading::Refused("Reading PDFs isn't supported yet.".to_owned());
    }
    let Some(decoded) = decode(bytes) else {
        let ext = ext.map(|ext| format!(".{ext} ")).unwrap_or_default();
        return Reading::Refused(format!(
            "This tool cannot read binary files. The file appears to be a binary {ext}file."
        ));
    };
    let text = decoded.text;
    let first = offset.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(DEFAULT_LIMIT).max(1);
    if text.is_empty() {
        return Reading::Text(TextRead {
            answer: "<system-reminder>Warning: the file exists but the contents are empty.</system-reminder>"
                .to_owned(),
            text: String::new(),
            first,
            partial: false,
        });
    }
    let mut all = 0u64;
    let mut read = Vec::new();
    for line in lines(text) {
        all += 1;
        if all >= first && all - first < limit {
            read.push(line);
        }
    }
    if read.is_empty() {
        return Reading::Text(TextRead {
            answer: format!(
                "<system-reminder>Warning: the file exists but is shorter than the provided offset ({first}). The file has {all} lines.</system-reminder>"
            ),
            text: String::new(),
            first,
            partial: true,
        });
    }
    let size: usize = read.iter().map(|line| line.len() + 1).sum::<usize>() - 1;
    let tokens = (size as u64 + 2) / 4;
    let most = if allow_large {
        MOST_TOKENS_LARGE
    } else {
        MOST_TOKENS
    };
    if tokens > most {
        return Reading::Refused(format!(
            "File content ({tokens} tokens) exceeds maximum allowed tokens ({most}). Use offset and limit parameters to read specific portions of the file, or search for specific content instead of reading the whole file."
        ));
    }
    let mut answer = String::with_capacity(size + read.len() * 8);
    for (n, line) in (first..).zip(&read) {
        if n > first {
            answer.push('\n');
        }
        answer.push_str(&n.to_string());
        answer.push('\t');
        answer.push_str(line);
    }
    Reading::Text(TextRead {
        answer,
        text: read.join("\n"),
        first,
        partial: first > 1 || read.len() as u64 != all,
    })
}

/// `text`'s lines, each without its `\n`, or a `\r` before it or at the
/// very end.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split_terminator('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// The type of the picture `bytes` are, if `ext` is a picture's and its
/// first bytes say it's one, of any type a read gives.
fn image_type(ext: Option<&str>, bytes: &[u8]) -> Option<&'static str> {
    if !matches!(ext, Some("png" | "jpg" | "jpeg" | "gif" | "webp")) {
        return None;
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Which of the file tools a message is for: each words some failures its
/// own way.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileTool {
    /// Read.
    Read,
    /// Edit.
    Edit,
    /// Write.
    Write,
}

/// What the model is answered when there's no file at the path it gave,
/// as Claude Code says it: with the folder its paths are taken from.
#[must_use]
pub fn missing(cwd: &Path) -> String {
    format!(
        "File does not exist. Note: your current working directory is {}.",
        cwd.display()
    )
}

/// What the model is answered when `tool` couldn't read or write the file
/// at `path`, as the model gave it, for `error`; `cwd` is the folder its
/// paths are taken from.
#[must_use]
pub fn file_error(error: &FileError, tool: FileTool, path: &Path, cwd: &Path) -> String {
    let path = path.display();
    match error {
        FileError::NotFound => missing(cwd),
        FileError::IsDirectory if tool == FileTool::Write => format!(
            "{path} is a directory, not a file. To create a file inside it, include the file name in file_path."
        ),
        FileError::IsDirectory => format!("{path} is a directory, not a file."),
        FileError::NotRegular if tool == FileTool::Write => format!(
            "{path} exists but is not a regular file (a device, FIFO or socket). Write only creates or overwrites regular files."
        ),
        FileError::NotRegular => {
            format!("{path} exists but is not a regular file (a device, FIFO or socket).")
        }
        FileError::TooBig { size } if tool == FileTool::Edit => crate::edit::too_big(*size),
        FileError::TooBig { size } => {
            format!("File is too large to read ({}).", format_size(*size))
        }
        FileError::Changed => crate::state::STALE.to_owned(),
        FileError::Denied(why) | FileError::Other(why) => why.clone(),
    }
}

#[cfg(test)]
mod tests;
