//! Adapter processes: going, and their last words; being let go only once
//! their group is empty; failing to start or initialize; being slow to
//! start; being stopped.

use serde_json::json;

use super::harness::{FIRST, Script, assert_has, host, sent, with, writes};
use super::{CHAT, PROC, SESSION, claude_init};
use crate::core::OpenChat;
use crate::event::{AgentEvent, Exit};
use crate::question::{Answer, SlowChoice};

#[test]
fn an_adapter_that_dies_mid_turn_is_heard_out_then_replaced_once_its_group_is_empty() {
    let mut script = Script::with_claude();
    let (first, _) = script.send(CHAT, "go");
    let (second, _) = script.send(CHAT, "and then this");
    let (first, second) = (first.expect("sent"), second.expect("queued"));
    script.agent(PROC, AgentEvent::Stderr(b"boom: it broke\n".to_vec()));

    script.at(1000);
    let out = script.agent(PROC, AgentEvent::Exited(Exit::Code(1)));
    // Its group is ended firmly at once; its last words are waited for
    // 2 s, and SIGKILL follows SIGTERM after 2 s too.
    assert_eq!(
        out,
        [
            json!({ "signal": PROC, "is": "Term" }),
            json!({ "wake_at": 3000 })
        ]
    );

    // Its last words: the turn ends as it really did.
    script.at(1100);
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "id": first.0, "life": { "type": "Finished" } }, { "id": second.0, "life": { "type": "Waiting" } }] }),
    );

    script.at(1200);
    let out = script.agent(PROC, AgentEvent::OutputClosed);
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Opening" } }),
    );
    assert!(
        with(&out, "start").is_empty(),
        "nothing new starts while the old group may have something in it"
    );
    assert_eq!(with(&out, "check_group"), [json!({ "check_group": PROC })]);
    assert_eq!(
        script.state()["pending"],
        json!([]),
        "every request to it has ended"
    );

    script.at(1250);
    let out = script.agent(PROC, AgentEvent::Group { empty: false });
    assert_eq!(out, [json!({ "wake_at": 1300 })]);
    script.at(1300);
    let out = script.woken();
    assert_eq!(with(&out, "check_group"), [json!({ "check_group": PROC })]);

    script.at(1320);
    let out = script.agent(PROC, AgentEvent::Group { empty: true });
    assert_eq!(
        &out[..2],
        [
            json!({ "forget": PROC }),
            json!({ "start": 1005, "program": "/opt/caf/claude-agent-acp" })
        ]
    );
    let out = script.answers(1005, 1000, &claude_init());
    // The session is continued where the agent can: resumed.
    assert_has(
        &sent(&out, "session/resume"),
        &json!({ "params": { "sessionId": SESSION, "cwd": "/work" } }),
    );
    let out = script.answers(1005, 1001, &json!({ "modes": super::default_modes() }));
    assert_has(
        &sent(&out, "session/prompt"),
        &json!({ "params": { "prompt": [{ "text": "and then this" }] } }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Open", "value": { "session": SESSION } } }),
    );
}

#[test]
fn an_adapter_that_dies_says_why_from_its_error_output() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    script.agent(
        PROC,
        AgentEvent::Stderr(b"Error: out of memory\nat line \x1b[31m3".to_vec()),
    );
    script.agent(PROC, AgentEvent::Stderr(b" for me@example.com\n".to_vec()));
    script.agent(PROC, AgentEvent::Exited(Exit::Signal(9)));
    script.agent(PROC, AgentEvent::OutputClosed);
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "AgentStopped", "value": { "stderr": ["Error: out of memory", "at line ^[[31m3 for <email>"] } } } },
            "turns": [{ "life": { "type": "Failed", "value": { "reason": { "type": "AgentStopped" }, "agent_said": null, "remedy": { "type": "SendAgain" } } } }]
        }),
    );
    assert_eq!(
        script.core.connections()[0].stderr,
        ["Error: out of memory", "at line ^[[31m3 for <email>"]
    );
}

#[test]
fn a_gone_adapter_that_never_closes_its_output_is_given_up_on() {
    let mut script = Script::with_claude();
    script.at(500);
    script.agent(
        PROC,
        AgentEvent::WriteFailed {
            error: "broken pipe".to_owned(),
        },
    );
    // It keeps talking: the wait grows, but never past five times.
    for ms in [2000, 3500, 5000, 6500, 8000, 9500] {
        script.at(ms);
        script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "." } }));
    }
    assert_eq!(
        script.state()["procs"]["1002"]["gone"]["until"],
        json!(10_500_000_000_u64)
    );
    script.at(10_500);
    let out = script.woken();
    assert_eq!(with(&out, "check_group"), [json!({ "check_group": PROC })]);
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Lost", "value": { "reason": { "type": "AgentStopped", "value": {} } } } }),
    );
}

#[test]
fn an_adapter_that_cant_start_fails_its_chats_waiting_messages() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    let (turn, _) = script.send(CHAT, "hello");
    let out = script.agent(
        PROC,
        AgentEvent::StartFailed {
            error: "No such file or directory".to_owned(),
        },
    );
    assert!(with(&out, "signal").is_empty(), "nothing ran: {out:#?}");
    // Let go at once, so the driver frees what it kept for it.
    assert_eq!(with(&out, "forget"), [json!({ "forget": PROC })]);
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "CouldNotStart", "value": { "error": "No such file or directory" } } } },
            "turns": [{ "id": turn.expect("sent").0, "life": { "type": "Failed", "value": { "reason": { "type": "AgentStopped" }, "remedy": { "type": "SendAgain" } } } }]
        }),
    );
    // The next message tries again, as turn 1004, on process 1005.
    let (_, out) = script.send(CHAT, "again");
    assert_has(&json!(with(&out, "start")), &json!([{ "start": 1005 }]));
}

#[test]
fn a_failed_initialize_fails_the_connection_with_a_typed_reason() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    script.send(CHAT, "hello").0.expect("sent");
    let out = script.fails(
        PROC,
        1000,
        &json!({ "code": -32000, "message": "Authentication required" }),
    );
    assert_eq!(
        with(&out, "signal"),
        [json!({ "signal": PROC, "is": "Term" })]
    );
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "SignInNeeded" } } },
            "turns": [{ "life": { "type": "Failed", "value": { "reason": { "type": "SignInNeeded" } } } }]
        }),
    );

    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    script.answers(
        PROC,
        1000,
        &json!({ "protocolVersion": 2, "agentCapabilities": {} }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Lost", "value": { "reason": { "type": "Incompatible", "value": { "type": "Protocol", "value": { "version": 2 } } } } } }),
    );

    // A version JavaScript can't read exactly is kept as not said, never
    // rounded.
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    script.answers(
        PROC,
        1000,
        &json!({ "protocolVersion": 1_u64 << 53, "agentCapabilities": {} }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Lost", "value": { "reason": { "type": "Incompatible", "value": { "type": "Protocol", "value": { "version": null } } } } } }),
    );
}

#[test]
fn capabilities_are_read_leniently() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("plain", "/work"))
        .0
        .expect("opened");
    let init = json!({
        "protocolVersion": 1,
        "agentCapabilities": { "loadSession": "yes", "sessionCapabilities": { "resume": true, "close": null }, "mcpCapabilities": 7 },
        "agentInfo": { "name": "plain-acp", "version": "1.0.0" },
    });
    let out = script.answers(PROC, 1000, &init);
    sent(&out, "session/new");
    assert_eq!(
        script.state()["procs"]["1002"]["init"],
        json!({ "type": "Ready", "value": { "load": false, "resume": false, "close": false, "mcp_http": false } })
    );
}

#[test]
fn an_adapter_that_doesnt_say_what_it_is_gets_the_cautious_profile() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    let init = json!({ "protocolVersion": 1, "agentCapabilities": {} });
    script.answers(PROC, 1000, &init);
    assert_has(
        &script.state(),
        &json!({ "conns": { "1001": { "profile": { "profile": { "type": "Cautious" }, "adapter": null, "checked": { "type": "Mismatch" } } } } }),
    );
}

#[test]
fn an_adapter_slow_to_start_asks_whether_to_keep_waiting() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    script.at(30_000);
    let out = script.woken();
    // The timer has just fired, and nothing else is due.
    assert!(with(&out, "wake_at").is_empty());
    assert_has(
        &script.chat(CHAT),
        &json!({ "questions": [{ "id": 1003, "kind": { "type": "SlowToOpen", "value": { "adapter_answered": false } } }] }),
    );

    script.at(31_000);
    let (answered, out) = script.answer(
        crate::ids::QuestionId(1003),
        Answer::SlowToOpen(SlowChoice::KeepWaiting),
    );
    answered.expect("answered");
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 61_000 })]);
    assert_eq!(script.chat(CHAT)["questions"], json!([]));

    script.at(40_000);
    let out = script.answers(PROC, 1000, &claude_init());
    sent(&out, "session/new");
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 70_000 })]);
}

#[test]
fn closing_the_last_chat_stops_its_adapter_gently_then_lets_it_go() {
    let mut script = Script::with_claude();
    script.at(100);
    let (closed, out) = script.close(CHAT);
    closed.expect("closed");
    assert_eq!(&out[..1], [json!({ "close_input": PROC })]);
    assert!(
        writes(&out).is_empty(),
        "no session/close: closing input ends every session"
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Closed" } }),
    );

    script.at(2100);
    assert_eq!(
        with(&script.woken(), "signal"),
        [json!({ "signal": PROC, "is": "Term" })]
    );
    script.at(2200);
    script.agent(PROC, AgentEvent::Exited(Exit::Signal(15)));
    let out = script.agent(PROC, AgentEvent::OutputClosed);
    assert_eq!(with(&out, "check_group"), [json!({ "check_group": PROC })]);
    assert!(!script.core.is_idle());
    let out = script.agent(PROC, AgentEvent::Group { empty: true });
    assert_eq!(out[0], json!({ "forget": PROC }));
    assert!(script.core.connections().is_empty());
    assert!(script.core.is_idle());
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": null })]);
}

/// What the core shows a debugger includes whether it's shutting down,
/// which changes what it does next, and what it was made with.
#[test]
fn the_cores_whole_state_says_it_is_shutting_down() {
    let mut script = Script::with_claude();
    assert_has(
        &script.state(),
        &json!({
            "shutting_down": false,
            "first_id": FIRST,
            "host": { "name": "caf-test" },
            "agents": { "claude": { "name": "claude" } }
        }),
    );
    script.shutdown();
    assert_eq!(script.state()["shutting_down"], json!(true));
}

#[test]
fn shutting_down_cuts_turns_off_and_stops_every_adapter() {
    let mut script = Script::with_claude();
    let (turn, _) = script.send(CHAT, "go");
    let (waiting, _) = script.send(CHAT, "later");
    let out = script.shutdown();
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "ShutDown" } } },
            "turns": [{ "id": turn.expect("sent").0, "life": { "type": "CutOff" } }, { "id": waiting.expect("queued").0, "life": { "type": "Stopped" } }]
        }),
    );
    assert_eq!(
        sent(&out, "session/cancel")["params"],
        json!({ "sessionId": SESSION })
    );
    assert_eq!(with(&out, "close_input"), [json!({ "close_input": PROC })]);
    script.agent(PROC, AgentEvent::Exited(Exit::Code(0)));
    script.agent(PROC, AgentEvent::OutputClosed);
    // Stays as it was: shut down, not stopped by the agent.
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Lost", "value": { "reason": { "type": "ShutDown" } } } }),
    );
    let out = script.agent(PROC, AgentEvent::Group { empty: true });
    assert!(with(&out, "start").is_empty(), "nothing starts again");
    assert!(script.core.is_idle());
}

#[test]
fn events_for_a_process_let_go_are_ignored() {
    let mut script = Script::with_claude();
    script.close(CHAT).0.expect("closed");
    script.agent(PROC, AgentEvent::OutputClosed);
    script.agent(PROC, AgentEvent::Group { empty: true });
    let before = script.state();
    let out = script.answers(PROC, 1001, &json!({}));
    assert_eq!(
        out.iter()
            .filter(|each| each.get("report").is_none())
            .count(),
        0,
        "{out:#?}"
    );
    assert_eq!(script.state(), before);
}

#[test]
fn retrying_an_adapter_that_hasnt_answered_restarts_it() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    // The agent's requests are answered at once, even before initialize
    // is: only CAF's own wait.
    let out = script.says(
        PROC,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "fs/read_text_file", "params": {} }),
    );
    assert_eq!(writes(&out).len(), 1);
    script.at(30_000);
    script.woken();
    let (retried, out) = script.answer(
        crate::ids::QuestionId(1003),
        Answer::SlowToOpen(SlowChoice::Retry),
    );
    retried.expect("retried");
    assert_eq!(
        with(&out, "signal"),
        [json!({ "signal": PROC, "is": "Term" })]
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Opening" }, "questions": [] }),
    );
    script.at(32_000);
    // SIGKILL follows SIGTERM if the group isn't empty by then.
    assert_eq!(
        with(&script.woken(), "signal"),
        [json!({ "signal": PROC, "is": "Kill" })]
    );
    script.agent(PROC, AgentEvent::Exited(Exit::Signal(9)));
    script.agent(PROC, AgentEvent::OutputClosed);
    let out = script.agent(PROC, AgentEvent::Group { empty: true });
    assert_has(&json!(with(&out, "start")), &json!([{ "start": 1004 }]));
}

#[test]
fn an_adapter_let_go_that_answers_late_changes_nothing() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    // Its only chat closes before it answers: it's let go.
    script.close(CHAT).0.expect("closed");
    // It answers after all, as another adapter, as it's being stopped.
    let mut other = claude_init();
    other["agentInfo"]["name"] = json!("someone-elses-acp");
    let out = script.answers(PROC, 1000, &other);
    assert!(writes(&out).is_empty(), "nothing asked of it: {out:#?}");
    assert!(with(&out, "status").is_empty(), "nothing run: {out:#?}");
    assert_eq!(script.agent_status("claude")["own_tools_off"], true);
}

#[test]
fn giving_up_on_a_slow_session_stops_its_waiting_message() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    let (turn, _) = script.send(CHAT, "hello");
    script.answers(PROC, 1000, &claude_init());
    script.at(30_000);
    script.woken();
    let question = script.chat(CHAT)["questions"][0]["id"]
        .as_u64()
        .expect("asked");
    assert_has(
        &script.chat(CHAT),
        &json!({ "questions": [{ "kind": { "type": "SlowToOpen", "value": { "adapter_answered": true } } }] }),
    );
    script
        .answer(
            crate::ids::QuestionId(question),
            Answer::SlowToOpen(SlowChoice::Stop),
        )
        .0
        .expect("answered");
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "Stopped" } } },
            "questions": [],
            "turns": [{ "id": turn.expect("sent").0, "life": { "type": "Stopped" } }]
        }),
    );
    // The agent answering late gets its session closed.
    let out = script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": "late" }),
    );
    sent(&out, "session/close");
    script.answers(PROC, 1002, &json!({}));
}

#[test]
fn a_session_refused_for_sign_in_says_so() {
    let mut script = Script::new(host());
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    let (turn, _) = script.send(CHAT, "hello");
    script.answers(PROC, 1000, &claude_init());
    script.fails(
        PROC,
        1001,
        &json!({ "code": -32000, "message": "Authentication required" }),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "SignInNeeded" } } },
            "turns": [{ "id": turn.expect("sent").0, "life": { "type": "Failed", "value": { "reason": { "type": "SignInNeeded" }, "remedy": { "type": "SignIn" } } } }]
        }),
    );
}

#[test]
fn a_process_that_exited_is_let_go_in_a_fixed_time_whatever_holds_its_output_open() {
    let mut script = Script::with_claude();
    script.at(1000);
    script.agent(PROC, AgentEvent::Exited(Exit::Code(0)));
    // Something it left behind keeps writing, so its output never closes,
    // and its group never says it's empty.
    let mut ms = 1000;
    while ms + 500 < 13_000 {
        ms += 500;
        script.at(ms);
        let out = script.woken();
        assert!(with(&out, "forget").is_empty(), "not yet, at {ms}");
        script.agent(PROC, AgentEvent::Stderr(b"still here\n".to_vec()));
        if script.state()["procs"]["1002"]["group"] == json!({ "type": "Asked" }) {
            script.agent(PROC, AgentEvent::Group { empty: false });
        }
    }
    // At most five times `gone` for its last words, then `kill_after`.
    script.at(13_000);
    let out = script.woken();
    assert_eq!(with(&out, "forget"), [json!({ "forget": PROC })]);
    assert_eq!(
        with(&out, "signal"),
        [json!({ "signal": PROC, "is": "Kill" })]
    );
    assert_eq!(with(&out, "report")[0]["report"], "Warn");
    assert_eq!(script.state()["procs"], json!({}));
    // The next message starts another.
    let (_, out) = script.send(CHAT, "again");
    assert_eq!(with(&out, "start").len(), 1);
}
