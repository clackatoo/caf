//! Serving CAF's tools, end to end: the fake adapter starts the real
//! relay (the fake's own program, which calls
//! `caf_relay::relay_if_asked`) from its session's MCP servers, and calls
//! the tools through it; a scripted runner runs them.

mod common;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use caf_core::agent::SpawnCommand;
use caf_core::chat::{Chat, ItemKind, Note, UserMessage};
use caf_core::directive::PermissionRequest;
use caf_core::event::{Event, PolicyAnswer};
use caf_core::ids::{CallId, ChatId};
use caf_core::mcp::Answer as McpAnswer;
use caf_core::question::{Answer, QuestionKind};
use caf_core::tool::{Action, CafTool, Piece, StoppedBy, Text, ToolCall, ToolStatus, Tools};
use caf_core::{NoAppItems, OpenChat};
use caf_driver_tokio::{
    AlwaysAsk, Caf, Call, ChatHandle, Config, Policy, Preview, Ran, RefuseAll, Relay, Runner,
    Running, StartError, Step, Steps,
};
use common::{Dir, PATIENCE, after_handshake, end_turn, fake, host, prompt, until};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// What the scripted runner does with a run.
#[derive(Clone, Copy)]
enum Doing {
    /// Answers at once, with this text.
    Answers(&'static str),
    /// Runs until it's dropped.
    Hangs,
}

/// Runs every call as `doing` says, telling the test of each run started
/// and each dropped before it finished.
struct Scripted {
    doing: Doing,
    started: mpsc::UnboundedSender<CallId>,
    dropped: mpsc::UnboundedSender<CallId>,
}

/// Says its call's run was dropped, unless it finished.
struct Dropped(Option<(CallId, mpsc::UnboundedSender<CallId>)>);

impl Drop for Dropped {
    fn drop(&mut self) {
        if let Some((call, dropped)) = self.0.take() {
            let _ = dropped.send(call);
        }
    }
}

impl Runner for Scripted {
    fn preview(&mut self, _call: &Call, _action: Action) -> Preview {
        Box::pin(std::future::ready(Ok(Vec::new())))
    }

    fn run(&mut self, call: Call, _action: Action) -> Pin<Box<dyn Future<Output = Ran> + Send>> {
        let _ = self.started.send(call.id());
        let dropped = Dropped(Some((call.id(), self.dropped.clone())));
        let doing = self.doing;
        Box::pin(async move {
            let mut dropped = dropped;
            match doing {
                Doing::Answers(text) => {
                    call.print(0, vec![text.to_owned()]);
                    dropped.0.take();
                    Ran::new(
                        true,
                        vec![Piece::Text(Text::new(text))],
                        McpAnswer::text(text),
                    )
                }
                Doing::Hangs => {
                    let _held = dropped;
                    std::future::pending().await
                }
            }
        })
    }
}

/// Allows every call.
struct AllowAll;

impl Policy for AllowAll {
    fn decide(
        &mut self,
        _: ChatId,
        _: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>> {
        Box::pin(std::future::ready(PolicyAnswer::Allow))
    }
}

/// Asks the user about the first call, and allows the rest.
#[derive(Default)]
struct AskFirst {
    asked: bool,
}

impl Policy for AskFirst {
    fn decide(
        &mut self,
        _: ChatId,
        _: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>> {
        let answer = if self.asked {
            PolicyAnswer::Allow
        } else {
            PolicyAnswer::AskUser
        };
        self.asked = true;
        Box::pin(std::future::ready(answer))
    }
}

/// Every event CAF's core was given, as JSON.
#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<Value>>>);

impl Steps<NoAppItems> for Events {
    fn step(&mut self, step: Step<'_, NoAppItems>) {
        let event = serde_json::to_value(step.event).unwrap();
        self.0.lock().unwrap().push(event);
    }
}

impl Events {
    fn all(&self) -> Vec<Value> {
        self.0.lock().unwrap().clone()
    }

    /// The relay changes CAF heard, in order.
    fn relays(&self) -> Vec<String> {
        (self.all().iter())
            .filter(|event| event["type"] == "Relay")
            .map(|event| {
                event["value"]["change"]["type"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    /// The kinds of tool event CAF heard, in order.
    fn tool_events(&self) -> Vec<String> {
        (self.all().iter())
            .filter(|event| event["type"] == "Tool")
            .map(|event| event["value"]["event"]["type"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// A test's CAF, and what it watches.
struct Serving {
    dir: Dir,
    caf: Caf,
    running: Option<Running>,
    events: Events,
    started: mpsc::UnboundedReceiver<CallId>,
    dropped: mpsc::UnboundedReceiver<CallId>,
}

/// CAF serving tools through the fake's own program, which runs
/// `script` as the agent, with `policy`, a runner that does as `doing`
/// says, and progress every `progress`.
fn serve(
    script: &[Value],
    policy: impl Policy,
    doing: Doing,
    progress: Option<Duration>,
) -> Serving {
    let dir = Dir::new();
    let path = dir.script("script.jsonl", script);
    let mut host = host();
    host.shows.permission_questions = true;
    let mut config = common::config_for(host, vec![fake(&dir, &path)]);
    let (started, run) = mpsc::unbounded_channel();
    let (dropped, drop_seen) = mpsc::unbounded_channel();
    config.policy = Box::new(policy);
    serve_with(&mut config, &dir);
    config.tools.runner = Some(Box::new(Scripted {
        doing,
        started,
        dropped,
    }));
    if let Some(progress) = progress {
        config.tools.progress_every = progress;
    }
    let events = Events::default();
    let (caf, running) = Caf::start_watching(config, events.clone());
    Serving {
        dir,
        caf,
        running: Some(running),
        events,
        started: run,
        dropped: drop_seen,
    }
}

/// Serves CAF's tools through the fake's program, with the socket in the
/// test's own folder: never the user's.
fn serve_with(config: &mut Config, dir: &Dir) {
    config.tools.relay = Relay::Command(SpawnCommand::new(caf_fake_agent::binary()));
    config.tools.folder = Some(dir.path("s"));
}

impl Serving {
    /// Opens a chat with all of CAF's tools, and sends it a message.
    async fn chat(&self) -> ChatHandle {
        let open = OpenChat::new("fake", &self.dir.0).with_tools(Tools::caf(CafTool::ALL));
        let chat = self.caf.open_chat(open).await.unwrap();
        chat.send(UserMessage::text("Read a.txt")).await.unwrap();
        chat
    }

    async fn started(&mut self) -> CallId {
        (tokio::time::timeout(PATIENCE, self.started.recv()).await)
            .expect("a run started in time")
            .unwrap()
    }

    async fn dropped(&mut self) -> CallId {
        (tokio::time::timeout(PATIENCE, self.dropped.recv()).await)
            .expect("a run dropped in time")
            .unwrap()
    }

    /// Waits for the fake's script to end, or a check of it to fail.
    async fn script_done(&self) {
        let ended = tokio::time::timeout(PATIENCE, async {
            loop {
                let record = self.dir.record();
                if let Some(failed) = record.iter().find(|line| line.get("failed").is_some()) {
                    panic!("the fake's check failed: {failed}\n{record:#?}");
                }
                if record.iter().any(|line| line["note"] == "done") {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            ended.is_ok(),
            "the script didn't end: {:#?}",
            self.dir.record()
        );
    }

    /// No check of the fake's failed.
    fn no_check_failed(&self) {
        let record = self.dir.record();
        assert!(
            !record.iter().any(|line| line.get("failed").is_some()),
            "{record:#?}"
        );
    }

    async fn shut_down(mut self) {
        common::shut_down(&self.caf, self.running.take().unwrap()).await;
    }
}

/// The relay started from the session's servers, initialized and listing
/// CAF's tools.
fn start_relay() -> Vec<Value> {
    vec![
        json!({"mcp_start": {"discover": true, "expect": {"result": {"serverInfo": {"name": "caf"}}}, "timeout": 30000}}),
        json!({"mcp_list": {"expect": ["Read", "Edit", "Write", "Bash"], "timeout": 30000}}),
    ]
}

/// A script: the handshake, the relay, then a prompt and `steps`, and a
/// note that they're done.
fn script(steps: &[Value]) -> Vec<Value> {
    let mut all = start_relay();
    all.push(prompt());
    all.extend_from_slice(steps);
    all.push(json!({"note": "done"}));
    after_handshake(&all)
}

/// Calls CAF's Read as Claude does, reporting it in session `s1` first.
fn call_read(dir: &Dir) -> Value {
    let path = dir.path("a.txt").display().to_string();
    json!({"mcp_call": {"tool": "Read", "arguments": {"file_path": path}, "tool_use_id": "toolu_1", "report": "s1"}})
}

/// The chat's call to CAF's tool.
fn the_call(chat: &Chat) -> Option<&ToolCall> {
    (chat.turns.iter().flat_map(|turn| &turn.items)).find_map(|item| match &item.kind {
        ItemKind::Tool(tool) => Some(&**tool),
        _ => None,
    })
}

fn has_note(chat: &Chat, note: &Note) -> bool {
    (chat.turns.iter().flat_map(|turn| &turn.items))
        .any(|item| matches!(&item.kind, ItemKind::Note(each) if each == note))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_through_the_relay_is_run_and_answered() {
    let dir = Dir::new();
    let steps = script(&[
        call_read(&dir),
        json!({"mcp_await": {"expect": {"result": {"isError": false, "content": [{"type": "text", "text": "contents"}]}}, "timeout": 30000}}),
        end_turn(),
    ]);
    let mut serving = serve(&steps, AllowAll, Doing::Answers("contents"), None);
    let chat = serving.chat().await;
    let mut watch = chat.watch();
    serving.started().await;
    serving.script_done().await;
    let done = until(&chat, &mut watch, "the call done", |chat| {
        the_call(chat).is_some_and(|call| call.status == ToolStatus::Done)
    })
    .await;
    let call = the_call(&done).unwrap();
    assert_eq!(call.stopped_by, None);
    assert_eq!(serving.events.relays(), ["Connected"]);
    assert_eq!(
        serving.events.tool_events(),
        ["Called", "Output", "Finished"]
    );
    // The first event is the serving one, before anything the host did.
    assert_eq!(serving.events.all()[0]["type"], "Serving");
    serving.no_check_failed();
    drop(dir);
    serving.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_the_user_is_asked_about_waits_with_progress_then_runs() {
    let dir = Dir::new();
    let steps = script(&[
        call_read(&dir),
        json!({"mcp_progress": {"expect": {"params": {"message": "Waiting for permission"}}, "timeout": 30000}}),
        json!({"mcp_await": {"expect": {"result": {"isError": false}}, "timeout": 30000}}),
        end_turn(),
    ]);
    let mut serving = serve(
        &steps,
        AlwaysAsk,
        Doing::Answers("contents"),
        Some(Duration::from_millis(200)),
    );
    let chat = serving.chat().await;
    let mut watch = chat.watch();
    let asked = until(&chat, &mut watch, "the question", |chat| {
        !chat.questions.is_empty()
    })
    .await;
    assert_eq!(the_call(&asked).unwrap().status, ToolStatus::Asking);
    // Answered once the agent has heard it's waiting.
    let heard = tokio::time::timeout(PATIENCE, async {
        while !(serving.dir.record().iter()).any(|line| {
            line.get("mcp_received")
                .is_some_and(|got| got.to_string().contains("Waiting for permission"))
        }) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(heard.is_ok(), "{:#?}", serving.dir.record());
    let question = &asked.questions[0];
    let QuestionKind::Permission(permission) = &question.kind else {
        panic!("{question:?}");
    };
    let allow = permission.options[0].id.clone();
    chat.answer(question.id, Answer::Permission(allow))
        .await
        .unwrap();
    serving.started().await;
    serving.script_done().await;
    until(&chat, &mut watch, "the call done", |chat| {
        the_call(chat).is_some_and(|call| call.status == ToolStatus::Done)
    })
    .await;
    drop(dir);
    serving.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_call_is_answered_as_an_error() {
    let dir = Dir::new();
    let steps = script(&[
        call_read(&dir),
        json!({"mcp_await": {"expect": {"result": {"isError": true}}, "timeout": 30000}}),
        end_turn(),
    ]);
    let serving = serve(&steps, RefuseAll, Doing::Answers("never"), None);
    let chat = serving.chat().await;
    let mut watch = chat.watch();
    serving.script_done().await;
    until(&chat, &mut watch, "the call refused", |chat| {
        the_call(chat).is_some_and(|call| call.status == ToolStatus::Refused)
    })
    .await;
    assert_eq!(serving.events.tool_events(), ["Called"]);
    drop(dir);
    serving.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_turn_cancels_a_running_call() {
    let dir = Dir::new();
    let steps = script(&[
        call_read(&dir),
        json!({"on": {"method": "session/cancel", "do": [{"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}}]}}),
        json!({"mcp_await": {"expect": {"result": {"isError": true, "content": [{"type": "text", "text": "Cancelled: the user stopped this turn"}]}}, "timeout": 30000}}),
    ]);
    let mut serving = serve(&steps, AllowAll, Doing::Hangs, None);
    let chat = serving.chat().await;
    let mut watch = chat.watch();
    let call = serving.started().await;
    chat.stop().await.unwrap();
    assert_eq!(serving.dropped().await, call);
    serving.script_done().await;
    let stopped = until(&chat, &mut watch, "the call cancelled", |chat| {
        the_call(chat).is_some_and(|call| call.status == ToolStatus::Cancelled)
    })
    .await;
    assert_eq!(
        the_call(&stopped).unwrap().stopped_by,
        Some(StoppedBy::User)
    );
    drop(dir);
    serving.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_the_agent_cancels_is_gone_and_never_answered() {
    let dir = Dir::new();
    let steps = script(&[
        call_read(&dir),
        json!({"wait_file": {"path": dir.path("running").display().to_string(), "timeout": 30000}}),
        json!({"mcp_cancel": {}}),
        json!({"mcp_await": {"none": true, "timeout": 500}}),
        end_turn(),
    ]);
    let mut serving = serve(&steps, AllowAll, Doing::Hangs, None);
    let chat = serving.chat().await;
    let mut watch = chat.watch();
    let call = serving.started().await;
    std::fs::write(dir.path("running"), "").unwrap();
    assert_eq!(serving.dropped().await, call);
    serving.script_done().await;
    let gone = until(&chat, &mut watch, "the call cancelled", |chat| {
        the_call(chat).is_some_and(|call| call.status == ToolStatus::Cancelled)
    })
    .await;
    assert_eq!(the_call(&gone).unwrap().stopped_by, Some(StoppedBy::Agent));
    assert_eq!(serving.events.tool_events(), ["Called", "Gone"]);
    drop(dir);
    serving.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_that_goes_ends_its_calls_and_the_chat_is_told() {
    let dir = Dir::new();
    let steps = script(&[
        call_read(&dir),
        json!({"wait_file": {"path": dir.path("running").display().to_string(), "timeout": 30000}}),
        json!({"mcp_drop": {}}),
        json!({"wait_file": {"path": dir.path("lost").display().to_string(), "timeout": 30000}}),
        end_turn(),
    ]);
    let mut serving = serve(&steps, AllowAll, Doing::Hangs, None);
    let chat = serving.chat().await;
    let mut watch = chat.watch();
    let call = serving.started().await;
    std::fs::write(dir.path("running"), "").unwrap();
    assert_eq!(serving.dropped().await, call);
    let lost = until(&chat, &mut watch, "the note", |chat| {
        has_note(chat, &Note::ToolsLost)
    })
    .await;
    assert_eq!(the_call(&lost).unwrap().status, ToolStatus::Cancelled);
    assert_eq!(serving.events.relays(), ["Connected", "Lost"]);
    std::fs::write(dir.path("lost"), "").unwrap();
    serving.script_done().await;
    drop(dir);
    serving.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_call_hears_progress() {
    let dir = Dir::new();
    let steps = script(&[
        call_read(&dir),
        json!({"mcp_progress": {"expect": {"params": {"message": "Running"}}, "timeout": 30000}}),
        json!({"mcp_progress": {"expect": {"params": {"message": "Running"}}, "timeout": 30000}}),
        json!({"mcp_cancel": {}}),
        end_turn(),
    ]);
    let mut serving = serve(
        &steps,
        AllowAll,
        Doing::Hangs,
        Some(Duration::from_millis(200)),
    );
    let _chat = serving.chat().await;
    let call = serving.started().await;
    serving.script_done().await;
    assert_eq!(serving.dropped().await, call);
    drop(dir);
    serving.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_with_the_wrong_token_is_turned_away() {
    let steps = after_handshake(&[
        json!({"mcp_start": {"env": {"CAF_RELAY_TOKEN": "0000"}, "timeout": 30000}}),
        prompt(),
        end_turn(),
    ]);
    let serving = serve(&steps, AllowAll, Doing::Hangs, None);
    let _chat = serving.chat().await;
    let closed = tokio::time::timeout(PATIENCE, async {
        // Closed as the fake reads, or before it writes.
        while !(serving.dir.record().iter()).any(|line| {
            line.get("mcp_closed").is_some()
                || line["failed"]
                    .as_str()
                    .is_some_and(|why| why.contains("Broken pipe"))
        }) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(closed.is_ok(), "{:#?}", serving.dir.record());
    assert!(serving.events.relays().is_empty());
    serving.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[should_panic(expected = "caf_relay::relay_if_asked()")]
async fn a_relay_without_the_hook_stops_caf_starting() {
    let dir = Dir::new();
    let mut config = common::config_for(host(), Vec::new());
    config.tools.relay = Relay::Command(SpawnCommand::new("/bin/true"));
    config.tools.folder = Some(dir.path("s"));
    let _ = Caf::<NoAppItems>::start(config);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_relay_a_chat_given_tools_is_refused() {
    let dir = Dir::new();
    let config = common::config_for(host(), vec![fake(&dir, &dir.path("none"))]);
    let events = Events::default();
    let (caf, running) = Caf::start_watching(config, events.clone());
    let open = OpenChat::new("fake", &dir.0).with_tools(Tools::caf([CafTool::Read]));
    let refused = caf.open_chat(open).await;
    assert!(
        refused.is_err(),
        "a chat given tools opened with none served"
    );
    assert!(!(events.all().iter()).any(|event| event["type"] == "Serving"));
    common::shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_never_holds_the_key() {
    let dir = Dir::new();
    let mut config = common::config_for(host(), Vec::new());
    serve_with(&mut config, &dir);
    config.record = Some(dir.path("run.jsonl"));
    let events = Events::default();
    let (caf, running) = Caf::start_watching(config, events.clone());
    common::shut_down(&caf, running).await;
    let serving = (events.all().into_iter())
        .find(|event| event["type"] == "Serving")
        .expect("CAF served");
    let key = serving["value"]["key"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 64, "{key}");
    let recording = std::fs::read_to_string(dir.path("run.jsonl")).unwrap();
    assert!(!recording.contains(&key), "{recording}");
    let line = (recording.lines())
        .find(|line| line.contains("\"Serving\""))
        .expect("the recording has the serving event");
    let line: Value = serde_json::from_str(line).unwrap();
    assert_eq!(line["event"]["value"]["key"], "[hidden]");
    let event: Event = serde_json::from_value(line["event"].clone()).unwrap();
    let Event::Serving(read) = event else {
        panic!("{line}");
    };
    assert_eq!(read.key.0, [0; 32]);
    // Numbered after the first CAF in this process.
    assert_eq!(read.socket.parent(), Some(&*dir.path("s")));
    let name = read.socket.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with(&format!("caf-test-{}", std::process::id())) && name.ends_with(".sock"),
        "{name}"
    );
    // The socket is gone once CAF is.
    assert!(!read.socket.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn try_start_says_why_it_cant_serve_and_starts_nothing() {
    let dir = Dir::new();
    let mut config = common::config_for(host(), Vec::new());
    config.tools.relay = Relay::Command(SpawnCommand::new("/bin/true"));
    config.tools.folder = Some(dir.path("s"));
    config.record = Some(dir.path("run.jsonl"));
    let Err(error) = Caf::<NoAppItems>::try_start(config) else {
        panic!("it started");
    };
    assert!(
        matches!(&error, StartError::NoRelayHook { relay, .. } if relay == std::path::Path::new("/bin/true")),
        "{error:?}"
    );
    assert!(
        error.to_string().contains("caf_relay::relay_if_asked()"),
        "{error}"
    );
    assert!(!dir.path("run.jsonl").exists(), "no recording was made");
    assert!(!dir.path("s").exists(), "no socket folder was made");

    let mut config = common::config_for(host(), Vec::new());
    serve_with(&mut config, &dir);
    let long = dir.path(&"x".repeat(120));
    config.tools.folder = Some(long.clone());
    let Err(error) = Caf::<NoAppItems>::try_start(config) else {
        panic!("it started");
    };
    let StartError::SocketPathTooLong { path } = &error else {
        panic!("{error:?}");
    };
    assert!(path.starts_with(&long));
    assert!(!long.exists(), "the folder wasn't made");
    assert!(
        error.to_string().contains(&long.display().to_string()),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_newest_relay_for_a_chat_wins() {
    let dir = Dir::new();
    let mut steps = common::handshake("s1");
    // The session's servers, started again from here, as Claude may while
    // it opens a session again.
    steps[3]["wait"]["as"] = json!("new");
    steps.extend(start_relay());
    steps.push(prompt());
    steps.push(call_read(&dir));
    let path = dir.path("a.txt").display().to_string();
    steps.extend([
        json!({"wait_file": {"path": dir.path("asked").display().to_string(), "timeout": 30000}}),
        json!({"mcp_start": {"from": "new", "as": "second", "timeout": 30000}}),
        json!({"mcp_list": {"server": "second", "expect": ["Read", "Edit", "Write", "Bash"], "timeout": 30000}}),
        json!({"mcp_call": {"server": "second", "tool": "Read", "arguments": {"file_path": path}, "tool_use_id": "toolu_2", "report": "s1"}}),
        json!({"mcp_await": {"expect": {"result": {"isError": false}}, "timeout": 30000}}),
        end_turn(),
        json!({"note": "done"}),
    ]);
    let mut serving = serve(
        &steps,
        AskFirst::default(),
        Doing::Answers("contents"),
        None,
    );
    let chat = serving.chat().await;
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the first call's question", |chat| {
        !chat.questions.is_empty()
    })
    .await;
    std::fs::write(dir.path("asked"), "").unwrap();
    serving.started().await;
    serving.script_done().await;
    // The first relay was closed for the second, its call gone, and the
    // chat never lost its tools.
    let closed = tokio::time::timeout(PATIENCE, async {
        while !(serving.dir.record().iter()).any(|line| line["mcp_closed"] == "caf") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(closed.is_ok(), "{:#?}", serving.dir.record());
    assert_eq!(serving.events.relays(), ["Connected", "Connected"]);
    assert_eq!(
        serving.events.tool_events(),
        ["Called", "Gone", "Called", "Output", "Finished"]
    );
    let done = until(&chat, &mut watch, "the second call done", |chat| {
        (chat.turns.iter().flat_map(|turn| &turn.items)).any(
            |item| matches!(&item.kind, ItemKind::Tool(tool) if tool.status == ToolStatus::Done),
        )
    })
    .await;
    assert!(!has_note(&done, &Note::ToolsLost));
    // The first call's question went with it.
    let gone = until(&chat, &mut watch, "the first question withdrawn", |chat| {
        chat.questions.is_empty()
    })
    .await;
    let first = the_call(&gone).unwrap();
    assert_eq!(
        (first.status, first.stopped_by),
        (ToolStatus::Cancelled, Some(StoppedBy::Agent))
    );
    drop(dir);
    serving.shut_down().await;
}
