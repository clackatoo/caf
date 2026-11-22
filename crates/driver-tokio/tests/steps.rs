//! Watching every step CAF takes, against the fake adapter.

mod common;

use std::sync::{Arc, Mutex};

use caf_core::agent::{EnvChange, McpServer, SpawnCommand};
use caf_core::chat::UserMessage;
use caf_core::event::{Event, HostCommand};
use caf_core::host::Host;
use caf_core::question::{Answer, OptionId};
use caf_core::{ChatId, Core, Directive, Now, OpenChat};
use caf_driver_tokio::{Caf, Error, Step, Steps};
use serde_json::{Value, json};

use common::*;

/// One step as the watcher saw it, written down as JSON, since events
/// and directives can't be copied.
#[derive(Debug)]
struct Seen {
    now: Now,
    event: Value,
    directives: Value,
    /// What the host asked for, if it's a host call.
    host: Option<&'static str>,
    /// Its line in the recording, if there's one.
    line: Option<String>,
}

/// A watcher that writes every step down.
#[derive(Clone, Default)]
struct Notebook(Arc<Mutex<Vec<Seen>>>);

impl Steps<caf_core::NoAppItems> for Notebook {
    fn step(&mut self, step: Step<'_, caf_core::NoAppItems>) {
        let host = match step.event {
            Event::Host(HostCommand::OpenChat { .. }) => Some("open"),
            Event::Host(HostCommand::Send { .. }) => Some("send"),
            Event::Host(HostCommand::Stop { .. }) => Some("stop"),
            Event::Host(HostCommand::Answer { .. }) => Some("answer"),
            Event::Host(HostCommand::CloseChat { .. }) => Some("close"),
            Event::Host(HostCommand::Shutdown) => Some("shutdown"),
            Event::Host(HostCommand::CheckAgents) => Some("check agents"),
            Event::Host(_) => Some("other"),
            _ => None,
        };
        self.0.lock().unwrap().push(Seen {
            now: step.now,
            event: serde_json::to_value(step.event).unwrap(),
            directives: serde_json::to_value(step.directives).unwrap(),
            host,
            line: step.line.map(str::to_owned),
        });
    }
}

impl Notebook {
    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

/// A recording's header.
#[derive(serde::Deserialize)]
struct Header {
    host: Host,
}

/// A recording's event.
#[derive(serde::Deserialize)]
struct Line {
    now: Now,
    event: Event,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watcher_sees_one_step_per_recording_line_with_its_directives() {
    let dir = Dir::new();
    let script = dir.script(
        "script.jsonl",
        &after_handshake(&[
            json!({"stderr": "warming up"}),
            prompt(),
            say("s1", "Looking."),
            ask_to_run(),
            json!({"await": {"to": "ask", "timeout": 60_000}}),
            say("s1", " Done."),
            end_turn(),
        ]),
    );
    let agents = || vec![fake(&dir, &script)];
    let recording = dir.path("run.jsonl");
    let mut config = config_for(host(), agents());
    config.record = Some(recording.clone());
    let notebook = Notebook::default();
    let (caf, running) = Caf::start_watching(config, notebook.clone());
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
    until(&chat, &mut watch, "the turn", finished).await;
    shut_down(&caf, running).await;

    let seen = notebook.take();
    let text = std::fs::read_to_string(&recording).unwrap();
    let mut lines = text.lines();
    let header: Header = serde_json::from_str(lines.next().unwrap()).unwrap();
    // Each step has its line of the recording, exactly.
    let texts: Vec<Option<&str>> = lines.clone().map(Some).collect();
    let given: Vec<Option<&str>> = seen.iter().map(|seen| seen.line.as_deref()).collect();
    assert_eq!(given, texts);
    let lines: Vec<Line> = lines
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(seen.len(), lines.len(), "one step per line: {seen:#?}");

    // The same events, in the same order, and the directives a fresh core
    // gives for them: what the core gave out live.
    let mut core = Core::new(header.host, agents());
    let mut out: Vec<Directive> = Vec::new();
    for (step, (seen, line)) in seen.iter().zip(lines).enumerate() {
        assert_eq!(seen.now, line.now, "step {step}");
        assert_eq!(
            seen.event,
            serde_json::to_value(&line.event).unwrap(),
            "step {step}"
        );
        core.handle(line.now, line.event, &mut out);
        assert_eq!(
            seen.directives,
            serde_json::to_value(&out).unwrap(),
            "step {step}'s directives"
        );
        out.clear();
    }
    let hosts: Vec<_> = seen.iter().filter_map(|seen| seen.host).collect();
    assert_eq!(
        hosts,
        ["check agents", "open", "send", "answer", "shutdown"],
        "CAF looks at its agents first, as it starts"
    );
    let open = (seen.iter())
        .find(|seen| seen.host == Some("open"))
        .expect("the chat was opened");
    assert_eq!(open.host, Some("open"));
    assert!(
        open.directives
            .as_array()
            .is_some_and(|out| !out.is_empty()),
        "opening starts the adapter: {open:?}"
    );
    assert!(
        seen.iter().any(|seen| seen.host.is_none()),
        "the adapter's events are steps too"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watcher_without_a_recording_sees_the_host_calls_the_core_took() {
    let dir = Dir::new();
    let script = dir.script(
        "script.jsonl",
        &after_handshake(&[prompt(), say("s1", "Hi."), end_turn()]),
    );
    let notebook = Notebook::default();
    let (caf, running) = Caf::start_watching(
        config_for(host(), vec![fake(&dir, &script)]),
        notebook.clone(),
    );
    let refused = caf.open_chat(OpenChat::new("nobody", &dir.0)).await.err();
    assert!(matches!(refused, Some(Error::Refused(_))), "{refused:?}");
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    let refused = caf
        .chat(ChatId::from_number(999_999))
        .send(UserMessage::text("?"))
        .await;
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
    chat.send(UserMessage::text("Hello")).await.unwrap();
    until(&chat, &mut watch, "the turn", finished).await;
    assert!(chat.stop().await.is_err(), "nothing to stop");
    caf.chat(chat.id).close().await.unwrap();
    shut_down(&caf, running).await;

    let seen = notebook.take();
    let hosts: Vec<_> = seen.iter().filter_map(|seen| seen.host).collect();
    assert_eq!(hosts, ["check agents", "open", "send", "close", "shutdown"]);
    assert!(
        seen.iter().all(|seen| seen.line.is_none()),
        "no recording, no lines"
    );
    // Every step after the first is later than or as late as the one
    // before.
    assert!(
        seen.windows(2)
            .all(|pair| pair[0].now.mono <= pair[1].now.mono)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watcher_sees_secrets_as_they_are_and_its_steps_replay_exactly() {
    let dir = Dir::new();
    let mut script = after_handshake(&[
        json!({"notify": {"method": "_auth/status_update", "params": {"authStatus": {"kind": "account", "email": "me@example.com"}}}}),
        prompt(),
        say("s1", "Hi."),
        end_turn(),
    ]);
    // An agent that reaches MCP servers over HTTP too.
    script[2]["respond"]["result"]["agentCapabilities"]["mcpCapabilities"] = json!({"http": true});
    let script = dir.script("script.jsonl", &script);
    let agents = || vec![fake(&dir, &script)];
    let recording = dir.path("run.jsonl");
    let mut config = config_for(host(), agents());
    config.record = Some(recording.clone());
    let notebook = Notebook::default();
    let (caf, running) = Caf::start_watching(config, notebook.clone());
    let mut open = OpenChat::new("fake", &dir.0);
    let mut tool = SpawnCommand::new("/bin/true");
    tool.env = vec![EnvChange::Set("TOKEN".into(), "s3cret-env".into())];
    open.mcp_servers = vec![
        McpServer::Stdio {
            name: "gh".into(),
            command: tool,
        },
        McpServer::Http {
            name: "maps".into(),
            url: "http://127.0.0.1:9/mcp".into(),
            headers: vec![("Authorization".into(), "Bearer s3cret-header".into())],
        },
    ];
    let chat = caf.open_chat(open).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hello")).await.unwrap();
    until(&chat, &mut watch, "the turn", finished).await;
    shut_down(&caf, running).await;

    let seen = notebook.take();
    let events =
        serde_json::to_string(&(seen.iter().map(|seen| &seen.event)).collect::<Vec<_>>()).unwrap();
    let text = std::fs::read_to_string(&recording).unwrap();
    // As text, or as the bytes a variable's value is written as.
    let holds = |text: &str, secret: &str| {
        let bytes = serde_json::to_string(secret.as_bytes()).unwrap();
        text.contains(secret) || text.contains(bytes.trim_matches(['[', ']']))
    };
    let lines: Vec<&str> = seen
        .iter()
        .filter_map(|seen| seen.line.as_deref())
        .collect();
    assert_eq!(lines.len(), seen.len(), "every step has its line");
    let lines = lines.join("\n");
    for secret in ["s3cret-env", "s3cret-header", "me@example.com"] {
        assert!(holds(&events, secret), "the watcher sees {secret}");
        assert!(!holds(&text, secret), "the recording hides {secret}");
        assert!(!holds(&lines, secret), "so do the steps' lines: {secret}");
    }
    // A fresh core given the steps' events gives out what the live one
    // did, step by step.
    let mut core = Core::new(host(), agents());
    let mut out: Vec<Directive> = Vec::new();
    for (step, seen) in seen.iter().enumerate() {
        let event: Event = serde_json::from_value(seen.event.clone()).unwrap();
        core.handle(seen.now, event, &mut out);
        assert_eq!(
            seen.directives,
            serde_json::to_value(&out).unwrap(),
            "step {step}'s directives"
        );
        out.clear();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hold_lasts_however_late_the_test_looks() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &handshake("s1"));
    let hold = Hold::default();
    let (caf, running) =
        Caf::start_watching(config_for(host(), vec![fake(&dir, &script)]), hold.clone());
    hold.next_step();
    let opening = tokio::spawn({
        let caf = caf.clone();
        let folder = dir.0.clone();
        async move { caf.open_chat(OpenChat::new("fake", folder)).await }
    });
    // The test is slow to look.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let held = tokio::time::timeout(std::time::Duration::from_secs(5), hold.held()).await;
    assert!(held.is_ok(), "CAF is still held up");
    hold.release();
    assert!(opening.await.unwrap().is_ok());
    shut_down(&caf, running).await;
}
