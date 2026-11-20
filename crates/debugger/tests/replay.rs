//! Replaying recordings, and moving back and forth through them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use caf_core::viewer::{ChatCopy, ToViewer};
use caf_core::{Chat, ChatId};
use caf_debugger::replay::{self, BUDGET, Player};
use serde_json::{Value, json};

/// A real run of claude-agent-acp 0.98.0, recorded by the tokio kit: the
/// same file caf-core's own trace test replays.
fn claude_live() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/src/core/tests/traces/claude-0.98.0-live.jsonl")
}

/// Whether `value` has everything `pattern` has: each key of an object,
/// and each item of an array, which must be as long.
fn has(value: &Value, pattern: &Value) -> bool {
    match (value, pattern) {
        (Value::Object(value), Value::Object(pattern)) => (pattern.iter())
            .all(|(key, pattern)| value.get(key).is_some_and(|value| has(value, pattern))),
        (Value::Array(value), Value::Array(pattern)) => {
            value.len() == pattern.len()
                && value
                    .iter()
                    .zip(pattern)
                    .all(|(value, pattern)| has(value, pattern))
        }
        _ => value == pattern,
    }
}

#[test]
fn the_live_claude_trace_replays_to_the_state_the_core_tests_expect() {
    let book = replay::load(&claude_live()).expect("the trace loads");
    let steps = book.after(0);
    assert_eq!(steps.len() as u64, book.end());
    assert_eq!(book.end(), 71, "every line after the header is a step");
    let mut player = Player::new(Arc::clone(&book.base));
    player.go(book.end(), &steps);
    let state = player.state();
    let chat = &state["chats"][0];
    assert!(
        has(
            chat,
            &json!({
                "id": 1,
                "state": { "type": "Lost", "value": { "reason": { "type": "ShutDown" } } },
                "turns": [
                    { "starter": { "type": "User", "value": { "parts": [{ "type": "Text", "value": "Reply with exactly the word pong." }] } }, "life": { "type": "Finished" }, "items": [{ "kind": { "type": "Words", "value": "pong" } }] },
                    { "life": { "type": "Stopped" }, "items": [{ "kind": { "type": "Words", "value": "**The Keeper of Halden Rock**\n\nFor thirty-one years, Ed" } }] }
                ],
                "questions": []
            })
        ),
        "{chat:#}"
    );
    assert!(chat["usage"]["used"].as_u64().is_some_and(|used| used > 0));
    assert_eq!(state["procs"], json!({}), "every adapter stopped");
    // The directives are the replay core's: the stop went out as a
    // cancel, once.
    let writes: Vec<String> = (steps.iter())
        .flat_map(|step| serde_json::from_str::<Vec<Value>>(step.directives.get()).unwrap())
        .filter(|directive| directive["type"] == "Agent" && directive["value"]["type"] == "Write")
        .map(|write| {
            let bytes: Vec<u8> =
                serde_json::from_value(write["value"]["value"]["bytes"].clone()).expect("bytes");
            String::from_utf8(bytes).expect("UTF-8")
        })
        .collect();
    assert_eq!(
        (writes.iter())
            .filter(|write| write.contains(r#""method":"session/cancel""#))
            .count(),
        1,
        "{writes:#?}"
    );
}

#[test]
fn replaying_the_live_trace_to_its_end_is_within_budget() {
    let book = replay::load(&claude_live()).expect("the trace loads");
    let steps = book.after(0);
    let started = Instant::now();
    let mut player = Player::new(Arc::clone(&book.base));
    player.go(book.end(), &steps);
    let took = started.elapsed();
    eprintln!(
        "replaying {} steps of the live trace took {took:?}, against a budget of {BUDGET:?}",
        steps.len()
    );
    // Debug builds are far slower than the release builds the budget is
    // for, so only a release build is held to it.
    if !cfg!(debug_assertions) {
        assert!(took < BUDGET, "{took:?}");
    }
}

/// The app's copies of the chats, fed only by Past messages, as the app
/// feeds them.
#[derive(Default)]
struct Copies {
    open: Vec<ChatId>,
    chats: Vec<(ChatId, ChatCopy)>,
    pictures: usize,
    changes: usize,
}

impl Copies {
    fn take(&mut self, past: Vec<ToViewer>) {
        let mut past = past.into_iter();
        let Some(ToViewer::Chats(open)) = past.next() else {
            panic!("a Past starts with the open chats");
        };
        self.open = open.iter().map(|chat| chat.id).collect();
        let listed = self.open.clone();
        self.chats.retain(|(each, _)| listed.contains(each));
        for message in past {
            match message {
                ToViewer::Picture { chat, latest } => {
                    self.pictures += 1;
                    let id = chat.id;
                    self.chats.retain(|(each, _)| *each != id);
                    self.chats.push((id, ChatCopy::new(chat, latest)));
                }
                ToViewer::Changes { chat, changes } => {
                    self.changes += 1;
                    let (_, copy) = (self.chats.iter_mut())
                        .find(|(each, _)| *each == chat)
                        .expect("Changes only for a chat the app has");
                    copy.take(&changes).expect("the changes follow on");
                }
                other => panic!("not a chat's: {other:?}"),
            }
        }
    }

    fn chats(&self) -> Vec<&Chat> {
        self.chats.iter().map(|(_, copy)| copy.chat()).collect()
    }
}

/// The chats a fresh core has at `at`.
fn chats_at(book: &caf_debugger::steps::Book, at: u64) -> Vec<Chat> {
    let mut player = Player::new(Arc::clone(&book.base));
    player.go(at, &book.after(0));
    (player.past().into_iter())
        .filter_map(|message| match message {
            ToViewer::Picture { chat, .. } => Some(chat),
            _ => None,
        })
        .collect()
}

#[test]
fn seeking_forward_gives_changes_and_back_gives_pictures() {
    let book = replay::load(&claude_live()).expect("the trace loads");
    let steps = book.after(0);
    let end = book.end();
    let mut player = Player::new(Arc::clone(&book.base));
    let mut copies = Copies::default();

    // Forward, a step at a time from the start: the chat comes whole
    // once, when it's first there, and only as changes after.
    for at in 0..=end {
        player.go(at, &steps);
        copies.take(player.past());
        let expected = chats_at(&book, at);
        assert_eq!(
            copies.chats(),
            expected.iter().collect::<Vec<_>>(),
            "at {at}"
        );
    }
    assert_eq!(copies.pictures, 1, "one chat, sent whole once");
    assert!(copies.changes > 10, "{} changes", copies.changes);

    // Back: every chat whole again, and no changes.
    for at in [end / 2, 3, 0] {
        let pictures = copies.pictures;
        let changes = copies.changes;
        player.go(at, &steps);
        copies.take(player.past());
        let expected = chats_at(&book, at);
        assert_eq!(
            copies.chats(),
            expected.iter().collect::<Vec<_>>(),
            "at {at}"
        );
        assert_eq!(copies.pictures - pictures, expected.len(), "at {at}");
        assert_eq!(copies.changes, changes, "at {at}");
    }

    // Forward again in one jump: changes only.
    let pictures = copies.pictures;
    player.go(end / 2, &steps);
    copies.take(player.past());
    player.go(end, &steps);
    copies.take(player.past());
    assert_eq!(
        copies.pictures,
        pictures + 1,
        "the chat was new at the first jump"
    );
    assert_eq!(
        copies.chats(),
        chats_at(&book, end).iter().collect::<Vec<_>>()
    );
}

#[test]
fn a_chat_not_yet_open_isnt_in_the_list() {
    let book = replay::load(&claude_live()).expect("the trace loads");
    let steps = book.after(0);
    let mut player = Player::new(Arc::clone(&book.base));
    let mut copies = Copies::default();
    copies.take(player.past());
    assert!(copies.open.is_empty(), "nothing open before the first step");
    player.go(1, &steps);
    copies.take(player.past());
    assert_eq!(copies.open, [ChatId(1)]);
}

#[test]
fn a_recording_that_isnt_one_is_refused() {
    let dir = std::env::temp_dir().join(format!("caf-debugger-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, text: &str| {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    let error = |path: PathBuf| replay::load(&path).err().expect("refused").to_string();
    assert!(error(write("empty.jsonl", "")).contains("empty"));
    assert!(error(write("v1.jsonl", r#"{"caf_recording":1}"#)).contains("version 1"));
    assert!(error(write("junk.jsonl", "junk")).contains("first line"));
    let header = std::fs::read_to_string(claude_live()).unwrap();
    let header = header.lines().next().unwrap();
    assert!(error(write("line.jsonl", &format!("{header}\nnot a step\n"))).contains("line 2"));
    // A recording cut short stops where it says so.
    let stopped = write(
        "stopped.jsonl",
        &format!("{header}\n{{\"stopped\":\"behind\"}}\n"),
    );
    assert_eq!(replay::load(&stopped).expect("it loads").end(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn seeking_further_forward_than_the_core_keeps_changes_gives_a_picture() {
    // The live trace, with a core that keeps only two changes.
    let text = std::fs::read_to_string(claude_live()).unwrap();
    let (header, rest) = text.split_once('\n').unwrap();
    let mut header: Value = serde_json::from_str(header).unwrap();
    header["host"]["limits"]["kept_changes"] = json!(2);
    let dir = std::env::temp_dir().join(format!("caf-debugger-kept-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kept.jsonl");
    std::fs::write(&path, format!("{header}\n{rest}")).unwrap();
    let book = replay::load(&path).expect("it loads");
    std::fs::remove_dir_all(&dir).unwrap();
    let steps = book.after(0);
    let mut player = Player::new(Arc::clone(&book.base));
    let mut copies = Copies::default();
    player.go(1, &steps);
    copies.take(player.past());
    assert_eq!(copies.pictures, 1);
    player.go(book.end(), &steps);
    copies.take(player.past());
    assert_eq!(
        (copies.pictures, copies.changes),
        (2, 0),
        "whole again, being behind"
    );
    assert_eq!(
        copies.chats(),
        chats_at(&book, book.end()).iter().collect::<Vec<_>>()
    );
}
