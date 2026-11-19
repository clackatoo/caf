//! Recorded traces: what happened, as JSON events, replayed into a fresh
//! core. A bug becomes one of these, with what the core must give out
//! and end as.

use serde_json::json;

use super::harness::{Script, assert_has, host};
use super::stop::permission;
use super::{CHAT, PROC, SESSION};
use crate::event::PolicyAnswer;

/// The scenario `traces/claude-turn.json` was recorded from.
fn claude_turn() -> Script {
    let mut script = Script::with_claude();
    script.at(2000);
    script.send(CHAT, "Run the tests").0.expect("sent");
    script.at(2100);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Running them." } }));
    script.says(PROC, &permission(0, "toolu_01"));
    script.at(2500);
    script.policy(1005, PolicyAnswer::Allow);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "toolu_01", "status": "completed", "rawOutput": "test result: ok" }));
    script.at(3000);
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
    script
}

#[test]
fn a_recorded_claude_turn_replays_from_json() {
    let script = Script::from_recording(host(), include_str!("traces/claude-turn.json"));
    assert_has(&script.chat(CHAT), &json!({
        "state": { "Open": { "session": SESSION } },
        "turns": [{ "life": "Finished", "items": [
            { "kind": { "Words": "Running them." } },
            { "kind": { "Tool": { "title": "Run `cargo test`", "status": "Done", "output": "test result: ok" } } }
        ] }]
    }));
    // The recording is what the scenario records today.
    assert_eq!(claude_turn().recording(), serde_json::from_str::<serde_json::Value>(include_str!("traces/claude-turn.json")).expect("JSON"));
}

#[test]
#[ignore = "prints the recording, to make the trace"]
fn print_claude_turn() {
    println!("{}", serde_json::to_string_pretty(&claude_turn().recording()).expect("JSON"));
}
