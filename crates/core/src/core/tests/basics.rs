//! Starting an adapter, opening a session, and a turn from start to end.

use serde_json::json;

use super::harness::{Script, assert_has, host, sent, with};
use super::{CHAT, PROC, SESSION, claude_init};
use crate::core::OpenChat;

#[test]
fn a_chat_starts_its_adapter_opens_a_session_and_runs_a_turn() {
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("claude", "/work"));
    assert_eq!(chat, Ok(CHAT));
    assert_has(
        &json!(out[0]),
        &json!({ "start": PROC, "program": "/opt/caf/claude-agent-acp" }),
    );
    assert_has(
        &sent(&out, "initialize"),
        &json!({
            "id": 1000,
            "params": {
                "protocolVersion": 1,
                "clientInfo": { "name": "caf-test", "version": "1.2.3" },
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                    "session": { "notices": {} },
                    "elicitation": { "form": {}, "url": {} }
                }
            }
        }),
    );
    // Waits for initialize: 30 s.
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 30_000 })]);
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Opening" } }),
    );

    script.at(240);
    let out = script.answers(PROC, 1000, &claude_init());
    assert_has(
        &sent(&out, "session/new"),
        &json!({ "id": 1001, "params": { "cwd": "/work", "mcpServers": [] } }),
    );
    // The session must open within 30 s.
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 30_240 })]);
    assert_has(
        &script.state(),
        &json!({ "conns": { "1001": { "profile": {
        "profile": { "type": "Builtin", "value": { "type": "ClaudeAgentAcp" } },
        "adapter": { "name": "@agentclientprotocol/claude-agent-acp", "version": "0.98.0" },
        "checked": { "type": "Yes" }
    } } } }),
    );

    script.at(1000);
    let out = script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": SESSION }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Open", "value": { "session": SESSION } } }),
    );
    // Nothing waits on a deadline now.
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": null })]);

    script.at(2000);
    let (turn, out) = script.send(CHAT, "Fix the tests");
    let turn = turn.expect("sent");
    assert_has(
        &sent(&out, "session/prompt"),
        &json!({
            "id": 1002,
            "params": { "sessionId": SESSION, "prompt": [{ "type": "text", "text": "Fix the tests" }] }
        }),
    );

    script.at(2100);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "On " } }));
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "it." } }));
    script.at(3000);
    let out = script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
    assert!(with(&out, "changed").len() == 1, "viewers hear: {out:#?}");
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{
        "id": turn.0,
        "starter": { "type": "User", "value": { "parts": [{ "type": "Text", "value": "Fix the tests" }] } },
        "life": { "type": "Finished" },
        "window": { "start": 1_800_000_002_000_i64, "end": { "type": "At", "value": 1_800_000_003_000_i64 } },
        "items": [{ "kind": { "type": "Words", "value": "On it." }, "window": { "end": { "type": "At", "value": 1_800_000_003_000_i64 } } }],
        "imported": false
    }] }),
    );
}
