//! The fake agent's script, so the debugger works with no real agent.
//!
//! The fake (`caf-fake-agent`) follows a script with no branches or
//! loops, so this one is in two parts:
//!
//! - **Its session** is opened by a handler, new or resumed: it says it
//!   can resume sessions, so a chat the debugger restores carries on
//!   with it, as it would with a real agent. It has no memory, so
//!   resuming is only saying yes.
//! - **Any message** is answered at once by a handler: a line of thinking,
//!   then a few words, each a chunk of its own, and the turn ends.
//! - **The message `fail`**, exactly, is answered at once with an error,
//!   the first time in a chat: CAF fails the turn, offering Send again.
//!   Sent again, it's answered as any message is, so Send again can be
//!   seen to work.
//! - **The message `sign in`**, exactly, is answered at once as wanting a
//!   sign-in (ACP's -32000), the first time in a chat: CAF fails the turn,
//!   offering Sign in where it can sign in to the agent, as it can with
//!   the `fake` profile.
//! - **The message `ask`**, exactly, is waited for by the script itself,
//!   round after round: a few words, then it asks permission to run a
//!   command, and once that's answered, the turn ends. While the question
//!   waits, Stop ends the turn as a real adapter does, with
//!   `cancelled`.
//!
//! A Stop while the question waits must end the turn `cancelled`, never
//! `end_turn`: the round makes sure of it with a request CAF refuses,
//! which comes back only after any cancel.
//!
//! A handler can't wait, and the fake's patterns match a prompt only
//! whole, which is why the round that asks needs its exact word. Nor can
//! the script stop partway through a round, so a round says nothing
//! after it starts to wait: a stopped turn hears nothing more, rather
//! than the rest of its words arriving as a turn of the agent's own. The
//! fake serves one session, so the debugger starts one for each chat.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The message that makes the fake ask permission.
pub const ASK: &str = "ask";

/// The message that makes the fake fail the turn, the first time.
pub const FAIL: &str = "fail";

/// The message that makes the fake want a sign-in, the first time.
pub const SIGN_IN: &str = "sign in";

/// How many times `ask` can be sent to one fake: more are answered as
/// any message is.
const ROUNDS: u32 = 200;

/// The session the fake opens.
const SESSION: &str = "fake-1";

/// The words the fake says to any message.
const REPLY: &str = "This is CAF's fake agent, with no model behind it, so it says this to \
                     every message. Send exactly \"ask\" to see it ask permission to run a \
                     command, \"fail\" to see a turn fail, or \"sign in\" to see one fail for \
                     want of signing in.";

/// The words before it asks.
const ASKING: &str = "I'd like to run a command. Allow it, refuse it, or stop me.";

/// The script, as the fake reads it.
#[must_use]
pub fn script() -> String {
    let mut lines = vec![
        "# caf-debugger's fake agent: see `fake.rs` in caf-debugger.".to_owned(),
        // Stop ends the asking round's prompt. The name `prompt` is given
        // to `initialize` first, answered at once, so a stop before any
        // asking round answers nothing rather than naming nothing.
        line(&json!({"on": {"method": "session/cancel", "do": [
            {"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}},
        ]}})),
        line(
            &json!({"on": {"method": "session/prompt", "match": prompt(FAIL), "once": true, "do": [
                {"respond": {"error": {"code": -32603, "message": "The fake agent failed, as it was asked to. Send the message again, and it won't."}}},
            ]}}),
        ),
        line(
            &json!({"on": {"method": "session/prompt", "match": prompt(SIGN_IN), "once": true, "do": [
                {"respond": {"error": {"code": -32000, "message": "Authentication required: the fake agent wants a sign-in, as it was asked to."}}},
            ]}}),
        ),
        line(&json!({"on": {"method": "session/prompt", "fallback": true, "do": reply()}})),
        // Anything else is refused, as an adapter refuses a method it
        // doesn't know.
        line(&json!({"on": {"fallback": true, "do": [
            {"respond": {"error": {"code": -32601, "message": "Method not found"}}},
        ]}})),
        // A chat's session, new or carried on from a saved chat's.
        line(&json!({"on": {"method": "session/new", "do": [
            {"respond": {"result": {"sessionId": SESSION}}},
        ]}})),
        line(&json!({"on": {"method": "session/resume", "do": [
            {"respond": {"result": {}}},
        ]}})),
        line(&json!({"wait": {"method": "initialize", "as": "prompt"}})),
        line(&json!({"respond": {"result": {
            "protocolVersion": 1,
            "agentCapabilities": {"sessionCapabilities": {"resume": {}}},
            "agentInfo": {"name": "caf-fake-agent", "version": env!("CARGO_PKG_VERSION")},
        }}})),
    ];
    for round in 1..=ROUNDS {
        lines.extend(asking(round).iter().map(line));
    }
    lines.join("\n") + "\n"
}

/// Writes the script into `dir`, replacing any there, and returns its
/// path.
///
/// # Errors
///
/// If it can't be written.
pub fn write(dir: &Path) -> std::io::Result<PathBuf> {
    let path = dir.join("fake-agent.jsonl");
    // Written beside it and moved into place, so a fake starting now
    // never reads half of it.
    let partial = dir.join(format!("fake-agent.jsonl.{}", std::process::id()));
    std::fs::write(&partial, script())?;
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

/// One step as a line.
fn line(step: &Value) -> String {
    step.to_string()
}

/// The answer to any message.
fn reply() -> Value {
    let mut steps = vec![
        json!({"think": {"session": SESSION, "text": "Nothing to think about: I'm the fake agent."}}),
    ];
    steps.extend(words(REPLY).map(|word| json!({"say": {"session": SESSION, "text": word}})));
    steps.push(json!({"respond": {"result": {"stopReason": "end_turn"}}}));
    Value::Array(steps)
}

/// One round of asking, for the message `ask`.
fn asking(round: u32) -> Vec<Value> {
    let tool = format!("fake-{round}");
    let mut steps = vec![
        json!({"wait": {"method": "session/prompt", "match": prompt(ASK), "as": "prompt"}}),
        json!({"think": {"session": SESSION, "text": "They asked me to ask."}}),
    ];
    steps.extend(words(ASKING).map(|word| json!({"say": {"session": SESSION, "text": word}})));
    steps.extend([
        json!({"request": {"method": "session/request_permission", "as": "permission", "params": {
            "sessionId": SESSION,
            "toolCall": {"toolCallId": tool, "title": "Run `echo hello`", "kind": "execute"},
            "options": [
                {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
            ],
        }}}),
        json!({"await": {"to": "permission"}}),
        // A Stop answers the question `cancelled` before it sends
        // `session/cancel`, so the cancel may not have been read yet.
        // CAF refuses this request, and answers it only after what it
        // sent before, so once its answer is in, so is any cancel, and
        // the cancel's handler has answered the prompt `cancelled`
        // before `end_turn` below, which then does nothing.
        json!({"request": {"method": "_caf_fake/sync", "as": "sync"}}),
        json!({"await": {"to": "sync"}}),
        json!({"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}}),
    ]);
    steps
}

/// A pattern matching a prompt of exactly `text`.
fn prompt(text: &str) -> Value {
    json!({"params": {"prompt": [{"type": "text", "text": text}]}})
}

/// Text in words, each with the space before it.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(' ').enumerate().map(|(i, word)| {
        if i == 0 {
            word.to_owned()
        } else {
            format!(" {word}")
        }
    })
}

#[cfg(test)]
mod tests {
    use caf_fake_agent::Script;

    use super::script;

    #[test]
    fn the_fake_agents_script_reads() {
        let text = script();
        Script::parse(&text).expect("the script reads");
    }
}
