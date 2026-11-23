//! Continuing a session the agent knows: resumed, loaded with its history
//! imported, or, when that can't be, a new one the chat says it is.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, host, replies, sent, with};
use super::{CHAT, PROC, claude_init, plain_init};
use crate::change::{Change, Read};
use crate::core::{OpenChat, SessionChoice};
use crate::ids::{AgentSessionId, ChangeNo};

/// Opening a chat with `agent` that continues session `old`.
fn continuing(agent: &str, old: &str) -> OpenChat {
    let mut open = OpenChat::new(agent, "/work");
    open.session = SessionChoice::Continue(AgentSessionId(old.to_owned()));
    open
}

/// A chunk of `kind` saying `text`.
fn chunk(kind: &str, text: &str) -> Value {
    json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
}

#[test]
fn a_session_the_agent_no_longer_has_is_opened_new_and_the_chat_says_so() {
    let mut script = Script::new(host());
    script.open(continuing("claude", "old")).0.expect("opened");
    let out = script.answers(PROC, 1000, &claude_init());
    assert_has(
        &sent(&out, "session/resume"),
        &json!({ "params": { "sessionId": "old", "cwd": "/work", "mcpServers": [] } }),
    );
    let out = script.fails(
        PROC,
        1001,
        &json!({ "code": -32002, "message": "Resource not found: old" }),
    );
    let new = sent(&out, "session/new");
    assert!(new["params"].get("sessionId").is_none());
    script.answers(
        PROC,
        1002,
        &json!({ "modes": super::default_modes(), "sessionId": "fresh" }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Open", "value": { "session": "fresh" } },
            "turns": [{ "starter": { "type": "Caf" }, "life": { "type": "Finished" }, "items": [{ "kind": { "type": "Note", "value": { "type": "SessionGone" } } }] }]
        }),
    );
}

#[test]
fn an_agent_that_cant_continue_sessions_gets_a_new_one_and_the_chat_says_so() {
    let mut script = Script::new(host());
    script.open(continuing("plain", "old")).0.expect("opened");
    let out = script.answers(PROC, 1000, &plain_init(false));
    // A new session, as the adapter can't load one.
    sent(&out, "session/new");
    script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": "fresh" }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "starter": { "type": "Caf" }, "items": [{ "kind": { "type": "Note", "value": { "type": "CannotContinue" } } }] }] }),
    );
}

#[test]
fn a_loaded_session_imports_its_history_and_nothing_replayed_acts() {
    let mut script = Script::new(host());
    let (_, out) = script.open(continuing("plain", "old"));
    let declared = &sent(&out, "initialize")["params"]["clientCapabilities"];
    assert_eq!(
        declared,
        &json!({ "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false })
    );
    let out = script.answers(PROC, 1000, &plain_init(true));
    assert_has(
        &sent(&out, "session/load"),
        &json!({ "id": 1001, "params": { "sessionId": "old" } }),
    );
    let (waiting, _) = script.send(CHAT, "and now?");
    let waiting = waiting.expect("queued");

    script.at(10_000);
    script.update(PROC, "old", &chunk("user_message_chunk", "first"));
    script.update(PROC, "old", &chunk("user_message_chunk", "with context"));
    script.update(PROC, "old", &chunk("agent_thought_chunk", "hmm"));
    script.update(PROC, "old", &chunk("agent_message_chunk", "Sure, "));
    script.update(PROC, "old", &chunk("agent_message_chunk", "done."));
    script.update(PROC, "old", &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit", "kind": "edit", "status": "in_progress" }));
    script.update(PROC, "old", &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "content": [{ "type": "content", "content": { "type": "text", "text": "edited" } }] }));
    script.at(20_000);
    let out = script.update(PROC, "old", &chunk("user_message_chunk", "second"));
    // Each update replayed shows the agent is still at it.
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 50_000 })]);
    let out = script.says(PROC, &json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/request_permission",
        "params": { "sessionId": "old", "toolCall": { "toolCallId": "t2" }, "options": [{ "optionId": "y", "name": "Yes", "kind": "allow_once" }] }
    }));
    assert_eq!(
        replies(&out, &json!(3))[0]["result"],
        json!({ "outcome": { "outcome": "cancelled" } })
    );
    assert!(with(&out, "decide").is_empty(), "no policy is asked");
    // A replayed turn shows once it's whole: the first is, the second
    // isn't yet.
    assert_eq!(script.chat(CHAT)["turns"].as_array().map(Vec::len), Some(2));

    let out = script.answers(PROC, 1001, &json!({}));
    assert_has(
        &sent(&out, "session/prompt"),
        &json!({ "params": { "sessionId": "old", "prompt": [{ "text": "and now?" }] } }),
    );
    let unknown = json!({ "start": null, "end": { "type": "Unknown" } });
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Open", "value": { "session": "old" } },
            "turns": [
                {
                    "starter": { "type": "User", "value": { "parts": [{ "type": "Text", "value": "first" }, { "type": "Text", "value": "with context" }] } },
                    "life": { "type": "Finished" }, "imported": true, "window": unknown,
                    "items": [
                        { "kind": { "type": "Thinking", "value": "hmm" }, "window": unknown },
                        { "kind": { "type": "Words", "value": "Sure, done." } },
                        { "kind": { "type": "Tool", "value": { "title": "Edit", "kind": { "type": "Edit" }, "status": { "type": "Failed" }, "result": [{ "type": "Text", "value": { "text": "edited", "cut": null } }] } } }
                    ]
                },
                { "starter": { "type": "User", "value": { "parts": [{ "type": "Text", "value": "second" }] } }, "imported": true, "items": [] },
                // History goes before the message sent while it loaded.
                { "id": waiting.0, "life": { "type": "Running" } }
            ]
        }),
    );
    // Viewers hear where each went.
    let Some(Read::Changes(changes)) = script.core.read(CHAT, Some(ChangeNo(999))) else {
        panic!("changes")
    };
    let inserted = changes.iter().filter(|each| matches!(&each.change, Change::TurnInserted { before, .. } if *before == waiting)).count();
    assert_eq!(inserted, 2);

    // After the load: chat-wide updates aren't turns, and a later update
    // to a replayed tool call lands on its own item, which stays as its
    // turn ended it.
    let out = script.update(
        PROC,
        "old",
        &json!({ "sessionUpdate": "available_commands_update", "availableCommands": [] }),
    );
    assert_eq!(with(&out, "report").len(), 1);
    script.update(
        PROC,
        "old",
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed", "rawOutput": "late" }),
    );
    assert_has(
        &script.chat(CHAT)["turns"][0]["items"][2],
        &json!({ "kind": { "type": "Tool", "value": { "status": { "type": "Failed" },
            "result": [{ "type": "Text", "value": { "text": "late", "cut": null } }] } } }),
    );
    assert_eq!(script.chat(CHAT)["turns"][2]["items"], json!([]));
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn a_load_that_fails_fails_the_waiting_message_visibly() {
    let mut script = Script::new(host());
    script.open(continuing("plain", "old")).0.expect("opened");
    script.answers(PROC, 1000, &plain_init(true));
    let (waiting, _) = script.send(CHAT, "and now?");
    script.update(PROC, "old", &chunk("user_message_chunk", "first"));
    script.fails(
        PROC,
        1001,
        &json!({ "code": -32603, "message": "Internal error: disk \u{202e}full" }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "Refused", "value": { "message": "Internal error: disk <U+202E>full" } } } },
            "turns": [
                { "imported": true, "starter": { "type": "User", "value": { "parts": [{ "type": "Text", "value": "first" }] } } },
                { "id": waiting.expect("queued").0, "life": { "type": "Failed", "value": { "reason": { "type": "Other" }, "agent_said": "Internal error: disk <U+202E>full" } } }
            ]
        }),
    );
}
