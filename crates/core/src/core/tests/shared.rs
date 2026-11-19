//! Chats sharing one adapter: Retry's rule, and restarts every chat
//! hears of.

use serde_json::json;

use super::harness::{Script, assert_has, host, sent, with, writes};
use super::{CHAT, PROC, SESSION, claude_init};
use crate::core::OpenChat;
use crate::event::{AgentEvent, Exit};
use crate::host::Grouping;
use crate::ids::{ChatId, QuestionId};
use crate::question::{Answer, SlowChoice};

/// The second chat, opened after the first's session.
const OTHER: ChatId = ChatId(1003);

#[test]
fn per_agent_grouping_shares_one_adapter_and_per_chat_grouping_doesnt() {
    let mut script = Script::with_claude();
    let (other, out) = script.open(OpenChat::new("claude", "/elsewhere"));
    assert_eq!(other, Ok(OTHER));
    assert!(with(&out, "start").is_empty());
    assert_has(&sent(&out, "session/new"), &json!({ "params": { "cwd": "/elsewhere" } }));
    assert_eq!(script.core.connections().len(), 1);

    let mut host = host();
    host.grouping = Grouping::PerChat;
    let mut script = Script::new(host);
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    let (_, out) = script.open(OpenChat::new("claude", "/work"));
    assert_eq!(with(&out, "start").len(), 1);
    assert_eq!(script.core.connections().len(), 2);
}

#[test]
fn retry_while_another_chat_is_mid_turn_only_asks_again() {
    let mut script = Script::with_claude();
    script.open(OpenChat::new("claude", "/elsewhere")).0.expect("opened");
    script.send(CHAT, "busy").0.expect("sent");
    script.at(30_000);
    script.woken();
    let question = script.chat(OTHER)["questions"][0]["id"].as_u64().expect("asked");
    assert_has(&script.chat(OTHER), &json!({ "questions": [{ "kind": { "SlowToOpen": { "adapter_answered": true } } }] }));

    let (retried, out) = script.answer(QuestionId(question), Answer::SlowToOpen(SlowChoice::Retry));
    retried.expect("retried");
    assert!(with(&out, "signal").is_empty(), "the busy chat's turn isn't cut off");
    assert_eq!(sent(&out, "session/new")["id"], 1004);
    // The first ask, abandoned, opens late: closed.
    let out = script.answers(PROC, 1002, &json!({ "sessionId": "abandoned" }));
    assert_eq!(sent(&out, "session/close")["params"], json!({ "sessionId": "abandoned" }));
    script.answers(PROC, 1004, &json!({ "sessionId": "sess-2" }));
    assert_has(&script.chat(OTHER), &json!({ "state": { "Open": { "session": "sess-2" } } }));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "life": "Running" }] }));
}

#[test]
fn retry_restarts_a_shared_adapter_every_chat_hears_and_waiting_messages_survive() {
    let mut script = Script::with_claude();
    script.open(OpenChat::new("claude", "/elsewhere")).0.expect("opened");
    let (waiting, _) = script.send(OTHER, "hi");
    let waiting = waiting.expect("queued");
    script.at(30_000);
    script.woken();
    let question = script.chat(OTHER)["questions"][0]["id"].as_u64().expect("asked");

    let (retried, out) = script.answer(QuestionId(question), Answer::SlowToOpen(SlowChoice::Retry));
    retried.expect("retried");
    assert_eq!(with(&out, "signal"), [json!({ "signal": PROC, "is": "Term" })]);
    // Both chats hear.
    assert_has(&script.chat(CHAT), &json!({ "state": { "Lost": { "reason": "Restarted" } } }));
    assert_has(&script.chat(OTHER), &json!({ "state": "Opening", "turns": [{ "id": waiting.0, "life": "Waiting" }] }));
    assert_eq!(with(&out, "changed").len(), 2);

    script.agent(PROC, AgentEvent::Exited(Exit::Signal(15)));
    script.agent(PROC, AgentEvent::OutputClosed);
    let out = script.agent(PROC, AgentEvent::Group { empty: true });
    assert_has(&json!(with(&out, "start")), &json!([{ "start": 1006 }]));
    let out = script.answers(1006, 1000, &claude_init());
    // Only the chat that was opening asks: the other opens again with its
    // next message.
    assert_eq!(writes(&out).len(), 1);
    sent(&out, "session/new");
    let out = script.answers(1006, 1001, &json!({ "sessionId": "sess-2" }));
    assert_has(&sent(&out, "session/prompt"), &json!({ "params": { "sessionId": "sess-2", "prompt": [{ "text": "hi" }] } }));

    let (_, out) = script.send(CHAT, "back again");
    assert_has(&sent(&out, "session/resume"), &json!({ "params": { "sessionId": SESSION } }));
}

#[test]
fn an_adapter_dying_tells_every_chat_on_it() {
    let mut script = Script::with_claude();
    script.open(OpenChat::new("claude", "/elsewhere")).0.expect("opened");
    script.answers(PROC, 1002, &json!({ "sessionId": "sess-2" }));
    script.send(OTHER, "work").0.expect("sent");
    script.agent(PROC, AgentEvent::Exited(Exit::Code(1)));
    let out = script.agent(PROC, AgentEvent::OutputClosed);
    assert_eq!(with(&out, "changed").len(), 2);
    for chat in [CHAT, OTHER] {
        assert_has(&script.chat(chat), &json!({ "state": { "Lost": { "reason": { "AgentStopped": { "stderr": [] } } } } }));
    }
    assert_has(&script.chat(OTHER), &json!({ "turns": [{ "life": { "Failed": { "reason": "AgentStopped" } } }] }));
}

#[test]
fn one_session_is_never_opened_twice() {
    let mut script = Script::with_claude();
    let mut open = OpenChat::new("claude", "/work");
    open.session = crate::core::SessionChoice::Continue(crate::ids::AgentSessionId(SESSION.to_owned()));
    let (other, out) = script.open(open);
    assert!(writes(&out).is_empty());
    assert_has(&script.chat(other.expect("opened")), &json!({ "state": { "Lost": { "reason": "InUse" } } }));
}
