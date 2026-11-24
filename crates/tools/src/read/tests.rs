use std::path::Path;

use super::*;

fn text(path: &str, bytes: &[u8], offset: Option<u64>, limit: Option<u64>) -> TextRead {
    match read(Path::new(path), bytes, offset, limit, false) {
        Reading::Text(read) => read,
        other => panic!("not text: {other:?}"),
    }
}

fn refused(path: &str, bytes: &[u8]) -> String {
    match read(Path::new(path), bytes, None, None, false) {
        Reading::Refused(why) => why,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn lines_are_numbered_from_1_with_a_tab() {
    let read = text("a.rs", b"fn main() {\n    hi();\n}\n", None, None);
    assert_eq!(read.answer, "1\tfn main() {\n2\t    hi();\n3\t}");
    assert_eq!(read.text, "fn main() {\n    hi();\n}");
    assert_eq!(read.first, 1);
    assert!(!read.partial);
}

#[test]
fn a_last_line_without_an_ending_is_read() {
    let read = text("a", b"one\ntwo", None, None);
    assert_eq!(read.answer, "1\tone\n2\ttwo");
}

#[test]
fn blank_lines_are_numbered() {
    let read = text("a", b"\n\nx\n\n", None, None);
    assert_eq!(read.answer, "1\t\n2\t\n3\tx\n4\t");
}

#[test]
fn carriage_returns_before_line_ends_are_dropped() {
    let read = text("a", b"one\r\ntwo\r\nthree\r", None, None);
    assert_eq!(read.answer, "1\tone\n2\ttwo\n3\tthree");
    // Only one: a line of its own ending in \r keeps the rest.
    let read = text("a", b"a\r\r\nb\rc\n", None, None);
    assert_eq!(read.answer, "1\ta\r\n2\tb\rc");
}

#[test]
fn a_bom_is_skipped() {
    let read = text("a", b"\xEF\xBB\xBFhello\n", None, None);
    assert_eq!(read.answer, "1\thello");
}

#[test]
fn an_empty_file_says_so() {
    let empty =
        "<system-reminder>Warning: the file exists but the contents are empty.</system-reminder>";
    for bytes in [&b""[..], b"\xEF\xBB\xBF"] {
        let read = text("a", bytes, None, None);
        assert_eq!(read.answer, empty);
        assert_eq!(read.text, "");
        assert!(!read.partial);
    }
    assert_eq!(text("a", b"", Some(50), None).answer, empty);
}

#[test]
fn an_offset_and_limit_choose_lines() {
    let file = b"1\n2\n3\n4\n5\n";
    let read = text("a", file, Some(2), Some(2));
    assert_eq!(read.answer, "2\t2\n3\t3");
    assert_eq!(read.text, "2\n3");
    assert_eq!(read.first, 2);
    assert!(read.partial);
    // To the end, from a line past the first: still partial.
    let read = text("a", file, Some(4), None);
    assert_eq!(read.answer, "4\t4\n5\t5");
    assert!(read.partial);
    // The whole file, with a limit just big enough.
    let read = text("a", file, Some(1), Some(5));
    assert!(!read.partial);
}

#[test]
fn offset_0_is_line_1_and_limit_0_is_1() {
    let file = b"a\nb\n";
    assert_eq!(text("a", file, Some(0), None).answer, "1\ta\n2\tb");
    assert_eq!(text("a", file, None, Some(0)).answer, "1\ta");
}

#[test]
fn an_offset_past_the_end_says_how_long_the_file_is() {
    let read = text("a", b"a\nb\nc\n", Some(4), None);
    assert_eq!(
        read.answer,
        "<system-reminder>Warning: the file exists but is shorter than the provided offset (4). The file has 3 lines.</system-reminder>"
    );
    assert_eq!(read.text, "");
    assert!(read.partial);
    let read = text("a", b"a\n", Some(u64::MAX), Some(u64::MAX));
    assert!(read.answer.contains(&format!("offset ({})", u64::MAX)));
}

#[test]
fn a_huge_limit_doesnt_overflow() {
    let read = text("a", b"a\nb\n", Some(2), Some(u64::MAX));
    assert_eq!(read.answer, "2\tb");
}

#[test]
fn the_default_limit_is_2000_lines() {
    let file = "x\n".repeat(2500);
    let read = text("a", file.as_bytes(), None, None);
    assert_eq!(read.answer.lines().count(), 2000);
    assert!(read.answer.ends_with("2000\tx"));
    assert!(read.partial);
}

#[test]
fn multibyte_text_is_read_whole() {
    let read = text("a", "héllo\n日本語\n🦀\n".as_bytes(), Some(2), None);
    assert_eq!(read.answer, "2\t日本語\n3\t🦀");
}

#[test]
fn too_many_tokens_is_refused_as_claude_code_does() {
    let line = "x".repeat(99);
    let file = format!("{line}\n").repeat(1001); // 100,100 bytes, ~25,025 tokens
    let why = match read(Path::new("big"), file.as_bytes(), None, None, false) {
        Reading::Refused(why) => why,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        why,
        "File content (25025 tokens) exceeds maximum allowed tokens (25000). Use offset and limit parameters to read specific portions of the file, or search for specific content instead of reading the whole file. To read it anyway, up to what still fits in your context, call Read again with allow_large: true \u{2014} only if you genuinely need all of it or the user asked for the whole file."
    );
    // Part of it is fine.
    assert!(matches!(
        read(Path::new("big"), file.as_bytes(), Some(1), Some(500), false),
        Reading::Text(_)
    ));
    // As is all of it, given allow_large.
    assert!(matches!(
        read(Path::new("big"), file.as_bytes(), None, None, true),
        Reading::Text(_)
    ));
}

#[test]
fn a_file_over_256_kb_read_whole_is_refused_as_claude_code_does() {
    let file = "x\n".repeat(150_000); // 300,000 bytes, few tokens a line
    let why = match read(Path::new("big"), file.as_bytes(), None, None, false) {
        Reading::Refused(why) => why,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        why,
        "File content (293KB) exceeds maximum allowed size (256KB). Use offset and limit parameters to read specific portions of the file, or search for specific content instead of reading the whole file. To read it anyway, up to what still fits in your context, call Read again with allow_large: true \u{2014} only if you genuinely need all of it or the user asked for the whole file."
    );
    // In part, it's read.
    assert!(matches!(
        read(Path::new("big"), file.as_bytes(), Some(1), Some(10), false),
        Reading::Text(_)
    ));
    assert!(matches!(
        read(Path::new("big"), file.as_bytes(), Some(5), None, false),
        Reading::Text(_)
    ));
    // Given allow_large, only the token cap counts.
    assert!(matches!(
        read(Path::new("big"), file.as_bytes(), None, None, true),
        Reading::Text(_)
    ));
    // Exactly 256KB is fine.
    let file = "x\n".repeat(131_072);
    assert!(matches!(
        read(Path::new("a"), file.as_bytes(), None, None, false),
        Reading::Text(_)
    ));
}

#[test]
fn allow_large_has_a_cap_too() {
    let file = "x".repeat(400_010);
    match read(Path::new("big"), file.as_bytes(), None, None, true) {
        Reading::Refused(why) => assert!(
            why.starts_with(
                "File content (100003 tokens) exceeds maximum allowed tokens (100000)."
            ),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn exactly_the_cap_is_fine() {
    let file = "x".repeat(100_001); // rounds to 25,000
    assert!(matches!(
        read(Path::new("a"), file.as_bytes(), None, None, false),
        Reading::Text(_)
    ));
}

#[test]
fn binary_files_are_refused_naming_their_extension() {
    assert_eq!(
        refused("prog.BIN", b"\x7fELF\0\0"),
        "This tool cannot read binary files. The file appears to be a binary .bin file. Use a skill for this file type if one is available, or a shell command or script that can read the format."
    );
    assert_eq!(
        refused("prog", b"\0"),
        "This tool cannot read binary files. The file appears to be a binary file. Use a skill for this file type if one is available, or a shell command or script that can read the format."
    );
    assert_eq!(
        refused("notes.txt", b"\xFF\xFEh\0i\0"),
        "This tool cannot read binary files. The file appears to be a binary .txt file. Use a skill for this file type if one is available, or a shell command or script that can read the format."
    );
    assert!(refused("latin.txt", b"caf\xE9").starts_with("This tool cannot read binary"));
}

#[test]
fn pdfs_are_refused_for_now() {
    assert_eq!(
        refused("a.PDF", b"%PDF-1.7"),
        "Reading PDFs isn't supported yet."
    );
}

#[test]
fn notebooks_are_read_as_their_json() {
    let read = text("n.ipynb", b"{\n \"cells\": []\n}\n", None, None);
    assert_eq!(read.answer, "1\t{\n2\t \"cells\": []\n3\t}");
}

#[test]
fn pictures_are_read_by_extension_and_first_bytes() {
    let cases: [(&str, &[u8], &str); 6] = [
        ("a.png", b"\x89PNG\r\n\x1a\n....", "image/png"),
        ("a.JPG", b"\xFF\xD8\xFF\xE0", "image/jpeg"),
        ("a.jpeg", b"\xFF\xD8\xFF\xDB", "image/jpeg"),
        ("a.gif", b"GIF89a..", "image/gif"),
        ("a.gif", b"GIF87a..", "image/gif"),
        ("a.webp", b"RIFF\x10\0\0\0WEBPVP8 ", "image/webp"),
    ];
    for (path, bytes, mime) in cases {
        assert_eq!(
            read(Path::new(path), bytes, None, None, false),
            Reading::Image(ImageRead { mime }),
            "{path}"
        );
    }
    // Its type is from its bytes, not its name.
    assert_eq!(
        read(Path::new("a.png"), b"GIF89a", None, None, false),
        Reading::Image(ImageRead { mime: "image/gif" })
    );
}

#[test]
fn a_picture_needs_both_its_extension_and_its_bytes() {
    // A text file named .png is read as text.
    assert!(matches!(
        read(Path::new("a.png"), b"not a picture", None, None, false),
        Reading::Text(_)
    ));
    // A PNG named otherwise is binary.
    assert!(refused("a.dat", b"\x89PNG\r\n\x1a\n\0").contains("binary .dat file"));
    // A RIFF that isn't a WebP, or too short to say.
    assert!(!matches!(
        read(Path::new("a.webp"), b"RIFF\0\0\0\0WAVE", None, None, false),
        Reading::Image(_)
    ));
    assert!(!matches!(
        read(Path::new("a.webp"), b"RIFF", None, None, false),
        Reading::Image(_)
    ));
}

#[test]
fn a_picture_over_the_most_kept_is_refused() {
    let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
    big.resize(MOST_IMAGE + 1, 0);
    assert_eq!(
        refused("a.png", &big),
        "This image is too large to read (2MB). The most CAF reads is 2MB."
    );
    big.truncate(MOST_IMAGE);
    assert!(matches!(
        read(Path::new("a.png"), &big, None, None, false),
        Reading::Image(_)
    ));
}

#[test]
fn file_errors_read_as_claude_code_words_them() {
    let path = Path::new("/w/src");
    let cwd = Path::new("/w");
    let say = |error: FileError, tool| file_error(&error, tool, path, cwd);
    assert_eq!(
        say(FileError::NotFound, FileTool::Read),
        "File does not exist. Note: your current working directory is /w."
    );
    assert_eq!(missing(cwd), say(FileError::NotFound, FileTool::Edit));
    assert_eq!(
        say(FileError::IsDirectory, FileTool::Read),
        "/w/src is a directory, not a file."
    );
    assert_eq!(
        say(FileError::IsDirectory, FileTool::Edit),
        "/w/src is a directory, not a file."
    );
    assert_eq!(
        say(FileError::IsDirectory, FileTool::Write),
        "/w/src is a directory, not a file. To create a file inside it, include the file name in file_path."
    );
    assert_eq!(
        say(FileError::NotRegular, FileTool::Write),
        "/w/src exists but is not a regular file (a device, FIFO or socket). Write only creates or overwrites regular files."
    );
    assert_eq!(
        say(FileError::NotRegular, FileTool::Read),
        "/w/src exists but is not a regular file (a device, FIFO or socket)."
    );
    assert_eq!(
        say(FileError::TooBig { size: 300 << 20 }, FileTool::Read),
        "File is too large to read (300MB)."
    );
    assert_eq!(
        say(FileError::TooBig { size: 17 << 20 }, FileTool::Edit),
        "File is too large to edit (17MB). Maximum editable file size is 16MB."
    );
    assert_eq!(
        say(FileError::Changed, FileTool::Write),
        "File has been modified since read, either by the user or by a linter. Read it again before attempting to write it."
    );
    assert_eq!(
        say(
            FileError::Denied("Not in this project.".into()),
            FileTool::Write
        ),
        "Not in this project."
    );
    assert_eq!(
        say(FileError::Other("Disk full.".into()), FileTool::Write),
        "Disk full."
    );
}
