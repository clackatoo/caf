//! Turns the agent takes by itself: opened only by its own output, ended
//! by a quiet spell or by Stop.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, sent, with};
use super::{CHAT, PROC, SESSION};

fn words(text: &str) -> Value {
    json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } })
}

#[test]
fn an_agent_turn_ends_after_a_quiet_minute_unless_a_tool_still_runs() {
    let mut script = Script::with_claude();
    script.at(1000);
    let out = script.update(PROC, SESSION, &words("Checking the build."));
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 61_000 })]);
    script.at(30_000);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "make", "status": "in_progress" }));
    script.at(90_000);
    script.woken();
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "starter": "Agent", "life": "Running" }] }));
    script.at(100_000);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed" }));
    script.at(160_000);
    script.woken();
    assert_has(&script.chat(CHAT), &json!({ "turns": [{
        "starter": "Agent", "life": "Finished",
        "items": [{ "kind": { "Words": "Checking the build." } }, { "kind": { "Tool": { "status": "Done" } } }]
    }] }));
}

#[test]
fn stop_ends_an_agent_turn_and_what_was_in_flight_opens_no_new_one() {
    let mut script = Script::with_claude();
    script.update(PROC, SESSION, &words("Committing."));
    let (stopped, out) = script.stop(CHAT);
    stopped.expect("stopped");
    assert_eq!(sent(&out, "session/cancel")["params"], json!({ "sessionId": SESSION }));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "starter": "Agent", "life": "Stopped" }] }));
    script.at(1000);
    let out = script.update(PROC, SESSION, &words(" done"));
    assert_eq!(with(&out, "report").len(), 1);
    assert_eq!(script.chat(CHAT)["turns"].as_array().map(Vec::len), Some(1));
    script.at(2000);
    script.update(PROC, SESSION, &words("Later work."));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "life": "Stopped" }, { "starter": "Agent", "life": "Running" }] }));
}

#[test]
fn notices_and_unknown_updates_never_open_a_turn() {
    let mut script = Script::with_claude();
    for update in [
        json!({ "sessionUpdate": "notice", "severity": "warning", "title": "Fast mode turned off" }),
        json!({ "sessionUpdate": "hologram" }),
        json!({ "sessionUpdate": "plan", "entries": [] }),
        words("   "),
    ] {
        let out = script.update(PROC, SESSION, &update);
        assert_eq!(with(&out, "report").len(), 1, "{update}");
    }
    assert_eq!(script.chat(CHAT)["turns"], json!([]));
    // Inside a turn, a notice is kept as an item.
    script.send(CHAT, "go").0.expect("sent");
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "notice", "severity": "info", "title": "Compacting" }));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "items": [{ "kind": { "Unknown": { "what": "notice" } } }] }] }));
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
}
