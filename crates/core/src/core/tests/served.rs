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
use crate::tool::{CafTool, Diff, Ending, MOST_LINE, MOST_OUTPUT, Output, Piece, Text};

/// The prompt's request id, as the first turn sends it.
const PROMPT: u64 = 1002;

/// A Claude chat with a turn running.
fn running() -> Script {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    script
}

/// The agent calls `tool` with `input`, giving its id for the call if
/// `agent_call`.
fn called(tool: CafTool, input: Value, agent_call: Option<&str>) -> ToolEvent {
    ToolEvent::Called {
        chat: CHAT,
        tool,
        input,
        agent_call: agent_call.map(str::to_owned),
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
fn told(out: &[Value]) -> Vec<Value> {
    with(out, "tool")
        .into_iter()
        .map(|each| each["tool"].clone())
        .collect()
}

/// The one tool directive in `out`.
fn one_told(out: &[Value]) -> Value {
    let told = told(out);
    assert_eq!(told.len(), 1, "{out:#?}");
    told[0].clone()
}

/// The ticket of the one decision asked for in `out`.
fn ticket(out: &[Value]) -> u64 {
    let decided = with(out, "decide");
    assert_eq!(decided.len(), 1, "{out:#?}");
    decided[0]["decide"].as_u64().expect("a ticket")
}

/// The chat's tool calls.
fn tools(script: &Script) -> Vec<Value> {
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
fn the_tool(script: &Script) -> Value {
    let tools = tools(script);
    assert_eq!(tools.len(), 1, "{tools:#?}");
    tools[0].clone()
}

fn kind(name: &str) -> Value {
    json!({ "type": name })
}

/// Bash running `command`, asked about and allowed: returns the script.
fn bash_running(command: &str) -> Script {
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
    script
        .open(crate::core::OpenChat::new("claude", "/work"))
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
    let mut script = Script::with_claude();
    let out = script.tool(1, called(CafTool::Read, json!({ "file_path": "/a" }), None));
    assert_eq!(
        one_told(&out),
        json!({ "type": "Refuse", "value": { "call": 1, "why": { "type": "NotRunning" } } })
    );
    let out = script.event(crate::Event::Tool {
        call: crate::ids::CallId(2),
        event: ToolEvent::Called {
            chat: crate::ids::ChatId::from_number(4242),
            tool: CafTool::Read,
            input: json!({ "file_path": "/a" }),
            agent_call: None,
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
    let out = script.tool(1, called(CafTool::Edit, input, Some("toolu_1")));
    assert!(with(&out, "decide").is_empty(), "not yet");
    assert_eq!(
        one_told(&out),
        json!({ "type": "Preview", "value": { "call": 1, "action": { "type": "Edit", "value": { "path": "/work/lib.rs" } } } })
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
        json!({ "type": "Run", "value": { "call": 1, "action": { "type": "Edit", "value": { "path": "/work/lib.rs" } } } })
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
        },
    );
    assert_eq!(tools(&script)[0]["result"][0]["value"]["text"], "127.0.0.1");
}

#[test]
fn a_host_that_cant_ask_the_user_has_the_call_refused() {
    let mut host = host();
    host.shows.permission_questions = false;
    let mut script = Script::new(host);
    script.open_claude();
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
    script.stop(CHAT).0.expect("stopping");
    // Stopping, the turn takes no new calls.
    let out = script.tool(4, called(CafTool::Read, json!({ "file_path": "/c" }), None));
    assert_eq!(
        one_told(&out)["value"]["why"],
        json!({ "type": "NotRunning" })
    );
    let out = script.answers(PROC, PROMPT, &json!({ "stopReason": "cancelled" }));
    let mut cancelled: Vec<Value> = told(&out);
    cancelled.sort_by_key(|each| each["value"]["call"].as_u64());
    assert_eq!(
        cancelled,
        [1, 2, 3].map(|call| json!({ "type": "Cancel", "value": { "call": call } }))
    );
    assert!(out.contains(&json!({ "withdrawn": deciding })));
    assert_eq!(script.chat(CHAT)["questions"], json!([]));
    for tool in tools(&script) {
        assert_eq!(tool["status"], kind("Cancelled"));
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
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
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
