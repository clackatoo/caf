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
                json!({ "lines": [], "dropped": 0, "ending": null })
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
                json!({ "what": "content", "raw": audio, "summary": null })
            ),
            piece(
                "Unknown",
                json!({ "what": "hologram", "raw": newer, "summary": null })
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
    script.answers(PROC, 1001, &json!({ "sessionId": SESSION }));
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

    // Stopped by the user: cancelled.
    let mut script = running();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "make", "status": "in_progress" }));
    script.stop(CHAT).0.expect("stopping");
    script.answers(PROC, 1002, &json!({ "stopReason": "cancelled" }));
    let tool = the_tool(&script);
    assert_eq!(tool["status"], json!({ "type": "Cancelled" }));
    assert_eq!(tool["decision"], json!({ "type": "NotAsked" }));
}
