//! Tests of patching a file, and of the version a patched install is of.

use sha2::{Digest, Sha256};

use crate::install::{Edit, FilePatch, Patch, PatchError, Recipe};
use crate::profiles::tests::plain_recipe;

/// `bytes`' SHA-256, in hex.
fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The file the patches of these tests are written for.
const ORIGINAL: &str = "const steer = false;\nfunction prompt() { return queue(); }\n";

/// What `ORIGINAL` becomes with `edits()`.
const PATCHED: &str = "const steer = true;\nfunction prompt() { return steerOr(queue()); }\n";

/// The edits that make `PATCHED` of `ORIGINAL`.
fn edits() -> Vec<Edit> {
    vec![
        Edit::new("steer = false", "steer = true"),
        Edit::new("return queue()", "return steerOr(queue())"),
    ]
}

/// A patch of `ORIGINAL` by `edits`, tested to give `after`.
fn patch(edits: Vec<Edit>, after: &str) -> FilePatch {
    FilePatch::new(
        "dist/acp-agent.js",
        sha256(ORIGINAL.as_bytes()),
        sha256(after.as_bytes()),
        edits,
    )
}

#[test]
fn a_file_as_pinned_is_patched_as_tested() {
    let patched = patch(edits(), PATCHED).apply(ORIGINAL.as_bytes());
    assert_eq!(patched.as_deref(), Ok(PATCHED.as_bytes()));
}

#[test]
fn a_hash_in_capitals_still_matches() {
    let mut patch = patch(edits(), PATCHED);
    patch.before = patch.before.to_uppercase();
    patch.after = patch.after.to_uppercase();
    assert_eq!(
        patch.apply(ORIGINAL.as_bytes()).as_deref(),
        Ok(PATCHED.as_bytes())
    );
}

#[test]
fn a_file_other_than_the_one_pinned_is_refused() {
    // A newer upstream that still holds every edit's text: the edits
    // would apply, but nobody tested them on it.
    let changed = format!("{ORIGINAL}// one line more\n");
    let refused = patch(edits(), PATCHED).apply(changed.as_bytes());
    assert_eq!(refused, Err(PatchError::NotAsPinned));
}

#[test]
fn an_edit_whose_text_isnt_there_exactly_once_is_refused() {
    // Each is pinned to the original, so only the edits can be wrong.
    let missing = vec![
        Edit::new("steer = false", "steer = true"),
        Edit::new("nowhere", "x"),
    ];
    assert_eq!(
        patch(missing, PATCHED).apply(ORIGINAL.as_bytes()),
        Err(PatchError::Found { edit: 1, times: 0 })
    );
    let twice = vec![Edit::new("()", "(x)")];
    assert_eq!(
        patch(twice, PATCHED).apply(ORIGINAL.as_bytes()),
        Err(PatchError::Found { edit: 0, times: 2 })
    );
    // Text that overlaps itself counts each time it starts: "aa" is
    // twice in "aaa", and an edit mustn't pick one.
    let overlapping = FilePatch::new(
        "a",
        sha256(b"aaa"),
        sha256(b"ba"),
        vec![Edit::new("aa", "b")],
    );
    assert_eq!(
        overlapping.apply(b"aaa"),
        Err(PatchError::Found { edit: 0, times: 2 })
    );
}

#[test]
fn each_edit_finds_its_text_as_the_ones_before_left_it() {
    // The second edit's text exists only once the first is made, and the
    // first's text is gone by the time the second looks.
    let chained = vec![
        Edit::new("steer = false", "steer = later"),
        Edit::new("later", "true"),
        Edit::new("return queue()", "return steerOr(queue())"),
    ];
    assert_eq!(
        patch(chained, PATCHED)
            .apply(ORIGINAL.as_bytes())
            .as_deref(),
        Ok(PATCHED.as_bytes())
    );
    let gone = vec![
        Edit::new("steer = false", "steer = true"),
        Edit::new("false", "no"),
    ];
    assert_eq!(
        patch(gone, PATCHED).apply(ORIGINAL.as_bytes()),
        Err(PatchError::Found { edit: 1, times: 0 })
    );
}

#[test]
fn a_result_other_than_the_one_tested_is_refused() {
    let wrong = vec![Edit::new("steer = false", "steer = maybe")];
    assert_eq!(
        patch(wrong, PATCHED).apply(ORIGINAL.as_bytes()),
        Err(PatchError::NotAsTested)
    );
}

#[test]
fn a_patched_install_is_of_a_version_of_its_own() {
    let mut recipe: Recipe = plain_recipe();
    assert_eq!(recipe.installed_version(), "1.0.0");
    recipe.patch = Some(Patch::new(1, vec![patch(edits(), PATCHED)]));
    assert_eq!(recipe.installed_version(), "1.0.0+caf.1");
    recipe.patch = Some(Patch::new(12, Vec::new()));
    assert_eq!(recipe.installed_version(), "1.0.0+caf.12");
    assert_eq!(recipe.version, "1.0.0", "the upstream version stays");
}

#[test]
fn an_edit_with_no_text_to_find_is_refused() {
    // Empty text is everywhere: in an empty file, exactly once.
    for original in [&b""[..], ORIGINAL.as_bytes()] {
        let empty = FilePatch::new(
            "a",
            sha256(original),
            sha256(b"x"),
            vec![Edit::new("", "x")],
        );
        assert_eq!(
            empty.apply(original),
            Err(PatchError::NothingToFind { edit: 0 })
        );
    }
}

#[test]
fn a_file_is_patched_as_bytes_whatever_they_hold() {
    // Not UTF-8, around the edit: kept byte for byte.
    let original = b"\xff\xfe head \x80 steer = false \xc3\x28 tail\x00";
    let patched = b"\xff\xfe head \x80 steer = true \xc3\x28 tail\x00";
    let patch = FilePatch::new(
        "a",
        sha256(original),
        sha256(patched),
        vec![Edit::new("steer = false", "steer = true")],
    );
    assert_eq!(patch.apply(original).as_deref(), Ok(&patched[..]));
    // CRLF line endings: a line break is found only as the file has it,
    // and the rest keep theirs.
    let original = b"let a = 1;\r\nlet b = 2;\r\n";
    let patched = b"let a = 1;\r\nlet b = 3;\r\n";
    let crlf = FilePatch::new(
        "a",
        sha256(original),
        sha256(patched),
        vec![Edit::new("2;\r\n", "3;\r\n")],
    );
    assert_eq!(crlf.apply(original).as_deref(), Ok(&patched[..]));
    let lf = FilePatch::new(
        "a",
        sha256(original),
        sha256(patched),
        vec![Edit::new("1;\nlet", "1;\nlet")],
    );
    assert_eq!(
        lf.apply(original),
        Err(PatchError::Found { edit: 0, times: 0 })
    );
}
