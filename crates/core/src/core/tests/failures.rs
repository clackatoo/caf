//! How turns end: by stop reason, or by failure, read through the
//! adapter's profile, with the agent's words kept apart from CAF's.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, host};
use super::{CHAT, PROC, SESSION, claude_init};
use crate::core::OpenChat;

/// The life of the only turn after its prompt is answered with `result`,
/// or failed with `error`.
fn ended(answer: Result<Value, Value>) -> Value {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    match answer {
        Ok(result) => script.answers(PROC, 1002, &result),
        Err(error) => script.fails(PROC, 1002, &error),
    };
    script.chat(CHAT)["turns"][0]["life"].clone()
}

fn failed(reason: &str, remedy: Value) -> Value {
    json!({ "Failed": { "reason": reason, "agent_said": null, "remedy": remedy } })
}

#[test]
fn a_turn_ends_as_its_stop_reason_says() {
    assert_eq!(ended(Ok(json!({ "stopReason": "end_turn" }))), json!("Finished"));
    assert_eq!(ended(Ok(json!({ "stopReason": "cancelled" }))), json!("Stopped"));
    assert_eq!(ended(Ok(json!({ "stopReason": "max_tokens" }))), failed("TooLong", Value::Null));
    assert_eq!(ended(Ok(json!({ "stopReason": "max_turn_requests" }))), failed("TooManySteps", json!("SendAgain")));
    assert_eq!(ended(Ok(json!({ "stopReason": "refusal" }))), failed("Refused", Value::Null));
    // Newer than CAF: read leniently.
    assert_eq!(ended(Ok(json!({ "stopReason": "paused_for_tea" }))), json!("Finished"));
    assert_eq!(ended(Ok(json!({}))), failed("BadAnswer", json!("SendAgain")));
}

#[test]
fn claudes_failures_are_read_by_their_kind() {
    let error = |kind: &str| Err(json!({ "code": -32603, "message": "Internal error: It broke", "data": { "errorKind": kind } }));
    let said = |reason: &str, remedy: Value| json!({ "Failed": { "reason": reason, "agent_said": "It broke", "remedy": remedy } });
    assert_eq!(ended(error("rate_limit")), said("UsageLimit", json!("SendAgain")));
    assert_eq!(ended(error("overloaded")), said("ProviderError", json!("SendAgain")));
    assert_eq!(ended(error("authentication_failed")), said("SignInNeeded", Value::Null));
    assert_eq!(ended(error("max_output_tokens")), said("TooLong", Value::Null));
    assert_eq!(ended(error("model_not_found")), said("Other", json!("SendAgain")));
    assert_eq!(ended(Err(json!({ "code": -32000, "message": "Please run /login" }))), said_sign_in("Please run /login"));
}

fn said_sign_in(words: &str) -> Value {
    json!({ "Failed": { "reason": "SignInNeeded", "agent_said": words, "remedy": null } })
}

#[test]
fn a_failure_the_agent_already_said_as_words_is_shown_once() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "API Error: Rate limited" } }));
    script.fails(PROC, 1002, &json!({ "code": -32603, "message": "Internal error: API Error: Rate limited", "data": { "errorKind": "rate_limit" } }));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{
        "life": { "Failed": { "reason": "UsageLimit", "agent_said": null, "remedy": "SendAgain" } },
        "items": [{ "kind": { "Words": "API Error: Rate limited" } }]
    }] }));
}

#[test]
fn the_agents_failure_text_is_cleaned_and_capped() {
    let mut host = host();
    host.limits.failure_text = 24;
    let mut script = Script::new(host);
    script.open_claude();
    script.send(CHAT, "go").0.expect("sent");
    let words = "  Run \u{202e}sudo rm\u{1b}[0m -rf / now, then everything else  ";
    script.fails(PROC, 1002, &json!({ "code": -32603, "message": words, "data": { "errorKind": "server_error" } }));
    assert_eq!(script.chat(CHAT)["turns"][0]["life"]["Failed"]["agent_said"], json!("Run <U+202E>sudo rm^[[0m…"));
}

#[test]
fn an_adapter_that_isnt_the_one_its_profile_is_for_is_read_cautiously() {
    let mut script = Script::new(host());
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    let mut init = claude_init();
    init["agentInfo"]["version"] = json!("0.97.0");
    script.answers(PROC, 1000, &init);
    script.answers(PROC, 1001, &json!({ "sessionId": SESSION }));
    assert_has(&script.state(), &json!({ "conns": { "1001": { "profile": { "profile": "Cautious", "checked": "Mismatch" } } } }));
    script.send(CHAT, "go").0.expect("sent");
    script.fails(PROC, 1002, &json!({ "code": -32603, "message": "x", "data": { "errorKind": "rate_limit" } }));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "life": { "Failed": { "reason": "Other" } } }] }));
}

#[test]
fn a_newer_claude_keeps_its_profile() {
    let mut script = Script::new(host());
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    let mut init = claude_init();
    init["agentInfo"]["version"] = json!("0.99.0");
    script.answers(PROC, 1000, &init);
    assert_has(&script.state(), &json!({ "conns": { "1001": { "profile": { "profile": { "Builtin": "ClaudeAgentAcp" }, "checked": "Newer" } } } }));
}

#[test]
fn sign_in_said_in_the_error_output_explains_the_adapter_stopping() {
    let mut script = Script::with_claude();
    script.agent(PROC, crate::event::AgentEvent::Stderr(b"Invalid API key \xc2\xb7 Please run /login\n".to_vec()));
    script.agent(PROC, crate::event::AgentEvent::Exited(crate::event::Exit::Code(1)));
    script.agent(PROC, crate::event::AgentEvent::OutputClosed);
    assert_has(&script.chat(CHAT), &json!({ "state": { "Lost": { "reason": "SignInNeeded" } } }));
}
