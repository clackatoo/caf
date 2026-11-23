//! Stop: answering the agent's questions first, waiting for it to stop,
//! and giving up on it if it doesn't.

use serde_json::json;

use super::harness::{Script, assert_has, host, replies, sent, with, writes};
use super::{CHAT, PROC, SESSION, claude_init};
use crate::core::{OpenChat, Refused};
use crate::event::{AgentEvent, Exit};

/// A permission request from the agent, as Claude's adapter sends one.
pub(super) fn permission(id: u64, tool: &str) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "session/request_permission",
        "params": {
            "sessionId": SESSION,
            "toolCall": { "toolCallId": tool, "title": "Run `cargo test`", "kind": "execute", "rawInput": { "command": "cargo test" } },
            "options": [
                { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                { "optionId": "always", "name": "Always allow", "kind": "allow_always" },
                { "optionId": "reject", "name": "Reject", "kind": "reject_once" }
            ]
        }
    })
}

#[test]
fn stop_answers_the_agents_questions_then_tells_it_and_waits() {
    let mut script = Script::with_claude();
    let (turn, _) = script.send(CHAT, "go");
    let turn = turn.expect("sent");
    let out = script.says(PROC, &permission(7, "tool-1"));
    assert_has(
        &json!(with(&out, "decide")),
        &json!([{ "decide": 1005, "chat": CHAT, "title": "Run `cargo test`", "input": "{\"command\":\"cargo test\"}" }]),
    );

    script.at(500);
    let (stopped, out) = script.stop(CHAT);
    stopped.expect("stopping");
    // ACP: the questions are answered "cancelled" before the agent hears
    // to stop.
    let written = writes(&out);
    assert_eq!(
        written[0].1,
        json!({ "jsonrpc": "2.0", "id": 7, "result": { "outcome": { "outcome": "cancelled" } } })
    );
    assert_eq!(written[1].1["method"], "session/cancel");
    assert_eq!(written[1].1["params"], json!({ "sessionId": SESSION }));
    assert!(out.contains(&json!({ "withdrawn": 1005 })));
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 15_500 })]);
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "id": turn.0, "life": { "type": "Stopping" } }] }),
    );

    script.at(600);
    let out = script.answers(PROC, 1002, &json!({ "stopReason": "cancelled" }));
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": null })]);
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{
        "id": turn.0,
        "life": { "type": "Stopped" },
        "items": [{ "id": 1004, "kind": { "type": "Tool", "value": { "title": "Run `cargo test`", "kind": { "type": "Execute" }, "status": { "type": "Cancelled" } } } }]
    }] }),
    );
}

#[test]
fn an_agent_that_doesnt_stop_fails_the_turn_and_is_restarted() {
    let mut script = Script::with_claude();
    let (turn, _) = script.send(CHAT, "go");
    script.at(1000);
    script.stop(CHAT).0.expect("stopping");

    script.at(16_000);
    let out = script.woken();
    assert_eq!(
        with(&out, "signal"),
        [json!({ "signal": PROC, "is": "Term" })]
    );
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "Restarted" } } },
            "turns": [{ "id": turn.expect("sent").0, "life": { "type": "Failed", "value": { "reason": { "type": "DidNotStop" }, "agent_said": null, "remedy": { "type": "SendAgain" } } } }]
        }),
    );
    assert_eq!(script.state()["pending"], json!([]));

    // Its late answer changes nothing.
    let before = script.chat(CHAT);
    let out = script.answers(PROC, 1002, &json!({ "stopReason": "cancelled" }));
    assert_eq!(with(&out, "report").len(), 1);
    assert_eq!(script.chat(CHAT), before);

    // The next message continues the session on a new process.
    script.agent(PROC, AgentEvent::Exited(Exit::Signal(15)));
    script.agent(PROC, AgentEvent::OutputClosed);
    script.agent(PROC, AgentEvent::Group { empty: true });
    let (_, out) = script.send(CHAT, "again");
    assert_has(&json!(with(&out, "start")), &json!([{ "start": 1005 }]));
    let out = script.answers(1005, 1000, &claude_init());
    sent(&out, "session/resume");
}

#[test]
fn stop_refuses_when_theres_nothing_to_stop_and_waits_for_a_stop_already_asked() {
    let mut script = Script::with_claude();
    assert_eq!(script.stop(CHAT).0, Err(Refused::NothingToStop));
    script.send(CHAT, "go").0.expect("sent");
    script.stop(CHAT).0.expect("stopping");
    let (again, out) = script.stop(CHAT);
    again.expect("still stopping");
    assert!(writes(&out).is_empty(), "the agent is told once");
}

#[test]
fn stop_while_opening_abandons_the_open_and_a_late_session_is_closed() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    let (turn, _) = script.send(CHAT, "hello");
    let out = script.answers(PROC, 1000, &claude_init());
    sent(&out, "session/new");

    let (stopped, out) = script.stop(CHAT);
    stopped.expect("stopped");
    assert!(writes(&out).is_empty(), "nothing to tell the agent");
    // Abandoned, the open still has a watchdog: 60 s.
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 60_000 })]);
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "Stopped" } } },
            "turns": [{ "id": turn.expect("sent").0, "life": { "type": "Stopped" } }]
        }),
    );

    let out = script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": "late" }),
    );
    assert_eq!(
        sent(&out, "session/close")["params"],
        json!({ "sessionId": "late" })
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Lost", "value": { "reason": { "type": "Stopped" } } } }),
    );
    // Closing it has a watchdog too.
    assert_eq!(
        script.state()["pending"][0]["pending"]["deadline"],
        json!(60_000_000_000_u64)
    );
    script.answers(PROC, 1002, &json!({}));
}

#[test]
fn a_permission_request_answered_after_stop_is_refused() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    let out = script.says(PROC, &permission(9, "tool-1"));
    let ticket = with(&out, "decide")[0]["decide"]
        .as_u64()
        .expect("a ticket");
    script.policy(ticket, crate::event::PolicyAnswer::AskUser);
    let question = script.chat(CHAT)["questions"][0]["id"]
        .as_u64()
        .expect("asked");
    script.stop(CHAT).0.expect("stopping");
    let (late, out) = script.answer(
        crate::ids::QuestionId(question),
        crate::question::Answer::Permission(crate::question::OptionId("allow".to_owned())),
    );
    assert_eq!(late, Err(Refused::NoSuchQuestion));
    assert!(replies(&out, &json!(9)).is_empty());
}

#[test]
fn a_session_opening_after_caf_gave_up_on_it_is_still_closed() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    script.answers(PROC, 1000, &claude_init());
    script.stop(CHAT).0.expect("stopped");
    script.at(60_000);
    script.woken();
    assert_eq!(script.state()["pending"], json!([]));
    let out = script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": "very-late" }),
    );
    assert_eq!(
        sent(&out, "session/close")["params"],
        json!({ "sessionId": "very-late" })
    );
    script.answers(PROC, 1002, &json!({}));
}

#[test]
fn a_turn_being_stopped_when_its_adapter_dies_ends_stopped() {
    let mut script = Script::with_claude();
    let (turn, _) = script.send(CHAT, "go");
    script.stop(CHAT).0.expect("stopping");
    script.agent(PROC, AgentEvent::Exited(Exit::Code(1)));
    script.agent(PROC, AgentEvent::OutputClosed);
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "id": turn.expect("sent").0, "life": { "type": "Stopped" } }] }),
    );
}

#[test]
fn a_stuck_session_that_cant_be_closed_isnt_continued() {
    let mut script = Script::new(host());
    script.open(OpenChat::new("plain", "/a")).0.expect("opened");
    script.answers(PROC, 1000, &super::plain_init(true));
    script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": "a" }),
    );
    let (other, _) = script.open(OpenChat::new("plain", "/b"));
    let other = other.expect("opened");
    script.answers(
        PROC,
        1002,
        &json!({ "modes": super::default_modes(), "sessionId": "b" }),
    );
    script.send(CHAT, "stuck").0.expect("sent");
    script.send(other, "busy").0.expect("sent");
    script.stop(CHAT).0.expect("stopping");
    script.at(15_000);
    let out = script.woken();
    assert!(
        with(&out, "signal").is_empty(),
        "the other chat's turn goes on"
    );
    let (_, out) = script.send(CHAT, "again");
    assert!(
        sent(&out, "session/new")["params"]
            .get("sessionId")
            .is_none()
    );
    script.answers(
        PROC,
        1005,
        &json!({ "modes": super::default_modes(), "sessionId": "a2" }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{}, { "starter": { "type": "Caf" }, "items": [{ "kind": { "type": "Note", "value": { "type": "SessionStuck" } } }] }, { "life": { "type": "Running" } }] }),
    );
    script.answers(PROC, 1004, &json!({ "stopReason": "end_turn" }));
    script.answers(PROC, 1006, &json!({ "stopReason": "end_turn" }));
}
