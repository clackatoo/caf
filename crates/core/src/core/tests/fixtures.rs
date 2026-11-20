//! The conformance fixtures in `fixtures/apply/`: what every SDK's
//! `apply` and `ChatCopy` must turn into exactly the chats the core had.
//!
//! Every script the core's tests run writes one per chat, through
//! [`check`]: the chat before its first change, then each numbered
//! change and the chat after it, as the core's own record went. A file
//! is named after its test and chat, such as
//! `basics.a_turn_streams--chat-1000.json`; a test's second script adds
//! `--2`, and so on.
//!
//! `CAF_BLESS=1 cargo test -p caf-core` writes them. Without
//! `CAF_BLESS`, a test fails if any is stale, missing or extra.
//! [`every_fixture_is_followed_by_a_chat_copy`] runs them through Rust's
//! [`ChatCopy`].
//!
//! Each file is one JSON document, one step to a line:
//!
//! ```text
//! {"picture":{"chat":…,"latest":999},"steps":[
//! {"change":{"no":1000,"change":…},"chat":…},
//! …
//! ]}
//! ```

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::change::Numbered;
use crate::chat::{Chat, NoAppItems};
use crate::core::record::Applied;
use crate::ids::{ChangeNo, ChatId};
use crate::shape::{CHANGES, change_kind};
use crate::viewer::ChatCopy;

/// One fixture, as it's read.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    /// The chat before its first change.
    picture: Picture,
    /// Each change, and the chat after it.
    steps: Vec<Step>,
}

/// A chat, whole, as in `ToViewer::Picture`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Picture {
    chat: Chat,
    latest: ChangeNo,
}

/// One change, and the chat after it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    change: Numbered,
    chat: Chat,
}

thread_local! {
    /// How many scripts this test has checked so far.
    static SCRIPTS: Cell<usize> = const { Cell::new(0) };
}

/// The folder they're kept in.
fn folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/apply")
}

/// The test running on this thread, as its fixtures are named: its path
/// under `core::tests`, with dots.
fn test_name() -> String {
    let thread = std::thread::current();
    let name = thread
        .name()
        .filter(|name| *name != "main")
        .expect("a test runs on a thread named after it");
    let name = name.strip_prefix("core::tests::").unwrap_or(name);
    name.replace("::", ".")
}

/// `value`, as compact JSON.
fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("it writes as JSON")
}

/// The fixture for one chat's record, as text.
fn write(applied: &Applied<NoAppItems>) -> String {
    let picture = Picture {
        chat: applied.picture.clone(),
        latest: applied.latest,
    };
    let steps: Vec<String> = (applied.steps.iter())
        .map(|(change, chat)| format!(r#"{{"change":{},"chat":{}}}"#, json(change), json(chat)))
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

/// What to run to bring the fixtures up to date.
const BLESS: &str = "CAF_BLESS=1 cargo test -p caf-core";

/// Checks this test's fixtures for a script's chats, or writes them
/// under `CAF_BLESS`: one per chat that changed.
#[expect(
    clippy::disallowed_methods,
    reason = "a test that writes and reads the fixtures: the core itself still does no I/O"
)]
pub(super) fn check(applied: &BTreeMap<ChatId, Applied<NoAppItems>>) {
    let script = SCRIPTS.with(|count| {
        count.set(count.get() + 1);
        count.get()
    });
    let mut prefix = test_name();
    if script > 1 {
        prefix = format!("{prefix}--{script}");
    }
    let prefix = format!("{prefix}--chat-");
    let folder = folder();
    let bless = blessing();
    let mut written = BTreeSet::new();
    for (chat, applied) in applied {
        let name = format!("{prefix}{}.json", chat.0);
        let path = folder.join(&name);
        let text = write(applied);
        let old = std::fs::read_to_string(&path).ok();
        if old.as_deref() != Some(text.as_str()) {
            if bless {
                std::fs::create_dir_all(&folder).expect("the fixtures' folder");
                // Written whole, then moved into place, so a test reading
                // the fixtures never sees half of one.
                let new = folder.join(format!(".{name}.new"));
                std::fs::write(&new, &text).expect("a fixture is written");
                std::fs::rename(&new, &path).expect("a fixture is moved into place");
            } else if old.is_none() {
                panic!("fixtures/apply/{name} is missing: run `{BLESS}`");
            } else {
                panic!("fixtures/apply/{name} is stale: run `{BLESS}`");
            }
        }
        written.insert(name);
    }
    for name in names(&folder) {
        if name.starts_with(&prefix) && !written.contains(&name) {
            if bless {
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
/// its name in its module's file. Fixtures of a test that's gone are
/// extra.
#[expect(
    clippy::disallowed_methods,
    reason = "a test that reads the tests' own source: the core itself still does no I/O"
)]
fn test_exists(fixture: &str) -> bool {
    let test = fixture.split("--").next().unwrap_or_default();
    let Some((module, function)) = test.rsplit_once('.') else {
        return false;
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/core/tests")
        .join(format!("{}.rs", module.replace('.', "/")));
    std::fs::read_to_string(source).is_ok_and(|text| text.contains(&format!("fn {function}()")))
}

/// Every fixture, read through Rust's [`ChatCopy`] change by change,
/// gives exactly the chat the core had after each change; and all at
/// once, and again, gives the last. Between them, they hold every kind
/// of change.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test that reads the fixtures: the core itself still does no I/O"
)]
fn every_fixture_is_followed_by_a_chat_copy() {
    let folder = folder();
    let names = names(&folder);
    assert!(!names.is_empty(), "fixtures/apply/ is empty: run `{BLESS}`");
    let mut kinds = [false; CHANGES];
    for name in names {
        if !test_exists(&name) {
            if blessing() {
                std::fs::remove_file(folder.join(&name)).expect("an old fixture is removed");
                continue;
            }
            panic!("fixtures/apply/{name} is no test's: run `{BLESS}`");
        }
        let text = std::fs::read_to_string(folder.join(&name)).expect("a fixture reads");
        let fixture: Fixture = serde_json::from_str(&text).expect("a fixture parses");
        let Picture { chat, latest } = fixture.picture;
        let mut copy = ChatCopy::new(chat.clone(), latest);
        for (at, step) in fixture.steps.iter().enumerate() {
            kinds[change_kind(&step.change.change)] = true;
            copy.take(std::slice::from_ref(&step.change))
                .unwrap_or_else(|_| panic!("{name}, step {at}: the copy needs a picture"));
            assert_eq!(copy.latest(), step.change.no, "{name}, step {at}");
            assert_eq!(copy.chat(), &step.chat, "{name}, step {at}");
        }
        let changes: Vec<Numbered> = (fixture.steps.iter())
            .map(|step| step.change.clone())
            .collect();
        let mut whole = ChatCopy::new(chat, latest);
        whole.take(&changes).expect("all at once");
        whole.take(&changes).expect("and again, as a repeat");
        assert_eq!(whole, copy, "{name}: all at once");
    }
    assert_eq!(
        kinds, [true; CHANGES],
        "the fixtures hold every kind of change"
    );
}
