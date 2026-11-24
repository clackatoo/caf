use std::path::Path;

use caf_core::tool::LineChange;
use proptest::prelude::*;

use super::*;

const PATH: &str = "/w/a.txt";

/// The file `bytes` at version 1, and a state in which it was read whole.
fn read(bytes: &[u8]) -> (FileRead, ReadState) {
    let file = FileRead::new(bytes.to_vec(), Version::new("1"));
    let mut state = ReadState::new();
    state.record(PATH.into(), Version::new("1"), bytes, false);
    (file, state)
}

/// `bytes` edited, the file read whole first.
fn edited(bytes: &[u8], old: &str, new: &str, all: bool) -> Result<Vec<u8>, EditError> {
    let (file, state) = read(bytes);
    edit(Path::new(PATH), Some(&file), &state, old, new, all).map(|edited| edited.bytes)
}

fn ok(bytes: &[u8], old: &str, new: &str) -> String {
    String::from_utf8(edited(bytes, old, new, false).unwrap()).unwrap()
}

#[test]
fn a_unique_string_is_replaced() {
    assert_eq!(ok(b"one two three", "two", "2"), "one 2 three");
    assert_eq!(ok(b"a\nb\nc\n", "b\nc", "B\nC"), "a\nB\nC\n");
}

#[test]
fn the_change_is_worked_out_with_its_version_and_diff() {
    let (file, state) = read(b"a\nb\nc\n");
    let edit = edit(Path::new(PATH), Some(&file), &state, "b", "B", false).unwrap();
    assert_eq!(edit.expected, Some(Version::new("1")));
    assert!(!edit.creates());
    let diff = edit.diff(PATH);
    assert_eq!(diff.path, PATH);
    assert!(!diff.new_file);
    assert_eq!((diff.added, diff.removed), (1, 1));
    let changed: Vec<_> = diff.hunks[0]
        .lines
        .iter()
        .filter(|line| line.change != LineChange::Kept)
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(changed, ["b", "B"]);
}

#[test]
fn identical_strings_are_refused_first() {
    assert_eq!(
        edit(Path::new(PATH), None, &ReadState::new(), "x", "x", false),
        Err(EditError::Identical)
    );
    assert_eq!(
        EditError::Identical.to_string(),
        "No changes to make: old_string and new_string are exactly the same."
    );
}

#[test]
fn a_missing_string_is_refused_naming_it() {
    let error = edited(b"abc", "xyz", "q", false).unwrap_err();
    assert_eq!(
        error.to_string(),
        "String to replace not found in file.\nString: xyz"
    );
}

#[test]
fn several_matches_need_replace_all() {
    let error = edited(b"x x x", "x", "y", false).unwrap_err();
    assert_eq!(
        error,
        EditError::Several {
            count: 3,
            old_string: "x".into()
        }
    );
    assert_eq!(
        error.to_string(),
        "Found 3 matches of the string to replace, but replace_all is false. To replace all occurrences, set replace_all to true. To replace only one occurrence, please provide more context to uniquely identify the instance.\nString: x"
    );
    assert_eq!(edited(b"x x x", "x", "y", true).unwrap(), b"y y y");
    // Matches don't overlap.
    assert_eq!(edited(b"aaaa", "aa", "b", true).unwrap(), b"bb");
    assert_eq!(edited(b"aaa", "aa", "b", false).unwrap(), b"ba");
}

#[test]
fn an_empty_old_string_makes_a_file() {
    let state = ReadState::new();
    let edit = edit(Path::new(PATH), None, &state, "", "new\n", false).unwrap();
    assert_eq!(edit.bytes, b"new\n");
    assert_eq!(edit.expected, None);
    assert!(edit.creates());
    assert!(edit.diff(PATH).new_file);
}

#[test]
fn an_empty_old_string_fills_an_empty_file() {
    assert_eq!(ok(b"", "", "x"), "x");
    assert_eq!(ok(b"  \n", "", "x"), "x");
    // Keeping its BOM.
    assert_eq!(
        edited(b"\xEF\xBB\xBF", "", "x", false).unwrap(),
        b"\xEF\xBB\xBFx"
    );
}

#[test]
fn an_empty_old_string_wont_overwrite_text() {
    let error = edited(b"text", "", "x", false).unwrap_err();
    assert_eq!(error, EditError::Exists);
    assert_eq!(
        error.to_string(),
        "Cannot create new file - file already exists."
    );
}

#[test]
fn a_missing_file_needs_an_empty_old_string() {
    let error = edit(Path::new(PATH), None, &ReadState::new(), "x", "y", false).unwrap_err();
    assert_eq!(error, EditError::Missing);
    assert_eq!(
        error.message(Path::new("/w")),
        "File does not exist. Note: your current working directory is /w."
    );
    // Other errors say the same either way.
    assert_eq!(
        EditError::Exists.message(Path::new("/w")),
        EditError::Exists.to_string()
    );
}

#[test]
fn a_file_never_read_isnt_edited() {
    let file = FileRead::new(b"abc".to_vec(), Version::new("1"));
    let state = ReadState::new();
    let error = edit(Path::new(PATH), Some(&file), &state, "b", "B", false).unwrap_err();
    assert_eq!(error, EditError::NotRead);
    assert_eq!(
        error.to_string(),
        "File has not been read yet. Read it first before writing to it."
    );
}

#[test]
fn a_file_read_in_part_is_edited() {
    // As Claude Code allows: a file too big to read whole can be edited.
    let file = FileRead::new(b"abc".to_vec(), Version::new("1"));
    let mut state = ReadState::new();
    state.record(PATH.into(), Version::new("1"), b"abc", true);
    let edit = edit(Path::new(PATH), Some(&file), &state, "b", "B", false).unwrap();
    assert_eq!(edit.bytes, b"aBc");
}

#[test]
fn a_file_changed_since_it_was_read_isnt_edited_if_the_edit_no_longer_applies() {
    let (_, state) = read(b"abc");
    let now = FileRead::new(b"axc".to_vec(), Version::new("2"));
    let error = edit(Path::new(PATH), Some(&now), &state, "b", "B", false).unwrap_err();
    assert_eq!(error, EditError::Stale);
    assert_eq!(
        error.to_string(),
        "File has been modified since read, either by the user or by a linter. Read it again before attempting to write it."
    );
}

#[test]
fn unchanged_bytes_at_a_new_version_are_written_expecting_it() {
    let (_, state) = read(b"abc");
    let now = FileRead::new(b"abc".to_vec(), Version::new("2"));
    let edit = edit(Path::new(PATH), Some(&now), &state, "b", "B", false).unwrap();
    assert_eq!(edit.expected, Some(Version::new("2")));
}

#[test]
fn checks_come_in_claude_codes_order() {
    // Not read, and the string's missing: not read.
    let file = FileRead::new(b"abc".to_vec(), Version::new("1"));
    let none = ReadState::new();
    assert_eq!(
        edit(Path::new(PATH), Some(&file), &none, "zzz", "y", false),
        Err(EditError::NotRead)
    );
    // Not read, and the file has text: it exists.
    assert_eq!(
        edit(Path::new(PATH), Some(&file), &none, "", "y", false),
        Err(EditError::Exists)
    );
}

#[test]
fn files_over_16_mib_arent_edited() {
    let big = vec![b'a'; (MOST_EDIT + 1) as usize];
    let error = edited(&big, "a", "b", false).unwrap_err();
    assert_eq!(
        error,
        EditError::TooBig {
            size: MOST_EDIT + 1
        }
    );
    assert_eq!(
        error.to_string(),
        "File is too large to edit (16MB). Maximum editable file size is 16MB."
    );
}

#[test]
fn files_that_arent_utf8_arent_edited() {
    for bytes in [&b"caf\xE9"[..], b"a\0b", b"\xFF\xFEa\0"] {
        let error = edited(bytes, "a", "b", false).unwrap_err();
        assert_eq!(error, EditError::NotText);
    }
    assert_eq!(
        EditError::NotText.to_string(),
        "This tool cannot edit binary files. The file is not UTF-8 text."
    );
}

#[test]
fn a_bom_is_kept() {
    let after = edited(b"\xEF\xBB\xBFhello", "hello", "bye", false).unwrap();
    assert_eq!(after, b"\xEF\xBB\xBFbye");
    // And the diff doesn't show it.
    let (file, state) = read(b"\xEF\xBB\xBFhello");
    let edit = edit(Path::new(PATH), Some(&file), &state, "hello", "bye", false).unwrap();
    let diff = edit.diff(PATH);
    assert!(
        diff.hunks[0]
            .lines
            .iter()
            .all(|line| !line.text.contains('\u{feff}'))
    );
}

#[test]
fn a_crlf_file_keeps_its_endings() {
    assert_eq!(
        ok(b"a\r\nb\r\nc\r\n", "a\nb", "x\ny\nz"),
        "x\r\ny\r\nz\r\nc\r\n"
    );
    // The model's own \r\n isn't doubled.
    assert_eq!(
        ok(b"a\r\nb\r\nc\r\n", "a\r\nb", "x\r\ny"),
        "x\r\ny\r\nc\r\n"
    );
    // On one line, nothing to convert.
    assert_eq!(ok(b"a\r\nb\r\n", "b", "c"), "a\r\nc\r\n");
}

#[test]
fn a_crlf_file_with_a_few_lf_lines_matches_them_too() {
    // The lines put in end as most of the file's do.
    let file = b"a\r\nb\r\nc\r\nd\ne\n";
    assert_eq!(ok(file, "d\ne", "D\nE"), "a\r\nb\r\nc\r\nD\r\nE\n");
}

#[test]
fn an_lf_file_is_left_lf() {
    assert_eq!(ok(b"a\nb\n", "a", "x\ny"), "x\ny\nb\n");
    // A model's \r\n in an LF file ends as the file's lines do.
    assert_eq!(ok(b"a\nb\n", "a", "x\r\ny"), "x\ny\nb\n");
}

#[test]
fn curly_quotes_match_straight_ones() {
    let file = "say “hi” and ‘bye’ now";
    assert_eq!(
        ok(
            file.as_bytes(),
            "say \"hi\" and 'bye'",
            "say \"yo\" and 'ok'"
        ),
        "say “yo” and ‘ok’ now"
    );
}

#[test]
fn an_exact_match_comes_before_a_curly_one() {
    let file = "“x” then \"x\"";
    assert_eq!(ok(file.as_bytes(), "\"x\"", "y"), "“x” then y");
}

#[test]
fn curly_quotes_match_many_times_with_multibyte_text() {
    let file = "é“a” — ü“a” — “a”";
    let after =
        String::from_utf8(edited(file.as_bytes(), "\"a\"", "\"b\"", true).unwrap()).unwrap();
    assert_eq!(after, "é“b” — ü“b” — “b”");
    let error = edited(file.as_bytes(), "\"a\"", "b", false).unwrap_err();
    assert_eq!(
        error,
        EditError::Several {
            count: 3,
            old_string: "\"a\"".into()
        }
    );
}

#[test]
fn apostrophes_curl_closing() {
    let file = "it’s ‘fine’";
    assert_eq!(
        ok(file.as_bytes(), "it's 'fine'", "don't 'panic' ('now')"),
        "don’t ‘panic’ (‘now’)"
    );
}

#[test]
fn quotes_of_a_kind_the_match_hasnt_stay_straight() {
    let file = "“a” b";
    assert_eq!(ok(file.as_bytes(), "\"a\"", "'c' \"d\""), "'c' “d” b");
}

#[test]
fn deleting_takes_the_line_ending_after() {
    assert_eq!(ok(b"a\nb\nc\n", "b", ""), "a\nc\n");
    assert_eq!(ok(b"a\r\nb\r\nc\r\n", "b", ""), "a\r\nc\r\n");
    // Not if the string ends with one.
    assert_eq!(ok(b"a\nb\nc\n", "b\n", ""), "a\nc\n");
    // At the end of the file, there's none.
    assert_eq!(ok(b"a\nb", "b", ""), "a\n");
    // Each, replacing all.
    assert_eq!(edited(b"x\ny\nx\n", "x", "", true).unwrap(), b"y\n");
}

#[test]
fn deleting_never_takes_the_start_of_the_next_match() {
    assert_eq!(edited(b"\nx\nx", "\nx", "", true).unwrap(), b"");
    assert_eq!(edited(b"a\na\n", "a\na", "", false).unwrap(), b"");
}

#[test]
fn multibyte_strings_are_replaced_whole() {
    assert_eq!(
        ok("日本語テキスト".as_bytes(), "本語", "🦀"),
        "日🦀テキスト"
    );
}

#[test]
fn write_makes_a_file() {
    let edit = write(Path::new(PATH), None, &ReadState::new(), "hi\r\n").unwrap();
    assert_eq!(edit.bytes, b"hi\r\n", "as given, endings and all");
    assert!(edit.creates());
    assert_eq!(edit.expected, None);
    assert!(edit.diff(PATH).new_file);
}

#[test]
fn write_overwrites_a_file_read_whole() {
    let (file, state) = read(b"old\r\n");
    let edit = write(Path::new(PATH), Some(&file), &state, "new\n").unwrap();
    assert_eq!(edit.bytes, b"new\n");
    assert!(!edit.creates());
    assert_eq!(edit.expected, Some(Version::new("1")));
    assert_eq!(edit.diff(PATH).removed, 1);
}

#[test]
fn write_overwrites_only_a_file_read_whole_and_unchanged() {
    let file = FileRead::new(b"old".to_vec(), Version::new("1"));
    let mut state = ReadState::new();
    assert_eq!(
        write(Path::new(PATH), Some(&file), &state, "new"),
        Err(EditError::NotRead)
    );
    // A read in part is a read.
    state.record(PATH.into(), Version::new("1"), b"old", true);
    assert!(write(Path::new(PATH), Some(&file), &state, "new").is_ok());
    state.record(PATH.into(), Version::new("1"), b"old", false);
    let now = FileRead::new(b"older".to_vec(), Version::new("2"));
    assert_eq!(
        write(Path::new(PATH), Some(&now), &state, "new"),
        Err(EditError::Stale)
    );
}

#[test]
fn write_overwrites_binary_files() {
    let (file, state) = read(b"\0\xFF");
    let edit = write(Path::new(PATH), Some(&file), &state, "text").unwrap();
    assert_eq!(edit.bytes, b"text");
    let _ = edit.diff(PATH);
}

/// What Claude Code adds to a change's answer: the model needn't read
/// the file back.
const CURRENT: &str = " (file state is current in your context — no need to Read it back)";

#[test]
fn answers_read_as_claude_codes() {
    let path = Path::new("/w/a.rs");
    let (file, state) = read(b"x y x");
    let one = edit(Path::new(PATH), Some(&file), &state, "y", "z", false).unwrap();
    assert_eq!(
        one.answer(path),
        format!("The file /w/a.rs has been updated successfully.{CURRENT}")
    );
    let all = edit(Path::new(PATH), Some(&file), &state, "x", "z", true).unwrap();
    assert_eq!(
        all.answer(path),
        format!(
            "The file /w/a.rs has been updated. All occurrences were successfully replaced.{CURRENT}"
        )
    );
    let made = write(Path::new(PATH), None, &state, "new").unwrap();
    assert_eq!(
        made.answer(path),
        format!("File created successfully at: /w/a.rs{CURRENT}")
    );
    let over = write(Path::new(PATH), Some(&file), &state, "new").unwrap();
    assert_eq!(
        over.answer(path),
        format!("The file /w/a.rs has been updated successfully.{CURRENT}")
    );
    // Edit making a file with an empty old_string says it updated it, as
    // Claude Code's does.
    let made = edit(Path::new(PATH), None, &state, "", "new", false).unwrap();
    assert_eq!(
        made.answer(path),
        format!("The file /w/a.rs has been updated successfully.{CURRENT}")
    );
}

#[test]
fn an_edit_that_still_applies_to_a_changed_file_is_made_and_says_so() {
    // As Claude Code 2.1.310 does: the change was elsewhere in the file.
    let (_, state) = read(b"abc\n");
    let now = FileRead::new(b"abc\nmore\n".to_vec(), Version::new("2"));
    let made = edit(Path::new(PATH), Some(&now), &state, "b", "B", false).unwrap();
    assert_eq!(made.bytes, b"aBc\nmore\n");
    assert_eq!(made.expected, Some(Version::new("2")));
    assert_eq!(
        made.answer(Path::new("/w/a.rs")),
        "The file /w/a.rs has been updated successfully. (note: the file had been modified on disk since you last read it — the edit applied cleanly, but the file contains other changes not in your context. Read it before edits that depend on surrounding content.)"
    );
    // Write has no such leniency.
    assert_eq!(
        write(Path::new(PATH), Some(&now), &state, "x"),
        Err(EditError::Stale)
    );
    // Nor does an edit that's ambiguous in the file as it is now.
    let now = FileRead::new(b"abc\nb\n".to_vec(), Version::new("2"));
    assert_eq!(
        edit(Path::new(PATH), Some(&now), &state, "b", "B", false),
        Err(EditError::Stale)
    );
}

#[test]
fn an_empty_file_never_read_can_be_filled() {
    // Claude Code checks an empty old_string before whether it was read.
    let file = FileRead::new(Vec::new(), Version::new("1"));
    let edit = edit(
        Path::new(PATH),
        Some(&file),
        &ReadState::new(),
        "",
        "x",
        false,
    )
    .unwrap();
    assert_eq!(edit.bytes, b"x");
    assert_eq!(edit.expected, Some(Version::new("1")));
}

#[test]
fn edits_span_mixed_line_endings() {
    // Read hides \r, so the model can't write it: a match ignores it.
    assert_eq!(ok(b"a\nb\nc\r\nd\n", "c\nd", "X"), "a\nb\nX\n");
    assert_eq!(
        ok(b"a\r\nb\r\nc\nd\r\n", "b\nc\nd", "B\nC"),
        "a\r\nB\r\nC\r\n"
    );
    // Many matches, each mapped back over the \r it skipped.
    assert_eq!(
        edited(b"x\r\ny\nx\r\ny\n", "x\ny", "z", true).unwrap(),
        b"z\nz\n"
    );
    // A lone \r isn't an ending, and stays.
    assert_eq!(ok(b"a\rb\nc\n", "a\rb", "q"), "q\nc\n");
}

#[test]
fn line_endings_and_curly_quotes_together() {
    let file = "say “hi”\r\nnow\r\n";
    assert_eq!(
        ok(file.as_bytes(), "say \"hi\"\nnow", "say \"yo\"\nthen"),
        "say “yo”\r\nthen\r\n"
    );
}

fn config() -> ProptestConfig {
    ProptestConfig {
        failure_persistence: None,
        cases: 512,
        ..ProptestConfig::default()
    }
}

/// Text from a few characters that matter here: quotes, both kinds,
/// line endings, multibyte letters.
fn tricky() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec![
            "a", "b", "\n", "\r\n", "\r", "'", "\"", "‘", "’", "“", "”", "é", "日", " ",
        ]),
        0..40,
    )
    .prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn edits_never_panic_and_give_utf8(
        file in tricky(),
        old in tricky(),
        new in tricky(),
        all in any::<bool>(),
        bom in any::<bool>(),
    ) {
        let mut bytes = if bom { BOM.to_vec() } else { Vec::new() };
        bytes.extend_from_slice(file.as_bytes());
        if let Ok(after) = edited(&bytes, &old, &new, all) {
            prop_assert_eq!(after.starts_with(BOM), bom);
            prop_assert!(std::str::from_utf8(&after).is_ok());
        }
    }

    #[test]
    fn a_unique_plain_match_is_replaced_exactly(
        before in "[a-c\n]{0,20}",
        after in "[a-c\n]{0,20}",
        new in "[xyz\n]{1,10}",
    ) {
        let file = format!("{before}MARK{after}");
        let got = edited(file.as_bytes(), "MARK", &new, false).unwrap();
        prop_assert_eq!(String::from_utf8(got).unwrap(), format!("{before}{new}{after}"));
    }

    #[test]
    fn replacing_all_leaves_none(
        parts in prop::collection::vec("[ab\n]{0,5}", 1..8),
    ) {
        let file = parts.join("Q");
        let result = edited(file.as_bytes(), "Q", "R", true);
        if parts.len() == 1 {
            prop_assert!(matches!(result, Err(EditError::NotFound { .. })), "no matches: {result:?}");
        } else {
            let got = String::from_utf8(result.unwrap()).unwrap();
            prop_assert_eq!(got, parts.join("R"));
        }
    }
}
