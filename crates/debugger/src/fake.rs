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
//! - **What a turn spent**, only if asked (`--fake-usage`), as Claude's
//!   adapter says it: how full its context is and what the session has
//!   cost so far, then the turn's tokens. It has no memory, so it says the
//!   same cost every time: a chat's first turn costs that, and every later
//!   one is estimated. Made-up figures, so off unless asked: the ledger
//!   the debugger writes to is, by default, the one every CAF app shares,
//!   and they would count in the user's real usage.
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
//! - **Its settings**, so the debugger's picker can be tried: two models,
//!   three effort levels and fast mode, offered as Claude offers its own,
//!   as ACP's session config options, whenever its session opens, new or
//!   resumed. They change nothing it says. A change is answered at once,
//!   without the list, which CAF takes as done as asked: the fake has no
//!   memory to list what else was chosen from.
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

/// The script's file.
pub const FILE: &str = "fake-agent.jsonl";

/// The script's file when it says what each turn spent.
pub const USAGE_FILE: &str = "fake-agent-usage.jsonl";

/// The session the fake opens.
const SESSION: &str = "fake-1";

/// The words the fake says to any message.
const REPLY: &str = "This is CAF's fake agent, with no model behind it, so it says this to \
                     every message. Send exactly \"ask\" to see it ask permission to run a \
                     command, \"fail\" to see a turn fail, or \"sign in\" to see one fail for \
                     want of signing in.";

/// The words before it asks.
const ASKING: &str = "I'd like to run a command. Allow it, refuse it, or stop me.";

/// The script, as the fake reads it, saying what each turn spent if
/// `usage`.
#[must_use]
pub fn script(usage: bool) -> String {
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
        line(&json!({"on": {"method": "session/prompt", "fallback": true, "do": reply(usage)}})),
        // Anything else is refused, as an adapter refuses a method it
        // doesn't know.
        line(&json!({"on": {"fallback": true, "do": [
            {"respond": {"error": {"code": -32601, "message": "Method not found"}}},
        ]}})),
        // A chat's session, new or carried on from a saved chat's, and
        // a change to its settings.
        line(&json!({"on": {"method": "session/new", "do": [
            {"respond": {"result": {"sessionId": SESSION, "configOptions": options()}}},
        ]}})),
        line(&json!({"on": {"method": "session/resume", "do": [
            {"respond": {"result": {"configOptions": options()}}},
        ]}})),
        line(
            &json!({"on": {"method": "session/set_config_option", "do": [
                {"respond": {"result": {}}},
            ]}}),
        ),
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

/// Writes the script into `dir`, replacing any there, saying what each
/// turn spent if `usage`, and returns its path. The one that says is a file
/// of its own, `fake-agent-usage.jsonl`, never `fake-agent.jsonl`, so a
/// debugger started with `--fake-usage` never makes another's fake, started
/// without and sharing the folder, record made-up spend.
///
/// # Errors
///
/// If it can't be written.
pub fn write(dir: &Path, usage: bool) -> std::io::Result<PathBuf> {
    let name = if usage { USAGE_FILE } else { FILE };
    let path = dir.join(name);
    // Written beside it and moved into place, so a fake starting now
    // never reads half of it.
    let partial = dir.join(format!("{name}.{}", std::process::id()));
    std::fs::write(&partial, script(usage))?;
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

/// One step as a line.
fn line(step: &Value) -> String {
    step.to_string()
}

/// The settings it offers as a session opens, as ACP's config options,
/// the first of each current.
fn options() -> Value {
    json!([
        {"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "small", "options": [
            {"value": "small", "name": "Small", "description": "The fake's own: it says the same whichever model"},
            {"value": "large", "name": "Large", "description": "The fake's own, as large: it says the same"},
        ]},
        {"id": "effort", "name": "Effort", "category": "thought_level", "type": "select", "currentValue": "low", "options": [
            {"value": "low", "name": "Low"},
            {"value": "medium", "name": "Medium"},
            {"value": "high", "name": "High"},
        ]},
        {"id": "fast", "name": "Fast mode", "category": "model_config", "type": "boolean", "currentValue": false},
    ])
}

/// The answer to any message, saying what the turn spent if `usage`.
fn reply(usage: bool) -> Value {
    let mut steps = vec![
        json!({"think": {"session": SESSION, "text": "Nothing to think about: I'm the fake agent."}}),
    ];
    steps.extend(words(REPLY).map(|word| json!({"say": {"session": SESSION, "text": word}})));
    if !usage {
        steps.push(json!({"respond": {"result": {"stopReason": "end_turn"}}}));
        return Value::Array(steps);
    }
    steps.push(json!({"update": {"session": SESSION, "update": {
        "sessionUpdate": "usage_update", "used": 24_928, "size": 200_000,
        "cost": {"amount": 0.0125, "currency": "USD"},
    }}}));
    steps.push(
        json!({"respond": {"result": {"stopReason": "end_turn", "usage": {
            "inputTokens": 2, "cachedReadTokens": 12_799, "cachedWriteTokens": 12_123,
            "outputTokens": 4, "totalTokens": 24_928,
        }}}}),
    );
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

    use super::{script, write};

    #[test]
    fn the_fake_agents_script_reads() {
        for usage in [false, true] {
            Script::parse(&script(usage)).expect("the script reads");
        }
    }

    #[test]
    fn the_script_that_says_what_turns_spent_is_a_file_of_its_own() {
        let dir = std::env::temp_dir().join(format!("caf-debugger-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = write(&dir, false).unwrap();
        let spending = write(&dir, true).unwrap();
        // Written after, it leaves the plain one as it was.
        let (plain_text, spending_text) = (
            std::fs::read_to_string(&plain).unwrap(),
            std::fs::read_to_string(&spending).unwrap(),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_ne!(plain, spending);
        assert_eq!(plain.file_name().unwrap(), super::FILE);
        assert_eq!(spending.file_name().unwrap(), super::USAGE_FILE);
        assert_eq!(plain_text, script(false));
        assert_eq!(spending_text, script(true));
    }

    #[test]
    fn the_fake_says_what_a_turn_spent_only_if_asked() {
        // Made-up figures, which would count in the user's real usage.
        assert!(!script(false).contains("usage_update"));
        assert!(!script(false).contains("inputTokens"));
        assert!(script(true).contains("usage_update"));
        assert!(script(true).contains("inputTokens"));
    }
}
