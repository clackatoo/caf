//! The conformance fixtures in `fixtures/apply/`: what every SDK's
//! `apply` and `ChatCopy` must turn into exactly the chats the core had,
//! and the changes they must refuse.
//!
//! Two kinds of test write them:
//! - every script the core's tests run, one fixture per chat that
//!   changed: the chat before its first change, then each numbered change
//!   and the chat after it, as the core's own record went. Named after the
//!   test and chat, such as `basics.a_turn_streams--chat-1000.json`; a
//!   test's second script adds `--2`, and so on;
//! - `Chat::apply`'s own tests, in `change/tests.rs`: one change to a
//!   chat of two turns, such as `change.tests.a_turn_moves_on_and_ends.json`,
//!   or one that doesn't fit, such as
//!   `change.tests.a_change_naming_what_the_chat_lacks_changes_nothing--words-for-no-item.json`.
//!
//! `CAF_BLESS=1 cargo test -p caf-core` writes them. Without `CAF_BLESS`,
//! a test fails if any is stale or missing, or if it names a test that's
//! gone, and [`every_fixture_is_followed_by_a_chat_copy`] runs them
//! through Rust's [`ChatCopy`]. A fixture no test writes any more, but
//! whose test is still there, is found only by regenerating them all,
//! which CI does: see [`BLESS`].
//!
//! Each file is one JSON document, one step to a line:
//!
//! ```text
//! {"picture":{"chat":…,"latest":999},"steps":[
//! {"change":{"no":1000,"change":…},"chat":…},
//! …
//! {"change":{"no":1004,"change":…},"mismatch":{"type":"NoItem","value":99}}
//! ]}
//! ```
//!
//! A step has exactly one of `chat`, the chat after the change, or
//! `mismatch`: the change doesn't fit, so `apply` gives that `Mismatch`
//! and leaves the chat as it was, and `ChatCopy` asks for a picture and
//! is left as it was too. A `mismatch` step is always the last, and its
//! number follows the one before, so it's never a gap.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::change::{Mismatch, Numbered};
use crate::chat::Chat;
use crate::ids::ChangeNo;
use crate::kinds::Seen;
use crate::viewer::{ChatCopy, NeedPicture};

/// One fixture, as it's read.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    /// The chat before its first change.
    picture: Picture,
    /// Each change, and what it made of the chat.
    steps: Vec<Step>,
}

/// A chat, whole, as in `ToViewer::Picture`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Picture {
    chat: Chat,
    latest: ChangeNo,
}

/// One change, and the chat after it or why it didn't fit.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    change: Numbered,
    chat: Option<Chat>,
    mismatch: Option<Mismatch>,
}

/// What a change made of a chat, as a fixture is written.
pub(crate) enum After<'a> {
    /// It fit: the chat after it.
    Chat(&'a Chat),
    /// It didn't fit, and the chat is as it was.
    Mismatch(&'a Mismatch),
}

thread_local! {
    /// How many scripts this test has written fixtures for so far.
    static SCRIPTS: Cell<usize> = const { Cell::new(0) };
}

/// The folder they're kept in.
fn folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/apply")
}

/// The test running on this thread, as its fixtures are named: its path,
/// without `core::tests::`, with dots.
pub(crate) fn test_name() -> String {
    let thread = std::thread::current();
    let name = thread
        .name()
        .filter(|name| *name != "main")
        .expect("a test runs on a thread named after it");
    let name = name.strip_prefix("core::tests::").unwrap_or(name);
    name.replace("::", ".")
}

/// What this test's next script's fixtures are named from: the test's
/// name, and `--2` and on for its second script and later.
pub(crate) fn next_script() -> String {
    let script = SCRIPTS.with(|count| {
        count.set(count.get() + 1);
        count.get()
    });
    match script {
        1 => test_name(),
        _ => format!("{}--{script}", test_name()),
    }
}

/// `value`, as compact JSON.
fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("it writes as JSON")
}

/// A fixture, as text: `chat` with changes to `latest`, then `steps`.
pub(crate) fn text<'a>(
    chat: &Chat,
    latest: ChangeNo,
    steps: impl IntoIterator<Item = (&'a Numbered, After<'a>)>,
) -> String {
    let picture = Picture {
        chat: chat.clone(),
        latest,
    };
    let steps: Vec<String> = (steps.into_iter())
        .map(|(change, after)| match after {
            After::Chat(chat) => format!(r#"{{"change":{},"chat":{}}}"#, json(change), json(chat)),
            After::Mismatch(mismatch) => format!(
                r#"{{"change":{},"mismatch":{}}}"#,
                json(change),
                json(mismatch)
            ),
        })
        .collect();
    format!(
        "{{\"picture\":{},\"steps\":[\n{}\n]}}\n",
        json(&picture),
        steps.join(",\n")
    )
}

/// Whether fixtures are being written rather than checked.
#[expect(
    clippy::disallowed_methods,
    reason = "tests read the environment: the core itself still doesn't"
)]
fn blessing() -> bool {
    std::env::var_os("CAF_BLESS").is_some()
}

/// What to run to bring the fixtures up to date. Removing them first
/// also removes any no test writes now: CI runs it, and fails if
/// anything in the folder changed.
const BLESS: &str = "rm -rf fixtures/apply && CAF_BLESS=1 cargo test -p caf-core";

/// Checks the fixture `name` holds `text`, or writes it under
/// `CAF_BLESS`.
#[expect(
    clippy::disallowed_methods,
    reason = "a test that writes and reads the fixtures: the core itself still does no I/O"
)]
pub(crate) fn check(name: &str, text: &str) {
    let folder = folder();
    let path = folder.join(name);
    let old = std::fs::read_to_string(&path).ok();
    if old.as_deref() == Some(text) {
        return;
    }
    if blessing() {
        std::fs::create_dir_all(&folder).expect("the fixtures' folder");
        // Written whole, then moved into place, so a test reading the
        // fixtures never sees half of one.
        let new = folder.join(format!(".{name}.new"));
        std::fs::write(&new, text).expect("a fixture is written");
        std::fs::rename(&new, &path).expect("a fixture is moved into place");
    } else if old.is_none() {
        panic!("fixtures/apply/{name} is missing: run `{BLESS}`");
    } else {
        panic!("fixtures/apply/{name} is stale: run `{BLESS}`");
    }
}

/// Checks a script's fixtures, `(chat id, text)` for each chat that
/// changed, named from `script` ([`next_script`]); and that it has no
/// others.
#[expect(
    clippy::disallowed_methods,
    reason = "a test that writes and reads the fixtures: the core itself still does no I/O"
)]
pub(crate) fn check_script(script: &str, chats: impl IntoIterator<Item = (u64, String)>) {
    let prefix = format!("{script}--chat-");
    let mut written = BTreeSet::new();
    for (chat, text) in chats {
        let name = format!("{prefix}{chat}.json");
        check(&name, &text);
        written.insert(name);
    }
    let folder = folder();
    for name in names(&folder) {
        if name.starts_with(&prefix) && !written.contains(&name) {
            if blessing() {
                std::fs::remove_file(folder.join(&name)).expect("an old fixture is removed");
            } else {
                panic!("fixtures/apply/{name} is no chat of this test's: run `{BLESS}`");
            }
        }
    }
}

/// The fixtures' file names.
#[expect(
    clippy::disallowed_methods,
    reason = "a test that reads the fixtures: the core itself still does no I/O"
)]
fn names(folder: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.expect("a directory entry").file_name())
        .filter_map(|name| name.into_string().ok())
        .filter(|name| name.ends_with(".json") && !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// Whether the test a fixture is named after still exists: a function of
/// its name in its module's file, under `src/core/tests/` or `src/`.
#[expect(
    clippy::disallowed_methods,
    reason = "a test that reads the tests' own source: the core itself still does no I/O"
)]
fn test_exists(fixture: &str) -> bool {
    let test = (fixture.strip_suffix(".json").unwrap_or(fixture).split("--"))
        .next()
        .unwrap_or_default();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (files, function) = match test.rsplit_once('.') {
        Some((module, function)) => {
            let file = format!("{}.rs", module.replace('.', "/"));
            (
                vec![src.join("core/tests").join(&file), src.join(file)],
                function,
            )
        }
        None => (vec![src.join("core/tests/mod.rs")], test),
    };
    let function = format!("fn {function}()");
    (files.iter())
        .any(|file| std::fs::read_to_string(file).is_ok_and(|text| text.contains(&function)))
}

/// Every fixture, read through Rust's [`ChatCopy`] change by change,
/// gives exactly the chat the core had after each change, and refuses
/// the change that doesn't fit; all at once, and again, it ends the
/// same. Between them, they hold every kind of change and of mismatch.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test that reads the fixtures: the core itself still does no I/O"
)]
fn every_fixture_is_followed_by_a_chat_copy() {
    let folder = folder();
    let names = names(&folder);
    let mut seen = Seen::default();
    for name in &names {
        if !test_exists(name) {
            if blessing() {
                std::fs::remove_file(folder.join(name)).expect("an old fixture is removed");
                continue;
            }
            panic!("fixtures/apply/{name} is no test's: run `{BLESS}`");
        }
        if blessing() {
            // Other tests may be rewriting their fixtures as this reads:
            // they're checked on the next run.
            continue;
        }
        let text = std::fs::read_to_string(folder.join(name)).expect("a fixture reads");
        let fixture: Fixture =
            serde_json::from_str(&text).unwrap_or_else(|error| panic!("{name}: {error}"));
        let Picture { chat, latest } = fixture.picture;
        let mut copy = ChatCopy::new(chat.clone(), latest);
        let last = fixture.steps.len().saturating_sub(1);
        let mut misfits = false;
        // The chat before this step: the picture's, then each step's.
        let mut previous = chat.clone();
        for (at, step) in fixture.steps.iter().enumerate() {
            seen.change(&step.change.change);
            let take = copy.take(std::slice::from_ref(&step.change));
            match (&step.chat, &step.mismatch) {
                (Some(after), None) => {
                    assert_eq!(take, Ok(()), "{name}, step {at}");
                    assert_eq!(copy.latest(), step.change.no, "{name}, step {at}");
                    assert_eq!(copy.chat(), after, "{name}, step {at}");
                    previous = after.clone();
                }
                (None, Some(mismatch)) => {
                    assert_eq!(at, last, "{name}: a mismatch is the last step");
                    assert_eq!(take, Err(NeedPicture), "{name}, step {at}");
                    assert_eq!(
                        copy.chat(),
                        &previous,
                        "{name}, step {at}: the copy is as it was"
                    );
                    let mut bare = previous.clone();
                    assert_eq!(bare.apply(&step.change.change).as_ref(), Err(mismatch));
                    assert_eq!(bare, previous, "{name}, step {at}: the chat is as it was");
                    assert_eq!(copy.latest().0 + 1, step.change.no.0, "{name}: not a gap");
                    seen.mark(mismatch);
                    misfits = true;
                }
                _ => panic!("{name}, step {at}: needs one of `chat` and `mismatch`"),
            }
        }
        let changes: Vec<Numbered> = (fixture.steps.iter())
            .map(|step| step.change.clone())
            .collect();
        let mut whole = ChatCopy::new(chat, latest);
        let want = if misfits { Err(NeedPicture) } else { Ok(()) };
        assert_eq!(whole.take(&changes), want, "{name}: all at once");
        assert_eq!(whole.take(&changes), want, "{name}: and again, as a repeat");
        assert_eq!(whole, copy, "{name}: all at once");
    }
    if blessing() {
        return;
    }
    assert!(!names.is_empty(), "fixtures/apply/ is empty: run `{BLESS}`");
    let missing: Vec<String> = [
        seen.missing::<crate::change::Change>(),
        seen.missing::<Mismatch>(),
    ]
    .into_iter()
    .flatten()
    .collect();
    assert!(missing.is_empty(), "the fixtures lack {missing:?}");
}
