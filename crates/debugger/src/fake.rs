//! The fake agent's script, so the debugger works with no real agent.
//!
//! The fake (`caf-fake-agent`) follows a script with no branches or
//! loops, so this one is in two parts:
//!
//! - **Any message** is answered at once by a handler: a line of thinking,
//!   then a few words, each a chunk of its own, and the turn ends.
//! - **The message `ask`**, exactly, is waited for by the script itself,
//!   round after round: a few words, then it asks permission to run a
//!   command, and once that's answered, the turn ends. While the question
//!   waits, Stop ends the turn as a real adapter does, with
//!   `cancelled`.
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

/// How many times `ask` can be sent to one fake: more are answered as
/// any message is.
const ROUNDS: u32 = 200;

/// The session the fake opens.
const SESSION: &str = "fake-1";

/// The words the fake says to any message.
const REPLY: &str = "This is CAF's fake agent, with no model behind it, so it says this to \
                     every message. Send exactly \"ask\" to see it ask permission to run a \
                     command.";

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
        line(&json!({"on": {"method": "session/prompt", "fallback": true, "do": reply()}})),
        // Anything else is refused, as an adapter refuses a method it
        // doesn't know.
        line(&json!({"on": {"fallback": true, "do": [
            {"respond": {"error": {"code": -32601, "message": "Method not found"}}},
        ]}})),
        line(&json!({"wait": {"method": "initialize", "as": "prompt"}})),
        line(&json!({"respond": {"result": {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "agentInfo": {"name": "caf-fake-agent", "version": env!("CARGO_PKG_VERSION")},
        }}})),
        line(&json!({"wait": {"method": "session/new"}})),
        line(&json!({"respond": {"result": {"sessionId": SESSION}}})),
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
    let ask_text = json!({"params": {"prompt": [{"type": "text", "text": ASK}]}});
    let mut steps = vec![
        json!({"wait": {"method": "session/prompt", "match": ask_text, "as": "prompt"}}),
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
        json!({"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}}),
    ]);
    steps
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
