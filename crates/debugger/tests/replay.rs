//! Replaying recordings, and moving back and forth through them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    let mut player = Player::new(Arc::clone(&book));
    player.go(book.end());
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

/// The live trace, with one of the story's chunks said again `times`
/// times after the last, as a long turn streams: written into `dir`.
fn long_run(dir: &Path, times: usize) -> PathBuf {
    let text = std::fs::read_to_string(claude_live()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    // Line 37 is a chunk of the second turn's story, and 38 its last.
    assert!(lines[36].contains("agent_message_chunk") && lines[37].contains("agent_message_chunk"));
    let mut out: Vec<&str> = lines[..38].to_vec();
    out.extend(std::iter::repeat_n(lines[36], times));
    out.extend(&lines[38..]);
    let path = dir.join("long.jsonl");
    std::fs::write(&path, out.join("\n") + "\n").unwrap();
    path
}

/// The core at `at`, replayed from the start with no copies.
fn replayed_at(book: &caf_debugger::steps::Book, at: u64) -> Value {
    let mut core = book.base.core();
    for step in book.range(0, at) {
        replay::take(&mut core, step.now, &step.event).expect("it reads");
    }
    core.inspect()
}

#[test]
fn a_long_run_moves_within_budget_from_copies_of_the_core() {
    let dir = std::env::temp_dir().join(format!("caf-debugger-long-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let book = replay::load(&long_run(&dir, 35_000)).expect("it loads");
    std::fs::remove_dir_all(&dir).unwrap();
    let end = book.end();
    assert!(end > 35_000);
    let spacing = book.spacing();

    // A replay of the whole run, for scale: a move takes a fraction of it
    // however busy the machine is. The time a move may take is held only
    // in a release build (CI runs this test so too), which the budget is
    // for.
    let steps = book.after(0);
    let started = Instant::now();
    let mut core = book.base.core();
    for step in &steps {
        replay::take(&mut core, step.now, &step.event).expect("it reads");
    }
    let whole = started.elapsed();
    let check = |to: u64, replayed: usize, took: Duration| {
        eprintln!(
            "moving to {to} replayed {replayed} steps in {took:?}, of {whole:?} for them all"
        );
        assert!(
            (replayed as u64) < spacing,
            "moving to {to} replayed {replayed} steps"
        );
        assert!(
            took < whole / 5,
            "moving to {to} took {took:?}, of {whole:?} for them all"
        );
        if !cfg!(debug_assertions) {
            assert!(took < BUDGET, "moving to {to} took {took:?}");
        }
    };
    // The first move, from a fresh core, starts from the last copy too.
    let mut player = Player::new(Arc::clone(&book));
    let started = Instant::now();
    let replayed = player.go(end);
    check(end, replayed, started.elapsed());
    // The worst: just before a copy, so the whole spacing less one is
    // replayed from the copy before.
    let worst = (end / spacing) * spacing - 1;
    assert_eq!(worst % spacing, spacing - 1);
    for to in [end - 10, worst, end - 1500, 2, end - 10] {
        let started = Instant::now();
        let replayed = player.go(to);
        check(to, replayed, started.elapsed());
    }

    // From a copy or not, the core is as a replay from the start makes it.
    for at in [999, 1000, 1001, 17_500, end - 10, end] {
        let mut player = Player::new(Arc::clone(&book));
        player.go(at);
        assert_eq!(player.state(), replayed_at(&book, at), "at {at}");
    }
    // And moving back and forth between copies too.
    let mut player = Player::new(Arc::clone(&book));
    for at in [30_001, 2500, 2499, 7000, 6999, 35_999] {
        player.go(at);
        assert_eq!(player.state(), replayed_at(&book, at), "at {at}");
    }
}

#[test]
fn replaying_the_live_trace_to_its_end_is_within_budget() {
    let book = replay::load(&claude_live()).expect("the trace loads");
    let started = Instant::now();
    let mut player = Player::new(Arc::clone(&book));
    player.go(book.end());
    let took = started.elapsed();
    assert!(took < BUDGET, "{took:?}");
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
fn chats_at(book: &Arc<caf_debugger::steps::Book>, at: u64) -> Vec<Chat> {
    let mut player = Player::new(Arc::clone(book));
    player.go(at);
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
    let end = book.end();
    let mut player = Player::new(Arc::clone(&book));
    let mut copies = Copies::default();

    // Forward, a step at a time from the start: the chat comes whole
    // once, when it's first there, and only as changes after.
    for at in 0..=end {
        player.go(at);
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
        player.go(at);
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
    player.go(end / 2);
    copies.take(player.past());
    player.go(end);
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
    let mut player = Player::new(Arc::clone(&book));
    let mut copies = Copies::default();
    copies.take(player.past());
    assert!(copies.open.is_empty(), "nothing open before the first step");
    player.go(1);
    copies.take(player.past());
    assert_eq!(copies.open, [ChatId::from_number(1)]);
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
    // An event this CAF doesn't know: a replay of it would be wrong from
    // there on.
    let text = std::fs::read_to_string(claude_live()).unwrap();
    let (first, rest) = text.split_once('\n').unwrap();
    let future = r#"{"now":{"wall":1,"mono":1},"event":{"type":"FromTheFuture","value":1}}"#;
    let refused = error(write("future.jsonl", &format!("{first}\n{future}\n{rest}")));
    assert!(refused.contains("line 2"), "{refused}");
    // The last line cut off partway, as a recording copied while CAF
    // wrote it ends: it replays up to there.
    let cut = write("cut.jsonl", &format!("{text}{{\"now\":{{\"wall\":1,"));
    assert_eq!(replay::load(&cut).expect("it loads").end(), 71);
    // But a line cut off with more after it isn't the end.
    let broken = format!("{first}\n{{\"now\":{{\"wall\":1,\n{rest}");
    assert!(error(write("broken.jsonl", &broken)).contains("line 2"));
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
    let mut player = Player::new(Arc::clone(&book));
    let mut copies = Copies::default();
    player.go(1);
    copies.take(player.past());
    assert_eq!(copies.pictures, 1);
    player.go(book.end());
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
