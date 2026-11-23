//! Tool calls the agent reports: what they gave back, read into pieces,
//! capped, and nothing dropped; whose tool each is, from the adapter's
//! metadata only; asking about them, and who decided; and every one
//! settled when its turn ends.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, host, with};
use super::stop::permission;
use super::{CHAT, PROC, SESSION};
use crate::core::OpenChat;
use crate::event::{Event, PolicyAnswer};
use crate::ids::{QuestionId, Ticket};
use crate::question::{Answer, OptionId};
use crate::tool::{MOST_IMAGE, MOST_INPUT, MOST_LOCATIONS, MOST_TEXT};

/// The tool calls in `chat`, as JSON, in order.
fn tools(chat: &Value) -> Vec<Value> {
    (chat["turns"].as_array().into_iter().flatten())
        .flat_map(|turn| turn["items"].as_array().cloned().unwrap_or_default())
        .filter(|item| item["kind"]["type"] == "Tool")
        .map(|item| item["kind"]["value"].clone())
        .collect()
}

/// The one tool call in the chat.
#[track_caller]
fn the_tool(script: &Script) -> Value {
    let tools = tools(&script.chat(CHAT));
    assert_eq!(tools.len(), 1, "{tools:#?}");
    tools[0].clone()
}

/// A Claude chat with a turn running.
fn running() -> Script {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    script
}

/// Claude's adapter's `_meta`, naming the tool `name`.
fn named(name: &str) -> Value {
    json!({ "claudeCode": { "toolName": name } })
}

/// A piece, as JSON.
fn piece(kind: &str, value: Value) -> Value {
    json!({ "type": kind, "value": value })
}

#[test]
fn what_a_call_gave_back_is_read_into_pieces_and_nothing_is_dropped() {
    let mut script = running();
    let audio = json!({ "type": "content", "content": { "type": "audio", "mimeType": "audio/wav", "data": "AAAA" } });
    let newer = json!({ "type": "hologram", "frames": 3 });
    script.update(PROC, SESSION, &json!({
        "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Look", "kind": "read", "status": "completed",
        "content": [
            { "type": "content", "content": { "type": "text", "text": "line\u{202e}one\r\n" } },
            { "type": "diff", "path": "/work/new.rs", "oldText": null, "newText": "fn main() {}\n" },
            { "type": "diff", "path": "/work/lib.rs", "oldText": "a\nb\n", "newText": "a\nc\n" },
            { "type": "terminal", "terminalId": "term-1" },
            { "type": "content", "content": { "type": "image", "mimeType": "image/png", "data": "aGVsbG8=" } },
            { "type": "content", "content": { "type": "resource_link", "uri": "file:///work/a.md", "name": "a.md" } },
            audio,
            newer,
        ],
    }));
    let tool = the_tool(&script);
    let added = |old: Option<u64>, new: Option<u64>, change: &str, text: &str| json!({ "change": { "type": change }, "old": old, "new": new, "text": text });
    assert_eq!(
        tool["result"],
        json!([
            piece("Text", json!({ "text": "line<U+202E>one\n", "cut": null })),
            piece(
                "Diff",
                json!({ "path": "/work/new.rs", "new_file": true, "added": 1, "removed": 0, "more": 0,
                "hunks": [{ "lines": [added(None, Some(1), "Added", "fn main() {}")] }] })
            ),
            piece(
                "Diff",
                json!({ "path": "/work/lib.rs", "new_file": false, "added": 1, "removed": 1, "more": 0,
                "hunks": [{ "lines": [
                    added(Some(1), Some(1), "Kept", "a"),
                    added(Some(2), None, "Removed", "b"),
                    added(None, Some(2), "Added", "c"),
                ] }] })
            ),
            piece(
                "Output",
                json!({ "lines": [], "dropped": 0, "ending": { "type": "Unknown" } })
            ),
            piece(
                "Image",
                json!({ "mime": "image/png", "data": "aGVsbG8=", "size": 5 })
            ),
            piece(
                "Link",
                json!({ "uri": "file:///work/a.md", "name": "a.md" })
            ),
            piece(
                "Unknown",
                json!({ "what": "content", "raw": audio, "summary": null, "cut": null })
            ),
            piece(
                "Unknown",
                json!({ "what": "hologram", "raw": newer, "summary": null, "cut": null })
            ),
        ])
    );
}

#[test]
fn an_update_replaces_what_a_call_gave_back_and_raw_output_is_the_fallback() {
    let mut script = running();
    // Its fixture would be mostly the long text.
    script.without_fixtures();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit lib.rs", "kind": "edit", "status": "pending",
        "content": [{ "type": "diff", "path": "/work/lib.rs", "oldText": "old snippet", "newText": "new snippet" }] }));
    // Claude sends the diff of the edit's input first, then the file's.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
        "content": [{ "type": "diff", "path": "/work/lib.rs", "oldText": "1\nold snippet\n3\n", "newText": "1\nnew snippet\n3\n" }] }));
    let result = &the_tool(&script)["result"];
    assert_eq!(result.as_array().map(Vec::len), Some(1));
    assert_eq!(result[0]["value"]["hunks"][0]["lines"][0]["text"], "1");
    // With no content, the raw output is its text.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "rawOutput": { "ok": true } }));
    assert_eq!(
        the_tool(&script)["result"],
        json!([piece(
            "Text",
            json!({ "text": "{\"ok\":true}", "cut": null })
        )])
    );
    // Text too long keeps its start.
    let long = "x".repeat(MOST_TEXT + 10);
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
        "content": [{ "type": "content", "content": { "type": "text", "text": long } }] }),
    );
    let text = &the_tool(&script)["result"][0]["value"];
    assert_eq!(text["text"].as_str().map(str::len), Some(MOST_TEXT));
    assert_eq!(
        text["cut"],
        json!({ "kept": MOST_TEXT, "total": MOST_TEXT + 10 })
    );
}

#[test]
fn an_image_too_big_keeps_only_its_size_and_input_and_locations_are_capped() {
    let mut script = running();
    // Its fixture would be mostly the long input.
    script.without_fixtures();
    let big = "A".repeat((MOST_IMAGE + 3) / 3 * 4);
    let locations: Vec<Value> = (0..MOST_LOCATIONS + 2)
        .map(|n| json!({ "path": format!("/work/{n}.rs"), "line": n }))
        .collect();
    let input = json!({ "content": "y".repeat(MOST_INPUT) });
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Shot", "status": "completed",
        "rawInput": input, "locations": locations,
        "content": [{ "type": "content", "content": { "type": "image", "mimeType": "image/png", "data": big } }] }));
    let tool = the_tool(&script);
    assert_eq!(tool["result"][0]["value"]["data"], Value::Null);
    assert_eq!(
        tool["result"][0]["value"]["size"],
        json!((MOST_IMAGE + 3) / 3 * 3)
    );
    assert_eq!(
        tool["locations"].as_array().map(Vec::len),
        Some(MOST_LOCATIONS)
    );
    assert_eq!(
        tool["locations"][1],
        json!({ "path": "/work/1.rs", "line": 1 })
    );
    assert_eq!(tool["locations_cut"], 2);
    let kept = tool["input"].as_str().expect("the start of its JSON");
    assert!(input.to_string().starts_with(kept) && kept.len() == MOST_INPUT);
    assert_eq!(
        tool["input_cut"],
        json!({ "kept": MOST_INPUT, "total": input.to_string().len() })
    );
}

#[test]
fn whose_tool_a_call_is_comes_from_the_adapters_metadata_never_its_title() {
    let mut script = running();
    let call = |id: &str, title: &str, meta: Option<Value>| {
        let mut update = json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": title, "status": "pending" });
        if let Some(meta) = meta {
            update["_meta"] = meta;
        }
        update
    };
    script.update(
        PROC,
        SESSION,
        &call("t1", "Search the web", Some(named("WebSearch"))),
    );
    script.update(
        PROC,
        SESSION,
        &call("t2", "Look", Some(named("mcp__eyespy__look_at"))),
    );
    script.update(
        PROC,
        SESSION,
        &call("t3", "Read a.rs", Some(named("mcp__caf__Read"))),
    );
    script.update(
        PROC,
        SESSION,
        &call("t4", "Fly", Some(named("mcp__caf__Fly"))),
    );
    // The model wrote these titles: they say nothing of whose tool it is.
    script.update(PROC, SESSION, &call("t5", "mcp__caf__Bash", None));
    script.update(
        PROC,
        SESSION,
        &call(
            "t6",
            "mcp__caf__Bash",
            Some(json!({ "claudeCode": { "toolName": 7 } })),
        ),
    );
    let origins: Vec<Value> = (tools(&script.chat(CHAT)).iter())
        .map(|tool| tool["origin"].clone())
        .collect();
    assert_eq!(
        origins,
        [
            json!({ "type": "Agent", "value": "WebSearch" }),
            json!({ "type": "Server", "value": { "server": "eyespy", "tool": "look_at" } }),
            json!({ "type": "Caf", "value": { "type": "Read" } }),
            json!({ "type": "Server", "value": { "server": "caf", "tool": "Fly" } }),
            json!({ "type": "NotRecorded" }),
            json!({ "type": "NotRecorded" }),
        ]
    );
    // A plain agent's metadata is its own: CAF doesn't read it.
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("plain", "/work"))
        .0
        .expect("opened");
    script.answers(PROC, 1000, &super::plain_init(false));
    script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": SESSION }),
    );
    script.send(CHAT, "go").0.expect("sent");
    script.update(
        PROC,
        SESSION,
        &call("t1", "Read", Some(named("mcp__caf__Read"))),
    );
    assert_eq!(
        the_tool(&script)["origin"],
        json!({ "type": "NotRecorded" })
    );
}

#[test]
fn a_call_the_user_is_asked_about_is_asking_and_records_the_option_chosen() {
    let mut script = running();
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "tool_call", "toolCallId": "tool-1", "title": "Run `cargo test`",
        "kind": "execute", "status": "pending", "_meta": named("mcp__shell__run") }),
    );
    assert_eq!(the_tool(&script)["decision"], json!({ "type": "NotAsked" }));
    let out = script.says(PROC, &permission(7, "tool-1"));
    assert_has(
        &json!(with(&out, "decide")),
        &json!([{ "item": 1004, "origin": { "type": "Server", "value": { "server": "shell", "tool": "run" } },
                  "action": { "type": "Tool", "value": { "name": "run" } } }]),
    );
    assert_eq!(
        the_tool(&script)["decision"],
        json!({ "type": "Undecided" })
    );
    let ticket = with(&out, "decide")[0]["decide"]
        .as_u64()
        .expect("a ticket");
    script.policy(ticket, PolicyAnswer::AskUser);
    assert_eq!(the_tool(&script)["status"], json!({ "type": "Asking" }));
    // What the agent says of it meanwhile doesn't take it from the user.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "tool-1", "status": "pending" }));
    assert_eq!(the_tool(&script)["status"], json!({ "type": "Asking" }));
    let question = script.chat(CHAT)["questions"][0]["id"]
        .as_u64()
        .expect("asked");
    script
        .answer(
            QuestionId(question),
            Answer::Permission(OptionId("always".to_owned())),
        )
        .0
        .expect("answered");
    let tool = the_tool(&script);
    assert_eq!(tool["status"], json!({ "type": "Pending" }));
    assert_eq!(
        tool["decision"],
        json!({ "type": "Allowed", "value": { "type": "User", "value": { "option": "Always allow" } } })
    );
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "tool-1", "status": "completed" }));
    assert_eq!(the_tool(&script)["status"], json!({ "type": "Done" }));
}

#[test]
fn a_call_refused_is_refused_with_the_policys_reason_and_stays_so() {
    let mut script = running();
    let out = script.says(PROC, &permission(7, "tool-1"));
    let ticket = with(&out, "decide")[0]["decide"]
        .as_u64()
        .expect("a ticket");
    script.event(Event::Policy {
        ticket: Ticket(ticket),
        answer: PolicyAnswer::Refuse,
        reason: Some("No tests on\u{202e} Fridays".to_owned()),
    });
    let refused = json!({ "type": "Refused", "value": { "type": "Policy", "value": { "reason": "No tests on<U+202E> Fridays" } } });
    let tool = the_tool(&script);
    assert_eq!(tool["status"], json!({ "type": "Refused" }));
    assert_eq!(tool["decision"], refused);
    // The agent then says it failed: it was refused, and never ran.
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "tool-1", "status": "failed" }),
    );
    assert_eq!(the_tool(&script)["status"], json!({ "type": "Refused" }));
    // The user refusing says which option they chose.
    let out = script.says(PROC, &permission(8, "tool-2"));
    let ticket = with(&out, "decide")[0]["decide"]
        .as_u64()
        .expect("a ticket");
    script.policy(ticket, PolicyAnswer::AskUser);
    let question = script.chat(CHAT)["questions"][0]["id"]
        .as_u64()
        .expect("asked");
    script
        .answer(
            QuestionId(question),
            Answer::Permission(OptionId("reject".to_owned())),
        )
        .0
        .expect("answered");
    let second = &tools(&script.chat(CHAT))[1];
    assert_eq!(second["status"], json!({ "type": "Refused" }));
    assert_eq!(
        second["decision"],
        json!({ "type": "Refused", "value": { "type": "User", "value": { "option": "Reject" } } })
    );
}

#[test]
fn a_permission_request_for_a_call_not_seen_says_whose_it_is_from_its_own_metadata() {
    let mut script = running();
    let mut request = permission(7, "tool-9");
    request["params"]["toolCall"]["_meta"] = json!({ "claudeCode": { "toolName": "mcp__eyes__look", "mcpServer": { "name": "eyes.app", "source": "sdk" } } });
    let out = script.says(PROC, &request);
    assert_has(
        &json!(with(&out, "decide")),
        &json!([{ "origin": { "type": "Server", "value": { "server": "eyes.app", "tool": "look" } },
                  "action": { "type": "Tool", "value": { "name": "look" } } }]),
    );
    assert_eq!(
        the_tool(&script)["origin"],
        json!({ "type": "Server", "value": { "server": "eyes.app", "tool": "look" } })
    );
    // A call seen with nothing said of whose it is takes the request's.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "tool-11", "title": "Search", "status": "pending" }));
    let mut request = permission(9, "tool-11");
    request["params"]["toolCall"]["_meta"] = named("WebSearch");
    script.says(PROC, &request);
    assert_eq!(
        tools(&script.chat(CHAT))[1]["origin"],
        json!({ "type": "Agent", "value": "WebSearch" })
    );
    // One that says nothing is not recorded, and has no action.
    let out = script.says(PROC, &permission(8, "tool-10"));
    assert_has(
        &json!(with(&out, "decide")),
        &json!([{ "origin": { "type": "NotRecorded" }, "action": null }]),
    );
    // The turn ending answers both.
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn every_call_settles_when_its_turn_ends() {
    // Finished: whatever hadn't ended failed.
    let mut script = running();
    for (id, status) in [
        ("t1", "pending"),
        ("t2", "in_progress"),
        ("t3", "completed"),
    ] {
        script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": id, "status": status }));
    }
    let out = script.says(PROC, &permission(7, "t4"));
    let ticket = with(&out, "decide")[0]["decide"]
        .as_u64()
        .expect("a ticket");
    script.policy(ticket, PolicyAnswer::AskUser);
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
    let statuses: Vec<Value> = (tools(&script.chat(CHAT)).iter())
        .map(|tool| tool["status"]["type"].clone())
        .collect();
    assert_eq!(
        statuses,
        [
            json!("Failed"),
            json!("Failed"),
            json!("Done"),
            json!("Failed")
        ]
    );
    // Asked, and never decided: it stays so.
    assert_eq!(
        tools(&script.chat(CHAT))[3]["decision"],
        json!({ "type": "Undecided" })
    );

    // A command's output that never said how it ended ends with its call.
    let mut script = running();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "make", "status": "in_progress",
        "content": [{ "type": "terminal", "terminalId": "term-1" }] }));
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
    let tool = the_tool(&script);
    assert_eq!(tool["status"], json!({ "type": "Failed" }));
    assert_eq!(
        tool["result"][0]["value"]["ending"],
        json!({ "type": "Unknown" })
    );

    // Stopped by the user: cancelled.
    let mut script = running();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "make", "status": "in_progress",
        "content": [{ "type": "terminal", "terminalId": "term-1" }] }));
    script.stop(CHAT).0.expect("stopping");
    script.answers(PROC, 1002, &json!({ "stopReason": "cancelled" }));
    let tool = the_tool(&script);
    assert_eq!(tool["status"], json!({ "type": "Cancelled" }));
    assert_eq!(
        tool["result"][0]["value"]["ending"],
        json!({ "type": "Unknown" })
    );
    // Nothing the agent says after reopens it.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "in_progress" }));
    assert_eq!(the_tool(&script)["status"], json!({ "type": "Cancelled" }));
    assert_eq!(tool["decision"], json!({ "type": "NotAsked" }));
}

#[test]
fn a_diff_of_part_of_a_file_is_numbered_from_where_its_location_says() {
    let mut script = running();
    // Claude's adapter, after an edit at line 500 and one at line 900:
    // one diff for each hunk, and a location for each, at the same place.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit lib.rs", "kind": "edit", "status": "completed",
        "content": [
            { "type": "diff", "path": "/work/lib.rs", "oldText": "a\nb\nc", "newText": "a\nB\nc" },
            { "type": "diff", "path": "/work/lib.rs", "oldText": null, "newText": "x\ny" },
        ],
        "locations": [{ "path": "/work/lib.rs", "line": 500 }, { "path": "/work/lib.rs", "line": 900 }] }));
    let result = &the_tool(&script)["result"];
    let numbers = |piece: usize| -> Vec<(Value, Value)> {
        (result[piece]["value"]["hunks"][0]["lines"]
            .as_array()
            .expect("lines")
            .iter())
        .map(|line| (line["old"].clone(), line["new"].clone()))
        .collect()
    };
    assert_eq!(
        numbers(0),
        [
            (json!(500), json!(500)),
            (json!(501), Value::Null),
            (Value::Null, json!(501)),
            (json!(502), json!(502))
        ]
    );
    assert_eq!(
        numbers(1),
        [(Value::Null, json!(900)), (Value::Null, json!(901))]
    );
    // A diff whose location is of another file, or has no line, is
    // numbered within what was sent.
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
        "content": [{ "type": "diff", "path": "/work/lib.rs", "oldText": "a", "newText": "b" }],
        "locations": [{ "path": "/work/other.rs", "line": 7 }] }),
    );
    assert_eq!(
        the_tool(&script)["result"][0]["value"]["hunks"][0]["lines"][0]["old"],
        1
    );
}

#[test]
fn a_call_has_one_output_and_a_piece_too_big_is_cut() {
    let mut script = running();
    script.without_fixtures();
    let second = json!({ "type": "terminal", "terminalId": "term-2" });
    let big = json!({ "type": "content", "content": { "type": "audio", "mimeType": "audio/wav", "data": "A".repeat(MOST_INPUT) } });
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "make", "status": "in_progress",
        "content": [{ "type": "terminal", "terminalId": "term-1" }, second, big] }));
    let result = the_tool(&script)["result"].clone();
    // A terminal's output runs as its call does.
    assert_eq!(
        result[0],
        piece(
            "Output",
            json!({ "lines": [], "dropped": 0, "ending": null })
        )
    );
    // A second terminal is kept as it came: a call has one output.
    assert_eq!(
        result[1],
        piece(
            "Unknown",
            json!({ "what": "terminal", "raw": second, "summary": null, "cut": null })
        )
    );
    let cut = &result[2]["value"];
    assert_eq!(result[2]["type"], "Unknown", "{cut:#}");
    let kept = cut["raw"]
        .as_str()
        .unwrap_or_else(|| panic!("{}", &cut.to_string()[..300]));
    assert!(big.to_string().starts_with(kept) && kept.len() == MOST_INPUT);
    assert_eq!(
        cut["cut"],
        json!({ "kept": MOST_INPUT, "total": big.to_string().len() })
    );
}

#[test]
fn a_location_or_a_status_that_cant_be_read_loses_nothing_else() {
    let mut script = running();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Look", "status": "in_progress",
        "locations": [
            { "path": "/work/a.rs", "line": 4_294_967_296_u64 },
            { "path": "/work/b.rs", "line": -1 },
            { "line": 3 },
            { "path": "/work/c.rs", "line": 3 },
        ] }));
    let tool = the_tool(&script);
    assert_eq!(
        tool["locations"],
        json!([{ "path": "/work/a.rs", "line": 4_294_967_296_u64 }, { "path": "/work/b.rs", "line": null }, { "path": "/work/c.rs", "line": 3 }])
    );
    assert_eq!(tool["locations_cut"], 1, "the one with no path");
    // A status newer than CAF, or one of ACP's next version, keeps the
    // rest of the update.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "paused", "kind": "teleport",
        "content": [{ "type": "content", "content": { "type": "text", "text": "half way" } }], "locations": "here" }));
    let tool = the_tool(&script);
    assert_eq!(tool["status"], json!({ "type": "Running" }));
    assert_eq!(tool["result"][0]["value"]["text"], "half way");
    assert_eq!(
        (tool["locations"].clone(), tool["locations_cut"].clone()),
        (json!([]), json!(1))
    );
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "cancelled" }),
    );
    assert_eq!(the_tool(&script)["status"], json!({ "type": "Cancelled" }));
}

#[test]
fn a_servers_name_is_told_from_its_tools_by_the_chats_own_servers() {
    let mut script = Script::with_claude_given(&["a__b", "my tools", "my", "my\u{1F600}"]);
    script.send(CHAT, "go").0.expect("sent");
    for (id, name) in [
        ("t1", "mcp__a__b__x"),
        ("t2", "mcp__my_tools__look"),
        ("t3", "mcp__my__look"),
        ("t4", "mcp__other__a__b"),
        // Claude writes an emoji as two `_`, one for each half of it.
        ("t5", "mcp__my____look"),
    ] {
        script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": "Go", "status": "pending", "_meta": named(name) }));
    }
    let origins: Vec<Value> = (tools(&script.chat(CHAT)).iter())
        .map(|tool| tool["origin"]["value"].clone())
        .collect();
    assert_eq!(
        origins,
        [
            json!({ "server": "a__b", "tool": "x" }),
            json!({ "server": "my tools", "tool": "look" }),
            json!({ "server": "my", "tool": "look" }),
            // Not the chat's: split at its first `__`.
            json!({ "server": "other", "tool": "a__b" }),
            json!({ "server": "my\u{1F600}", "tool": "look" }),
        ]
    );
    // A permission request naming the server corrects what was read.
    let mut request = permission(7, "t4");
    request["params"]["toolCall"]["_meta"] = json!({ "claudeCode": { "toolName": "mcp__other__a__b", "mcpServer": { "name": "other__a", "source": "sdk" } } });
    let out = script.says(PROC, &request);
    let fixed = json!({ "type": "Server", "value": { "server": "other__a", "tool": "b" } });
    assert_has(&json!(with(&out, "decide")), &json!([{ "origin": fixed }]));
    assert_eq!(tools(&script.chat(CHAT))[3]["origin"], fixed);
    // The tool named otherwise, what it does is too.
    assert_eq!(
        tools(&script.chat(CHAT))[3]["action"],
        json!({ "type": "Tool", "value": { "name": "b" } })
    );
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn a_call_claudes_adapter_says_failed_as_the_user_stops_its_turn_was_cancelled() {
    let mut script = running();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "toolu_01", "title": "Search", "kind": "search", "status": "in_progress" }));
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "toolu_02", "title": "Read", "kind": "read", "status": "completed" }));
    script.stop(CHAT).0.expect("stopping");
    // claude-agent-acp 0.98.0, to a client of ACP's first version, after
    // session/cancel and before it answers the prompt.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "toolu_01", "status": "failed",
        "content": [{ "type": "content", "content": { "type": "text", "text": "The turn was cancelled before this tool call returned a result" } }] }));
    script.answers(PROC, 1002, &json!({ "stopReason": "cancelled" }));
    let tools = tools(&script.chat(CHAT));
    assert_eq!(tools[0]["status"], json!({ "type": "Cancelled" }));
    assert_eq!(
        tools[0]["result"][0]["value"]["text"],
        "The turn was cancelled before this tool call returned a result"
    );
    assert_eq!(tools[1]["status"], json!({ "type": "Done" }));
    // Not while no one is stopping it: then it failed.
    let mut script = running();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Search", "status": "failed" }));
    assert_eq!(the_tool(&script)["status"], json!({ "type": "Failed" }));
}

#[test]
fn a_reported_call_does_what_its_origin_says() {
    let mut script = running();
    for (id, name) in [
        ("t1", Some("WebSearch")),
        ("t2", Some("mcp__eyespy__look")),
        ("t3", Some("mcp__caf__Read")),
        ("t4", None),
    ] {
        let mut update = json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": "Go", "status": "pending" });
        if let Some(name) = name {
            update["_meta"] = named(name);
        }
        script.update(PROC, SESSION, &update);
    }
    let actions: Vec<Value> = (tools(&script.chat(CHAT)).iter())
        .map(|tool| tool["action"].clone())
        .collect();
    assert_eq!(
        actions,
        [
            json!({ "type": "Tool", "value": { "name": "WebSearch" } }),
            json!({ "type": "Tool", "value": { "name": "look" } }),
            // CAF reads its own tools' actions itself.
            Value::Null,
            Value::Null,
        ]
    );
}

#[test]
fn a_call_imported_unfinished_failed_and_its_output_ended() {
    let mut script = Script::new(host());
    let mut open = OpenChat::new("plain", "/work");
    open.session =
        crate::core::SessionChoice::Continue(crate::ids::AgentSessionId("old".to_owned()));
    script.open(open).0.expect("opened");
    script.answers(PROC, 1000, &super::plain_init(true));
    script.update(PROC, "old", &json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "build it" } }));
    script.update(PROC, "old", &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "make", "status": "in_progress",
        "content": [{ "type": "terminal", "terminalId": "term-1" }] }));
    script.answers(PROC, 1001, &json!({}));
    let tool = the_tool(&script);
    assert_eq!(tool["status"], json!({ "type": "Failed" }));
    assert_eq!(
        tool["result"][0]["value"]["ending"],
        json!({ "type": "Unknown" })
    );
}

#[test]
fn the_diffs_of_one_update_share_what_working_them_out_may_cost() {
    let mut script = running();
    script.without_fixtures();
    // Three diffs of 4,000 lines, every tenth changed: 8,000 lines to
    // compare each, and 10,000 between them all.
    let before: String = (0..4_000).map(|n| format!("{n}\n")).collect();
    let after: String = (0..4_000)
        .map(|n| {
            if n % 10 == 0 {
                format!("x{n}\n")
            } else {
                format!("{n}\n")
            }
        })
        .collect();
    let diff =
        |path: &str| json!({ "type": "diff", "path": path, "oldText": before, "newText": after });
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit", "status": "completed",
        "content": [diff("/a"), diff("/b"), diff("/c")] }));
    let result = the_tool(&script)["result"].clone();
    let shape = |at: usize| {
        let diff = &result[at]["value"];
        (
            diff["hunks"].as_array().map_or(0, Vec::len) > 0,
            diff["more"].clone(),
            diff["added"].clone(),
        )
    };
    assert!(shape(0).0, "the first is worked out");
    for at in [1, 2] {
        assert_eq!(shape(at), (false, json!(1), json!(3_991)), "diff {at}");
    }
}
