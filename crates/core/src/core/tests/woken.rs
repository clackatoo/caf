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
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "starter": { "type": "Agent" }, "life": { "type": "Running" } }] }),
    );
    script.at(100_000);
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed" }),
    );
    script.at(160_000);
    script.woken();
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{
        "starter": { "type": "Agent" }, "life": { "type": "Finished" },
        "items": [{ "kind": { "type": "Words", "value": "Checking the build." } }, { "kind": { "type": "Tool", "value": { "status": { "type": "Done" } } } }]
    }] }),
    );
}

#[test]
fn stop_ends_an_agent_turn_and_what_was_in_flight_opens_no_new_one() {
    let mut script = Script::with_claude();
    script.update(PROC, SESSION, &words("Committing."));
    let (stopped, out) = script.stop(CHAT);
    stopped.expect("stopped");
    assert_eq!(
        sent(&out, "session/cancel")["params"],
        json!({ "sessionId": SESSION })
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "starter": { "type": "Agent" }, "life": { "type": "Stopped" } }] }),
    );
    script.at(1000);
    let out = script.update(PROC, SESSION, &words(" done"));
    assert_eq!(with(&out, "report").len(), 1);
    assert_eq!(script.chat(CHAT)["turns"].as_array().map(Vec::len), Some(1));
    script.at(2000);
    script.update(PROC, SESSION, &words("Later work."));
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "life": { "type": "Stopped" } }, { "starter": { "type": "Agent" }, "life": { "type": "Running" } }] }),
    );
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
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "notice", "severity": "info", "title": "Compacting" }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "items": [{ "kind": { "type": "Unknown", "value": { "what": "notice" } } }] }] }),
    );
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn an_agent_turn_ended_any_way_leaves_no_wake_up_behind() {
    // Ended by its quiet timer.
    let mut script = Script::with_claude();
    script.at(1000);
    script.update(PROC, SESSION, &words("hi"));
    script.at(70_000);
    script.woken();
    assert_eq!(script.state()["wake"], Value::Null);
    // Ended by the next message.
    let mut script = Script::with_claude();
    script.at(1000);
    script.update(PROC, SESSION, &words("hi"));
    script.send(CHAT, "go").0.expect("sent");
    assert_eq!(script.state()["runs"]["1000"]["quiet_by"], Value::Null);
    script.at(70_000);
    script.woken();
    script.answers(PROC, 1003, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn an_agent_turn_waiting_on_a_tool_or_a_question_isnt_quiet() {
    let mut script = Script::with_claude();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "rm", "status": "pending" }));
    let out = script.says(PROC, &super::stop::permission(7, "t2"));
    let ticket = with(&out, "decide")[0]["decide"]
        .as_u64()
        .expect("a ticket");
    script.policy(ticket, crate::event::PolicyAnswer::AskUser);
    script.at(60_000);
    script.woken();
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "starter": { "type": "Agent" }, "life": { "type": "Running" } }] }),
    );
    assert_eq!(
        script.chat(CHAT)["questions"].as_array().map(Vec::len),
        Some(1)
    );
    script.stop(CHAT).0.expect("stopped");
}

#[test]
fn a_tool_call_dropped_as_in_flight_stays_dropped() {
    let mut script = Script::with_claude();
    script.update(PROC, SESSION, &words("Committing."));
    script.stop(CHAT).0.expect("stopped");
    script.at(500);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "late", "title": "git commit", "status": "in_progress" }));
    script.at(5000);
    let out = script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "late", "status": "completed" }));
    assert_eq!(with(&out, "report").len(), 1);
    assert_eq!(script.chat(CHAT)["turns"].as_array().map(Vec::len), Some(1));
}

#[test]
fn a_notice_outside_a_turn_is_reported_with_what_it_says() {
    let mut script = Script::with_claude();
    let out = script.update(PROC, SESSION, &json!({
        "sessionUpdate": "notice", "severity": "warning", "title": "Fast mode\u{202e} turned off", "description": "Usage limit reached."
    }));
    let message = with(&out, "report")[0]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("Fast mode<U+202E> turned off")
            && message.contains("Usage limit reached."),
        "{message}"
    );
}
