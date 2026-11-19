//! How turns end: by stop reason, or by failure, read through the
//! adapter's profile, with the agent's words kept apart from CAF's.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, host, sent, with};
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
    assert_eq!(ended(error("model_not_found")), said("BadRequest", Value::Null));
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

#[test]
fn every_kind_of_claude_failure_is_read_and_offered_send_again_only_where_it_helps() {
    let again = json!("SendAgain");
    for (kinds, reason, remedy) in [
        (&["authentication_failed", "oauth_org_not_allowed"][..], "SignInNeeded", Value::Null),
        (&["billing_error", "account_on_hold"][..], "UsageLimit", Value::Null),
        (&["rate_limit"][..], "UsageLimit", again.clone()),
        (&["verification_required", "cloud_credential_error"][..], "AccessDenied", Value::Null),
        (&["invalid_request", "model_not_found"][..], "BadRequest", Value::Null),
        (&["max_output_tokens"][..], "TooLong", Value::Null),
        (
            &["overloaded", "server_error", "unknown", "no_result", "incomplete_tool_call", "worker_shutdown", "transport_lost", "newer_than_caf"][..],
            "ProviderError",
            again.clone(),
        ),
    ] {
        for kind in kinds {
            let life = ended(Err(json!({ "code": -32603, "message": "Internal error: x", "data": { "errorKind": kind } })));
            assert_eq!(life, json!({ "Failed": { "reason": reason, "agent_said": "x", "remedy": remedy } }), "{kind}");
        }
    }
}

#[test]
fn a_bare_internal_error_says_its_details() {
    let life = ended(Err(json!({ "code": -32603, "message": "Internal error", "data": { "details": "spawn ENOMEM" } })));
    assert_eq!(life, json!({ "Failed": { "reason": "Other", "agent_said": "spawn ENOMEM", "remedy": "SendAgain" } }));
}

/// A prompt answered "the adapter has no such session", as the ACP SDK
/// sends Claude's adapter's error, or as ACP's own error.
fn not_found(claude_text: bool) -> Value {
    if claude_text {
        json!({ "code": -32603, "message": "Internal error", "data": { "details": "Session not found" } })
    } else {
        json!({ "code": -32002, "message": "Resource not found" })
    }
}

#[test]
fn a_prompt_to_a_session_the_adapter_dropped_resumes_it_and_sends_again() {
    for claude_text in [true, false] {
        let mut script = Script::with_claude();
        let (turn, _) = script.send(CHAT, "go");
        let out = script.fails(PROC, 1002, &not_found(claude_text));
        // The conversation is kept on disk: resumed, not replaced.
        assert_has(&sent(&out, "session/resume"), &json!({ "params": { "sessionId": SESSION } }));
        let out = script.answers(PROC, 1003, &json!({}));
        assert_has(&sent(&out, "session/prompt"), &json!({ "params": { "sessionId": SESSION, "prompt": [{ "text": "go" }] } }));
        assert_has(&script.chat(CHAT), &json!({ "state": { "Open": { "session": SESSION } }, "turns": [{ "id": turn.expect("sent").0, "life": "Running" }] }));
        script.answers(PROC, 1004, &json!({ "stopReason": "end_turn" }));
    }
}

#[test]
fn a_dropped_session_that_cant_be_resumed_is_replaced_and_the_chat_says_so() {
    let mut script = Script::with_claude();
    let (turn, _) = script.send(CHAT, "go");
    script.fails(PROC, 1002, &not_found(true));
    let out = script.fails(PROC, 1003, &json!({ "code": -32002, "message": "Resource not found" }));
    assert!(sent(&out, "session/new")["params"].get("sessionId").is_none());
    let out = script.answers(PROC, 1004, &json!({ "sessionId": "fresh" }));
    sent(&out, "session/prompt");
    assert_has(&script.chat(CHAT), &json!({
        "state": { "Open": { "session": "fresh" } },
        "turns": [{ "starter": "Caf", "items": [{ "kind": { "Note": "SessionGone" } }] }, { "id": turn.expect("sent").0, "life": "Running" }]
    }));
    script.answers(PROC, 1005, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn a_message_is_sent_again_to_a_dropped_session_at_most_once() {
    let mut script = Script::with_claude();
    let (turn, _) = script.send(CHAT, "go");
    script.fails(PROC, 1002, &not_found(true));
    script.answers(PROC, 1003, &json!({}));
    let out = script.fails(PROC, 1004, &not_found(true));
    assert!(with(&out, "write").iter().all(|write| write["message"]["method"] != "session/prompt"));
    assert_has(&script.chat(CHAT), &json!({
        "turns": [{ "id": turn.expect("sent").0, "life": { "Failed": { "reason": "AgentStopped", "agent_said": null, "remedy": "SendAgain" } } }]
    }));
    // The chat still opens its session again, for the next message.
    script.answers(PROC, 1005, &json!({}));
    assert_has(&script.chat(CHAT), &json!({ "state": { "Open": { "session": SESSION } } }));
}

#[test]
fn a_turn_being_stopped_isnt_sent_again_when_its_session_was_dropped() {
    let mut script = Script::with_claude();
    let (turn, _) = script.send(CHAT, "go");
    script.stop(CHAT).0.expect("stopping");
    let out = script.fails(PROC, 1002, &not_found(true));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "id": turn.expect("sent").0, "life": "Stopped" }] }));
    sent(&out, "session/resume");
    let out = script.answers(PROC, 1003, &json!({}));
    assert!(with(&out, "write").is_empty(), "nothing sent: {out:#?}");
}

#[test]
fn a_cautious_profile_matches_no_agents_text() {
    let mut script = Script::new(host());
    script.open(OpenChat::new("plain", "/work")).0.expect("opened");
    script.answers(PROC, 1000, &super::plain_init(false));
    script.answers(PROC, 1001, &json!({ "sessionId": SESSION }));
    script.send(CHAT, "go").0.expect("sent");
    script.fails(PROC, 1002, &json!({ "code": -32603, "message": "Session not found" }));
    assert_has(&script.chat(CHAT), &json!({ "state": { "Open": {} }, "turns": [{ "life": { "Failed": { "reason": "Other" } } }] }));
}

#[test]
fn failure_text_streamed_at_the_end_of_the_words_is_shown_once() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Working on it… API Error: Rate limited\n" } }));
    script.fails(PROC, 1002, &json!({ "code": -32603, "message": "Internal error: API Error: Rate limited", "data": { "errorKind": "rate_limit" } }));
    assert_eq!(script.chat(CHAT)["turns"][0]["life"]["Failed"]["agent_said"], Value::Null);
}

#[test]
fn a_prerelease_of_the_checked_version_isnt_it() {
    let mut script = Script::new(host());
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    let mut init = claude_init();
    init["agentInfo"]["version"] = json!("0.98.0-rc.1");
    script.answers(PROC, 1000, &init);
    assert_has(&script.state(), &json!({ "conns": { "1001": { "profile": { "checked": "Mismatch" } } } }));
}

#[test]
fn a_tool_call_with_no_title_is_still_a_tool() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "kind": "read", "status": "in_progress" }));
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "title": "Read a.rs", "status": "completed" }));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "items": [{ "kind": { "Tool": { "title": "Read a.rs", "kind": "Read", "status": "Done" } } }] }] }));
}
