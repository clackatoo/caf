//! A call CAF serves that's running as its turn ends: stopped, and ended
//! as the tools driver says, as only it knows whether the work stopped or
//! had finished; or as stopped all the same, if it never says, or the
//! chat is let go first.

use serde_json::{Value, json};

use super::harness::with;
use super::served::{PROMPT, bash_running, kind, the_tool, told};
use super::{CHAT, PROC};
use crate::event::{Exit, ToolEvent};
use crate::tool::{Ending, Output, Piece};

/// The command's output, ended as `ending`, as the driver reports it.
fn ended_output(ending: Ending) -> Piece {
    Piece::Output(Output {
        lines: Vec::new(),
        dropped: 0,
        ending: Some(ending),
    })
}

/// The call's end, as the driver reports it.
fn finished(done: bool, pieces: Vec<Piece>) -> ToolEvent {
    ToolEvent::Finished {
        done,
        pieces,
        folder: None,
        notice: None,
        item: None,
    }
}

/// The warnings in `out`.
fn warnings(out: &[Value]) -> Vec<String> {
    (with(out, "report").into_iter())
        .filter(|report| report["report"] == "Warn")
        .map(|report| report["message"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// A Bash call running `sleep 100` whose turn the user stopped: the
/// driver was told to stop it, and the call is stopping.
fn stopping() -> super::harness::Script {
    let mut script = bash_running("sleep 100");
    script.stop(CHAT).0.expect("stopping");
    let out = script.answers(PROC, PROMPT, &json!({ "stopReason": "cancelled" }));
    assert_eq!(
        told(&out),
        [json!({ "type": "Cancel", "value": { "call": 1 } })]
    );
    script
}

#[test]
fn a_running_call_whose_turn_is_stopped_ends_cancelled_once_the_driver_says() {
    let mut script = stopping();
    // The turn has ended; the call goes on until the driver says.
    assert_eq!(script.chat(CHAT)["turns"][0]["life"], kind("Stopped"));
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Running"));
    assert_eq!(tool["stopped_by"], kind("User"));
    assert_eq!(tool["end"]["type"], "Open");
    let out = script.tool(1, ToolEvent::Stopped);
    assert!(told(&out).is_empty());
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Cancelled"));
    assert_eq!(tool["stopped_by"], kind("User"));
    assert_ne!(tool["end"]["type"], "Open");
    // The command's output ended, not saying how.
    assert_eq!(
        tool["result"][0]["value"],
        json!({ "lines": [], "dropped": 0, "ending": kind("Unknown") })
    );
    assert!(script.core.calls.is_empty());
    // A finish or a stop that comes after is ignored.
    script.tool(1, finished(true, Vec::new()));
    script.tool(1, ToolEvent::Stopped);
    assert_eq!(the_tool(&script)["status"], kind("Cancelled"));
}

#[test]
fn a_call_that_had_finished_as_its_turn_was_stopped_ends_as_it_went() {
    let mut script = stopping();
    let out = script.tool(
        1,
        finished(true, vec![ended_output(Ending::Exited(Exit::Code(0)))]),
    );
    assert!(told(&out).is_empty());
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Done"));
    assert_eq!(tool["stopped_by"], Value::Null, "it wasn't stopped");
    assert_eq!(
        tool["result"][0]["value"]["ending"],
        json!({ "type": "Exited", "value": { "type": "Code", "value": 0 } })
    );
    assert_ne!(tool["end"]["type"], "Open");
    assert!(script.core.calls.is_empty());

    // And one that had failed, failed.
    let mut script = stopping();
    script.tool(1, finished(false, Vec::new()));
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
    assert_eq!(the_tool(&script)["stopped_by"], Value::Null);
}

#[test]
fn a_running_call_in_a_turn_that_ends_under_it_fails_once_stopped() {
    let mut script = bash_running("make");
    let out = script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    assert_eq!(
        told(&out),
        [json!({ "type": "Cancel", "value": { "call": 1 } })]
    );
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Running"));
    assert_eq!(tool["stopped_by"], Value::Null, "no one stopped it");
    script.tool(1, ToolEvent::Stopped);
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Failed"));
    assert_eq!(tool["stopped_by"], Value::Null);
    assert_ne!(tool["end"]["type"], "Open");
}

#[test]
fn a_call_being_stopped_still_prints_and_its_agent_giving_up_ends_it_as_stopped() {
    let mut script = stopping();
    script.tool(
        1,
        ToolEvent::Output {
            from: 0,
            lines: vec!["Terminated".to_owned()],
        },
    );
    assert_eq!(
        the_tool(&script)["result"][0]["value"]["lines"],
        json!(["Terminated"])
    );
    // Never a job: its Cancel stops it.
    script.tool(
        1,
        ToolEvent::Detached {
            job: crate::ids::JobId(1),
        },
    );
    assert_eq!(the_tool(&script)["job"], Value::Null);
    let out = script.tool(1, ToolEvent::Gone);
    assert!(told(&out).is_empty());
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Cancelled"));
    assert_eq!(tool["stopped_by"], kind("User"));
    assert_eq!(tool["result"][0]["value"]["ending"], kind("Unknown"));
    assert!(script.core.calls.is_empty());
}

#[test]
fn a_driver_that_never_says_has_ten_seconds_then_the_call_ends_as_stopped() {
    let mut script = bash_running("sleep 100");
    script.at(1_000);
    script.stop(CHAT).0.expect("stopping");
    let out = script.answers(PROC, PROMPT, &json!({ "stopReason": "cancelled" }));
    assert!(
        with(&out, "wake_at").contains(&json!({ "wake_at": 11_000 })),
        "{out:#?}"
    );
    script.at(10_999);
    let out = script.woken();
    assert!(warnings(&out).is_empty());
    assert_eq!(the_tool(&script)["status"], kind("Running"));
    script.at(11_000);
    let out = script.woken();
    assert_eq!(
        warnings(&out),
        [
            "the tools driver never said whether call 1 stopped or had finished, in 10s: it ends \
             as stopped"
        ]
    );
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Cancelled"));
    assert_eq!(tool["stopped_by"], kind("User"));
    assert_ne!(tool["end"]["type"], "Open");
    assert!(script.core.calls.is_empty());
    // What it says late is ignored.
    script.tool(1, finished(true, Vec::new()));
    assert_eq!(the_tool(&script)["status"], kind("Cancelled"));
}

#[test]
fn a_chat_let_go_while_a_call_is_stopping_ends_it_at_once() {
    let mut script = stopping();
    let (closed, out) = script.close(CHAT);
    closed.expect("closed");
    // It was cancelled already: the driver isn't told again.
    assert!(
        !told(&out).iter().any(|each| each["type"] == "Cancel"),
        "{out:#?}"
    );
    let tool = the_tool(&script);
    assert_eq!(tool["status"], kind("Cancelled"));
    assert_eq!(tool["stopped_by"], kind("User"));
    assert_ne!(tool["end"]["type"], "Open");
    assert!(script.core.calls.is_empty());
    script.tool(1, ToolEvent::Stopped);
    assert_eq!(the_tool(&script)["status"], kind("Cancelled"));

    // As CAF shuts down, too: as its turn's end said.
    let mut script = bash_running("make");
    script.answers(PROC, PROMPT, &json!({ "stopReason": "end_turn" }));
    script.shutdown();
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
    assert!(script.core.calls.is_empty());
    script.tool(1, finished(true, Vec::new()));
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
}

#[test]
fn a_second_stop_leaves_a_call_being_stopped_alone() {
    let mut script = stopping();
    script.send(CHAT, "again").0.expect("sent");
    let out = script.stop(CHAT).1;
    assert!(told(&out).is_empty(), "{out:#?}");
    assert_eq!(the_tool(&script)["status"], kind("Running"));
    script.tool(1, ToolEvent::Stopped);
    assert_eq!(the_tool(&script)["status"], kind("Cancelled"));
}

#[test]
fn a_chat_saved_as_a_call_was_stopping_restores_it_cancelled() {
    let script = stopping();
    let chat: crate::chat::Chat = serde_json::from_value(script.chat(CHAT)).expect("a chat");
    let now = script.now().wall;
    let changes = crate::core::saving::tidy(&chat, now, now);
    let changes = serde_json::to_value(&changes).expect("JSON");
    let changes = changes.as_array().expect("a list");
    assert_eq!(changes[0]["type"], "ToolChanged");
    let tool = &changes[0]["value"]["tool"];
    assert_eq!(tool["status"], kind("Cancelled"));
    assert_eq!(tool["stopped_by"], kind("User"));
    assert_eq!(changes[1]["type"], "ItemEnded");
    // Its call ends here, too, so the script's checks hold.
    let mut script = script;
    script.tool(1, ToolEvent::Stopped);
}
