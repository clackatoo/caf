//! Permission requests: the policy decides, the user is asked if it says
//! so, and only an option the agent offered is ever answered.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, host, replies, with};
use super::stop::permission;
use super::{CHAT, PROC, SESSION};
use crate::core::Refused;
use crate::event::PolicyAnswer;
use crate::ids::QuestionId;
use crate::question::{Answer, OptionId};

/// The ticket of the one decision asked for in `out`.
fn ticket(out: &[Value]) -> u64 {
    let decided = with(out, "decide");
    assert_eq!(decided.len(), 1, "{out:#?}");
    decided[0]["decide"].as_u64().expect("a ticket")
}

/// The answer written to the agent's request `id` in `out`.
fn reply(out: &[Value], id: u64) -> Value {
    let replies = replies(out, &json!(id));
    assert_eq!(replies.len(), 1, "{out:#?}");
    replies[0].get("result").cloned().unwrap_or_else(|| replies[0]["error"].clone())
}

/// A chat with a turn running and permission request 7 waiting on the
/// policy.
fn asked(script: &mut Script) -> u64 {
    script.send(CHAT, "go").0.expect("sent");
    ticket(&script.says(PROC, &permission(7, "tool-1")))
}

fn selected(option: &str) -> Value {
    json!({ "outcome": { "outcome": "selected", "optionId": option } })
}

#[test]
fn the_policy_allows_once_or_refuses_once() {
    let mut script = Script::with_claude();
    let decision = asked(&mut script);
    assert_eq!(reply(&script.policy(decision, PolicyAnswer::Allow), 7), selected("allow"));
    let out = script.says(PROC, &permission(8, "tool-2"));
    assert_eq!(reply(&script.policy(ticket(&out), PolicyAnswer::Refuse), 8), selected("reject"));
}

#[test]
fn the_user_is_asked_when_the_policy_says_so() {
    let mut script = Script::with_claude();
    let decision = asked(&mut script);
    script.policy(decision, PolicyAnswer::AskUser);
    assert_has(&script.chat(CHAT), &json!({ "questions": [{
        "id": 1006,
        "kind": { "Permission": {
            "tool": 1004,
            "title": "Run `cargo test`",
            "options": [
                { "id": "allow", "label": "Allow", "kind": "AllowOnce" },
                { "id": "always", "label": "Always allow", "kind": "AllowAlways" },
                { "id": "reject", "label": "Reject", "kind": "RejectOnce" }
            ]
        } }
    }] }));
    let (wrong, out) = script.answer(QuestionId(1006), Answer::Permission(OptionId("yes please".to_owned())));
    assert_eq!(wrong, Err(Refused::WrongAnswer));
    assert!(out.is_empty());
    let (right, out) = script.answer(QuestionId(1006), Answer::Permission(OptionId("always".to_owned())));
    right.expect("answered");
    assert_eq!(reply(&out, 7), selected("always"));
    assert_eq!(script.chat(CHAT)["questions"], json!([]));
    // Answered once: a second answer, or the policy again, does nothing.
    assert_eq!(script.answer(QuestionId(1006), Answer::Permission(OptionId("allow".to_owned()))).0, Err(Refused::NoSuchQuestion));
    let out = script.policy(decision, PolicyAnswer::Allow);
    assert!(replies(&out, &json!(7)).is_empty());
}

#[test]
fn a_host_that_cant_ask_the_user_gets_a_safe_refusal() {
    let mut host = host();
    host.shows.permission_questions = false;
    let mut script = Script::new(host);
    script.open_claude();
    let decision = asked(&mut script);
    assert_eq!(reply(&script.policy(decision, PolicyAnswer::AskUser), 7), selected("reject"));

    // With no reject option offered, "cancelled"; an option of a kind CAF
    // doesn't know is never chosen.
    let out = script.says(PROC, &json!({
        "jsonrpc": "2.0", "id": 8, "method": "session/request_permission",
        "params": { "sessionId": SESSION, "toolCall": { "toolCallId": "tool-2" }, "options": [
            { "optionId": "maybe", "name": "Maybe", "kind": "allow_sometimes" },
            { "optionId": "always", "name": "Always", "kind": "allow_always" }
        ] }
    }));
    assert_eq!(reply(&script.policy(ticket(&out), PolicyAnswer::Allow), 8), json!({ "outcome": { "outcome": "cancelled" } }));
}

#[test]
fn allowing_once_when_the_agent_doesnt_offer_it_asks_the_user() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    let out = script.says(PROC, &json!({
        "jsonrpc": "2.0", "id": "p-1", "method": "session/request_permission",
        "params": { "sessionId": SESSION, "toolCall": { "toolCallId": "tool-1", "title": "Plan" }, "options": [
            { "optionId": "go", "name": "Go", "kind": "something_new" },
            { "optionId": "no", "name": "No", "kind": "reject_once" }
        ] }
    }));
    let out = script.policy(ticket(&out), PolicyAnswer::Allow);
    assert!(replies(&out, &json!("p-1")).is_empty(), "never chosen by CAF");
    assert_has(&script.chat(CHAT), &json!({ "questions": [{ "kind": { "Permission": { "options": [{ "kind": "Other" }, { "kind": "RejectOnce" }] } } }] }));
    let question = script.chat(CHAT)["questions"][0]["id"].as_u64().expect("asked");
    let (_, out) = script.answer(QuestionId(question), Answer::Permission(OptionId("go".to_owned())));
    assert_eq!(replies(&out, &json!("p-1"))[0]["result"], selected("go"));
}

#[test]
fn the_agent_withdrawing_a_request_takes_its_question_away_and_still_gets_an_answer() {
    let mut script = Script::with_claude();
    let decision = asked(&mut script);
    let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "method": "$/cancel_request", "params": { "requestId": 7 } }));
    assert!(out.contains(&json!({ "withdrawn": decision })));
    assert_eq!(reply(&out, 7), json!({ "code": -32800, "message": "cancelled" }));
    // The policy's answer, coming anyway, does nothing.
    assert!(replies(&script.policy(decision, PolicyAnswer::Allow), &json!(7)).is_empty());

    let out = script.says(PROC, &permission(8, "tool-2"));
    script.policy(ticket(&out), PolicyAnswer::AskUser);
    assert_eq!(script.chat(CHAT)["questions"].as_array().map(Vec::len), Some(1));
    let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "method": "$/cancel_request", "params": { "requestId": 8 } }));
    assert_eq!(reply(&out, 8)["code"], -32800);
    assert!(with(&out, "withdrawn").is_empty(), "the policy had already answered");
    assert_eq!(script.chat(CHAT)["questions"], json!([]));
}

#[test]
fn a_permission_request_names_a_tool_call_not_seen_yet() {
    let mut script = Script::with_claude();
    let decision = asked(&mut script);
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "items": [{ "id": 1004, "kind": { "Tool": {
        "title": "Run `cargo test`", "kind": "Execute", "status": "Pending", "input": { "command": "cargo test" }
    } } }] }] }));
    // When it comes, it's the same item.
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "tool-1", "title": "cargo test", "kind": "execute", "status": "in_progress" }));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "items": [{ "id": 1004, "kind": { "Tool": { "title": "cargo test", "status": "Running" } } }] }] }));
    script.policy(decision, PolicyAnswer::Allow);
}

#[test]
fn a_permission_request_for_a_session_no_chat_holds_is_refused() {
    let mut script = Script::with_claude();
    let mut request = permission(7, "tool-1");
    request["params"]["sessionId"] = json!("someone-elses");
    let out = script.says(PROC, &request);
    assert_eq!(reply(&out, 7), json!({ "outcome": { "outcome": "cancelled" } }));
    assert!(with(&out, "decide").is_empty());
}

#[test]
fn a_turn_ending_answers_its_open_questions_cancelled() {
    let mut script = Script::with_claude();
    let decision = asked(&mut script);
    let out = script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
    assert!(out.contains(&json!({ "withdrawn": decision })));
    assert_eq!(reply(&out, 7), json!({ "outcome": { "outcome": "cancelled" } }));
}
