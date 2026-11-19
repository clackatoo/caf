//! Recorded traces: what happened, as JSON events, replayed into a fresh
//! core. A bug becomes one of these, with what the core must give out
//! and end as.

use serde_json::json;

use super::harness::{Script, assert_has, host, render, with, writes};
use super::stop::permission;
use super::{CHAT, PROC, SESSION};
use crate::core::Core;
use crate::event::PolicyAnswer;
use crate::ids::ChatId;

/// The scenario `traces/claude-turn.json` was recorded from.
fn claude_turn() -> Script {
    let mut script = Script::with_claude();
    script.at(2000);
    script.send(CHAT, "Run the tests").0.expect("sent");
    script.at(2100);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Running them." } }));
    script.says(PROC, &permission(0, "toolu_01"));
    script.at(2500);
    script.policy(1005, PolicyAnswer::Allow);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "toolu_01", "status": "completed", "rawOutput": "test result: ok" }));
    script.at(3000);
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
    script
}

#[test]
fn a_recorded_claude_turn_replays_from_json() {
    let script = Script::from_recording(host(), include_str!("traces/claude-turn.json"));
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "Open": { "session": SESSION } },
            "turns": [{ "life": "Finished", "items": [
                { "kind": { "Words": "Running them." } },
                { "kind": { "Tool": { "title": "Run `cargo test`", "status": "Done", "output": "test result: ok" } } }
            ] }]
        }),
    );
    // The recording is what the scenario records today.
    assert_eq!(
        claude_turn().recording(),
        serde_json::from_str::<serde_json::Value>(include_str!("traces/claude-turn.json"))
            .expect("JSON")
    );
}

#[test]
#[ignore = "prints the recording, to make the trace"]
fn print_claude_turn() {
    println!(
        "{}",
        serde_json::to_string_pretty(&claude_turn().recording()).expect("JSON")
    );
}

/// A real run of claude-agent-acp 0.98.0 on Haiku, recorded by the tokio
/// kit (`Config::record`): the chat asks for "pong" and gets it, then
/// asks for a long story and stops it as the words come in, then CAF
/// shuts down.
///
/// Scrubbed before it was checked in, of what's the user's own and
/// changes no state: the adapter's path and the chat's folder, a stack
/// trace on stderr naming a home folder, the user's own skills from the
/// command lists, and their own agents and models from the session's
/// settings. The kit itself wrote Claude's sign-in status without its
/// params, the agent's variables without their values, and error output
/// as text.
const CLAUDE_LIVE: &str = include_str!("traces/claude-0.98.0-live.jsonl");

/// Replays a recording of the tokio kit's into a fresh core, returning
/// it and what each event gave out.
fn replay_recording(text: &str) -> (Core, Vec<Vec<serde_json::Value>>) {
    #[derive(serde::Deserialize)]
    struct Header {
        host: crate::host::Host,
        agents: Vec<crate::agent::AgentSpec>,
    }
    #[derive(serde::Deserialize)]
    struct Step {
        now: crate::time::Now,
        event: crate::event::Event,
    }
    let mut lines = text.lines();
    let header: Header =
        serde_json::from_str(lines.next().expect("a header")).expect("the header reads");
    let mut core = Core::new(header.host, header.agents);
    let mut outs = Vec::new();
    // A recording cut short says so, and ends there.
    for line in lines.take_while(|line| !line.starts_with(r#"{"stopped":"#)) {
        let step: Step = serde_json::from_str(line).expect("a step reads");
        let mut out = Vec::new();
        core.handle(step.now, step.event, &mut out);
        let rendered: Vec<_> = out.iter().map(render).collect();
        assert!(
            with(&rendered, "wake_at").len() <= 1,
            "one wake-up at most: {rendered:#?}"
        );
        outs.push(rendered);
    }
    (core, outs)
}

#[test]
fn a_live_claude_run_replays_to_its_end() {
    let (core, outs) = replay_recording(CLAUDE_LIVE);
    let chat =
        serde_json::to_value(core.chat(ChatId(1)).expect("the chat")).expect("chats serialise");
    assert_has(
        &chat,
        &json!({
            "state": { "Lost": { "reason": "ShutDown" } },
            "turns": [
                { "starter": { "User": { "parts": [{ "Text": "Reply with exactly the word pong." }] } }, "life": "Finished", "items": [{ "kind": { "Words": "pong" } }] },
                { "starter": { "User": {} }, "life": "Stopped", "items": [{ "kind": { "Words": "**The Keeper of Halden Rock**\n\nFor thirty-one years, Ed" } }] }
            ],
            "questions": []
        }),
    );
    assert_eq!(
        chat["usage"]["used"].as_u64().map(|used| used > 0),
        Some(true),
        "{chat:#}"
    );
    assert!(core.is_idle(), "every adapter stopped");
    // The stop was sent as a cancel, and the adapter's process let go.
    let all: Vec<_> = outs.iter().flatten().cloned().collect();
    assert_eq!(
        writes(&all)
            .iter()
            .filter(|(_, message)| message["method"] == "session/cancel")
            .count(),
        1
    );
    assert_eq!(with(&all, "forget").len(), 1);
    // Claude's sign-in status came, and was recorded without the email.
    assert_eq!(
        CLAUDE_LIVE
            .matches(r#""method":"_auth/status_update","params":null"#)
            .count(),
        3
    );
    // The same recording replays the same way again.
    let (again, outs_again) = replay_recording(CLAUDE_LIVE);
    assert_eq!(outs_again, outs);
    assert_eq!(again.snapshot(), core.snapshot());
}
