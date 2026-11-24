//! Background jobs: calls that go on after the model was answered, past
//! their turn, until they end by themselves, TaskStop stops them, or
//! their chat is let go; the notice of each one's end; TaskStop; a
//! preview that fails; the shell's folder; and the app's notices.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, sent, with, writes};
use super::served::{PROMPT, called, kind, one_told, running, the_tool, ticket, told, tools};
use super::{CHAT, PROC, SESSION};
use crate::core::Refused;
use crate::event::{Exit, PolicyAnswer, ToolEvent};
use crate::ids::JobId;
use crate::tool::{CafTool, Ending, Output, Piece, Text};

/// What the agent is told of job 1's end, in these tests.
const NOTICE: &str = "<task-notification>\n<task-id>1</task-id>\n</task-notification>";

/// A turn running Bash's `command` in the background, allowed, which
/// became job 1: call 1.
fn job_running(command: &str) -> Script {
    let mut script = running();
    let out = script.tool(
        1,
        called(
            CafTool::Bash,
            json!({ "command": command, "run_in_background": true }),
            Some("toolu_1"),
        ),
    );
    script.policy(ticket(&out), PolicyAnswer::Allow);
    script.tool(1, ToolEvent::Detached { job: JobId(1) });
    script
}

/// A call ending, `done` or not, with `pieces`, its shell in `folder`,
/// and the agent told `notice`.
fn finished(
    done: bool,
    pieces: Vec<Piece>,
    folder: Option<&str>,
    notice: Option<&str>,
) -> ToolEvent {
    ToolEvent::Finished {
        done,
        pieces,
        folder: folder.map(str::to_owned),
        notice: notice.map(str::to_owned),
    }
}

/// A command's output, ended as `ending` says.
fn ended(ending: Ending) -> Piece {
    Piece::Output(Output {
        lines: Vec::new(),
        dropped: 0,
        ending: Some(ending),
    })
}

/// The chat's turns' starters and lives, in order.
fn turns(script: &Script) -> Vec<(Value, Value)> {
    (script.chat(CHAT)["turns"].as_array().into_iter().flatten())
        .map(|turn| (turn["starter"].clone(), turn["life"].clone()))
        .collect()
}

/// The text of the one prompt sent in `out`, and its request id.
fn prompted(out: &[Value]) -> (String, u64) {
    let prompt = sent(out, "session/prompt");
    let text = prompt["params"]["prompt"][0]["text"]
        .as_str()
        .expect("text");
    (text.to_owned(), prompt["id"].as_u64().expect("an id"))
}

#[test]
fn a_job_outlives_its_turn_and_its_end_is_sent_as_a_notice() {
    let mut script = job_running("cargo test");
    assert_has(
        &the_tool(&script),
        &json!({ "status": kind("Running"), "job": 1 }),
    );
    // The turn ends with the job still running: nothing is cancelled.
    let out = script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    assert!(told(&out).is_empty(), "{out:#?}");
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Running"));
    assert_eq!(tool["end"]["type"], "Open");
    assert_eq!(turns(&script)[0].1, kind("Finished"));
    // Its output still comes.
    script.tool(
        1,
        ToolEvent::Output {
            from: 0,
            lines: vec!["running 3 tests".to_owned()],
        },
    );
    assert_eq!(
        the_tool(&script)["result"][0]["value"]["lines"],
        json!(["running 3 tests"])
    );
    // It ends, and the agent is told, in a notice turn of its own.
    let out = script.tool(
        1,
        finished(
            true,
            vec![ended(Ending::Exited(Exit::Code(0)))],
            Some("/elsewhere"),
            Some(NOTICE),
        ),
    );
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Done"));
    assert_ne!(tool["end"]["type"], "Open");
    let (text, id) = prompted(&out);
    assert_eq!(text, NOTICE);
    assert_eq!(
        turns(&script)[1],
        (
            json!({ "type": "Notice", "value": NOTICE }),
            kind("Running")
        )
    );
    script.answers(PROC, id, &json!({ "stopReason": "end_turn" }));
    assert_eq!(turns(&script)[1].1, kind("Finished"));
    // A job's shell isn't the chat's: the next command runs in its folder.
    let out = script.send(CHAT, "again");
    let next = prompted(&out.1).1;
    let out = script.tool(2, called(CafTool::Bash, json!({ "command": "ls" }), None));
    assert_eq!(
        with(&out, "decide")[0]["action"]["value"]["folder"],
        "/work"
    );
    script.answers(PROC, next, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn a_jobs_notice_waits_for_the_turn_running_and_a_finish_without_one_sends_none() {
    let mut script = job_running("make");
    script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    let (_, out) = script.send(CHAT, "meanwhile");
    let (_, second) = prompted(&out);
    // It ends while another turn runs: its notice waits for it.
    let out = script.tool(
        1,
        finished(
            false,
            vec![ended(Ending::Exited(Exit::Signal(9)))],
            None,
            Some(NOTICE),
        ),
    );
    assert!(writes(&out).is_empty(), "{out:#?}");
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
    assert_eq!(turns(&script)[2].1, kind("Waiting"));
    let out = script.answers(PROC, second, &json!({ "stopReason": "end_turn" }));
    let (text, third) = prompted(&out);
    assert_eq!(text, NOTICE);
    script.answers(PROC, third, &json!({ "stopReason": "end_turn" }));

    // One that ends with no notice is just ended.
    let mut script = job_running("make");
    script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    let out = script.tool(1, finished(true, Vec::new(), None, None));
    assert!(writes(&out).is_empty());
    assert_eq!(turns(&script).len(), 1);
    // And a call that isn't a job sends none, whatever the driver says.
    let mut script = running();
    let out = script.tool(1, called(CafTool::Read, json!({ "file_path": "/a" }), None));
    script.policy(ticket(&out), PolicyAnswer::Allow);
    let out = script.tool(1, finished(true, Vec::new(), None, Some(NOTICE)));
    assert!(writes(&out).is_empty());
    assert_eq!(turns(&script).len(), 1);
}

#[test]
fn the_users_stop_and_the_agent_letting_go_leave_a_job_running() {
    let mut script = job_running("sleep 100");
    let (stopped, out) = script.stop(CHAT);
    stopped.expect("stopping");
    assert!(told(&out).is_empty());
    let out = script.answers(PROC, PROMPT, &json!({ "stopReason": "cancelled" }));
    assert!(told(&out).is_empty(), "{out:#?}");
    assert_eq!(the_tool(&script)["status"], kind("Running"));
    // Its connection dropping doesn't end it either.
    let out = script.tool(1, ToolEvent::Gone);
    assert!(told(&out).is_empty());
    assert_eq!(the_tool(&script)["status"], kind("Running"));
    assert_eq!(script.core.calls.len(), 1);
    // A second Detached, or one before it ran, changes nothing.
    script.tool(1, ToolEvent::Detached { job: JobId(2) });
    assert_eq!(the_tool(&script)["job"], 1);
}

#[test]
fn a_job_in_a_turn_the_agent_started_lets_the_turn_go_quiet() {
    let mut script = Script::with_claude_serving();
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "On it" } }),
    );
    let out = script.tool(
        1,
        called(
            CafTool::Bash,
            json!({ "command": "make", "run_in_background": true }),
            None,
        ),
    );
    script.policy(ticket(&out), PolicyAnswer::Allow);
    script.tool(1, ToolEvent::Detached { job: JobId(1) });
    script.at(61_000);
    script.woken();
    assert_eq!(turns(&script)[0].1, kind("Finished"));
    assert_eq!(the_tool(&script)["status"], kind("Running"));
}

#[test]
fn task_stop_stops_the_chats_own_job_unasked() {
    let mut script = job_running("sleep 100");
    // Neither an unknown job, nor one that isn't a number, is stopped.
    for (call, id) in [(2, "9"), (5, "b2")] {
        let out = script.tool(
            call,
            called(CafTool::TaskStop, json!({ "task_id": id }), None),
        );
        assert_eq!(
            one_told(&out),
            json!({ "type": "Refuse", "value": { "call": call, "why": {
                "type": "BadInput", "value": format!("No task {id} in this conversation"),
            } } })
        );
    }
    // Its own job is, with no one asked.
    let out = script.tool(
        3,
        called(
            CafTool::TaskStop,
            json!({ "task_id": "1" }),
            Some("toolu_3"),
        ),
    );
    assert!(with(&out, "decide").is_empty());
    assert_eq!(
        one_told(&out),
        json!({ "type": "Run", "value": { "call": 3, "action": { "type": "Stop", "value": { "job": 1 } } } })
    );
    let stop = tools(&script)[1].clone();
    assert_has(
        &stop,
        &json!({
            "title": "Stop task 1", "kind": kind("Other"), "status": kind("Running"),
            "origin": { "type": "Caf", "value": kind("TaskStop") }, "decision": kind("NotAsked"),
        }),
    );
    // The driver ends the job, then the TaskStop.
    script.tool(
        1,
        finished(
            false,
            vec![ended(Ending::Exited(Exit::Signal(15)))],
            None,
            None,
        ),
    );
    script.tool(
        3,
        finished(
            true,
            vec![Piece::Text(Text::new("Stopped task 1"))],
            None,
            None,
        ),
    );
    let tools = tools(&script);
    assert_has(
        &tools[0],
        &json!({ "status": kind("Cancelled"), "stopped_by": kind("Agent") }),
    );
    assert_eq!(tools[1]["status"], kind("Done"));
    // Once it's ended, it's no task to stop.
    let out = script.tool(
        4,
        called(CafTool::TaskStop, json!({ "task_id": "1" }), None),
    );
    assert_eq!(one_told(&out)["type"], "Refuse");
}

#[test]
fn task_stop_never_stops_another_chats_job() {
    let mut script = job_running("sleep 100");
    let other = crate::ids::ChatId::from_number(9);
    let out = script.tool(
        2,
        ToolEvent::Called {
            chat: other,
            tool: CafTool::TaskStop,
            input: json!({ "task_id": "1" }),
            agent_call: None,
        },
    );
    assert_eq!(one_told(&out)["type"], "Refuse");
    assert_eq!(the_tool(&script)["status"], kind("Running"));
}

#[test]
fn a_chat_closed_cancels_its_jobs_and_shutting_down_fails_them() {
    let mut script = job_running("sleep 100");
    script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    let (closed, out) = script.close(CHAT);
    closed.expect("closed");
    assert!(told(&out).contains(&json!({ "type": "Cancel", "value": { "call": 1 } })));
    assert_has(
        &the_tool(&script),
        &json!({ "status": kind("Cancelled"), "stopped_by": kind("User") }),
    );
    assert!(script.core.calls.is_empty());

    // An idle chat's jobs stop as CAF shuts down, cut off with it.
    let mut script = job_running("sleep 100");
    script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    let out = script.shutdown();
    assert!(told(&out).contains(&json!({ "type": "Cancel", "value": { "call": 1 } })));
    let tool = the_tool(&script);
    assert_has(
        &tool,
        &json!({ "status": kind("Failed"), "stopped_by": null }),
    );
    assert_eq!(
        tool["result"][0]["value"]["ending"],
        kind("Unknown"),
        "its output ends, not saying how"
    );
    assert!(script.core.calls.is_empty());
}

#[test]
fn a_restored_chats_job_is_failed_in_its_finished_turn() {
    let mut script = job_running("sleep 100");
    script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    let chat: crate::chat::Chat = serde_json::from_value(script.chat(CHAT)).expect("a chat");
    let now = script.now().wall;
    let changes = crate::core::saving::tidy(&chat, now, now);
    let changes = serde_json::to_value(&changes).expect("JSON");
    let changes = changes.as_array().expect("a list");
    assert_eq!(changes[0]["type"], "ToolChanged");
    assert_eq!(changes[0]["value"]["tool"]["status"], kind("Failed"));
    assert_eq!(changes[1]["type"], "ItemEnded");
    assert!(
        changes.iter().all(|change| change["type"] != "TurnLife"),
        "its turn stays finished: {changes:#?}"
    );
}

#[test]
fn a_preview_that_fails_ends_the_call_and_asks_no_one() {
    let mut script = running();
    let out = script.tool(
        1,
        called(
            CafTool::Edit,
            json!({ "file_path": "/a", "old_string": "x", "new_string": "y" }),
            None,
        ),
    );
    assert_eq!(one_told(&out)["type"], "Preview");
    let why = "File has not been read yet. Read it first before writing to it.";
    let out = script.tool(
        1,
        finished(false, vec![Piece::Text(Text::new(why))], None, None),
    );
    assert!(with(&out, "decide").is_empty());
    assert!(told(&out).is_empty(), "the driver answered the model");
    let tool = the_tool(&script);
    assert_has(
        &tool,
        &json!({ "status": kind("Failed"), "decision": kind("Undecided") }),
    );
    assert_eq!(tool["result"][0]["value"]["text"], why);
    assert_ne!(tool["end"]["type"], "Open");
    assert!(script.core.calls.is_empty());
    // A preview that says it did what it was asked isn't taken as one.
    let out = script.tool(
        2,
        called(
            CafTool::Write,
            json!({ "file_path": "/b", "content": "" }),
            None,
        ),
    );
    assert_eq!(one_told(&out)["type"], "Preview");
    script.tool(2, finished(true, Vec::new(), None, None));
    assert_eq!(tools(&script)[1]["status"], kind("Pending"));
}

#[test]
fn a_command_runs_where_the_last_ones_shell_ended() {
    let mut script = running();
    let bash = |script: &mut Script, call: u64| {
        let out = script.tool(
            call,
            called(CafTool::Bash, json!({ "command": "cd src" }), None),
        );
        let folder = with(&out, "decide")[0]["action"]["value"]["folder"].clone();
        script.policy(ticket(&out), PolicyAnswer::Allow);
        folder
    };
    assert_eq!(bash(&mut script, 1), "/work");
    script.tool(1, finished(true, Vec::new(), Some("/work/src"), None));
    assert_eq!(bash(&mut script, 2), "/work/src");
    // One that isn't absolute is ignored.
    script.tool(2, finished(true, Vec::new(), Some("lib"), None));
    assert_eq!(bash(&mut script, 3), "/work/src");
    // Files are still found from the chat's folder.
    script.tool(3, finished(true, Vec::new(), None, None));
    let out = script.tool(
        4,
        called(CafTool::Read, json!({ "file_path": "a.rs" }), None),
    );
    assert_eq!(
        with(&out, "decide")[0]["action"]["value"]["path"],
        "/work/a.rs"
    );
    script.policy(ticket(&out), PolicyAnswer::Refuse);
}

#[test]
fn the_app_notifies_the_agent_with_a_notice_turn() {
    let mut script = Script::with_claude_serving();
    let (turn, out) = script.notify(CHAT, "The build failed");
    turn.expect("queued");
    let (text, id) = prompted(&out);
    assert_eq!(text, "The build failed");
    assert_eq!(
        turns(&script)[0],
        (
            json!({ "type": "Notice", "value": "The build failed" }),
            kind("Running")
        )
    );
    // While it runs, another waits.
    let (_, out) = script.notify(CHAT, "And the tests");
    assert!(writes(&out).is_empty());
    assert_eq!(turns(&script)[1].1, kind("Waiting"));
    let out = script.answers(PROC, id, &json!({ "stopReason": "end_turn" }));
    assert_eq!(prompted(&out).0, "And the tests");
    assert_eq!(script.notify(CHAT, "").0, Err(Refused::Unsupported));
    assert_eq!(
        script.notify(crate::ids::ChatId::from_number(9), "x").0,
        Err(Refused::NoSuchChat)
    );
}
