//! Recorded traces: what happened, as JSON events, replayed into a fresh
//! core. A bug becomes one of these, with what the core must give out
//! and end as.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, check_fixtures, host, render, with, writes};
use super::stop::permission;
use super::{CHAT, PROC, SESSION};
use crate::core::Core;
use crate::event::PolicyAnswer;
use crate::ids::ChatId;
use crate::profiles::{Builtin, ProfileChoice, Reported};

/// The scenario `traces/claude-turn.json` was recorded from.
fn claude_turn() -> Script {
    let mut script = Script::with_claude();
    // Its fixture is the recording's, from `a_recorded_claude_turn_replays_from_json`.
    script.without_fixtures();
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
            "state": { "type": "Open", "value": { "session": SESSION } },
            "turns": [{ "life": { "type": "Finished" }, "items": [
                { "kind": { "type": "Words", "value": "Running them." } },
                { "kind": { "type": "Tool", "value": { "title": "Run `cargo test`", "status": { "type": "Done" }, "output": "test result: ok" } } }
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
        caf_recording: u32,
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
    assert_eq!(
        header.caf_recording,
        crate::event::RECORDING_VERSION,
        "a recording of this version"
    );
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
    let chat = serde_json::to_value(core.chat(ChatId::from_number(1)).expect("the chat"))
        .expect("chats serialise");
    assert_has(
        &chat,
        &json!({
            "state": { "type": "Lost", "value": { "reason": { "type": "ShutDown" } } },
            "turns": [
                { "starter": { "type": "User", "value": { "parts": [{ "type": "Text", "value": "Reply with exactly the word pong." }] } }, "life": { "type": "Finished" }, "items": [{ "kind": { "type": "Words", "value": "pong" } }] },
                { "starter": { "type": "User", "value": {} }, "life": { "type": "Stopped" }, "items": [{ "kind": { "type": "Words", "value": "**The Keeper of Halden Rock**\n\nFor thirty-one years, Ed" } }] }
            ],
            "questions": []
        }),
    );
    assert_eq!(
        chat["usage"]["used"].as_u64().map(|used| used > 0),
        Some(true),
        "{chat:#}"
    );
    // What each turn spent: the first by Claude's split, which counts
    // more than its usage does; the second, stopped, by its usage, as its
    // split is empty, against the model Claude said it used during it. The
    // session's cost was said during it, but no higher, as Claude's adapter
    // leaves a stopped turn out of its total: not reported, so estimated.
    let tokens = |input: u64, read: u64, written: u64, output: u64, thinking: Value| json!({ "input": input, "cache_read": read, "cache_write": written, "output": output, "thinking": thinking });
    // As serde_json reads it from the trace.
    let first = &chat["turns"][0]["spent"]["cost"]["amount"];
    assert!(
        first
            .as_f64()
            .is_some_and(|amount| (amount - 0.002_679_39).abs() < 1e-15)
    );
    assert_eq!(
        chat["turns"][0]["spent"],
        json!({
            "models": [{ "model": "claude-haiku-5-5", "name": "Haiku 5.5", "tokens": tokens(1178, 12799, 12123, 18, json!(0)) }],
            "cost": { "amount": first, "currency": "USD" },
            "fast": false,
        })
    );
    assert_eq!(
        chat["turns"][1]["spent"],
        json!({
            "models": [{ "model": "claude-haiku-5-5", "name": "Haiku 5.5", "tokens": tokens(2, 15847, 11185, 0, Value::Null) }],
            "cost": null,
            "fast": false,
        })
    );
    assert_eq!(chat["turns"][1]["model"]["id"], "haiku");
    assert_eq!(chat["spend"]["turns"], 2);
    let all: Vec<_> = outs.iter().flatten().cloned().collect();
    assert_eq!(
        (with(&all, "usage").iter())
            .map(|line| line["usage"]["value"]["usage"].clone())
            .collect::<Vec<_>>(),
        [
            chat["turns"][0]["spent"].clone(),
            chat["turns"][1]["spent"].clone()
        ],
        "a ledger line for each"
    );
    assert!(core.is_idle(), "every adapter stopped");
    // The stop was sent as a cancel, and the adapter's process let go.
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
    check_fixtures(&core);
    // The same recording replays the same way again.
    let (again, outs_again) = replay_recording(CLAUDE_LIVE);
    assert_eq!(outs_again, outs);
    assert_eq!(again.inspect(), core.inspect());
}

/// A real run of CAF's patched claude-agent-acp, `0.98.0+caf.1`, freshly
/// installed and on Haiku, recorded by the tokio kit: CAF installs it,
/// learns its settings, and the chat asks for "pong" and gets it, then
/// CAF shuts down. Claude Code's first report of the account's limits,
/// from its quota probe as it starts, comes before the turn is active and
/// with nothing measured yet, so the adapter sends it as the limits alone
/// (`_caf/limitsOnly`).
///
/// Scrubbed before it was checked in, as [`CLAUDE_LIVE`] was, of the
/// install's and the chat's folders, the chat's id, and the user's own
/// agents and models
/// in the session's settings; and the report's figures (shares, thresholds,
/// resets and the state of extra usage, which reads as allowed and on) are
/// made up, as they're the account's own.
const CLAUDE_PATCHED_LIVE: &str = include_str!("traces/claude-0.98.0+caf.1-limits.jsonl");

#[test]
fn a_live_run_of_claudes_patched_adapter_hears_its_first_limits_report() {
    use crate::ids::AgentName;

    let (core, _) = replay_recording(CLAUDE_PATCHED_LIVE);
    // The adapter said it's patched.
    assert_eq!(
        CLAUDE_PATCHED_LIVE
            .matches(r#""version":"0.98.0+caf.1""#)
            .count(),
        2
    );
    let limits = serde_json::to_value(core.agent_limits(&AgentName("claude".into())))
        .expect("limits serialise");
    assert_eq!(
        limits["windows"],
        json!([{ "kind": { "type": "Week" }, "used": 50, "resets": 1_795_600_000_000_i64,
            "state": { "type": "Warning" }, "heard": limits["windows"][0]["heard"] }])
    );
    assert_eq!(limits["extra_usage"]["in_use"], false);
    // The report came before the turn's first measure of the agent's
    // memory, and said nothing of it.
    let alone = CLAUDE_PATCHED_LIVE
        .find("_caf/limitsOnly")
        .expect("the report");
    let measured = CLAUDE_PATCHED_LIVE
        .find(r#""sessionUpdate":"usage_update","used":6"#)
        .expect("a measure");
    assert!(alone < measured);
    let chat = serde_json::to_value(core.chat(ChatId::from_number(1)).expect("the chat"))
        .expect("chats serialise");
    assert_has(
        &chat,
        &json!({ "turns": [{ "life": { "type": "Finished" }, "items": [{ "kind": { "type": "Words", "value": "pong" } }] }] }),
    );
    assert!(
        chat["usage"]["used"].as_u64().is_some_and(|used| used > 0),
        "{chat:#}"
    );
    check_fixtures(&core);
}

/// Every count of what a turn spent in the live trace, as the Claude
/// profile reads it, adds up to the total Claude gave: so input is
/// uncached, and thinking is part of output, not counted beside it. An
/// adapter that changed either would fail here.
#[test]
fn every_count_claude_gave_of_what_a_turn_spent_adds_up_to_its_total() {
    let claude = ProfileChoice::Builtin(Builtin::ClaudeAgentAcp);
    let mut rows = 0;
    for line in CLAUDE_LIVE.lines() {
        let line: Value = serde_json::from_str(line).expect("each line is JSON");
        let response = line.pointer("/event/value/event/value/value/value");
        let Some(result) = response.and_then(|response| response.pointer("/outcome/Ok")) else {
            continue;
        };
        let (Reported::Rows(each), _) = claude.spent(result) else {
            continue;
        };
        for row in each {
            let tokens = &row.tokens;
            let added = tokens.input + tokens.cache_read + tokens.cache_write + tokens.output;
            assert_eq!(added, row.total, "{row:?}");
            assert_eq!(added, tokens.total());
            rows += 1;
        }
    }
    assert_eq!(rows, 2, "one for each turn");
}

/// A copy of a core, taken partway through a replay, carries on as the
/// original does: what debugging tools keep as checkpoints.
#[cfg(feature = "inspect")]
#[test]
fn a_copy_of_the_core_carries_on_as_the_core_does() {
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
    let mut lines = claude_live_lines();
    let header: Header = serde_json::from_str(lines.remove(0)).expect("the header reads");
    let mut core = Core::new(header.host, header.agents);
    let half = lines.len() / 2;
    let step = |line: &str| -> Step { serde_json::from_str(line).expect("a step reads") };
    for line in &lines[..half] {
        let step = step(line);
        core.handle(step.now, step.event, &mut Vec::new());
    }
    let mut copy = core.clone();
    assert_eq!(copy.inspect(), core.inspect());
    for line in &lines[half..] {
        let (one, two) = (step(line), step(line));
        let (mut out, mut copied) = (Vec::new(), Vec::new());
        core.handle(one.now, one.event, &mut out);
        copy.handle(two.now, two.event, &mut copied);
        let render =
            |out: &[crate::directive::Directive]| out.iter().map(render).collect::<Vec<_>>();
        assert_eq!(render(&copied), render(&out));
    }
    assert_eq!(copy.inspect(), core.inspect());
}

/// The live trace's lines, up to where it says it stopped, if it does.
#[cfg(feature = "inspect")]
fn claude_live_lines() -> Vec<&'static str> {
    (CLAUDE_LIVE.lines())
        .take_while(|line| !line.starts_with(r#"{"stopped":"#))
        .collect()
}
