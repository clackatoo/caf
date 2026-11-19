//! What the agent sends, sorted by shape: requests answered at once,
//! notifications, answers CAF never asked for, and lines too big or not
//! JSON-RPC at all.

use serde_json::json;

use super::harness::{Script, replies, with};
use super::{CHAT, PROC};
use crate::acp::{Frame, RequestId, Shape};
use crate::event::{AgentEvent, Exit};

#[test]
fn requests_for_what_caf_doesnt_offer_are_refused_at_once() {
    let mut script = Script::with_claude();
    for (id, method) in [(1, "fs/read_text_file"), (2, "fs/write_text_file"), (3, "terminal/create"), (4, "_session/steering"), (5, "something/new")] {
        let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": {} }));
        assert_eq!(replies(&out, &json!(id))[0]["error"], json!({ "code": -32601, "message": format!("{method} isn't supported") }));
    }
    // Forms are declined until hosts can show them.
    let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "id": "f", "method": "elicitation/create", "params": { "mode": "form" } }));
    assert_eq!(replies(&out, &json!("f"))[0]["result"], json!({ "action": "decline" }));
}

#[test]
fn an_answer_to_a_request_caf_never_sent_is_reported() {
    let mut script = Script::with_claude();
    let before = script.state();
    for id in [json!(42), json!("x")] {
        let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "id": id, "result": {} }));
        assert_eq!(with(&out, "report").len(), 1);
        assert_eq!(with(&out, "report")[0]["report"], "Warn");
    }
    assert_eq!(script.state(), before);
}

#[test]
fn a_line_too_big_to_read_still_ends_what_it_was() {
    let mut script = Script::with_claude();
    let out = script.agent(PROC, AgentEvent::Frame(Frame::TooBig { id: Some(RequestId::Number(9)), method: Some("fs/read_text_file".to_owned()), len: 70 << 20, shape: Shape::Call }));
    assert_eq!(replies(&out, &json!(9))[0]["error"], json!({ "code": -32600, "message": "too big to read" }));
    // A request whose method lies past the head is still a request: its
    // head showed params, so it's refused rather than taken for an answer.
    let out = script.agent(PROC, AgentEvent::Frame(Frame::TooBig { id: Some(RequestId::Number(10)), method: None, len: 70 << 20, shape: Shape::Call }));
    assert_eq!(replies(&out, &json!(10))[0]["error"], json!({ "code": -32600, "message": "too big to read" }));

    script.send(CHAT, "go").0.expect("sent");
    script.agent(PROC, AgentEvent::Frame(Frame::TooBig { id: Some(RequestId::Number(1002)), method: None, len: 70 << 20, shape: Shape::Answer }));
    assert_eq!(script.chat(CHAT)["turns"][0]["life"], json!({ "Failed": { "reason": "BadAnswer", "agent_said": null, "remedy": "SendAgain" } }));

    let out = script.agent(PROC, AgentEvent::Frame(Frame::TooBig { id: None, method: None, len: 70 << 20, shape: Shape::Unknown }));
    assert_eq!(with(&out, "report")[0]["report"], "Warn");
    let out = script.agent(PROC, AgentEvent::Frame(Frame::Junk { len: 12 }));
    assert_eq!(with(&out, "report")[0]["report"], "Debug");
}

#[test]
fn claudes_sign_in_status_is_never_kept_or_reported() {
    let mut script = Script::with_claude();
    let before = script.state();
    let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "method": "_auth/status_update", "params": { "email": "me@example.com" } }));
    assert!(out.is_empty(), "{out:#?}");
    assert_eq!(script.state(), before);
    let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "method": "_claude/something", "params": { "email": "me@example.com" } }));
    assert!(!format!("{out:?}").contains("example.com"));
}

#[test]
fn a_request_from_a_process_that_has_gone_is_dropped_and_reported() {
    let mut script = Script::with_claude();
    script.agent(PROC, AgentEvent::Exited(Exit::Code(1)));
    let out = script.says(PROC, &json!({ "jsonrpc": "2.0", "id": 5, "method": "fs/read_text_file", "params": {} }));
    assert!(replies(&out, &json!(5)).is_empty());
    assert_eq!(with(&out, "report").len(), 1);
}

#[test]
fn error_output_reads_back_from_bytes_or_text() {
    use crate::event::Event;
    let bytes: Event = serde_json::from_str(r#"{"Agent":{"process":7,"event":{"Stderr":[104,105,10]}}}"#).expect("bytes");
    let text: Event = serde_json::from_str(r#"{"Agent":{"process":7,"event":{"Stderr":"hi\n"}}}"#).expect("text");
    for event in [bytes, text] {
        let Event::Agent { event: AgentEvent::Stderr(read), .. } = event else { panic!("{event:?}") };
        assert_eq!(read, b"hi\n");
    }
}
