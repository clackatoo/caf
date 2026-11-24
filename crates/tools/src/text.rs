//! Files as text: decoding, line endings and sizes.

/// UTF-8's byte order mark, kept on a file that has one.
pub(crate) const BOM: &[u8] = b"\xEF\xBB\xBF";

/// How far a file is looked into for a NUL, which makes it binary.
const BINARY_SNIFF: usize = 8000;

/// How far a file is looked into for its line ending, in characters.
const ENDING_SNIFF: usize = 4096;

/// A file's bytes as text: UTF-8, with its byte order mark, if any, set
/// apart.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Decoded<'a> {
    /// Whether it starts with a byte order mark.
    pub(crate) bom: bool,
    /// The text after it.
    pub(crate) text: &'a str,
}

/// `bytes` as text, or `None` if they aren't: a NUL in the first 8000
/// bytes, a UTF-16 byte order mark, or anything that isn't UTF-8.
pub(crate) fn decode(bytes: &[u8]) -> Option<Decoded<'_>> {
    if bytes.starts_with(b"\xFF\xFE") || bytes.starts_with(b"\xFE\xFF") {
        return None;
    }
    if bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0) {
        return None;
    }
    let (bom, rest) = match bytes.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    let text = std::str::from_utf8(rest).ok()?;
    Some(Decoded { bom, text })
}

/// A file's line ending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// `\n`.
    Lf,
    /// `\r\n`.
    CrLf,
}

/// The line ending most of `text`'s first 4096 characters end with:
/// [`Ending::Lf`] if there are as many of each, or none.
pub(crate) fn dominant(text: &str) -> Ending {
    let end = text
        .char_indices()
        .nth(ENDING_SNIFF)
        .map_or(text.len(), |(at, _)| at);
    let start = &text.as_bytes()[..end];
    let (mut crlf, mut lf) = (0usize, 0usize);
    for (at, byte) in start.iter().enumerate() {
        if *byte == b'\n' {
            if at > 0 && start[at - 1] == b'\r' {
                crlf += 1;
            } else {
                lf += 1;
            }
        }
    }
    if crlf > lf { Ending::CrLf } else { Ending::Lf }
}

/// A size in bytes as Claude Code writes one: `512 bytes`, `1.5KB`,
/// `16MB`, `2.1GB`, to a tenth, without a `.0`.
#[must_use]
pub fn format_size(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "a size to a tenth")]
    let kb = bytes as f64 / 1024.0;
    if kb < 1.0 {
        return format!("{bytes} bytes");
    }
    let tenth = |value: f64| {
        let text = format!("{value:.1}");
        text.strip_suffix(".0")
            .map_or_else(|| text.clone(), str::to_owned)
    };
    if kb < 1024.0 {
        return format!("{}KB", tenth(kb));
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{}MB", tenth(mb));
    }
    format!("{}GB", tenth(mb / 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_decodes_and_its_bom_is_set_apart() {
        assert_eq!(
            decode(b"hi\n"),
            Some(Decoded {
                bom: false,
                text: "hi\n"
            })
        );
        assert_eq!(
            decode(b"\xEF\xBB\xBFhi"),
            Some(Decoded {
                bom: true,
                text: "hi"
            })
        );
        assert_eq!(
            decode(b""),
            Some(Decoded {
                bom: false,
                text: ""
            })
        );
        assert_eq!(decode("é".as_bytes()).unwrap().text, "é");
    }

    #[test]
    fn binary_isnt_text() {
        assert_eq!(decode(b"a\0b"), None);
        assert_eq!(decode(b"\xFF\xFEh\0i\0"), None);
        assert_eq!(decode(b"\xFE\xFF\0h\0i"), None);
        assert_eq!(decode(b"caf\xE9"), None, "Latin-1 isn't UTF-8");
        // Cut in the middle of a character.
        assert_eq!(decode(&"é".as_bytes()[..1]), None);
    }

    #[test]
    fn a_nul_only_counts_in_the_first_8000_bytes() {
        let mut late = vec![b'a'; BINARY_SNIFF];
        late.push(0);
        assert!(decode(&late).is_some(), "a NUL is valid UTF-8");
        let mut early = vec![b'a'; BINARY_SNIFF - 1];
        early.push(0);
        assert_eq!(decode(&early), None);
    }

    #[test]
    fn the_dominant_ending_is_the_most_common() {
        assert_eq!(dominant(""), Ending::Lf);
        assert_eq!(dominant("a\nb\n"), Ending::Lf);
        assert_eq!(dominant("a\r\nb\r\n"), Ending::CrLf);
        assert_eq!(dominant("a\r\nb\r\nc\n"), Ending::CrLf);
        assert_eq!(dominant("a\r\nb\n"), Ending::Lf, "a tie is \\n");
        assert_eq!(dominant("\r\n"), Ending::CrLf);
        assert_eq!(dominant("a\rb\r"), Ending::Lf, "a lone \\r ends no line");
    }

    #[test]
    fn the_ending_is_judged_from_the_first_4096_characters() {
        let text = format!("{}{}", "é\n".repeat(ENDING_SNIFF / 2), "x\r\n".repeat(5000));
        assert_eq!(dominant(&text), Ending::Lf);
        let text = format!("{}{}", "x\r\n".repeat(ENDING_SNIFF / 3), "\n".repeat(9000));
        assert_eq!(dominant(&text), Ending::CrLf);
    }

    #[test]
    fn sizes_read_as_claude_codes() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(1023), "1023 bytes");
        assert_eq!(format_size(1024), "1KB");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(30_001), "29.3KB");
        assert_eq!(format_size(16 * 1024 * 1024), "16MB");
        assert_eq!(format_size(17_000_000), "16.2MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3GB");
    }
}
