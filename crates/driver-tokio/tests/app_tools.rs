//! The app's own tools, end to end: the fake adapter starts the real
//! relay and calls the app's tools beside CAF's own, which the tools
//! driver reads, titles, previews and runs, leaving the app's items on
//! their calls.

mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use caf_core::agent::SpawnCommand;
use caf_core::chat::{AppItem, Chat, ItemKind, UserMessage};
use caf_core::directive::PermissionRequest;
use caf_core::event::PolicyAnswer;
use caf_core::ids::{ChatId, JobId};
use caf_core::mcp::Answer;
use caf_core::tool::{
    CafTool, Hints, Origin, Piece, StoppedBy, Text, ToolCall, ToolKind, ToolStatus, Tools,
};
use caf_core::{OpenChat, Refused};
use caf_driver_tokio::{
    AppTool, AppTools, BoxFuture, Caf, Call, ChatHandle, Config, Done, Error, Policy, Relay,
    Running, ToolSpec, tool,
};
use common::{Dir, PATIENCE, after_handshake, end_turn, fake, host, prompt, until};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The app's item: a clip it trimmed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Trimmed(u64);

impl AppItem for Trimmed {
    fn summary(&self) -> String {
        format!("Trimmed clip {}", self.0)
    }
}

/// Allows every call, keeping what it was asked.
struct AllowAll(Arc<Mutex<Vec<PermissionRequest>>>);

impl Policy for AllowAll {
    fn decide(
        &mut self,
        _: ChatId,
        request: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PolicyAnswer> + Send>> {
        self.0.lock().unwrap().push(request);
        Box::pin(std::future::ready(PolicyAnswer::Allow))
    }
}

/// A clip to trim.
#[derive(Deserialize, schemars::JsonSchema)]
struct Clip {
    /// The clip.
    clip: u64,
}

/// trim_clip: previews, then trims, leaving the clip trimmed as its item.
/// There's no clip 0: its preview fails, leaving an item all the same.
/// Clip 7 waits until it's dropped, saying when it starts and when it's
/// dropped in the test's folder; clip
/// 8 does too, answering early, as a job; clip 9 answers early, going on
/// as a job, until the test's `go` file is there.
struct Trim {
    work: PathBuf,
}

impl AppTool<Trimmed> for Trim {
    type Input = Clip;

    fn spec(&self) -> ToolSpec {
        let mut spec = ToolSpec::new("trim_clip");
        spec.title = "Trim a clip".into();
        spec.description = "Trims a clip.".into();
        spec.hints.destructive = true;
        spec.hints.kind = ToolKind::Edit;
        spec.previews = true;
        spec
    }

    fn title(&self, input: &Clip) -> String {
        format!("Trim clip {}", input.clip)
    }

    fn preview(
        &self,
        _call: &Call,
        input: &Clip,
    ) -> Option<BoxFuture<Result<Vec<Piece>, Done<Trimmed>>>> {
        let clip = input.clip;
        Some(Box::pin(async move {
            match clip {
                0 => Err(Done::failed("There's no clip 0").with_item(Trimmed(0))),
                clip => Ok(vec![Piece::Text(Text::new(format!(
                    "Would trim clip {clip}"
                )))]),
            }
        }))
    }

    fn run(&self, mut call: Call, input: Clip) -> BoxFuture<Done<Trimmed>> {
        let work = self.work.clone();
        Box::pin(async move {
            match input.clip {
                7 => {
                    let _dropped = Dropped(work.join("dropped"));
                    let _ = std::fs::write(work.join("started"), "");
                    std::future::pending::<()>().await;
                }
                8 => {
                    // Before it answers, so TaskStop always finds it.
                    let _dropped = Dropped(work.join("dropped"));
                    call.answer_now(|job| Answer::text(format!("Trimming as job {}", job.0)));
                    std::future::pending::<()>().await;
                }
                9 => {
                    call.answer_now(|job| Answer::text(format!("Trimming as job {}", job.0)));
                    while !work.join("go").exists() {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
                _ => {}
            }
            Done::answer(format!("Trimmed clip {}", input.clip))
                .with_record(vec![Piece::Text(Text::new("trimmed"))])
                .with_item(Trimmed(input.clip))
        })
    }
}

/// Writes its file when dropped.
struct Dropped(PathBuf);

impl Drop for Dropped {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, "");
    }
}

/// Some text to count the words of.
#[derive(Deserialize, schemars::JsonSchema)]
struct Words {
    /// The text.
    text: String,
}

/// A test's CAF, serving its own Read and the app's trim_clip and
/// word_count.
struct App {
    dir: Dir,
    work: PathBuf,
    caf: Caf<Trimmed>,
    running: Option<Running>,
    asked: Arc<Mutex<Vec<PermissionRequest>>>,
}

/// CAF serving through the fake's program, which runs `steps` (after the
/// handshake, the relay, its list and the first prompt) as the agent.
fn serve(steps: impl FnOnce(&Path) -> Vec<Value>) -> App {
    let dir = Dir::new();
    let work = dir.path("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(work).unwrap();
    let mut all = vec![
        json!({"mcp_start": {"discover": true, "expect": {"result": {"serverInfo": {"name": "caf"}}}, "timeout": 30000}}),
        json!({"mcp_list": {"expect": ["Read", "TaskStop", "trim_clip", "word_count"], "timeout": 30000}}),
        prompt(),
    ];
    all.extend(steps(&work));
    all.push(json!({"note": "done"}));
    let path = dir.script("script.jsonl", &after_handshake(&all));
    let mut config = Config::<Trimmed>::new(host(), vec![fake(&dir, &path)]);
    config.usage = None;
    let asked = Arc::default();
    config.policy = Box::new(AllowAll(Arc::clone(&asked)));
    config.tools.relay = Relay::Command(SpawnCommand::new(caf_fake_agent::binary()));
    config.tools.folder = Some(dir.path("s"));
    config.record = Some(dir.path("run.jsonl"));
    let words = tool("word_count", |_call, input: Words| async move {
        Done::answer(format!("{} words", input.text.split_whitespace().count()))
    })
    .title(|input: &Words| format!("Count {} bytes", input.text.len()))
    .describe("Counts the words in some text.")
    .read_only()
    .kind(ToolKind::Read);
    config.tools.app = AppTools::new()
        .with(Trim { work: work.clone() })
        .unwrap()
        .with(words)
        .unwrap();
    let (caf, running) = Caf::start(config);
    App {
        dir,
        work,
        caf,
        running: Some(running),
        asked,
    }
}

impl App {
    /// Opens a chat given Read and both the app's tools, and sends it a
    /// message.
    async fn chat(&self) -> ChatHandle<Trimmed> {
        let tools = Tools::caf([CafTool::Read]).and_app(["trim_clip", "word_count"]);
        let open = OpenChat::new("fake", &self.work).with_tools(tools);
        let chat = self.caf.open_chat(open).await.unwrap();
        chat.send(UserMessage::text("Go")).await.unwrap();
        chat
    }

    /// Waits for the fake's script to end, or a check of it to fail.
    async fn script_done(&self) {
        let ended = tokio::time::timeout(PATIENCE, async {
            loop {
                let record = self.dir.record();
                if let Some(failed) = record.iter().find(|line| line.get("failed").is_some()) {
                    panic!("the fake's check failed: {failed}");
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

    /// The texts of the prompts the agent was sent, in order.
    fn prompts(&self) -> Vec<String> {
        (self.dir.record().iter())
            .filter(|line| line["received"]["method"] == "session/prompt")
            .map(|line| {
                (line["received"]["params"]["prompt"]
                    .as_array()
                    .unwrap()
                    .iter())
                .filter_map(|block| block["text"].as_str())
                .collect()
            })
            .collect()
    }

    /// The events CAF recorded, as written.
    fn recorded(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir.path("run.jsonl")).unwrap_or_default();
        (text.lines())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The tool definitions the caf server listed.
    fn listed(&self) -> Vec<Value> {
        (self.dir.record().iter())
            .find_map(|line| {
                line["mcp_received"]["message"]["result"]["tools"]
                    .as_array()
                    .cloned()
            })
            .expect("the tools were listed")
    }

    async fn shut_down(&mut self) {
        let running = self.running.take().unwrap();
        let done = tokio::time::timeout(PATIENCE, async {
            self.caf.shutdown().await;
            running.await;
        })
        .await;
        assert!(done.is_ok(), "shutting down took too long");
    }
}

/// Calls `tool` with `arguments`, as call `id`, and checks its answer is
/// `expect`.
fn call(tool: &str, id: &str, arguments: Value, expect: Value) -> [Value; 2] {
    [
        json!({"mcp_call": {"tool": tool, "arguments": arguments, "tool_use_id": id, "report": "s1", "as": id}}),
        json!({"mcp_await": {"to": id, "expect": {"result": expect}, "timeout": 30000}}),
    ]
}

/// An answer of one text.
fn says(text: &str, error: bool) -> Value {
    json!({"isError": error, "content": [{"type": "text", "text": text}]})
}

/// The chat's tool calls, in order.
fn calls(chat: &Chat<Trimmed>) -> Vec<&ToolCall<Trimmed>> {
    (chat.turns.iter().flat_map(|turn| &turn.items))
        .filter_map(|item| match &item.kind {
            ItemKind::Tool(tool) => Some(&**tool),
            _ => None,
        })
        .collect()
}

fn ended(call: &ToolCall<Trimmed>) -> bool {
    call.status.ended()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_apps_tools_are_listed_called_and_leave_their_items() {
    let mut app = serve(|_| {
        [
            call(
                "trim_clip",
                "t1",
                json!({"clip": 3}),
                says("Trimmed clip 3", false),
            ),
            call(
                "word_count",
                "w1",
                json!({"text": "one two three"}),
                says("3 words", false),
            ),
            [end_turn(), json!({"note": "turn ended"})],
        ]
        .concat()
    });
    let chat = app.chat().await;
    let mut watch = chat.watch();
    app.script_done().await;
    let done = until(&chat, &mut watch, "both calls ended", |chat| {
        calls(chat).len() == 2 && calls(chat).iter().all(|call| ended(call))
    })
    .await;
    let all = calls(&done);
    let trim = all[0];
    assert_eq!(trim.status, ToolStatus::Done);
    assert_eq!(trim.title, "Trim clip 3");
    assert_eq!(trim.kind, ToolKind::Edit);
    assert_eq!(trim.origin, Origin::App("trim_clip".into()));
    assert_eq!(trim.app, Some(Trimmed(3)));
    // What it previewed is kept, and what it recorded follows.
    assert_eq!(
        trim.result,
        [
            Piece::Text(Text::new("Would trim clip 3")),
            Piece::Text(Text::new("trimmed"))
        ]
    );
    let words = all[1];
    assert_eq!(words.status, ToolStatus::Done);
    assert_eq!(words.title, "Count 13 bytes");
    assert_eq!(words.kind, ToolKind::Read);
    assert_eq!(words.origin, Origin::App("word_count".into()));
    assert_eq!(words.app, None);
    // The policy was asked of each, with its hints.
    let asked = app.asked.lock().unwrap().clone();
    let hints: Vec<Option<Hints>> = asked.iter().map(|request| request.hints).collect();
    let mut destructive = Hints::default();
    destructive.destructive = true;
    destructive.kind = ToolKind::Edit;
    let mut read_only = Hints::default();
    read_only.read_only = true;
    read_only.kind = ToolKind::Read;
    assert_eq!(hints, [Some(destructive), Some(read_only)]);
    // The app's are listed after CAF's, with their schemas and hints.
    let listed = app.listed();
    let names: Vec<&str> = listed
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    // TaskStop is served with the app's tools, so their jobs can be
    // stopped.
    assert_eq!(names, ["Read", "TaskStop", "trim_clip", "word_count"]);
    assert_eq!(listed[3]["description"], "Counts the words in some text.");
    assert_eq!(
        listed[3]["inputSchema"]["properties"]["text"]["description"],
        "The text."
    );
    assert_eq!(listed[3]["annotations"]["readOnlyHint"], true);
    assert_eq!(listed[2]["annotations"]["destructiveHint"], true);
    app.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_tools_preview_is_shown_before_its_asked_about() {
    let mut app = serve(|_| {
        [
            call(
                "trim_clip",
                "t1",
                json!({"clip": 4}),
                says("Trimmed clip 4", false),
            ),
            [end_turn(), json!({"note": "turn ended"})],
        ]
        .concat()
    });
    let chat = app.chat().await;
    let mut watch = chat.watch();
    app.script_done().await;
    until(&chat, &mut watch, "the call ended", |chat| {
        calls(chat).len() == 1 && ended(calls(chat)[0])
    })
    .await;
    let asked = app.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].title, "Trim clip 4");
    assert_eq!(asked[0].origin, Origin::App("trim_clip".into()));
    app.shut_down().await;
    // It was previewed, and the policy answered after.
    let events: Vec<String> = (app.recorded().iter())
        .map(|line| line["event"].to_string())
        .collect();
    let previewed = (events.iter())
        .position(|event| event.contains("Previewed") && event.contains("Would trim clip 4"))
        .expect("it was previewed");
    let decided = (events.iter())
        .position(|event| event.starts_with(r#"{"type":"Policy""#))
        .expect("the policy decided");
    assert!(previewed < decided, "{events:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_tools_call_whose_input_doesnt_fit_fails_asking_no_one() {
    let mut app = serve(|_| {
        [
            vec![
                json!({"mcp_call": {"tool": "trim_clip", "arguments": {"clip": "three"}, "tool_use_id": "t1", "report": "s1", "as": "t1"}}),
                json!({"mcp_await": {"to": "t1", "expect": {"result": {"isError": true}}, "timeout": 30000}}),
            ],
            vec![end_turn(), json!({"note": "turn ended"})],
        ]
        .concat()
    });
    let chat = app.chat().await;
    let mut watch = chat.watch();
    app.script_done().await;
    let done = until(&chat, &mut watch, "the call ended", |chat| {
        calls(chat).len() == 1 && ended(calls(chat)[0])
    })
    .await;
    let trim = calls(&done)[0];
    assert_eq!(trim.status, ToolStatus::Failed);
    assert_eq!(trim.title, "Trim a clip");
    assert_eq!(trim.app, None);
    assert!(
        app.asked.lock().unwrap().is_empty(),
        "the policy wasn't asked"
    );
    let answer = (app.dir.record().iter())
        .find_map(|line| {
            let message = &line["mcp_received"]["message"];
            message["result"]["isError"]
                .as_bool()
                .filter(|&error| error)
                .map(|_| {
                    message["result"]["content"][0]["text"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
        })
        .unwrap();
    assert!(answer.starts_with("Refused: invalid type"), "{answer}");
    app.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_turn_drops_an_app_tools_run() {
    let mut app = serve(|_| {
        vec![
            json!({"mcp_call": {"tool": "trim_clip", "arguments": {"clip": 7}, "tool_use_id": "t1", "report": "s1", "as": "t1"}}),
            json!({"on": {"method": "session/cancel", "do": [{"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}}]}}),
            json!({"mcp_await": {"to": "t1", "expect": {"result": {"isError": true}}, "timeout": 30000}}),
        ]
    });
    let chat = app.chat().await;
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the call running", |chat| {
        calls(chat)
            .first()
            .is_some_and(|call| call.status == ToolStatus::Running)
    })
    .await;
    // Running, as the core has it, may be before its future is first
    // polled, on the app's tools' own threads: so before it has anything
    // to drop.
    tokio::time::timeout(PATIENCE, async {
        while !app.work.join("started").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("its run started");
    chat.stop().await.unwrap();
    app.script_done().await;
    let done = until(&chat, &mut watch, "the call cancelled", |chat| {
        calls(chat).first().is_some_and(|call| ended(call))
    })
    .await;
    assert_eq!(calls(&done)[0].status, ToolStatus::Cancelled);
    assert!(app.work.join("dropped").exists(), "its run was dropped");
    app.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_tool_answering_early_goes_on_as_a_job_and_its_end_is_a_notice() {
    let mut app = serve(|_| {
        vec![
            json!({"mcp_call": {"tool": "trim_clip", "arguments": {"clip": 9}, "tool_use_id": "t1", "report": "s1", "as": "t1"}}),
            json!({"mcp_await": {"to": "t1", "expect": {"result": says("Trimming as job 1", false)}, "timeout": 30000}}),
            end_turn(),
            json!({"wait": {"method": "session/prompt", "as": "notice", "timeout": 30000}}),
            json!({"respond": {"to": "notice", "result": {"stopReason": "end_turn"}}}),
        ]
    });
    let chat = app.chat().await;
    let mut watch = chat.watch();
    until(&chat, &mut watch, "it went on as a job", |chat| {
        calls(chat).first().is_some_and(|call| call.job.is_some())
    })
    .await;
    std::fs::write(app.work.join("go"), "").unwrap();
    let done = until(&chat, &mut watch, "the job ended", |chat| {
        calls(chat)
            .first()
            .is_some_and(|call| ended(call) && call.job.is_some())
    })
    .await;
    app.script_done().await;
    let trim = calls(&done)[0];
    assert_eq!((trim.status, trim.job), (ToolStatus::Done, Some(JobId(1))));
    assert_eq!(trim.app, Some(Trimmed(9)));
    let prompts = app.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:#?}");
    let notice = &prompts[1];
    assert!(notice.contains("<tool-use-id>t1</tool-use-id>"), "{notice}");
    assert!(notice.contains("<status>completed</status>"), "{notice}");
    assert!(
        notice.contains("<result>Trimmed clip 9</result>"),
        "{notice}"
    );
    app.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_given_an_app_tool_never_registered_is_refused() {
    let mut app = serve(|_| Vec::new());
    let tools = Tools::caf([CafTool::Read]).and_app(["trim_clip", "nope"]);
    let open = OpenChat::new("fake", &app.work).with_tools(tools);
    match app.caf.open_chat(open).await {
        Err(Error::Refused(Refused::UnknownTool(name))) => assert_eq!(name, "nope"),
        other => panic!("expected UnknownTool, got {:?}", other.map(|_| ())),
    }
    app.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_tools_preview_that_fails_ends_its_call_with_its_item() {
    let mut app = serve(|_| {
        [
            call(
                "trim_clip",
                "t1",
                json!({"clip": 0}),
                says("There's no clip 0", true),
            ),
            [end_turn(), json!({"note": "turn ended"})],
        ]
        .concat()
    });
    let chat = app.chat().await;
    let mut watch = chat.watch();
    app.script_done().await;
    let done = until(&chat, &mut watch, "the call ended", |chat| {
        calls(chat).len() == 1 && ended(calls(chat)[0])
    })
    .await;
    let trim = calls(&done)[0];
    assert_eq!(trim.status, ToolStatus::Failed);
    assert_eq!(trim.app, Some(Trimmed(0)));
    assert!(
        app.asked.lock().unwrap().is_empty(),
        "the policy wasn't asked"
    );
    app.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_stop_stops_an_app_tools_job_before_it_answers() {
    let mut app = serve(|_| {
        [
            vec![
                json!({"mcp_call": {"tool": "trim_clip", "arguments": {"clip": 8}, "tool_use_id": "t1", "report": "s1", "as": "t1"}}),
                json!({"mcp_await": {"to": "t1", "expect": {"result": says("Trimming as job 1", false)}, "timeout": 30000}}),
            ],
            call("TaskStop", "s1", json!({"task_id": "1"}), says("Stopped task 1", false)).to_vec(),
            vec![end_turn(), json!({"note": "turn ended"})],
        ]
        .concat()
    });
    let chat = app.chat().await;
    let mut watch = chat.watch();
    app.script_done().await;
    let done = until(&chat, &mut watch, "both calls ended", |chat| {
        calls(chat).len() == 2 && calls(chat).iter().all(|call| ended(call))
    })
    .await;
    // Its task was stopped before TaskStop was answered.
    assert!(app.work.join("dropped").exists(), "its task was stopped");
    let all = calls(&done);
    assert_eq!(
        (all[0].status, all[0].job, all[0].stopped_by),
        (
            ToolStatus::Cancelled,
            Some(JobId(1)),
            Some(StoppedBy::Agent)
        )
    );
    assert_eq!(all[1].status, ToolStatus::Done);
    app.shut_down().await;
    // The job's end came before the TaskStop's, and with no notice: the
    // agent stopped it, so it isn't told it ended.
    let finished: Vec<(u64, Value)> = (app.recorded().iter())
        .filter(|line| line["event"]["type"] == "Tool")
        .filter(|line| line["event"]["value"]["event"]["type"] == "Finished")
        .map(|line| {
            let value = &line["event"]["value"];
            (
                value["call"].as_u64().unwrap(),
                value["event"]["value"]["notice"].clone(),
            )
        })
        .collect();
    assert_eq!(finished.len(), 2, "{finished:?}");
    assert!(
        finished[0].0 < finished[1].0,
        "the job's first: {finished:?}"
    );
    assert_eq!(finished[0].1, Value::Null);
    assert_eq!(app.prompts().len(), 1, "no notice turn");
}
