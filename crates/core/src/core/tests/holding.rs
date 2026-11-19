//! One session is never held twice, and no chat waits without a deadline
//! or a question.

use serde_json::json;

use super::harness::{Script, assert_has, host, sent, with, writes};
use super::{CHAT, PROC, claude_init, plain_init};
use crate::core::{OpenChat, SessionChoice};
use crate::event::{AgentEvent, Exit};
use crate::host::Grouping;
use crate::ids::{AgentSessionId, QuestionId};
use crate::question::{Answer, SlowChoice};

/// Opening a chat with `agent` that continues session `old`.
fn continuing(agent: &str, old: &str) -> OpenChat {
    let mut open = OpenChat::new(agent, "/work");
    open.session = SessionChoice::Continue(AgentSessionId(old.to_owned()));
    open
}

/// A host with one adapter process per chat.
fn per_chat() -> crate::host::Host {
    let mut host = host();
    host.grouping = Grouping::PerChat;
    host
}

#[test]
fn a_session_held_on_another_connection_is_in_use() {
    let mut script = Script::new(per_chat());
    script.open(continuing("claude", "X")).0.expect("opened");
    let out = script.answers(PROC, 1000, &claude_init());
    sent(&out, "session/resume");
    let (other, out) = script.open(continuing("claude", "X"));
    let other = other.expect("opened");
    let proc = with(&out, "start")[0]["start"].as_u64().expect("its own process");
    let out = script.answers(proc, 1000, &claude_init());
    assert!(writes(&out).is_empty(), "never resumed twice: {out:#?}");
    assert_has(&script.chat(other), &json!({ "state": { "Lost": { "reason": "InUse" } } }));
}

#[test]
fn a_session_waits_for_a_process_that_still_has_it_to_go() {
    let mut script = Script::new(per_chat());
    script.open(continuing("claude", "X")).0.expect("opened");
    script.answers(PROC, 1000, &claude_init());
    script.answers(PROC, 1001, &json!({}));
    // Closed, its process is stopping, but still has the session.
    script.close(CHAT).0.expect("closed");
    let (other, out) = script.open(continuing("claude", "X"));
    let other = other.expect("opened");
    let proc = with(&out, "start")[0]["start"].as_u64().expect("its own process");
    let out = script.answers(proc, 1000, &claude_init());
    assert!(writes(&out).is_empty(), "not while the old process may still write it");
    assert_has(&script.chat(other), &json!({ "state": "Opening" }));
    script.agent(PROC, AgentEvent::Exited(Exit::Code(0)));
    script.agent(PROC, AgentEvent::OutputClosed);
    let out = script.agent(PROC, AgentEvent::Group { empty: true });
    assert_has(&sent(&out, "session/resume"), &json!({ "params": { "sessionId": "X" } }));
}

#[test]
fn a_session_isnt_asked_for_again_while_an_abandoned_open_of_it_is_pending() {
    let mut script = Script::new(host());
    script.open(continuing("plain", "old")).0.expect("opened");
    script.answers(PROC, 1000, &plain_init(true));
    script.stop(CHAT).0.expect("stopped");
    let (_, out) = script.send(CHAT, "again");
    assert!(writes(&out).is_empty(), "not loaded twice at once");
    // The first load's replay isn't the chat's.
    script.update(PROC, "old", &json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "first load" } }));
    // Once it ends, the chat asks.
    let out = script.answers(PROC, 1001, &json!({}));
    assert_has(&sent(&out, "session/load"), &json!({ "id": 1002, "params": { "sessionId": "old" } }));
    script.update(PROC, "old", &json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "second load" } }));
    script.answers(PROC, 1002, &json!({}));
    assert_has(&script.chat(CHAT), &json!({ "turns": [
        { "imported": true, "starter": { "User": { "parts": [{ "Text": "second load" }] } } },
        { "starter": { "User": { "parts": [{ "Text": "again" }] } }, "life": "Running" }
    ] }));
    script.answers(PROC, 1003, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn a_chat_waiting_on_a_process_already_slow_is_asked_at_once() {
    let mut script = Script::new(host());
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    script.at(30_000);
    script.woken();
    let (other, _) = script.open(OpenChat::new("claude", "/b"));
    let other = other.expect("opened");
    assert_has(&script.chat(other), &json!({ "questions": [{ "kind": { "SlowToOpen": { "adapter_answered": false } } }] }));

    // Keep waiting covers every chat waiting on it.
    let first = QuestionId(script.chat(CHAT)["questions"][0]["id"].as_u64().expect("asked"));
    let (_, out) = script.answer(first, Answer::SlowToOpen(SlowChoice::KeepWaiting));
    assert_eq!(script.chat(other)["questions"], json!([]));
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 60_000 })]);
    script.at(60_000);
    script.woken();
    assert_eq!(script.chat(CHAT)["questions"].as_array().map(Vec::len), Some(1));
    assert_eq!(script.chat(other)["questions"].as_array().map(Vec::len), Some(1));
    script.shutdown();
}

#[test]
fn a_slow_adapter_nobody_waits_for_is_stopped_and_the_next_message_starts_another() {
    let mut script = Script::new(host());
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    script.at(30_000);
    script.woken();
    let question = QuestionId(script.chat(CHAT)["questions"][0]["id"].as_u64().expect("asked"));
    let (_, out) = script.answer(question, Answer::SlowToOpen(SlowChoice::Stop));
    assert_eq!(with(&out, "signal"), [json!({ "signal": PROC, "is": "Term" })]);
    let (_, out) = script.send(CHAT, "again");
    // It waits on the old process, which has its own deadlines.
    assert!(with(&out, "start").is_empty());
    assert!(script.state()["wake"].is_number());
    script.agent(PROC, AgentEvent::Exited(Exit::Signal(15)));
    script.agent(PROC, AgentEvent::OutputClosed);
    let out = script.agent(PROC, AgentEvent::Group { empty: true });
    assert_eq!(with(&out, "start").len(), 1);
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 60_000 })]);
}

#[test]
fn a_session_being_closed_isnt_opened_again_until_it_is() {
    let mut script = Script::new(host());
    script.open(continuing("claude", "X")).0.expect("opened");
    script.answers(PROC, 1000, &claude_init());
    script.stop(CHAT).0.expect("stopped");
    script.send(CHAT, "again").0.expect("waits");
    // The abandoned resume opens late: closed, and not resumed meanwhile.
    let out = script.answers(PROC, 1001, &json!({}));
    sent(&out, "session/close");
    assert_eq!(writes(&out).len(), 1, "{out:#?}");
    let out = script.answers(PROC, 1002, &json!(null));
    assert_has(&sent(&out, "session/resume"), &json!({ "params": { "sessionId": "X" } }));
    script.answers(PROC, 1003, &json!({}));
    script.answers(PROC, 1004, &json!({ "stopReason": "end_turn" }));
}
