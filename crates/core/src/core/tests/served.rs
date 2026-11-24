//! Calls to the tools CAF serves: refused, or joined to the agent's
//! report of them in either order, previewed, decided by the policy or
//! the user, run, and settled, each answered exactly once.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, host, with};
use super::stop::permission;
use super::{CHAT, PROC, SESSION};
use crate::core::Refused;
use crate::event::{Exit, PolicyAnswer, ToolEvent};
use crate::ids::QuestionId;
use crate::question::{Answer, OptionId};
use crate::tool::{CafTool, Diff, Ending, MOST_LINE, MOST_OUTPUT, Output, Piece, ServedTool, Text};

/// The prompt's request id, as the first turn sends it.
pub(super) const PROMPT: u64 = 1002;

/// A Claude chat with a turn running.
pub(super) fn running() -> Script {
    let mut script = Script::with_claude_serving();
    script.send(CHAT, "go").0.expect("sent");
    script
}

/// `open`, given all of CAF's tools.
fn given(open: crate::core::OpenChat) -> crate::core::OpenChat {
    open.with_tools(crate::tool::Tools::caf(CafTool::ALL))
}

/// The agent calls `tool` with `input`, giving its id for the call if
/// `agent_call`.
pub(super) fn called(tool: CafTool, input: Value, agent_call: Option<&str>) -> ToolEvent {
    ToolEvent::Called {
        chat: CHAT,
        tool: ServedTool::Caf(tool),
        input,
        agent_call: agent_call.map(str::to_owned),
        title: None,
        bad_input: None,
    }
}

/// Claude's report of a call to CAF's `tool`, as `id`.
fn report(id: &str, tool: &str, status: &str, input: &Value) -> Value {
    json!({
        "sessionUpdate": "tool_call", "toolCallId": id, "title": format!("mcp__caf__{tool}"),
        "kind": "other", "status": status, "rawInput": input,
        "_meta": { "claudeCode": { "toolName": format!("mcp__caf__{tool}") } },
    })
}

/// The tool directives in `out`, as `[type, value]`.
pub(super) fn told(out: &[Value]) -> Vec<Value> {
    with(out, "tool")
        .into_iter()
        .map(|each| each["tool"].clone())
        .collect()
}

/// The one tool directive in `out`.
pub(super) fn one_told(out: &[Value]) -> Value {
    let told = told(out);
    assert_eq!(told.len(), 1, "{out:#?}");
    told[0].clone()
}

/// The ticket of the one decision asked for in `out`.
pub(super) fn ticket(out: &[Value]) -> u64 {
    let decided = with(out, "decide");
    assert_eq!(decided.len(), 1, "{out:#?}");
    decided[0]["decide"].as_u64().expect("a ticket")
}

/// The chat's tool calls.
pub(super) fn tools(script: &Script) -> Vec<Value> {
    (script.chat(CHAT)["turns"].as_array().into_iter().flatten())
        .flat_map(|turn| turn["items"].as_array().cloned().unwrap_or_default())
        .filter(|item| item["kind"]["type"] == "Tool")
        .map(|item| {
            let mut tool = item["kind"]["value"].clone();
            tool["id"] = item["id"].clone();
            tool["end"] = item["window"]["end"].clone();
            tool
        })
        .collect()
}

/// The chat's one tool call.
pub(super) fn the_tool(script: &Script) -> Value {
    let tools = tools(script);
    assert_eq!(tools.len(), 1, "{tools:#?}");
    tools[0].clone()
}

pub(super) fn kind(name: &str) -> Value {
    json!({ "type": name })
}

/// Bash running `command`, asked about and allowed: returns the script.
pub(super) fn bash_running(command: &str) -> Script {
    let mut script = running();
    let out = script.tool(
        1,
        called(
            CafTool::Bash,
            json!({ "command": command }),
            Some("toolu_1"),
        ),
    );
    script.policy(ticket(&out), PolicyAnswer::Allow);
    script
}

/// The lines kept of the one call's output.
fn output(script: &Script) -> Value {
    the_tool(script)["result"][0]["value"].clone()
}

#[test]
fn a_call_comes_first_makes_the_item_and_the_report_joins_it() {
    let mut script = running();
    let input = json!({ "file_path": "src/../src/main.rs", "offset": 3, "limit": 2 });
    script.at(10);
    let out = script.tool(1, called(CafTool::Read, input.clone(), Some("toolu_1")));
    assert_eq!(
        with(&out, "decide"),
        [json!({
            "decide": 1005, "chat": CHAT, "item": 1004, "title": "Read src/main.rs",
            "origin": { "type": "Caf", "value": { "type": "Read" } },
            "action": { "type": "Read", "value": { "path": "/work/src/main.rs", "lines": { "first": 3, "last": 4 } } },
            "input": input.to_string(),
            // CAF's own tools say what they are too.
            "hints": { "read_only": true, "destructive": false, "open_world": false, "kind": { "type": "Read" } },
        })]
    );
    let tool = the_tool(&script);
    assert_has(
        &tool,
        &json!({
            "title": "Read src/main.rs", "kind": kind("Read"), "status": kind("Pending"),
            "origin": { "type": "Caf", "value": kind("Read") },
            "input": input, "decision": kind("Undecided"),
            "locations": [{ "path": "/work/src/main.rs", "line": 3 }],
        }),
    );
    // The agent's report changes nothing of CAF's record.
    script.at(20);
    script.update(PROC, SESSION, &report("toolu_1", "Read", "pending", &input));
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "toolu_1", "status": "failed", "title": "Oops" }),
    );
    assert_eq!(the_tool(&script), tool);
    script.policy(1005, PolicyAnswer::Allow);
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: Vec::new(),
            folder: None,
            notice: None,
            item: None,
        },
    );
}

#[test]
fn a_report_comes_first_makes_the_item_and_the_call_joins_it() {
    let mut script = running();
    let input = json!({ "command": "cargo test\ncargo build" });
    script.at(10);
    script.update(
        PROC,
        SESSION,
        &report("toolu_1", "Bash", "pending", &json!({})),
    );
    let started = the_tool(&script)["id"].clone();
    script.at(20);
    let out = script.tool(7, called(CafTool::Bash, input.clone(), Some("toolu_1")));
    let tool = the_tool(&script);
    assert_eq!(tool["id"], started, "one call, one item");
    assert_has(
        &tool,
        &json!({
            "title": "cargo test …", "kind": kind("Execute"), "status": kind("Pending"),
            "origin": { "type": "Caf", "value": kind("Bash") },
            "action": { "type": "Run", "value": { "command": "cargo test\ncargo build", "folder": "/work", "background": false } },
            "input": input,
        }),
    );
    // It started when the agent reported it.
    let item = &script.chat(CHAT)["turns"][0]["items"][0];
    assert_eq!(item["window"]["start"], json!(super::harness::EPOCH + 10));
    script.policy(ticket(&out), PolicyAnswer::Refuse);
}

#[test]
fn without_the_agents_id_a_call_joins_the_oldest_report_with_the_same_input() {
    let mut script = Script::new(host());
    script.serving();
    script
        .open(given(crate::core::OpenChat::new("claude", "/work")))
        .0
        .expect("opened");
    script.answers(PROC, 1000, &super::claude_init());
    script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": SESSION }),
    );
    script.send(CHAT, "go").0.expect("sent");
    let a = json!({ "file_path": "/work/a" });
    let b = json!({ "file_path": "/work/b" });
    script.update(PROC, SESSION, &report("r1", "Read", "pending", &a));
    script.update(PROC, SESSION, &report("r2", "Read", "pending", &b));
    script.update(PROC, SESSION, &report("r3", "Read", "pending", &b));
    let ids: Vec<Value> = tools(&script)
        .iter()
        .map(|tool| tool["id"].clone())
        .collect();
    let out = script.tool(1, called(CafTool::Read, b.clone(), None));
    script.policy(ticket(&out), PolicyAnswer::Allow);
    let out = script.tool(2, called(CafTool::Read, b.clone(), None));
    script.policy(ticket(&out), PolicyAnswer::Allow);
    let titles: Vec<Value> = tools(&script)
        .iter()
        .map(|tool| tool["title"].clone())
        .collect();
    assert_eq!(tools(&script).len(), 3, "joined, not added");
    assert_eq!(titles[1], "Read b");
    assert_eq!(titles[2], "Read b");
    assert_eq!(tools(&script)[1]["id"], ids[1], "the oldest first");
    // A call to another tool, or with other input, joins nothing.
    let out = script.tool(
        3,
        called(
            CafTool::Edit,
            json!({ "file_path": "/work/a", "old_string": "x", "new_string": "y" }),
            None,
        ),
    );
    assert_eq!(tools(&script).len(), 4);
    assert_eq!(one_told(&out)["type"], "Preview");
    // And a call that comes first is joined by a report of the same.
    let c = json!({ "file_path": "/work/c" });
    let out = script.tool(4, called(CafTool::Read, c.clone(), None));
    script.update(PROC, SESSION, &report("r4", "Read", "pending", &c));
    assert_eq!(tools(&script).len(), 5);
    assert_eq!(tools(&script)[4]["title"], "Read c");
    script.policy(ticket(&out), PolicyAnswer::Refuse);
    script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn a_call_with_no_turn_running_or_input_that_makes_no_action_is_refused() {
    let mut script = Script::with_claude_serving();
    let out = script.tool(1, called(CafTool::Read, json!({ "file_path": "/a" }), None));
    assert_eq!(
        one_told(&out),
        json!({ "type": "Refuse", "value": { "call": 1, "why": { "type": "NotRunning" } } })
    );
    let out = script.event(crate::Event::Tool {
        call: crate::ids::CallId(2),
        event: ToolEvent::Called {
            chat: crate::ids::ChatId::from_number(4242),
            tool: ServedTool::Caf(CafTool::Read),
            input: json!({ "file_path": "/a" }),
            agent_call: None,
            title: None,
            bad_input: None,
        },
    });
    assert_eq!(
        one_told(&out)["value"]["why"],
        json!({ "type": "NotRunning" })
    );
    assert!(tools(&script).is_empty());

    script.send(CHAT, "go").0.expect("sent");
    for (call, tool, input, why) in [
        (
            3,
            CafTool::Read,
            json!({ "path": "/a" }),
            "`file_path` is missing.",
        ),
        (
            4,
            CafTool::Bash,
            json!({ "command": 3 }),
            "`command` must be a string.",
        ),
        (
            5,
            CafTool::Write,
            json!("/a"),
            "The input must be an object.",
        ),
    ] {
        let out = script.tool(call, called(tool, input, None));
        assert_eq!(
            one_told(&out),
            json!({ "type": "Refuse", "value": { "call": call, "why": { "type": "BadInput", "value": why } } })
        );
    }
    assert!(tools(&script).is_empty(), "no item for a call refused");
    // The agent's own report of one says it failed, and why.
    script.update(
        PROC,
        SESSION,
        &report("toolu_9", "Edit", "pending", &json!({ "file_path": "/a" })),
    );
    script.tool(
        6,
        called(CafTool::Edit, json!({ "file_path": "/a" }), Some("toolu_9")),
    );
    assert_has(
        &the_tool(&script),
        &json!({ "status": kind("Failed"), "result": [{ "type": "Text", "value": { "text": "`old_string` is missing." } }] }),
    );
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "toolu_9", "status": "completed" }),
    );
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
}

#[test]
fn an_edit_is_previewed_before_its_asked_about() {
    let mut script = running();
    let input = json!({ "file_path": "/work/lib.rs", "old_string": "a", "new_string": "b" });
    let out = script.tool(1, called(CafTool::Edit, input.clone(), Some("toolu_1")));
    assert!(with(&out, "decide").is_empty(), "not yet");
    // The driver is given the model's input whole, for the strings.
    assert_eq!(
        one_told(&out),
        json!({ "type": "Preview", "value": {
            "call": 1, "action": { "type": "Edit", "value": { "path": "/work/lib.rs" } }, "input": input,
        } })
    );
    let diff = Diff::new("/work/lib.rs", Some("a\n"), "b\n");
    let out = script.tool(
        1,
        ToolEvent::Previewed {
            pieces: vec![Piece::Diff(diff.clone())],
        },
    );
    let decided = ticket(&out);
    assert_eq!(
        the_tool(&script)["result"],
        json!([serde_json::to_value(Piece::Diff(diff)).expect("JSON")])
    );
    // A second preview changes nothing.
    script.tool(1, ToolEvent::Previewed { pieces: Vec::new() });
    assert_eq!(
        the_tool(&script)["result"].as_array().map(Vec::len),
        Some(1)
    );
    let out = script.policy_saying(decided, PolicyAnswer::Allow, "edits\u{202e} are fine");
    assert_eq!(
        one_told(&out),
        json!({ "type": "Run", "value": {
            "call": 1, "action": { "type": "Edit", "value": { "path": "/work/lib.rs" } }, "input": input,
        } })
    );
    assert_has(
        &the_tool(&script),
        &json!({
            "title": "Edit lib.rs", "status": kind("Running"),
            "decision": { "type": "Allowed", "value": { "type": "Policy", "value": { "reason": "edits<U+202E> are fine" } } },
        }),
    );
    let out = script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: Vec::new(),
            folder: None,
            notice: None,
            item: None,
        },
    );
    assert!(told(&out).is_empty());
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Done"));
    assert_ne!(tool["end"]["type"], "Open", "it ended");
}

#[test]
fn the_policy_refuses_with_its_reason_and_nothing_runs() {
    let mut script = running();
    let out = script.tool(
        1,
        called(
            CafTool::Write,
            json!({ "file_path": "x.md", "content": "hi" }),
            None,
        ),
    );
    assert_eq!(one_told(&out)["type"], "Preview");
    let out = script.tool(1, ToolEvent::Previewed { pieces: Vec::new() });
    let out = script.policy_saying(ticket(&out), PolicyAnswer::Refuse, "read-only");
    assert_eq!(
        one_told(&out),
        json!({ "type": "Refuse", "value": { "call": 1, "why": { "type": "NotAllowed" } } })
    );
    let tool = the_tool(&script);
    assert_has(
        &tool,
        &json!({
            "title": "Write x.md", "status": kind("Refused"),
            "decision": { "type": "Refused", "value": { "type": "Policy", "value": { "reason": "read-only" } } },
        }),
    );
    assert_ne!(tool["end"]["type"], "Open");
    // What the driver says of it afterwards is ignored.
    let out = script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: Vec::new(),
            folder: None,
            notice: None,
            item: None,
        },
    );
    assert!(told(&out).is_empty());
    assert_eq!(the_tool(&script), tool);
}

#[test]
fn the_user_is_asked_with_cafs_own_two_options() {
    let mut script = running();
    let out = script.tool(
        1,
        called(CafTool::Read, json!({ "file_path": "/etc/hosts" }), None),
    );
    let decided = ticket(&out);
    let out = script.policy(decided, PolicyAnswer::AskUser);
    assert!(told(&out).is_empty());
    assert_eq!(the_tool(&script)["status"], kind("Asking"));
    let question = &script.chat(CHAT)["questions"][0];
    assert_eq!(
        question["kind"],
        json!({ "type": "Permission", "value": {
            "tool": 1004, "title": "Read /etc/hosts",
            "options": [
                { "id": "allow", "label": "Allow", "kind": kind("AllowOnce") },
                { "id": "refuse", "label": "Refuse", "kind": kind("RejectOnce") }
            ]
        } })
    );
    let id = QuestionId(question["id"].as_u64().expect("an id"));
    let (wrong, out) = script.answer(id, Answer::Permission(OptionId("always".to_owned())));
    assert_eq!(wrong, Err(Refused::WrongAnswer));
    assert!(out.is_empty());
    let (right, out) = script.answer(id, Answer::Permission(OptionId("allow".to_owned())));
    right.expect("answered");
    assert_eq!(one_told(&out)["type"], "Run");
    assert_eq!(script.chat(CHAT)["questions"], json!([]));
    assert_has(
        &the_tool(&script),
        &json!({ "status": kind("Running"), "decision": { "type": "Allowed", "value": { "type": "User", "value": { "option": "Allow" } } } }),
    );
    assert_eq!(
        script
            .answer(id, Answer::Permission(OptionId("allow".to_owned())))
            .0,
        Err(Refused::NoSuchQuestion)
    );
    // The policy answering late does nothing.
    assert!(
        script
            .policy(decided, PolicyAnswer::Refuse)
            .iter()
            .all(|each| each.get("tool").is_none())
    );

    // Refused by the user.
    let out = script.tool(
        2,
        called(CafTool::Read, json!({ "file_path": "/etc/passwd" }), None),
    );
    script.policy(ticket(&out), PolicyAnswer::AskUser);
    let id = QuestionId(
        script.chat(CHAT)["questions"][0]["id"]
            .as_u64()
            .expect("an id"),
    );
    let (_, out) = script.answer(id, Answer::Permission(OptionId("refuse".to_owned())));
    assert_eq!(
        one_told(&out)["value"],
        json!({ "call": 2, "why": { "type": "NotAllowed" } })
    );
    assert_has(
        &tools(&script)[1],
        &json!({ "status": kind("Refused"), "decision": { "type": "Refused", "value": { "type": "User", "value": { "option": "Refuse" } } } }),
    );
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: vec![Piece::Text(Text::new("127.0.0.1"))],
            folder: None,
            notice: None,
            item: None,
        },
    );
    assert_eq!(tools(&script)[0]["result"][0]["value"]["text"], "127.0.0.1");
}

#[test]
fn a_host_that_cant_ask_the_user_has_the_call_refused() {
    let mut host = host();
    host.shows.permission_questions = false;
    let mut script = Script::new(host);
    script.serving();
    script.open_claude_as(given(crate::core::OpenChat::new("claude", "/work")));
    script.send(CHAT, "go").0.expect("sent");
    let out = script.tool(1, called(CafTool::Bash, json!({ "command": "ls" }), None));
    let out = script.policy(ticket(&out), PolicyAnswer::AskUser);
    assert_eq!(one_told(&out)["type"], "Refuse");
    assert_eq!(the_tool(&script)["status"], kind("Refused"));
    assert_eq!(script.chat(CHAT)["questions"], json!([]));
}

#[test]
fn a_stopped_turn_cancels_its_calls_and_a_finished_one_fails_them() {
    let mut script = bash_running("sleep 100");
    let out = script.tool(2, called(CafTool::Read, json!({ "file_path": "/a" }), None));
    let deciding = ticket(&out);
    let out = script.tool(3, called(CafTool::Read, json!({ "file_path": "/b" }), None));
    script.policy(ticket(&out), PolicyAnswer::AskUser);
    // Those waiting to be allowed are cancelled at once.
    let (stopped, out) = script.stop(CHAT);
    stopped.expect("stopping");
    assert_eq!(
        told(&out),
        [2, 3].map(|call| json!({ "type": "Cancel", "value": { "call": call } }))
    );
    assert!(out.contains(&json!({ "withdrawn": deciding })));
    assert_eq!(script.chat(CHAT)["questions"], json!([]));
    // Stopping, the turn takes no new calls.
    let out = script.tool(4, called(CafTool::Read, json!({ "file_path": "/c" }), None));
    assert_eq!(
        one_told(&out)["value"]["why"],
        json!({ "type": "NotRunning" })
    );
    // The one running is stopped as the turn ends, and ends once the
    // driver says it stopped.
    let out = script.answers(PROC, PROMPT, &json!({ "stopReason": "cancelled" }));
    assert_eq!(
        told(&out),
        [json!({ "type": "Cancel", "value": { "call": 1 } })]
    );
    assert_eq!(tools(&script)[0]["status"], kind("Running"));
    script.tool(1, ToolEvent::Stopped);
    for tool in tools(&script) {
        assert_eq!(tool["status"], kind("Cancelled"));
        assert_eq!(tool["stopped_by"], kind("User"));
        assert_ne!(tool["end"]["type"], "Open");
    }
    // The command's output ended, not saying how.
    assert_eq!(
        tools(&script)[0]["result"][0]["value"],
        json!({ "lines": [], "dropped": 0, "ending": kind("Unknown") })
    );
    // Its output and finish, coming anyway, are ignored.
    let out = script.tool(
        1,
        ToolEvent::Output {
            from: 0,
            lines: vec!["late".to_owned()],
        },
    );
    assert!(told(&out).is_empty());
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: Vec::new(),
            folder: None,
            notice: None,
            item: None,
        },
    );
    assert_eq!(tools(&script)[0]["status"], kind("Cancelled"));
    assert!(script.core.calls.is_empty());

    // A turn that ends by itself fails what's left.
    let mut script = bash_running("make");
    let out = script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    assert_eq!(
        one_told(&out),
        json!({ "type": "Cancel", "value": { "call": 1 } })
    );
    script.tool(1, ToolEvent::Stopped);
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
    assert_eq!(
        the_tool(&script)["stopped_by"],
        Value::Null,
        "only a cancelled call was stopped"
    );
}

#[test]
fn a_call_the_agent_gives_up_on_is_cancelled_and_not_answered() {
    let mut script = running();
    let out = script.tool(1, called(CafTool::Bash, json!({ "command": "ls" }), None));
    let deciding = ticket(&out);
    let out = script.tool(1, ToolEvent::Gone);
    assert!(told(&out).is_empty());
    assert!(out.contains(&json!({ "withdrawn": deciding })));
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Cancelled"));
    assert_eq!(tool["stopped_by"], kind("Agent"));
    assert_ne!(tool["end"]["type"], "Open");
    // Nothing more comes of it.
    let out = script.policy(deciding, PolicyAnswer::Allow);
    assert!(told(&out).is_empty());
    let out = script.tool(1, ToolEvent::Gone);
    assert!(told(&out).is_empty());
    let out = script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    assert!(
        told(&out).is_empty(),
        "never answered: the agent gave up on it"
    );
}

#[test]
fn output_grows_and_a_progress_bar_rewrites_its_line() {
    let mut script = bash_running("cargo build");
    assert_eq!(
        output(&script),
        json!({ "lines": [], "dropped": 0, "ending": null })
    );
    let lines = |text: &[&str]| text.iter().map(|&line| line.to_owned()).collect::<Vec<_>>();
    script.tool(
        1,
        ToolEvent::Output {
            from: 0,
            lines: lines(&["Compiling a", "[=>   ] 1/4"]),
        },
    );
    script.tool(
        1,
        ToolEvent::Output {
            from: 1,
            lines: lines(&["[===> ] 3/4"]),
        },
    );
    script.tool(
        1,
        ToolEvent::Output {
            from: 1,
            lines: lines(&["[=====] 4/4", "\u{1b}[32mFinished\u{1b}[0m"]),
        },
    );
    assert_eq!(
        output(&script)["lines"],
        json!(["Compiling a", "[=====] 4/4", "\u{1b}[32mFinished\u{1b}[0m"])
    );
    let changes: Vec<Value> = (script.state()["logs"][CHAT.to_string()]["changes"]
        .as_array()
        .into_iter()
        .flatten())
    .map(|numbered| numbered["change"].clone())
    .filter(|change| change["type"] == "OutputChanged")
    .collect();
    assert_eq!(changes.len(), 3, "one each time it changed");
    assert_eq!(
        changes.last().map(|change| change["value"].clone()),
        Some(
            json!({ "item": 1004, "from": 1, "lines": ["[=====] 4/4", "\u{1b}[32mFinished\u{1b}[0m"], "dropped": 0 })
        )
    );
    let ended = Output {
        lines: Vec::new(),
        dropped: 0,
        ending: Some(Ending::Exited(Exit::Code(0))),
    };
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: vec![Piece::Output(ended)],
            folder: None,
            notice: None,
            item: None,
        },
    );
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Done"));
    assert_eq!(
        tool["result"].as_array().map(Vec::len),
        Some(1),
        "one output"
    );
    assert_eq!(
        tool["result"][0]["value"]["ending"],
        json!({ "type": "Exited", "value": { "type": "Code", "value": 0 } })
    );
}

#[test]
fn output_keeps_its_end_and_splits_overlong_lines() {
    let mut script = bash_running("yes");
    let line = "y".repeat(1000);
    let many = MOST_OUTPUT / 1000 + 10;
    for from in (0..many).step_by(16) {
        let batch: Vec<String> = (from..(from + 16).min(many))
            .map(|_| line.clone())
            .collect();
        script.tool(
            1,
            ToolEvent::Output {
                from: from as u64,
                lines: batch,
            },
        );
    }
    let kept = output(&script);
    let lines = kept["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), MOST_OUTPUT / 1000);
    assert_eq!(
        kept["dropped"].as_u64(),
        Some((many - MOST_OUTPUT / 1000) as u64)
    );
    // An overlong line is split, at character boundaries.
    let long = "é".repeat(MOST_LINE);
    script.tool(
        1,
        ToolEvent::Output {
            from: many as u64,
            lines: vec![long.clone()],
        },
    );
    let kept = output(&script);
    let lines: Vec<&str> = (kept["lines"].as_array().expect("lines").iter())
        .filter_map(Value::as_str)
        .collect();
    assert!(lines.iter().all(|line| line.len() <= MOST_LINE));
    assert_eq!(lines[lines.len() - 2..].concat(), long);
    assert!(lines.iter().map(|line| line.len()).sum::<usize>() <= MOST_OUTPUT);
    script.tool(
        1,
        ToolEvent::Finished {
            done: false,
            pieces: vec![Piece::Text(Text::new("killed"))],
            folder: None,
            notice: None,
            item: None,
        },
    );
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
}

#[test]
fn the_agent_asking_permission_for_cafs_own_call_is_allowed_at_once() {
    let mut script = running();
    script.update(
        PROC,
        SESSION,
        &report("toolu_1", "Bash", "pending", &json!({ "command": "ls" })),
    );
    let mut request = permission(7, "toolu_1");
    request["params"]["toolCall"]["_meta"] =
        json!({ "claudeCode": { "toolName": "mcp__caf__Bash" } });
    let out = script.says(PROC, &request);
    assert!(with(&out, "decide").is_empty(), "the policy isn't asked");
    assert_eq!(
        super::harness::replies(&out, &json!(7))[0]["result"],
        json!({ "outcome": { "outcome": "selected", "optionId": "allow" } })
    );
    // The call itself is asked about as CAF's.
    let out = script.tool(
        1,
        called(CafTool::Bash, json!({ "command": "ls" }), Some("toolu_1")),
    );
    script.policy(ticket(&out), PolicyAnswer::Refuse);
}

#[test]
fn a_stop_ends_the_calls_waiting_to_be_allowed_at_once() {
    let mut script = running();
    let out = script.tool(
        1,
        called(CafTool::Bash, json!({ "command": "rm -rf x" }), None),
    );
    let deciding = ticket(&out);
    let out = script.tool(2, called(CafTool::Read, json!({ "file_path": "/b" }), None));
    script.policy(ticket(&out), PolicyAnswer::AskUser);
    let question = QuestionId(
        script.chat(CHAT)["questions"][0]["id"]
            .as_u64()
            .expect("asked"),
    );
    let out = script.tool(
        3,
        called(
            CafTool::Edit,
            json!({ "file_path": "/c", "old_string": "a", "new_string": "b" }),
            None,
        ),
    );
    assert_eq!(one_told(&out)["type"], "Preview");
    let (stopped, out) = script.stop(CHAT);
    stopped.expect("stopping");
    let mut cancelled = told(&out);
    cancelled.sort_by_key(|each| each["value"]["call"].as_u64());
    assert_eq!(
        cancelled,
        [1, 2, 3].map(|call| json!({ "type": "Cancel", "value": { "call": call } }))
    );
    assert!(out.contains(&json!({ "withdrawn": deciding })));
    assert_eq!(script.chat(CHAT)["questions"], json!([]));
    for tool in tools(&script) {
        assert_eq!(tool["status"], kind("Cancelled"));
        assert_eq!(tool["stopped_by"], kind("User"));
    }
    // Nothing allows them after.
    assert!(told(&script.policy(deciding, PolicyAnswer::Allow)).is_empty());
    let (answered, out) = script.answer(question, Answer::Permission(OptionId("allow".to_owned())));
    assert_eq!(answered, Err(Refused::NoSuchQuestion));
    assert!(told(&out).is_empty());
    assert!(told(&script.tool(3, ToolEvent::Previewed { pieces: Vec::new() })).is_empty());
    script.answers(PROC, PROMPT, &json!({ "stopReason": "cancelled" }));
}

#[test]
fn a_call_without_the_agents_id_joins_a_report_of_input_too_long_to_keep_whole() {
    let input = json!({ "file_path": "/work/a", "content": "x".repeat(MOST_OUTPUT + 10) });
    let mut script = running();
    script.update(PROC, SESSION, &report("r1", "Write", "pending", &input));
    script.tool(1, called(CafTool::Write, input.clone(), None));
    assert_eq!(tools(&script).len(), 1, "joined");
    assert_eq!(the_tool(&script)["title"], "Write a");
    script.tool(1, ToolEvent::Gone);
}

#[test]
fn a_report_whose_input_comes_later_joins_the_call_it_turns_out_to_be() {
    let mut script = running();
    let a = json!({ "file_path": "/work/a" });
    let b = json!({ "file_path": "/work/b" });
    let first = script.tool(1, called(CafTool::Read, a.clone(), None));
    let second = script.tool(2, called(CafTool::Read, b.clone(), None));
    // Reported with no input yet, then with it: it's the second call.
    script.update(PROC, SESSION, &report("r1", "Read", "pending", &json!({})));
    let mut update = report("r1", "Read", "pending", &b);
    update["sessionUpdate"] = json!("tool_call_update");
    script.update(PROC, SESSION, &update);
    assert_eq!(tools(&script).len(), 2, "no item of its own");
    // The first's report then joins the first.
    script.update(PROC, SESSION, &report("r2", "Read", "pending", &json!({})));
    let mut update = report("r2", "Read", "pending", &a);
    update["sessionUpdate"] = json!("tool_call_update");
    script.update(PROC, SESSION, &update);
    assert_eq!(tools(&script).len(), 2);
    let titles: Vec<Value> = tools(&script)
        .iter()
        .map(|tool| tool["title"].clone())
        .collect();
    assert_eq!(titles, [json!("Read a"), json!("Read b")]);
    script.policy(ticket(&first), PolicyAnswer::Refuse);
    script.policy(ticket(&second), PolicyAnswer::Refuse);
}

/// Opens a plain agent's chat, whose adapter says nothing of whose a
/// tool is, with a turn running.
fn plain_running() -> Script {
    let mut script = Script::new(host());
    script.serving();
    script
        .open(given(crate::core::OpenChat::new("plain", "/work")))
        .0
        .expect("opened");
    script.answers(PROC, 1000, &super::plain_init(false));
    script.answers(PROC, 1001, &json!({ "sessionId": SESSION }));
    script.send(CHAT, "go").0.expect("sent");
    script
}

#[test]
fn the_agents_own_permission_request_never_changes_a_call_caf_serves() {
    let mut script = plain_running();
    let input = json!({ "command": "cargo test" });
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "tool_call", "toolCallId": "tool-1", "title": "Run", "status": "pending", "rawInput": input }),
    );
    // Its adapter doesn't say it's CAF's: the policy is asked.
    let out = script.says(PROC, &permission(7, "tool-1"));
    let agents = ticket(&out);
    let out = script.tool(1, called(CafTool::Bash, input, Some("tool-1")));
    let cafs = ticket(&out);
    script.policy(agents, PolicyAnswer::AskUser);
    assert_eq!(the_tool(&script)["status"], kind("Pending"));
    let question = QuestionId(
        script.chat(CHAT)["questions"][0]["id"]
            .as_u64()
            .expect("asked"),
    );
    script
        .answer(question, Answer::Permission(OptionId("reject".to_owned())))
        .0
        .expect("answered");
    let tool = the_tool(&script);
    assert_has(
        &tool,
        &json!({ "status": kind("Pending"), "decision": kind("Undecided") }),
    );
    script.policy(cafs, PolicyAnswer::Allow);
    assert_eq!(the_tool(&script)["status"], kind("Running"));
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: Vec::new(),
            folder: None,
            notice: None,
            item: None,
        },
    );
}

#[test]
fn output_from_past_its_end_keeps_the_drivers_numbering() {
    let mut script = bash_running("make");
    let lines = |text: &[&str]| text.iter().map(|&line| line.to_owned()).collect::<Vec<_>>();
    script.tool(
        1,
        ToolEvent::Output {
            from: 0,
            lines: lines(&["a"]),
        },
    );
    let out = script.tool(
        1,
        ToolEvent::Output {
            from: 3,
            lines: lines(&["d", "e"]),
        },
    );
    assert!(
        with(&out, "report")
            .iter()
            .any(|report| report["report"] == "Warn"),
        "{out:#?}"
    );
    assert_eq!(output(&script)["lines"], json!(["a", "d", "e"]));
    // Its line 4 is "e".
    script.tool(
        1,
        ToolEvent::Output {
            from: 4,
            lines: lines(&["E"]),
        },
    );
    assert_eq!(output(&script)["lines"], json!(["a", "d", "E"]));
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: Vec::new(),
            folder: None,
            notice: None,
            item: None,
        },
    );
}

#[test]
fn a_finished_edit_gives_its_diff_in_place_of_the_previews() {
    let mut script = running();
    let out = script.tool(
        1,
        called(
            CafTool::Edit,
            json!({ "file_path": "/work/a", "old_string": "a", "new_string": "b" }),
            None,
        ),
    );
    assert_eq!(one_told(&out)["type"], "Preview");
    let preview = Diff::new("/work/a", Some("a\n"), "b\n");
    let out = script.tool(
        1,
        ToolEvent::Previewed {
            pieces: vec![Piece::Diff(preview)],
        },
    );
    script.policy(ticket(&out), PolicyAnswer::Allow);
    let done = Diff::new("/work/a", Some("a\nz\n"), "b\nz\n");
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: vec![Piece::Diff(done.clone())],
            folder: None,
            notice: None,
            item: None,
        },
    );
    assert_eq!(
        the_tool(&script)["result"],
        json!([serde_json::to_value(Piece::Diff(done)).expect("JSON")])
    );
}

#[test]
fn a_finished_command_ends_its_last_output() {
    let mut script = bash_running("make");
    // A driver giving a second output: the last is the one that ends.
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: vec![Piece::Output(Output {
                lines: Vec::new(),
                dropped: 0,
                ending: Some(Ending::Exited(Exit::Code(2))),
            })],
            folder: None,
            notice: None,
            item: None,
        },
    );
    let result = &the_tool(&script)["result"];
    assert_eq!(result.as_array().map(Vec::len), Some(1));
    assert_eq!(
        result[0]["value"]["ending"],
        json!({ "type": "Exited", "value": { "type": "Code", "value": 2 } })
    );
}

#[test]
fn a_call_reported_twice_or_naming_an_earlier_calls_id_is_its_own() {
    let mut script = running();
    let out = script.tool(
        1,
        called(CafTool::Read, json!({ "file_path": "/a" }), Some("toolu_1")),
    );
    let first = ticket(&out);
    // The same call again: ignored.
    let out = script.tool(
        1,
        called(CafTool::Read, json!({ "file_path": "/z" }), Some("toolu_1")),
    );
    assert!(told(&out).is_empty() && with(&out, "decide").is_empty());
    assert_eq!(tools(&script).len(), 1);
    // Another call giving the first's id: an item of its own, and the
    // first's report still lands on the first.
    let out = script.tool(
        2,
        called(CafTool::Read, json!({ "file_path": "/b" }), Some("toolu_1")),
    );
    assert!(
        with(&out, "report")
            .iter()
            .any(|report| report["report"] == "Warn")
    );
    let second = ticket(&out);
    assert_eq!(tools(&script).len(), 2);
    script.update(
        PROC,
        SESSION,
        &report(
            "toolu_1",
            "Read",
            "completed",
            &json!({ "file_path": "/a" }),
        ),
    );
    let all = tools(&script);
    assert_eq!(all[0]["title"], "Read /a");
    assert_eq!(all[1]["title"], "Read /b");
    assert_eq!(all[0]["status"], kind("Pending"));
    script.policy(first, PolicyAnswer::Refuse);
    script.policy(second, PolicyAnswer::Refuse);
}

#[test]
fn two_chats_may_use_the_same_agent_id() {
    const OTHER: crate::ids::ChatId = crate::ids::ChatId::from_number(1004);
    let mut script = running();
    let (other, out) = script.open(given(crate::core::OpenChat::new("claude", "/elsewhere")));
    assert_eq!(other, Ok(OTHER));
    let new = super::harness::sent(&out, "session/new")["id"]
        .as_u64()
        .expect("an id");
    script.answers(
        PROC,
        new,
        &json!({ "sessionId": "sess-2", "modes": super::default_modes() }),
    );
    script.send(OTHER, "go too").0.expect("sent");
    let out = script.tool(
        1,
        called(CafTool::Read, json!({ "file_path": "/a" }), Some("toolu_1")),
    );
    let first = ticket(&out);
    let out = script.tool(
        2,
        ToolEvent::Called {
            chat: OTHER,
            tool: ServedTool::Caf(CafTool::Read),
            input: json!({ "file_path": "/b" }),
            agent_call: Some("toolu_1".to_owned()),
            title: None,
            bad_input: None,
        },
    );
    let second = ticket(&out);
    assert_eq!(tools(&script).len(), 1);
    assert_eq!(
        script.chat(OTHER)["turns"][0]["items"][0]["kind"]["value"]["title"],
        "Read /b"
    );
    script.policy(first, PolicyAnswer::Refuse);
    script.policy(second, PolicyAnswer::Refuse);
}

#[test]
fn a_call_the_agent_gives_up_on_while_previewed_asked_or_running_is_cancelled() {
    let mut script = running();
    let out = script.tool(
        1,
        called(
            CafTool::Write,
            json!({ "file_path": "/a", "content": "" }),
            None,
        ),
    );
    assert_eq!(one_told(&out)["type"], "Preview");
    assert!(told(&script.tool(1, ToolEvent::Gone)).is_empty());
    // Its preview, coming late, asks nobody.
    let out = script.tool(1, ToolEvent::Previewed { pieces: Vec::new() });
    assert!(with(&out, "decide").is_empty());

    let out = script.tool(2, called(CafTool::Read, json!({ "file_path": "/b" }), None));
    script.policy(ticket(&out), PolicyAnswer::AskUser);
    script.tool(2, ToolEvent::Gone);
    assert_eq!(script.chat(CHAT)["questions"], json!([]));

    let out = script.tool(3, called(CafTool::Bash, json!({ "command": "ls" }), None));
    script.policy(ticket(&out), PolicyAnswer::Allow);
    assert!(told(&script.tool(3, ToolEvent::Gone)).is_empty());
    let statuses: Vec<Value> = tools(&script)
        .iter()
        .map(|tool| tool["status"].clone())
        .collect();
    assert_eq!(
        statuses,
        [kind("Cancelled"), kind("Cancelled"), kind("Cancelled")]
    );
    assert_eq!(
        tools(&script)[2]["result"][0]["value"]["ending"],
        kind("Unknown")
    );
}

#[test]
fn a_call_in_a_turn_the_agent_started_is_served_in_it() {
    let mut script = Script::with_claude_serving();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Checking" } }));
    let out = script.tool(1, called(CafTool::Read, json!({ "file_path": "/a" }), None));
    let turn = &script.chat(CHAT)["turns"][0];
    assert_eq!(turn["starter"]["type"], "Agent");
    assert_eq!(turn["items"][1]["kind"]["value"]["title"], "Read /a");
    script.policy(ticket(&out), PolicyAnswer::Allow);
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: Vec::new(),
            folder: None,
            notice: None,
            item: None,
        },
    );
}

#[test]
fn input_that_makes_no_action_fails_the_report_waiting_for_it_without_the_agents_id() {
    let mut script = running();
    let input = json!({ "file_path": "/a" });
    script.update(PROC, SESSION, &report("r1", "Edit", "pending", &input));
    let out = script.tool(1, called(CafTool::Edit, input, None));
    assert_eq!(one_told(&out)["value"]["why"]["type"], "BadInput");
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
}

#[test]
fn a_report_before_its_input_is_joined_by_the_call_without_the_agents_id() {
    let mut script = running();
    let input = json!({ "file_path": "/work/a" });
    // The usual order: the adapter reports the call, then calls.
    script.update(PROC, SESSION, &report("r1", "Read", "pending", &json!({})));
    let started = the_tool(&script)["id"].clone();
    let out = script.tool(1, called(CafTool::Read, input.clone(), None));
    assert_eq!(tools(&script).len(), 1, "joined");
    assert_eq!(the_tool(&script)["id"], started);
    assert_eq!(the_tool(&script)["title"], "Read a");
    // Its input then comes, and it's the same call: still one item, CAF's.
    let mut update = report("r1", "Read", "pending", &input);
    update["sessionUpdate"] = json!("tool_call_update");
    update["title"] = json!("mcp__caf__Read");
    script.update(PROC, SESSION, &update);
    assert_eq!(tools(&script).len(), 1);
    assert_eq!(the_tool(&script)["title"], "Read a");
    // A report then saying another input was another call's: let go.
    script.update(PROC, SESSION, &report("r2", "Read", "pending", &json!({})));
    let other = script.tool(
        2,
        called(CafTool::Read, json!({ "file_path": "/work/b" }), None),
    );
    let mut update = report("r2", "Read", "pending", &json!({ "file_path": "/work/c" }));
    update["sessionUpdate"] = json!("tool_call_update");
    script.update(PROC, SESSION, &update);
    let titles: Vec<Value> = tools(&script)
        .iter()
        .map(|tool| tool["title"].clone())
        .collect();
    assert_eq!(titles.len(), 3, "{titles:?}");
    assert_eq!(titles[1], "Read b", "the call keeps the item");
    script.policy(ticket(&out), PolicyAnswer::Refuse);
    script.policy(ticket(&other), PolicyAnswer::Refuse);
    script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn a_call_naming_a_report_held_only_for_now_takes_it() {
    let mut script = running();
    script.update(PROC, SESSION, &report("r1", "Read", "pending", &json!({})));
    // A call without the agent's id holds the report for now.
    let first = script.tool(
        1,
        called(CafTool::Read, json!({ "file_path": "/work/a" }), None),
    );
    assert_eq!(tools(&script).len(), 1);
    // Then the call the report is of names it: it's that call's.
    let out = script.tool(
        2,
        called(CafTool::Read, json!({ "file_path": "/work/b" }), Some("r1")),
    );
    assert!(
        with(&out, "report")
            .iter()
            .all(|report| report["report"] != "Warn"),
        "not an earlier call's id: {out:#?}"
    );
    let second = ticket(&out);
    assert_eq!(tools(&script).len(), 2);
    let mut update = report(
        "r1",
        "Read",
        "completed",
        &json!({ "file_path": "/work/b" }),
    );
    update["sessionUpdate"] = json!("tool_call_update");
    script.update(PROC, SESSION, &update);
    assert_eq!(tools(&script).len(), 2, "it landed on the second call");
    // The first is free for its own report.
    script.update(
        PROC,
        SESSION,
        &report("r2", "Read", "pending", &json!({ "file_path": "/work/a" })),
    );
    assert_eq!(tools(&script).len(), 2, "joined the first");
    let statuses: Vec<Value> = tools(&script)
        .iter()
        .map(|tool| tool["status"].clone())
        .collect();
    assert_eq!(statuses, [kind("Pending"), kind("Pending")]);
    script.policy(ticket(&first), PolicyAnswer::Refuse);
    script.policy(second, PolicyAnswer::Refuse);
}

#[test]
fn a_call_the_agent_gives_up_on_as_the_user_stops_its_turn_was_stopped_by_the_user() {
    let mut script = bash_running("sleep 100");
    script.stop(CHAT).0.expect("stopping");
    // Claude Code ends its MCP calls as its turn is cancelled.
    let out = script.tool(1, ToolEvent::Gone);
    assert!(told(&out).is_empty(), "nothing to answer");
    script.answers(PROC, PROMPT, &json!({ "stopReason": "cancelled" }));
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Cancelled"));
    assert_eq!(tool["stopped_by"], kind("User"));
}
