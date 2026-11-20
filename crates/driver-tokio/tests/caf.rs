//! The whole kit, end to end, against the fake adapter.

mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;

use caf_core::agent::{AgentSpec, EnvChange, McpServer, SpawnCommand};
use caf_core::change::Change;
use caf_core::chat::{ChatState, FailureReason, ItemKind, Lost, Turn, TurnLife, UserMessage};
use caf_core::directive::PermissionRequest;
use caf_core::event::{Event, PolicyAnswer};
use caf_core::host::Host;
use caf_core::profiles::ProfileChoice;
use caf_core::question::{Answer, OptionId, QuestionKind, SlowChoice};
use caf_core::{Chat, ChatId, Core, Now, OpenChat};
use caf_driver_tokio::{Caf, ChatHandle, Config, Error, Policy, Update, Watch};
use serde_json::{Value, json};

/// Long enough for anything a test waits for, on a loaded machine.
const PATIENCE: Duration = Duration::from_secs(60);

/// A folder of its own for each test, under `TMPDIR`.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(1000);
        let dir = std::env::temp_dir().join(format!(
            "caf-wire-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Writes a script of these steps, and returns its path.
    fn script(&self, name: &str, steps: &[Value]) -> PathBuf {
        let path = self.path(name);
        let text: String = steps.iter().map(|step| format!("{step}\n")).collect();
        std::fs::write(&path, text).unwrap();
        path
    }

    /// The fake's record, one JSON value a line.
    fn record(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.path("record.jsonl")).unwrap_or_default();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The fake, as agent `fake`, running `script` with its record in `dir`.
fn fake(dir: &Dir, script: &Path) -> AgentSpec {
    let mut command = SpawnCommand::new(caf_fake_agent::binary());
    command.env = vec![
        EnvChange::Set(caf_fake_agent::SCRIPT_VAR.into(), script.into()),
        EnvChange::Set(
            caf_fake_agent::RECORD_VAR.into(),
            dir.path("record.jsonl").into(),
        ),
    ];
    AgentSpec::new("fake", command, ProfileChoice::Cautious)
}

/// The fake started by a shell that runs it as it is told: for tests
/// that need its pid, or a different script the second time.
fn fake_by_shell(dir: &Dir, shell: &str) -> AgentSpec {
    let mut command = SpawnCommand::new("/bin/sh");
    command.args = vec!["-c".into(), shell.into()];
    command.env = vec![
        EnvChange::Set("FAKE".into(), caf_fake_agent::binary().into()),
        EnvChange::Set("DIR".into(), dir.0.clone().into()),
    ];
    AgentSpec::new("fake", command, ProfileChoice::Cautious)
}

/// A host for tests: an adapter that has gone is waited for briefly.
fn host() -> Host {
    let mut host = Host::new("caf-test", "1.0.0");
    host.timers.gone = Duration::from_millis(300);
    host.timers.kill_after = Duration::from_millis(500);
    host
}

/// Refuses whatever the script doesn't expect, as an adapter does.
fn fallback() -> Value {
    json!({"on": {"fallback": true, "do": [{"respond": {"error": {"code": -32601, "message": "Method not found"}}}]}})
}

/// Starts as an adapter does, and opens one session, `session`.
fn handshake(session: &str) -> Vec<Value> {
    vec![
        fallback(),
        json!({"wait": {"method": "initialize"}}),
        json!({"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}, "agentInfo": {"name": "fake", "version": "1.0.0"}}}}),
        json!({"wait": {"method": "session/new"}}),
        json!({"respond": {"result": {"sessionId": session}}}),
    ]
}

fn say(session: &str, text: &str) -> Value {
    json!({"say": {"session": session, "text": text}})
}

fn prompt() -> Value {
    json!({"wait": {"method": "session/prompt", "as": "prompt"}})
}

fn end_turn() -> Value {
    json!({"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}})
}

/// `steps` after `handshake("s1")`.
fn after_handshake(steps: &[Value]) -> Vec<Value> {
    let mut script = handshake("s1");
    script.extend_from_slice(steps);
    script
}

/// A fresh picture of the chat.
async fn picture(chat: &ChatHandle) -> Chat {
    match tokio::time::timeout(PATIENCE, chat.watch().next()).await {
        Ok(Some(Update::Picture(picture))) => picture,
        other => panic!("a new watch starts with a picture, not {other:?}"),
    }
}

/// Waits, with `watch`, until `done` is true of the chat, and returns it.
async fn until(
    chat: &ChatHandle,
    watch: &mut Watch,
    what: &str,
    done: impl Fn(&Chat) -> bool,
) -> Chat {
    let waited = tokio::time::timeout(PATIENCE, async {
        loop {
            let now = picture(chat).await;
            if done(&now) {
                return now;
            }
            assert!(
                watch.next().await.is_some(),
                "the watch ended waiting for {what}: {now:#?}"
            );
        }
    })
    .await;
    match waited {
        Ok(chat) => chat,
        Err(_) => panic!("gave up waiting for {what}: {:#?}", picture(chat).await),
    }
}

/// The words of a turn, run together.
fn words(turn: &Turn) -> String {
    (turn.items.iter())
        .filter_map(|item| match &item.kind {
            ItemKind::Words(words) => Some(words.as_str()),
            _ => None,
        })
        .collect()
}

fn last_life(chat: &Chat) -> Option<&TurnLife> {
    chat.turns.last().map(|turn| &turn.life)
}

fn finished(chat: &Chat) -> bool {
    last_life(chat) == Some(&TurnLife::Finished)
}

/// Starts CAF with `config`.
fn start(config: Config) -> (Caf, caf_driver_tokio::Running) {
    Caf::start(config)
}

/// Shuts CAF down and waits for its task to end.
async fn shut_down(caf: &Caf, running: caf_driver_tokio::Running) {
    let done = tokio::time::timeout(PATIENCE, async {
        caf.shutdown().await;
        running.await;
    })
    .await;
    assert!(done.is_ok(), "shutting down took too long");
}

/// Whether process `pid` has gone, reaped or not.
fn gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        // The state follows the name, which is in brackets.
        Ok(stat) => stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
    }
}

/// Waits until process `pid` has gone.
async fn until_gone(pid: u32) {
    let waited = tokio::time::timeout(PATIENCE, async {
        while !gone(pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "process {pid} is still there");
}

/// Reads a pid file, waiting for it to be written.
async fn read_pid(path: &Path) -> u32 {
    let waited = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    waited.unwrap_or_else(|_| panic!("no pid in {}", path.display()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_watch_send_and_the_words_stream_in() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &after_handshake(&[
        prompt(),
        say("s1", "Hello "),
        json!({"sleep": 50}),
        say("s1", "there"),
        json!({"update": {"session": "s1", "update": {"sessionUpdate": "usage_update", "used": 1200, "size": 200_000}}}),
        end_turn(),
    ]));
    let (caf, running) = start(Config::new(host(), vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    let Some(Update::Picture(first)) = watch.next().await else {
        panic!("a picture first")
    };
    assert_eq!(first.id, chat.id);
    assert!(first.turns.is_empty());
    let turn = chat.send(UserMessage::text("Say hello")).await.unwrap();

    // The words come in as changes, before the turn finishes.
    let mut heard = String::new();
    let waited = tokio::time::timeout(PATIENCE, async {
        loop {
            match watch.next().await {
                Some(Update::Changes(changes)) => {
                    for numbered in changes {
                        match numbered.change {
                            Change::ItemAdded { item, .. } => {
                                if let ItemKind::Words(words) = item.kind {
                                    heard.push_str(&words);
                                }
                            }
                            Change::Appended { text, .. } => heard.push_str(&text),
                            Change::TurnLife {
                                turn: done,
                                life: TurnLife::Finished,
                                ..
                            } if done == turn => return,
                            _ => {}
                        }
                    }
                }
                Some(Update::Picture(_)) => panic!("a watch that keeps up gets changes"),
                other => panic!("the watch ended: {other:?}"),
            }
        }
    })
    .await;
    assert!(waited.is_ok(), "the turn never finished; heard {heard:?}");
    assert_eq!(heard, "Hello there");

    let now = picture(&chat).await;
    assert_eq!(now.turns.len(), 1);
    assert_eq!(now.turns[0].id, turn);
    assert_eq!(words(&now.turns[0]), "Hello there");
    assert_eq!(now.usage.as_ref().map(|usage| usage.used), Some(1200));
    assert!(
        matches!(now.state, ChatState::Open { .. }),
        "{:?}",
        now.state
    );

    shut_down(&caf, running).await;
    // The watch hears how shutting down left the chat, then ends.
    match watch.next().await {
        Some(Update::Picture(last)) => assert!(
            matches!(
                last.state,
                ChatState::Lost {
                    reason: Lost::ShutDown
                }
            ),
            "{:?}",
            last.state
        ),
        Some(Update::Changes(changes)) => assert!(
            changes
                .iter()
                .any(|numbered| matches!(numbered.change, Change::State(ChatState::Lost { .. })))
        ),
        other => panic!("expected how the chat was left, not {other:?}"),
    }
    assert!(watch.next().await.is_none());
    assert_eq!(
        chat.send(UserMessage::text("Anyone?")).await,
        Err(Error::Gone)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_permission_request_is_asked_and_answered() {
    let dir = Dir::new();
    let ask = json!({"request": {"method": "session/request_permission", "as": "ask", "params": {
        "sessionId": "s1",
        "toolCall": {"toolCallId": "t1", "title": "Run ls"},
        "options": [
            {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
            {"optionId": "reject", "name": "Reject", "kind": "reject_once"}
        ]
    }}});
    let script = dir.script("script.jsonl", &after_handshake(&[
        prompt(),
        ask,
        json!({"await": {"to": "ask", "expect": {"result": {"outcome": {"outcome": "selected", "optionId": "allow"}}}, "timeout": 60_000}}),
        say("s1", "Ran it."),
        end_turn(),
    ]));
    // AlwaysAsk is the default policy.
    let (caf, running) = start(Config::new(host(), vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("List the files"))
        .await
        .unwrap();

    let asked = until(&chat, &mut watch, "the question", |chat| {
        !chat.questions.is_empty()
    })
    .await;
    let question = &asked.questions[0];
    let QuestionKind::Permission(permission) = &question.kind else {
        panic!("{question:?}")
    };
    assert_eq!(permission.title, "Run ls");
    assert_eq!(permission.options.len(), 2);
    let wrong = chat
        .answer(question.id, Answer::Permission(OptionId("nope".into())))
        .await;
    assert!(matches!(wrong, Err(Error::Refused(_))), "{wrong:?}");
    chat.answer(question.id, Answer::Permission(OptionId("allow".into())))
        .await
        .unwrap();
    // Answered once only.
    let again = chat
        .answer(question.id, Answer::Permission(OptionId("allow".into())))
        .await;
    assert!(matches!(again, Err(Error::Refused(_))), "{again:?}");

    let done = until(&chat, &mut watch, "the turn to finish", finished).await;
    assert!(done.questions.is_empty());
    assert_eq!(words(&done.turns[0]), "Ran it.");
    shut_down(&caf, running).await;
    assert!(
        !dir.record().iter().any(|line| line.get("failed").is_some()),
        "{:#?}",
        dir.record()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_mid_turn() {
    let dir = Dir::new();
    let script = dir.script(
        "script.jsonl",
        &after_handshake(&[
            prompt(),
            say("s1", "one "),
            json!({"wait": {"method": "session/cancel", "match": {"params": {"sessionId": "s1"}}}}),
            json!({"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}}),
        ]),
    );
    let (caf, running) = start(Config::new(host(), vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    assert!(
        matches!(chat.stop().await, Err(Error::Refused(_))),
        "nothing to stop yet"
    );
    chat.send(UserMessage::text("Count")).await.unwrap();
    until(&chat, &mut watch, "the first words", |chat| {
        chat.turns.first().is_some_and(|turn| words(turn) == "one ")
    })
    .await;
    chat.stop().await.unwrap();
    let stopped = until(&chat, &mut watch, "the turn to stop", |chat| {
        last_life(chat) == Some(&TurnLife::Stopped)
    })
    .await;
    assert_eq!(words(&stopped.turns[0]), "one ");
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adapter_dying_mid_turn_is_started_again_by_the_next_message() {
    let dir = Dir::new();
    dir.script(
        "first.jsonl",
        &after_handshake(&[prompt(), say("s1", "half"), json!({"exit": 1})]),
    );
    let mut second = vec![
        fallback(),
        json!({"wait": {"method": "initialize"}}),
        json!({"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}, "agentInfo": {"name": "fake", "version": "1.0.0"}}}}),
        json!({"wait": {"method": "session/load", "match": {"params": {"sessionId": "s1"}}}}),
        json!({"respond": {"result": {}}}),
    ];
    second.extend([prompt(), say("s1", "back"), end_turn()]);
    dir.script("second.jsonl", &second);
    let shell = r#"if [ -e "$DIR/started" ]; then script=second; else : > "$DIR/started"; script=first; fi
exec "$FAKE" "$DIR/$script.jsonl" --record "$DIR/record.jsonl""#;
    let (caf, running) = start(Config::new(host(), vec![fake_by_shell(&dir, shell)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("First")).await.unwrap();
    let failed = until(&chat, &mut watch, "the turn to fail", |chat| {
        matches!(last_life(chat), Some(TurnLife::Failed(_)))
    })
    .await;
    let Some(TurnLife::Failed(failure)) = last_life(&failed) else {
        unreachable!()
    };
    assert_eq!(failure.reason, FailureReason::AgentStopped);
    assert_eq!(words(&failed.turns[0]), "half");
    assert!(
        matches!(failed.state, ChatState::Lost { .. }),
        "{:?}",
        failed.state
    );

    chat.send(UserMessage::text("Again")).await.unwrap();
    let done = until(&chat, &mut watch, "the second turn to finish", finished).await;
    assert_eq!(words(done.turns.last().unwrap()), "back");
    assert!(
        matches!(done.state, ChatState::Open { .. }),
        "{:?}",
        done.state
    );
    shut_down(&caf, running).await;
    let started = dir
        .record()
        .iter()
        .filter(|line| line.get("started").is_some())
        .count();
    assert_eq!(started, 2, "{:#?}", dir.record());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_adapter_is_asked_about_and_waited_for() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &[
        fallback(),
        json!({"wait": {"method": "initialize"}}),
        json!({"sleep": 1500}),
        json!({"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {}, "agentInfo": {"name": "fake", "version": "1.0.0"}}}}),
        json!({"wait": {"method": "session/new"}}),
        json!({"respond": {"result": {"sessionId": "s1"}}}),
        prompt(),
        say("s1", "Worth the wait."),
        end_turn(),
    ]);
    let mut host = host();
    host.timers.initialize = Duration::from_millis(300);
    let (caf, running) = start(Config::new(host, vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hello?")).await.unwrap();

    let asked = until(&chat, &mut watch, "the slow question", |chat| {
        !chat.questions.is_empty()
    })
    .await;
    let QuestionKind::SlowToOpen(slow) = &asked.questions[0].kind else {
        panic!("{:?}", asked.questions)
    };
    assert!(!slow.adapter_answered);
    assert_eq!(asked.turns[0].life, TurnLife::Waiting);
    // Keep waiting, as often as asked.
    let done = tokio::time::timeout(PATIENCE, async {
        loop {
            let now = picture(&chat).await;
            if finished(&now) {
                return now;
            }
            for question in &now.questions {
                let _ = chat
                    .answer(question.id, Answer::SlowToOpen(SlowChoice::KeepWaiting))
                    .await;
            }
            watch.next().await.unwrap();
        }
    })
    .await
    .expect("the turn finishes");
    assert_eq!(words(&done.turns[0]), "Worth the wait.");
    let starts = dir
        .record()
        .iter()
        .filter(|line| line.get("started").is_some())
        .count();
    assert_eq!(starts, 1, "waiting doesn't restart it");
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_chats_share_one_adapter() {
    let dir = Dir::new();
    let (one, two) = (dir.path("one"), dir.path("two"));
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let new_in = |folder: &Path, session: &str| {
        [
            json!({"wait": {"method": "session/new", "match": {"params": {"cwd": folder}}}}),
            json!({"respond": {"result": {"sessionId": session}}}),
        ]
    };
    let prompt_to = |session: &str, text: &str| {
        [
            json!({"wait": {"method": "session/prompt", "as": "prompt", "match": {"params": {"sessionId": session}}}}),
            say(session, text),
            end_turn(),
        ]
    };
    let mut steps = vec![
        fallback(),
        json!({"wait": {"method": "initialize"}}),
        json!({"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {}, "agentInfo": {"name": "fake", "version": "1.0.0"}}}}),
    ];
    steps.extend(new_in(&one, "s1"));
    steps.extend(new_in(&two, "s2"));
    steps.extend(prompt_to("s1", "for one"));
    steps.extend(prompt_to("s2", "for two"));
    let script = dir.script("script.jsonl", &steps);
    let (caf, running) = start(Config::new(host(), vec![fake(&dir, &script)]));
    let first = caf.open_chat(OpenChat::new("fake", &one)).await.unwrap();
    let second = caf.open_chat(OpenChat::new("fake", &two)).await.unwrap();
    assert_ne!(first.id, second.id);
    let (mut watch_one, mut watch_two) = (first.watch(), second.watch());
    second.send(UserMessage::text("Second")).await.unwrap();
    first.send(UserMessage::text("First")).await.unwrap();
    let done_one = until(&first, &mut watch_one, "the first chat's turn", finished).await;
    let done_two = until(&second, &mut watch_two, "the second chat's turn", finished).await;
    assert_eq!(words(&done_one.turns[0]), "for one");
    assert_eq!(words(&done_two.turns[0]), "for two");
    shut_down(&caf, running).await;
    let starts = dir
        .record()
        .iter()
        .filter(|line| line.get("started").is_some())
        .count();
    assert_eq!(starts, 1, "one adapter for both");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutting_down_leaves_no_processes() {
    let dir = Dir::new();
    let holder = dir.path("holder.pid");
    dir.script(
        "script.jsonl",
        &after_handshake(&[
            json!({"spawn_holder": {"pid_file": holder, "secs": 600}}),
            prompt(),
            say("s1", "working"),
            "hang".into(),
        ]),
    );
    let shell = r#"echo $$ > "$DIR/fake.pid"; exec "$FAKE" "$DIR/script.jsonl" --record "$DIR/record.jsonl""#;
    let (caf, running) = start(Config::new(host(), vec![fake_by_shell(&dir, shell)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Work")).await.unwrap();
    until(&chat, &mut watch, "the turn to run", |chat| {
        chat.turns
            .first()
            .is_some_and(|turn| words(turn) == "working")
    })
    .await;
    let (fake_pid, holder_pid) = (
        read_pid(&dir.path("fake.pid")).await,
        read_pid(&holder).await,
    );
    assert!(!gone(fake_pid) && !gone(holder_pid));

    let never_read = chat.watch();
    shut_down(&caf, running).await;
    // The adapter is reaped by the time CAF has stopped; what it started
    // goes with its group.
    assert!(gone(fake_pid), "the adapter outlived shutting down");
    until_gone(holder_pid).await;
    let Some(Update::Picture(last)) = watch.next().await else {
        panic!("the watch gets how CAF left the chat")
    };
    assert_eq!(last.turns[0].life, TurnLife::CutOff);
    // Watches that never read get how CAF left the chat too, once.
    for mut late in [never_read, chat.watch()] {
        let Some(Update::Picture(last)) = late.next().await else {
            panic!("a watch that never read gets how CAF left the chat")
        };
        assert_eq!(last.turns[0].life, TurnLife::CutOff);
        assert!(late.next().await.is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_that_falls_behind_gets_a_fresh_picture() {
    let dir = Dir::new();
    let mut steps = vec![prompt()];
    steps.extend((0..40).map(|n| say("s1", &format!("{n} "))));
    steps.push(end_turn());
    let script = dir.script("script.jsonl", &after_handshake(&steps));
    let mut host = host();
    host.limits.kept_changes = 8;
    let (caf, running) = start(Config::new(host, vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut keeping_up = chat.watch();
    let mut behind = chat.watch();
    assert!(matches!(behind.next().await, Some(Update::Picture(_))));
    assert!(matches!(keeping_up.next().await, Some(Update::Picture(_))));
    chat.send(UserMessage::text("Count")).await.unwrap();
    let expected: String = (0..40).map(|n| format!("{n} ")).collect();
    until(&chat, &mut keeping_up, "the turn to finish", finished).await;

    match behind.next().await {
        Some(Update::Picture(now)) => {
            assert!(finished(&now));
            assert_eq!(words(&now.turns[0]), expected);
        }
        other => panic!("a watch far behind gets a picture, not {other:?}"),
    }
    shut_down(&caf, running).await;
}

/// Whether `text` holds `secret`, as text or as the array of bytes Rust
/// writes an `OsString` as.
fn holds(text: &str, secret: &str) -> bool {
    let bytes: Vec<String> = secret.bytes().map(|byte| byte.to_string()).collect();
    text.contains(secret) || text.contains(&bytes.join(","))
}

/// A recording's header.
#[derive(serde::Deserialize)]
struct Header {
    caf_recording: u32,
    host: Host,
    agents: Vec<AgentSpec>,
}

/// A recording's event.
#[derive(serde::Deserialize)]
struct Step {
    now: Now,
    event: Event,
}

/// Replays a recording into a fresh core.
fn replay(text: &str) -> Core {
    let mut lines = text.lines();
    let header: Header =
        serde_json::from_str(lines.next().expect("a header")).expect("the header reads");
    assert_eq!(header.caf_recording, 2, "a recording of version 2");
    let mut core = Core::new(header.host, header.agents);
    let mut out = Vec::new();
    // A recording cut short says so, and ends there.
    for line in lines.take_while(|line| !line.starts_with(r#"{"stopped":"#)) {
        let step: Step = serde_json::from_str(line).expect("a step reads");
        core.handle(step.now, step.event, &mut out);
    }
    core
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recorded_run_replays_to_the_same_state() {
    let dir = Dir::new();
    let ask = json!({"request": {"method": "session/request_permission", "as": "ask", "params": {
        "sessionId": "s1",
        "toolCall": {"toolCallId": "t1", "title": "Run ls"},
        "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}]
    }}});
    let script = dir.script("script.jsonl", &after_handshake(&[
        json!({"notify": {"method": "_auth/status_update", "params": {"email": "someone@example.com"}}}),
        json!({"stderr": "warming up"}),
        prompt(),
        say("s1", "Looking."),
        ask,
        json!({"await": {"to": "ask", "timeout": 60_000}}),
        say("s1", " Done."),
        end_turn(),
        prompt(),
        say("s1", "Stopping? "),
        json!({"wait": {"method": "session/cancel"}}),
        json!({"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}}),
    ]));
    let recording = dir.path("run.jsonl");
    let mut agent = fake(&dir, &script);
    agent.command.env.push(EnvChange::Set(
        "SECRET_KEY".into(),
        "sk-do-not-record".into(),
    ));
    agent.command.args = vec!["--key".into(), "sk-arg-do-not-record".into()];
    let mut config = Config::new(host(), vec![agent]);
    config.record = Some(recording.clone());
    let (caf, running) = start(config);
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Look")).await.unwrap();
    let asked = until(&chat, &mut watch, "the question", |chat| {
        !chat.questions.is_empty()
    })
    .await;
    chat.answer(
        asked.questions[0].id,
        Answer::Permission(OptionId("allow".into())),
    )
    .await
    .unwrap();
    until(&chat, &mut watch, "the first turn", finished).await;
    chat.send(UserMessage::text("Again")).await.unwrap();
    until(&chat, &mut watch, "the second turn's words", |chat| {
        chat.turns
            .get(1)
            .is_some_and(|turn| words(turn) == "Stopping? ")
    })
    .await;
    chat.stop().await.unwrap();
    until(&chat, &mut watch, "the second turn to stop", |chat| {
        last_life(chat) == Some(&TurnLife::Stopped)
    })
    .await;
    shut_down(&caf, running).await;

    let last = match watch.next().await {
        Some(Update::Picture(last)) => last,
        other => panic!("the watch gets how CAF left the chat, not {other:?}"),
    };
    let text = std::fs::read_to_string(&recording).unwrap();
    assert!(
        !text.contains("example.com"),
        "the account email is never recorded"
    );
    assert!(
        text.contains(
            r#"{"type":"Notification","value":{"method":"_auth/status_update","params":null}}"#
        ),
        "the sign-in status is recorded, without its params"
    );
    assert!(
        !holds(&text, "sk-do-not-record"),
        "agents' variables' values aren't recorded"
    );
    assert!(
        !holds(&text, "sk-arg-do-not-record"),
        "agents' arguments aren't recorded"
    );
    let header: Header = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    let command = &header.agents[0].command;
    assert!(command.args.is_empty());
    assert!(
        command
            .env
            .contains(&EnvChange::Set("SECRET_KEY".into(), "[redacted]".into())),
        "{:?}",
        command.env
    );
    assert!(
        text.contains(r#""event":{"type":"Stderr","value":"warming up\n"}"#),
        "error output is written as text"
    );
    let core = replay(&text);
    assert_eq!(core.chat(chat.id), Some(&last));
    assert!(core.is_idle());
    // The same recording replays the same way twice.
    assert_eq!(replay(&text).chat(chat.id), core.chat(chat.id));

    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&recording).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_is_never_written_through_a_link() {
    let dir = Dir::new();
    let target = dir.path("target");
    std::fs::write(&target, "keep").unwrap();
    std::os::unix::fs::symlink(&target, dir.path("link")).unwrap();
    let mut config = Config::new(host(), Vec::new());
    config.record = Some(dir.path("link"));
    let (caf, running) = start(config);
    shut_down(&caf, running).await;
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_and_unknown_chats() {
    let (caf, running) = start(Config::new(host(), Vec::new()));
    let refused = caf.open_chat(OpenChat::new("nobody", "/tmp")).await;
    assert!(matches!(refused, Err(Error::Refused(_))));
    let nobody = caf.chat(ChatId(4242));
    assert!(matches!(
        nobody.send(UserMessage::text("hi")).await,
        Err(Error::Refused(_))
    ));
    assert!(
        nobody.watch().next().await.is_none(),
        "no such chat to watch"
    );
    shut_down(&caf, running).await;
    // Shutting down again is done already.
    caf.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_a_chat_ends_its_watches() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &handshake("s1"));
    let (caf, running) = start(Config::new(host(), vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    let handle = caf.chat(chat.id);
    chat.close().await.unwrap();
    let ended =
        tokio::time::timeout(PATIENCE, async { while watch.next().await.is_some() {} }).await;
    assert!(ended.is_ok(), "the watch ends once the chat is closed");
    assert_eq!(picture(&handle).await.state, ChatState::Closed);
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_every_handle_shuts_caf_down() {
    let dir = Dir::new();
    dir.script("script.jsonl", &handshake("s1"));
    let shell = r#"echo $$ > "$DIR/fake.pid"; exec "$FAKE" "$DIR/script.jsonl" --record "$DIR/record.jsonl""#;
    let (caf, running) = start(Config::new(host(), vec![fake_by_shell(&dir, shell)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    let pid = read_pid(&dir.path("fake.pid")).await;
    drop((caf, chat, watch));
    assert!(
        tokio::time::timeout(PATIENCE, running).await.is_ok(),
        "CAF stops once nobody can use it"
    );
    assert!(gone(pid));
}

#[test]
#[should_panic(expected = "biggest line")]
fn the_driver_and_the_core_must_agree_on_a_too_big_line() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut config = Config::new(host(), Vec::new());
        config.agent_limits.biggest_line = 1 << 20;
        let _ = start(config);
    });
}

/// A policy that panics.
struct Panics;

impl Policy for Panics {
    fn decide(
        &mut self,
        _: ChatId,
        _: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>> {
        Box::pin(async { panic!("a policy with a bug") })
    }
}

/// A policy that never decides, and says when its decision is dropped.
struct NeverDecides(Option<tokio::sync::oneshot::Sender<()>>);

/// Says it's dropped.
struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for Dropped {
    fn drop(&mut self) {
        if let Some(dropped) = self.0.take() {
            let _ = dropped.send(());
        }
    }
}

impl Policy for NeverDecides {
    fn decide(
        &mut self,
        _: ChatId,
        _: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>> {
        let dropped = Dropped(self.0.take());
        Box::pin(async move {
            let _dropped = dropped;
            std::future::pending().await
        })
    }
}

/// A policy whose `decide` panics, before it gives a future.
struct PanicsAtOnce;

impl Policy for PanicsAtOnce {
    fn decide(
        &mut self,
        _: ChatId,
        _: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>> {
        panic!("a policy with a bug, outside its future")
    }
}

/// The agent asks to run something, offering to allow or reject it.
fn ask_to_run() -> Value {
    json!({"request": {"method": "session/request_permission", "as": "ask", "params": {
        "sessionId": "s1",
        "toolCall": {"toolCallId": "t1", "title": "Run rm"},
        "options": [
            {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
            {"optionId": "reject", "name": "Reject", "kind": "reject_once"}
        ]
    }}})
}

/// The answers the client gave the agent's request `id`, from the
/// fake's record.
fn answers_to(dir: &Dir, id: u64) -> Vec<Value> {
    (dir.record().into_iter())
        .filter_map(|line| line.get("received").cloned())
        .filter(|message| message.get("method").is_none() && message["id"] == id)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_policy_that_panics_refuses() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &after_handshake(&[
        prompt(),
        ask_to_run(),
        json!({"await": {"to": "ask", "expect": {"result": {"outcome": {"outcome": "selected", "optionId": "reject"}}}, "timeout": 60_000}}),
        end_turn(),
    ]));
    let mut config = Config::new(host(), vec![fake(&dir, &script)]);
    config.policy = Box::new(Panics);
    let (caf, running) = start(config);
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Clean up")).await.unwrap();
    let done = until(&chat, &mut watch, "the turn to finish", finished).await;
    assert!(done.questions.is_empty(), "never asked the user");
    shut_down(&caf, running).await;
    assert_eq!(answers_to(&dir, 0).len(), 1, "{:#?}", dir.record());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withdrawn_decision_is_stopped() {
    let dir = Dir::new();
    let script = dir.script(
        "script.jsonl",
        &after_handshake(&[
            prompt(),
            ask_to_run(),
            say("s1", "asking"),
            json!({"await": {"to": "ask", "expect": {"result": {"outcome": {"outcome": "cancelled"}}}, "timeout": 60_000}}),
            json!({"wait": {"method": "session/cancel"}}),
            json!({"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}}),
        ]),
    );
    let mut config = Config::new(host(), vec![fake(&dir, &script)]);
    let (dropped, decision_dropped) = tokio::sync::oneshot::channel();
    config.policy = Box::new(NeverDecides(Some(dropped)));
    let (caf, running) = start(config);
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Clean up")).await.unwrap();
    // A process's lines reach the core in order, so once the words after
    // the request are in, the policy has the request.
    until(&chat, &mut watch, "the words after the request", |chat| {
        chat.turns
            .first()
            .is_some_and(|turn| words(turn) == "asking")
    })
    .await;
    chat.stop().await.unwrap();
    until(&chat, &mut watch, "the turn to stop", |chat| {
        last_life(chat) == Some(&TurnLife::Stopped)
    })
    .await;
    // Stopping withdrew the request, which stopped the decision, long
    // before shutting down would have.
    let stopped = tokio::time::timeout(PATIENCE, decision_dropped).await;
    assert!(matches!(stopped, Ok(Ok(()))), "the decision was dropped");
    assert_eq!(last_life(&picture(&chat).await), Some(&TurnLife::Stopped));
    shut_down(&caf, running).await;
    let answers = answers_to(&dir, 0);
    assert_eq!(answers.len(), 1, "answered once: {answers:#?}");
    assert_eq!(answers[0]["result"]["outcome"]["outcome"], "cancelled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_policy_that_panics_in_decide_refuses_and_caf_goes_on() {
    let dir = Dir::new();
    let script = dir.script(
        "script.jsonl",
        &after_handshake(&[
            prompt(),
            ask_to_run(),
            json!({"await": {"to": "ask", "expect": {"result": {"outcome": {"outcome": "selected", "optionId": "reject"}}}, "timeout": 60_000}}),
            end_turn(),
            prompt(),
            say("s1", "still here"),
            end_turn(),
        ]),
    );
    let mut config = Config::new(host(), vec![fake(&dir, &script)]);
    config.policy = Box::new(PanicsAtOnce);
    let (caf, running) = start(config);
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Clean up")).await.unwrap();
    until(&chat, &mut watch, "the first turn", finished).await;
    chat.send(UserMessage::text("Again")).await.unwrap();
    let done = until(&chat, &mut watch, "the second turn", |chat| {
        chat.turns.len() == 2 && finished(chat)
    })
    .await;
    assert_eq!(words(&done.turns[1]), "still here");
    shut_down(&caf, running).await;
    assert_eq!(answers_to(&dir, 0).len(), 1, "{:#?}", dir.record());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_secrets_are_hidden_in_a_recording() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &handshake("s1"));
    let recording = dir.path("run.jsonl");
    let mut config = Config::new(host(), vec![fake(&dir, &script)]);
    config.record = Some(recording.clone());
    let (caf, running) = start(config);
    let mut open = OpenChat::new("fake", &dir.0);
    let mut tool = SpawnCommand::new("/bin/true");
    tool.env = vec![EnvChange::Set(
        "GITHUB_TOKEN".into(),
        "ghp_do_not_record".into(),
    )];
    open.mcp_servers = vec![
        McpServer::Stdio {
            name: "gh".into(),
            command: tool,
        },
        McpServer::Http {
            name: "maps".into(),
            url: "http://127.0.0.1:9/mcp".into(),
            headers: vec![("Authorization".into(), "Bearer do-not-record".into())],
        },
    ];
    caf.open_chat(open).await.unwrap();
    shut_down(&caf, running).await;
    let text = std::fs::read_to_string(&recording).unwrap();
    assert!(!holds(&text, "ghp_do_not_record"));
    assert!(!holds(&text, "do-not-record"));
    // Their names are kept.
    assert!(holds(&text, "GITHUB_TOKEN"));
    assert!(text.contains(r#"["Authorization","[redacted]"]"#), "{text}");
    // And the core takes it as it did.
    replay(&text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_never_replaces_a_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Dir::new();
    let there = dir.path("there.jsonl");
    std::fs::write(&there, "keep").unwrap();
    std::fs::set_permissions(&there, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut config = Config::new(host(), Vec::new());
    config.record = Some(there.clone());
    let (caf, running) = start(config);
    shut_down(&caf, running).await;
    assert_eq!(std::fs::read_to_string(&there).unwrap(), "keep");
    let mode = std::fs::metadata(&there).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o644);
}

#[test]
fn a_recording_never_waits_on_a_pipe() {
    let dir = Dir::new();
    let pipe = dir.path("pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(&pipe)
        .status()
        .unwrap();
    assert!(made.success());
    let (done, started) = std::sync::mpsc::channel();
    // On a thread of its own, so a start that waits on the pipe fails the
    // test rather than hanging it.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut config = Config::new(host(), Vec::new());
            config.record = Some(pipe);
            let (caf, running) = start(config);
            let _ = done.send(());
            shut_down(&caf, running).await;
        });
    });
    assert!(
        started.recv_timeout(PATIENCE).is_ok(),
        "starting waited on the pipe"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_doesnt_keep_caf_running() {
    let dir = Dir::new();
    dir.script("script.jsonl", &handshake("s1"));
    let shell = r#"echo $$ > "$DIR/fake.pid"; exec "$FAKE" "$DIR/script.jsonl" --record "$DIR/record.jsonl""#;
    let (caf, running) = start(Config::new(host(), vec![fake_by_shell(&dir, shell)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut read = chat.watch();
    until(&chat, &mut read, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    let pid = read_pid(&dir.path("fake.pid")).await;
    let mut unread = chat.watch();
    // A watch waiting on a task, as an app's would be.
    let waiting = tokio::spawn(async move {
        let mut last = None;
        while let Some(update) = read.next().await {
            last = Some(update);
        }
        last
    });
    drop((caf, chat));
    assert!(
        tokio::time::timeout(PATIENCE, running).await.is_ok(),
        "CAF stops though watches are left"
    );
    assert!(gone(pid));
    let last = tokio::time::timeout(PATIENCE, waiting)
        .await
        .unwrap()
        .unwrap();
    let Some(Update::Picture(last)) = last else {
        panic!("the waiting watch gets how CAF left the chat: {last:?}")
    };
    assert!(matches!(
        last.state,
        ChatState::Lost {
            reason: Lost::ShutDown
        }
    ));
    assert!(matches!(unread.next().await, Some(Update::Picture(_))));
    assert!(unread.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watches_left_partway_through_a_read_dont_hold_caf_up() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &after_handshake(&[prompt()]));
    let hold = common::Hold::default();
    let (caf, running) =
        Caf::start_watching(Config::new(host(), vec![fake(&dir, &script)]), hold.clone());
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    hold.next_step();
    let sending = tokio::spawn({
        let chat = caf.chat(chat.id);
        async move { chat.send(UserMessage::text("Hello")).await }
    });
    hold.held().await;
    // While CAF is held up, many watches start reading, and are then left
    // as they are: never asked again, and never dropped.
    let mut left = Vec::new();
    for id in 5000..5200 {
        let mut watch = caf.chat(ChatId(id)).watch();
        let mut next = Box::pin(async move { watch.next().await });
        std::future::poll_fn(|cx| {
            let _ = next.as_mut().poll(cx);
            Poll::Ready(())
        })
        .await;
        left.push(next);
    }
    hold.release();
    let patience = Duration::from_secs(10);
    let sent = tokio::time::timeout(patience, sending).await;
    assert!(matches!(sent, Ok(Ok(Ok(_)))), "the host is heard: {sent:?}");
    let stopped = tokio::time::timeout(patience, chat.stop()).await;
    assert!(stopped.is_ok(), "the host is heard after the hold, too");
    drop((caf, chat, watch));
    assert!(
        tokio::time::timeout(patience, running).await.is_ok(),
        "CAF stops though watches were left partway through a read"
    );
    drop(left);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_sent_before_a_command_is_answered_first() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &handshake("s1"));
    let hold = common::Hold::default();
    let (caf, running) =
        Caf::start_watching(Config::new(host(), vec![fake(&dir, &script)]), hold.clone());
    let mut chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    for round in 0..40 {
        // Held up opening the next round's chat.
        hold.next_step();
        let opening = tokio::spawn({
            let caf = caf.clone();
            let folder = dir.0.clone();
            async move { caf.open_chat(OpenChat::new("fake", folder)).await }
        });
        hold.held().await;
        // Meanwhile, a watch sends its read, then the chat is closed.
        let mut watch = chat.watch();
        let mut next = Box::pin(async move { watch.next().await });
        std::future::poll_fn(|cx| {
            let _ = next.as_mut().poll(cx);
            Poll::Ready(())
        })
        .await;
        let closing = tokio::spawn({
            let chat = caf.chat(chat.id);
            async move { chat.close().await }
        });
        // Time for the close to be sent. If it isn't sent by then, the
        // round tests less, but never fails wrongly.
        tokio::time::sleep(Duration::from_millis(20)).await;
        hold.release();
        let first = tokio::time::timeout(PATIENCE, next).await.unwrap();
        let Some(Update::Picture(first)) = first else {
            panic!("round {round}: a picture first, not {first:?}")
        };
        assert_ne!(
            first.state,
            ChatState::Closed,
            "round {round}: the read is answered before the close"
        );
        closing.await.unwrap().unwrap();
        chat = opening.await.unwrap().unwrap();
    }
    shut_down(&caf, running).await;
}
