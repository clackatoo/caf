//! One test for each of the guarantees in the crate's docs.
//!
//! Every script also checks guarantee 1 as it runs (see [`harness`]): no
//! request of the agent's is answered twice, and each is answered by the
//! end, unless its process went first.
//!
//! [`harness`]: super::harness

use serde_json::{Value, json};

use super::harness::{Script, assert_has, host, replies, sent, with};
use super::stop::permission;
use super::{CHAT, PROC, SESSION, plain_init};
use crate::acp::{Frame, RequestId};
use crate::core::{OpenChat, Refused, SessionChoice};
use crate::event::{AgentEvent, Exit, PolicyAnswer};
use crate::ids::{AgentSessionId, ChatId, QuestionId};
use crate::question::{Answer, OptionId, SlowChoice};

/// The second chat, opened after the first's session.
const OTHER: ChatId = ChatId(1003);

/// How many answers to the agent's request `id` are in `out`.
fn answered(out: &[Value], id: &Value) -> usize {
    replies(out, id).len()
}

#[test]
fn guarantee_1_every_agent_request_answered_once() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    // Answered at once.
    for (id, method) in [(1, "something/new"), (2, "_session/steering"), (3, "fs/read_text_file"), (4, "terminal/create"), (5, "elicitation/create")] {
        let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": {} }));
        assert_eq!(answered(&out, &json!(id)), 1, "{method}");
    }
    let out = script.agent(PROC, AgentEvent::Frame(Frame::TooBig { id: Some(RequestId::Number(6)), method: Some("x".to_owned()), len: 1 << 30 }));
    assert_eq!(answered(&out, &json!(6)), 1);
    let mut nobodys = permission(7, "t7");
    nobodys["params"]["sessionId"] = json!("nobody");
    assert_eq!(answered(&script.says(PROC, &nobodys), &json!(7)), 1);

    // Answered by the policy, once, however often it answers.
    let decide = |out: &[Value]| with(out, "decide")[0]["decide"].as_u64().expect("a ticket");
    let policy = decide(&script.says(PROC, &permission(8, "t8")));
    assert_eq!(answered(&script.policy(policy, PolicyAnswer::Allow), &json!(8)), 1);
    assert_eq!(answered(&script.policy(policy, PolicyAnswer::Refuse), &json!(8)), 0);

    // Answered by the user.
    let asked = decide(&script.says(PROC, &permission(9, "t9")));
    script.policy(asked, PolicyAnswer::AskUser);
    let question = script.chat(CHAT)["questions"][0]["id"].as_u64().expect("asked");
    let (_, out) = script.answer(QuestionId(question), Answer::Permission(OptionId("reject".to_owned())));
    assert_eq!(answered(&out, &json!(9)), 1);

    // Withdrawn by the agent: still answered.
    script.says(PROC, &permission(10, "t10"));
    let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "method": "$/cancel_request", "params": { "requestId": 10 } }));
    assert_eq!(answered(&out, &json!(10)), 1);

    // Ended by Stop: answered "cancelled".
    script.says(PROC, &permission(11, "t11"));
    let (_, out) = script.stop(CHAT);
    assert_eq!(answered(&out, &json!(11)), 1);
    script.answers(PROC, 1002, &json!({ "stopReason": "cancelled" }));

    // The one exception: a process that has gone can't be answered.
    script.says(PROC, &permission(12, "t12"));
    script.agent(PROC, AgentEvent::Exited(Exit::Code(1)));
    let out = script.agent(PROC, AgentEvent::OutputClosed);
    assert_eq!(answered(&out, &json!(12)), 0);
    assert_eq!(script.state()["asks"], json!({}));
}

#[test]
fn guarantee_2_every_caf_request_ends() {
    let mut script = Script::with_claude();
    script.open(OpenChat::new("claude", "/elsewhere")).0.expect("opened");
    script.answers(PROC, 1002, &json!({ "sessionId": "sess-2" }));
    script.send(CHAT, "busy").0.expect("sent");
    // A prompt has no watchdog of its own.
    assert_eq!(script.state()["wake"], Value::Null);

    // Closing a session has one. Overdue while another chat is mid-turn,
    // it ends without a restart.
    script.at(1000);
    let (_, out) = script.close(OTHER);
    assert_eq!(sent(&out, "session/close")["id"], 1004);
    script.at(61_000);
    let out = script.woken();
    assert!(with(&out, "signal").is_empty());
    assert_eq!(with(&out, "report")[0]["report"], "Warn");
    assert_eq!(script.state()["pending"].as_array().map(Vec::len), Some(1), "only the prompt");

    // Once stopped, the prompt has a watchdog too.
    script.stop(CHAT).0.expect("stopping");
    script.at(76_000);
    script.woken();
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "life": { "Failed": { "reason": "DidNotStop" } } }] }));
    assert_eq!(script.state()["pending"], json!([]));

    // So does a request nobody waits for any more: it just ends.
    let mut abandoned = Script::new(host());
    abandoned.open(OpenChat::new("claude", "/work")).0.expect("opened");
    abandoned.answers(PROC, 1000, &super::claude_init());
    abandoned.close(CHAT).0.expect("closed");
    abandoned.at(60_000);
    abandoned.woken();
    assert_eq!(abandoned.state()["pending"], json!([]));

    // And a lost connection ends every request in flight.
    script.agent(PROC, AgentEvent::OutputClosed);
    script.agent(PROC, AgentEvent::Group { empty: true });
    let (_, out) = script.send(CHAT, "again");
    let new = with(&out, "start")[0]["start"].as_u64().expect("started");
    script.answers(new, 1000, &super::claude_init());
    script.answers(new, 1001, &json!({}));
    assert_eq!(script.state()["pending"].as_array().map(Vec::len), Some(1));
    script.agent(new, AgentEvent::Exited(Exit::Code(1)));
    script.agent(new, AgentEvent::OutputClosed);
    assert_eq!(script.state()["pending"], json!([]));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{}, { "life": { "Failed": { "reason": "AgentStopped" } } }] }));
}

#[test]
fn guarantee_3_nothing_the_agent_sends_is_silently_dropped() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    let new_kind = json!({ "sessionUpdate": "hologram", "beam": [1, 2] });
    let out = script.update(PROC, SESSION, &new_kind);
    assert!(with(&out, "changed").len() == 1);
    let unreadable = json!({ "sessionUpdate": "usage_update", "used": "lots" });
    script.update(PROC, SESSION, &unreadable);
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "items": [
        { "kind": { "Unknown": { "what": "hologram", "raw": new_kind } } },
        { "kind": { "Unknown": { "what": "usage_update", "raw": unreadable } } }
    ] }] }));
    // What fits nowhere is reported.
    for message in [
        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "nobody", "update": {} } }),
        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": SESSION, "update": { "sessionUpdate": "plan", "entries": [] } } }),
        json!({ "jsonrpc": "2.0", "method": "_claude/news", "params": {} }),
        json!({ "jsonrpc": "2.0", "id": 77, "result": {} }),
    ] {
        assert_eq!(with(&script.says(PROC, &message), "report").len(), 1, "{message}");
    }
    let out = script.agent(PROC, AgentEvent::Frame(Frame::Junk { len: 3 }));
    assert_eq!(with(&out, "report").len(), 1);
}

#[test]
fn guarantee_4_every_event_names_what_it_belongs_to() {
    let mut script = Script::with_claude();
    script.open(OpenChat::new("claude", "/elsewhere")).0.expect("opened");
    script.answers(PROC, 1002, &json!({ "sessionId": "sess-2" }));
    script.send(CHAT, "first").0.expect("sent");
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Build", "status": "in_progress" }));
    script.answers(PROC, 1003, &json!({ "stopReason": "end_turn" }));
    script.send(CHAT, "second").0.expect("sent");
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "now" } }));
    // A late update to the first turn's tool lands on its own item.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed", "rawOutput": "ok" }));
    // Another session's update lands in its own chat.
    script.update(PROC, "sess-2", &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "elsewhere" } }));
    assert_has(&script.chat(CHAT), &json!({ "turns": [
        { "items": [{ "kind": { "Tool": { "title": "Build", "status": "Done", "output": "ok" } } }] },
        { "items": [{ "kind": { "Words": "now" } }] }
    ] }));
    assert_has(&script.chat(OTHER), &json!({ "turns": [{ "starter": "Agent", "life": "Running", "items": [{ "kind": { "Words": "elsewhere" } }] }] }));
    script.answers(PROC, 1004, &json!({ "stopReason": "end_turn" }));
    // Its own turn ends on its own deadline, once quiet.
    script.at(60_000);
    script.woken();
    assert_has(&script.chat(OTHER), &json!({ "turns": [{ "starter": "Agent", "life": "Finished" }] }));
}

#[test]
fn guarantee_5_stale_results_are_ignored() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    let out = script.says(PROC, &permission(7, "t1"));
    let ticket = with(&out, "decide")[0]["decide"].as_u64().expect("a ticket");
    script.stop(CHAT).0.expect("stopping");
    // The policy's answer to a request withdrawn by Stop.
    let before = script.state();
    let out = script.policy(ticket, PolicyAnswer::Allow);
    assert!(with(&out, "write").is_empty() && with(&out, "changed").is_empty());
    assert_eq!(script.state(), before);

    // After a restart, the old process's words change nothing, and its
    // group answer only counts when asked for.
    script.at(15_000);
    script.woken();
    let before = script.chat(CHAT);
    let out = script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "ghost" } }));
    assert!(with(&out, "changed").is_empty());
    let out = script.agent(PROC, AgentEvent::Group { empty: true });
    assert!(with(&out, "forget").is_empty(), "never asked yet");
    assert_eq!(script.chat(CHAT), before);
    // A process ticket the core never gave out.
    let out = script.agent(4242, AgentEvent::OutputClosed);
    assert_eq!(with(&out, "report").len(), 1);
}

#[test]
fn guarantee_6_a_question_is_answered_once_and_only_while_it_stands() {
    let mut script = Script::new(host());
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    script.at(30_000);
    script.woken();
    let slow = QuestionId(script.chat(CHAT)["questions"][0]["id"].as_u64().expect("asked"));
    // It stops standing once the adapter answers.
    script.answers(PROC, 1000, &super::claude_init());
    let (late, out) = script.answer(slow, Answer::SlowToOpen(SlowChoice::Retry));
    assert_eq!(late, Err(Refused::NoSuchQuestion));
    assert!(out.is_empty());
    script.answers(PROC, 1001, &json!({ "sessionId": SESSION }));

    script.send(CHAT, "go").0.expect("sent");
    let out = script.says(PROC, &permission(7, "t1"));
    script.policy(with(&out, "decide")[0]["decide"].as_u64().expect("a ticket"), PolicyAnswer::AskUser);
    let question = QuestionId(script.chat(CHAT)["questions"][0]["id"].as_u64().expect("asked"));
    assert_eq!(script.answer(question, Answer::SlowToOpen(SlowChoice::Stop)).0, Err(Refused::WrongAnswer));
    script.answer(question, Answer::Permission(OptionId("allow".to_owned()))).0.expect("answered");
    let (again, out) = script.answer(question, Answer::Permission(OptionId("allow".to_owned())));
    assert_eq!(again, Err(Refused::NoSuchQuestion));
    assert!(out.is_empty());
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn guarantee_7_nothing_replayed_acts() {
    let mut script = Script::new(host());
    let mut open = OpenChat::new("plain", "/work");
    open.session = SessionChoice::Continue(AgentSessionId("old".to_owned()));
    script.open(open).0.expect("opened");
    script.answers(PROC, 1000, &plain_init(true));
    script.update(PROC, "old", &json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "build it" } }));
    script.update(PROC, "old", &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "cargo build", "kind": "execute", "status": "in_progress" }));
    let mut asked = permission(5, "t1");
    asked["params"]["sessionId"] = json!("old");
    let out = script.says(PROC, &asked);
    assert_eq!(replies(&out, &json!(5))[0]["result"], json!({ "outcome": { "outcome": "cancelled" } }));
    assert!(with(&out, "decide").is_empty());
    let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "id": 6, "method": "fs/write_text_file", "params": { "sessionId": "old" } }));
    assert_eq!(replies(&out, &json!(6))[0]["error"]["code"], -32601);
    script.answers(PROC, 1001, &json!({}));
    assert_has(&script.chat(CHAT), &json!({
        "questions": [],
        "turns": [{ "imported": true, "life": "Finished", "items": [{ "kind": { "Tool": { "status": "Failed" } } }] }]
    }));
    assert_eq!(script.state()["asks"], json!({}));
}

#[test]
fn guarantee_8_restarts_are_visible_to_every_chat_on_the_adapter() {
    let mut script = Script::with_claude();
    script.open(OpenChat::new("claude", "/elsewhere")).0.expect("opened");
    script.answers(PROC, 1002, &json!({ "sessionId": "sess-2" }));
    script.send(CHAT, "stuck").0.expect("sent");
    script.stop(CHAT).0.expect("stopping");
    script.at(15_000);
    let out = script.woken();
    assert_eq!(with(&out, "changed").len(), 2, "both chats hear");
    assert_has(&script.chat(CHAT), &json!({ "state": { "Lost": { "reason": "Restarted" } } }));
    assert_has(&script.chat(OTHER), &json!({ "state": { "Lost": { "reason": "Restarted" } } }));
}

#[test]
fn guarantee_9_agent_text_is_never_passed_off_as_cafs() {
    let mut host = host();
    host.limits.failure_text = 32;
    let mut script = Script::new(host);
    script.open_claude();
    script.send(CHAT, "go").0.expect("sent");
    let sneaky = "Run\u{200b} this\u{202e} \u{1b}[8mhidden\u{1b}[0m and keep going for a very long while";
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": sneaky, "status": "pending" }));
    script.fails(PROC, 1002, &json!({ "code": -32603, "message": sneaky, "data": { "errorKind": "overloaded" } }));
    let chat = script.chat(CHAT);
    // CAF's own words are its typed reason; the agent's are apart, cleaned
    // and capped.
    assert_eq!(chat["turns"][0]["life"], json!({ "Failed": {
        "reason": "ProviderError",
        "agent_said": "Run<U+200B> this<U+202E> ^[[8mhi…",
        "remedy": "SendAgain"
    } }));
    assert_eq!(chat["turns"][0]["items"][0]["kind"]["Tool"]["title"], json!("Run<U+200B> this<U+202E> ^[[8mhidden^[[0m and keep going for a very long while"));

    let mut script = Script::new(super::harness::host());
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    script.answers(PROC, 1000, &super::claude_init());
    script.fails(PROC, 1001, &json!({ "code": -32603, "message": "no\u{2066} cwd" }));
    assert_has(&script.chat(CHAT), &json!({ "state": { "Lost": { "reason": { "Refused": { "message": "no<U+2066> cwd" } } } } }));
}
