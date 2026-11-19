//! Agent text made safe to show: nothing in it hidden, nothing too long,
//! and no secrets.

use std::collections::VecDeque;
use std::fmt::Write as _;

use serde::Serialize;

/// `text`, something an agent said, trimmed, with nothing in it hidden
/// (see [`unhidden`]), and cut to `most` bytes with `…`.
pub(crate) fn agent_text(text: &str, most: usize) -> String {
    cut(unhidden(text.trim()), most)
}

/// `text` with nothing in it that could go unseen or change how the text
/// around it shows. Control characters but line breaks and tabs show as
/// `^U` (DEL as `^?`). Characters that reorder text, such as the
/// right-to-left override, characters of no width, and tag characters,
/// which mirror ASCII unseen, show as `<U+202E>`. `\r\n` becomes `\n`.
///
/// Simpler than clackatoo's `unhidden`, which keeps joiners inside emoji
/// and flags: here those show as code points too.
pub(crate) fn unhidden(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut shown = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' | '\t' => shown.push(ch),
            '\u{7f}' => shown.push_str("^?"),
            ch if u32::from(ch) < 0x20 => {
                shown.push('^');
                // A C0 control is below 0x20, so this is `@` to `_`.
                shown.push(char::from(b'@' + u8::try_from(u32::from(ch)).unwrap_or(0)));
            }
            ch if hidden(ch) => {
                let _ = write!(shown, "<U+{:04X}>", u32::from(ch));
            }
            ch => shown.push(ch),
        }
    }
    shown
}

/// Whether `ch` shows nothing, or changes how what's around it shows.
fn hidden(ch: char) -> bool {
    matches!(
        ch,
        // C1 controls.
        '\u{80}'..='\u{9f}'
        // Bidirectional controls.
        | '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        // No width: spaces, joiners, the word joiner, invisible operators,
        // the byte-order mark and the soft hyphen.
        | '\u{200b}'..='\u{200d}' | '\u{2060}'..='\u{2064}' | '\u{feff}' | '\u{ad}'
        // Tags, and the supplementary variation selectors.
        | '\u{e0000}'..='\u{e007f}' | '\u{e0100}'..='\u{e01ef}'
    )
}

/// `text` cut to at most `most` bytes, on a character boundary, with `…`
/// after it if anything was cut.
pub(crate) fn cut(mut text: String, most: usize) -> String {
    if text.len() > most {
        let at = text.floor_char_boundary(most);
        text.truncate(at);
        text.push('…');
    }
    text
}

/// `line` with anything that looks like an email address replaced by
/// `<email>`. Claude's adapter knows the account's email, and an adapter
/// may print it; CAF never passes it on.
pub(crate) fn without_emails(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for (n, word) in line.split(' ').enumerate() {
        if n > 0 {
            out.push(' ');
        }
        let email = word
            .split_once('@')
            .is_some_and(|(name, host)| !name.is_empty() && host.contains('.'));
        out.push_str(if email { "<email>" } else { word });
    }
    out
}

/// The last few lines an adapter wrote to its error output, cleaned:
/// what CAF shows when explaining why an adapter stopped.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Tail {
    /// The kept lines, oldest first.
    pub(crate) lines: VecDeque<String>,
    /// The unended line so far.
    #[serde(skip)]
    partial: Vec<u8>,
    /// The unended line grew past the limit: the rest of it is skipped.
    #[serde(skip)]
    skipping: bool,
}

impl Tail {
    /// Takes more of the error output, keeping the last `keep` non-empty
    /// lines of at most `line_bytes` each, and an unended line of at most
    /// `biggest` bytes. Returns the lines it finished, cleaned.
    pub(crate) fn push(
        &mut self,
        bytes: &[u8],
        keep: usize,
        line_bytes: usize,
        biggest: usize,
    ) -> Vec<String> {
        let mut finished = Vec::new();
        let mut rest = bytes;
        while let Some(at) = rest.iter().position(|&b| b == b'\n') {
            let (line, after) = rest.split_at(at);
            if !std::mem::take(&mut self.skipping) {
                self.partial.extend_from_slice(line);
                let line = std::mem::take(&mut self.partial);
                if let Some(line) = self.keep(&line, keep, line_bytes) {
                    finished.push(line);
                }
            }
            rest = &after[1..];
        }
        if !self.skipping {
            self.partial.extend_from_slice(rest);
            if self.partial.len() > biggest {
                self.partial = Vec::new();
                self.skipping = true;
            }
        }
        finished
    }

    fn keep(&mut self, line: &[u8], keep: usize, line_bytes: usize) -> Option<String> {
        let line = String::from_utf8_lossy(line);
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let line = cut(unhidden(&without_emails(line)), line_bytes);
        self.lines.push_back(line.clone());
        while self.lines.len() > keep {
            self.lines.pop_front();
        }
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_characters_show_as_what_they_are() {
        assert_eq!(
            unhidden("a\u{202e}b\u{1b}[31m\u{7f}\r\nc\u{200b}"),
            "a<U+202E>b^[[31m^?\nc<U+200B>"
        );
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        assert_eq!(cut("ééé".to_owned(), 3), "é…");
        assert_eq!(cut("abc".to_owned(), 3), "abc");
    }

    #[test]
    fn the_tail_keeps_the_last_lines_cleaned_and_without_emails() {
        let mut tail = Tail::default();
        tail.push(b"one\n\ntwo me@example.com\nthr", 2, 300, 100);
        tail.push(b"ee\n", 2, 300, 100);
        assert_eq!(tail.lines, ["two <email>", "three"]);
        tail.push(&[b'x'; 200], 2, 300, 100);
        tail.push(b"xx\nfour\n", 2, 300, 100);
        assert_eq!(tail.lines, ["three", "four"]);
    }
}
