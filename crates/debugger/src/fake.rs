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
//! - **Its account's limits**, only if asked (`--fake-limits`), as
//!   Claude's adapter says them: after the first words of each answer to
//!   a message, a `usage_update` with `_meta._claude/rateLimit`. Its
//!   five hours climb a few percent a turn, warning past 80%, and its
//!   week, and its week with extra usage counted in, more slowly, all in
//!   `unifiedWindows`, with Opus's own week as the limit itself, until
//!   they stop at the [last](CLIMB) turn's. Their
//!   resets are made up from when the script was written, as the
//!   debugger starts: the five hours three hours on, the weeks four days.
//!   The script can't say the time, so once the five hours' reset has
//!   passed, CAF forgets them, and the fake goes on saying a reset gone
//!   by, until the debugger starts again. A chat's fake has no memory, so
//!   each starts the climb again, and as the latest news wins, a new
//!   chat's first turn brings the limits back down. Made up too, and kept
//!   in the usage folder every CAF app shares, under the fake's profile,
//!   so off unless asked.
//! - **The message `limit`**, exactly, with `--fake-limits`, is answered
//!   at once as Claude's adapter is when the account's five hours are
//!   used up, the first time in a chat: a few words, the five hours
//!   reported refused (`rejected`), and an error, so CAF fails the turn.
//!   Sent again, it's answered as any message is, and says the limits are
//!   fine again, so a refused window can be seen to go.
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
//! - **The message `tools`**, exactly, is answered at once with a tool
//!   call of each kind it can report, as Claude's adapter reports them,
//!   then a few words: an edit with its diff and the place it's about, a
//!   command with its terminal, and a web search giving back text, a
//!   picture, a web link, a link that mustn't be followed, and a piece CAF
//!   can't read. So the debugger's drawing of each can be seen, and its
//!   end-to-end tests drive it.
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
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

/// The message that makes the fake ask permission.
pub const ASK: &str = "ask";

/// The message that makes the fake report a tool call of every kind.
pub const TOOLS: &str = "tools";

/// The message that makes the fake fail the turn, the first time.
pub const FAIL: &str = "fail";

/// The message that makes the fake want a sign-in, the first time.
pub const SIGN_IN: &str = "sign in";

/// The message that makes the fake's limits refuse the turn, the first
/// time, with `--fake-limits`.
pub const LIMIT: &str = "limit";

/// How many turns the fake's limits climb for, in one chat: after that,
/// every turn says the last's.
pub const CLIMB: u32 = 6;

/// How many times `ask` can be sent to one fake: more are answered as
/// any message is.
const ROUNDS: u32 = 200;

/// The script's file.
pub const FILE: &str = "fake-agent.jsonl";

/// The script's file when it says what each turn spent.
pub const USAGE_FILE: &str = "fake-agent-usage.jsonl";

/// The script's file when it says its account's limits.
pub const LIMITS_FILE: &str = "fake-agent-limits.jsonl";

/// The script's file when it says both.
pub const USAGE_LIMITS_FILE: &str = "fake-agent-usage-limits.jsonl";

/// The session the fake opens.
const SESSION: &str = "fake-1";

/// The words the fake says to any message.
const REPLY: &str = "This is CAF's fake agent, with no model behind it, so it says this to \
                     every message. Send exactly \"ask\" to see it ask permission to run a \
                     command, \"tools\" to see a tool call of every kind, \"fail\" to see a \
                     turn fail, or \"sign in\" to see one fail for want of signing in.";

/// The words it adds when it says its limits.
const REPLY_LIMITS: &str = " Send \"limit\" to see one refused for its account's limits.";

/// The words it says as its limits refuse a turn.
const REFUSED: &str = "You've hit your limit.";

/// The words before it asks.
const ASKING: &str = "I'd like to run a command. Allow it, refuse it, or stop me.";

/// The script, as the fake reads it, saying what each turn spent if
/// `usage`, and its account's limits if given `limits`, the time their
/// resets are made up from.
#[must_use]
pub fn script(usage: bool, limits: Option<SystemTime>) -> String {
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
        line(&json!({"on": {"method": "session/prompt", "match": prompt(TOOLS), "do": tools()}})),
        line(
            &json!({"on": {"method": "session/prompt", "match": prompt(SIGN_IN), "once": true, "do": [
                {"respond": {"error": {"code": -32000, "message": "Authentication required: the fake agent wants a sign-in, as it was asked to."}}},
            ]}}),
        ),
    ];
    if let Some(now) = limits {
        lines.push(line(
            &json!({"on": {"method": "session/prompt", "match": prompt(LIMIT), "once": true, "do": refused(now)}}),
        ));
        // A handler for each turn the limits climb for, each taking one
        // message, then the last's for every message after.
        for turn in 0..CLIMB - 1 {
            let report = report(now, turn);
            lines.push(line(
                &json!({"on": {"method": "session/prompt", "fallback": true, "once": true, "do": reply(usage, Some(&report))}}),
            ));
        }
    }
    let last = limits.map(|now| report(now, CLIMB - 1));
    lines.extend([
        line(
            &json!({"on": {"method": "session/prompt", "fallback": true, "do": reply(usage, last.as_ref())}}),
        ),
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
    ]);
    for round in 1..=ROUNDS {
        lines.extend(asking(round).iter().map(line));
    }
    lines.join("\n") + "\n"
}

/// Writes the script into `dir`, replacing any there, saying what each
/// turn spent if `usage`, and its account's limits if given `limits`, and
/// returns its path. One that says either is a file of its own,
/// `fake-agent-usage.jsonl`, `fake-agent-limits.jsonl` or
/// `fake-agent-usage-limits.jsonl`, never `fake-agent.jsonl`, so a
/// debugger started with `--fake-usage` or `--fake-limits` never makes
/// another's fake, started without and sharing the folder, record made-up
/// spend or limits.
///
/// # Errors
///
/// If it can't be written.
pub fn write(dir: &Path, usage: bool, limits: Option<SystemTime>) -> std::io::Result<PathBuf> {
    let name = match (usage, limits.is_some()) {
        (false, false) => FILE,
        (true, false) => USAGE_FILE,
        (false, true) => LIMITS_FILE,
        (true, true) => USAGE_LIMITS_FILE,
    };
    let path = dir.join(name);
    // Written beside it and moved into place, so a fake starting now
    // never reads half of it.
    let partial = dir.join(format!("{name}.{}", std::process::id()));
    std::fs::write(&partial, script(usage, limits))?;
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

/// The answer to any message, saying what the turn spent if `usage`, and
/// `limits`, Claude's `rate_limit_info`, if given, after its first word.
fn reply(usage: bool, limits: Option<&Value>) -> Value {
    let mut steps = vec![
        json!({"think": {"session": SESSION, "text": "Nothing to think about: I'm the fake agent."}}),
    ];
    let text = match limits {
        Some(_) => format!("{REPLY}{REPLY_LIMITS}"),
        None => REPLY.to_owned(),
    };
    for (i, word) in words(&text).enumerate() {
        steps.push(json!({"say": {"session": SESSION, "text": word}}));
        // Claude's adapter passes its limits on only once the turn has
        // had some of the reply.
        if let (0, Some(limits)) = (i, limits) {
            steps.push(said_limits(limits));
        }
    }
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

/// The answer to `limit`: a few words, the five hours used up, and the
/// turn refused, as Claude's adapter answers when they are.
fn refused(now: SystemTime) -> Value {
    let mut steps: Vec<_> = (words(REFUSED))
        .map(|word| json!({"say": {"session": SESSION, "text": word}}))
        .collect();
    let five_hours = window(now, FIVE_HOURS_LEFT, 1.0, "rejected");
    steps.push(said_limits(&json!({
        "status": "rejected",
        "resetsAt": five_hours["resetsAt"],
        "rateLimitType": "five_hour",
        "utilization": 1.0,
        "isUsingOverage": false,
        // Only the five hours: the rest are as the last turn said.
        "unifiedWindows": {"five_hour": five_hours},
    })));
    steps.push(json!({"respond": {"error": {
        "code": -32603,
        "message": "The fake agent's five hours are used up, as it was asked. Send the message again, and they aren't.",
        "data": {"errorKind": "rate_limit"},
    }}}));
    Value::Array(steps)
}

/// How long until the fake's five hours reset, from when its script was
/// written: two of the five gone.
const FIVE_HOURS_LEFT: Duration = Duration::from_secs(3 * 60 * 60);

/// How long until its weeks reset: three days of seven gone.
const WEEK_LEFT: Duration = Duration::from_secs(4 * 24 * 60 * 60);

/// Its limits on its `turn`th answer in a chat, counting from 0, as
/// Claude's `rate_limit_info`: the five hours, the week and the week with
/// extra usage counted in, in `unifiedWindows`, and Opus's own week as the
/// limit itself.
///
/// The limit itself has its own `status`, not the worst window's: CAF
/// reads it as that window's state, as Claude's is the status of the one
/// its `rateLimitType` names, so a warning on the five hours would show on
/// Opus's week.
fn report(now: SystemTime, turn: u32) -> Value {
    let five_hours = 0.60 + 0.07 * f64::from(turn);
    let status = if five_hours >= 0.80 {
        "allowed_warning"
    } else {
        "allowed"
    };
    let week = 0.30 + 0.02 * f64::from(turn);
    let opus = window(now, WEEK_LEFT, 0.20 + 0.01 * f64::from(turn), "allowed");
    json!({
        "status": "allowed",
        "resetsAt": opus["resetsAt"],
        "rateLimitType": "seven_day_opus",
        "utilization": opus["utilization"],
        "isUsingOverage": false,
        "unifiedWindows": {
            "five_hour": window(now, FIVE_HOURS_LEFT, five_hours, status),
            "seven_day": window(now, WEEK_LEFT, week, "allowed"),
            "seven_day_overage_included": window(now, WEEK_LEFT, week + 0.04, "allowed"),
        },
    })
}

/// One of `unifiedWindows`: `used`, a share of 1, resetting `left` after
/// `now`, in seconds since the epoch, as Claude says it.
fn window(now: SystemTime, left: Duration, used: f64, status: &str) -> Value {
    let resets = (now + left)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Two places, as a sum of tenths isn't exact.
    let used = (used * 100.0).round() / 100.0;
    json!({"utilization": used, "resetsAt": resets, "status": status})
}

/// A `usage_update` saying `limits`, as Claude's adapter sends them, with
/// the context it last heard.
fn said_limits(limits: &Value) -> Value {
    json!({"update": {"session": SESSION, "update": {
        "sessionUpdate": "usage_update", "used": 24_928, "size": 200_000,
        "_meta": {"_claude/rateLimit": limits},
    }}})
}

/// A picture of one red pixel, as a PNG, in base64.
const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// The answer to `tools`: a call of each kind, as Claude's adapter reports
/// them, then a few words. The edit is reported, then finished with its
/// diff, as an adapter does; the others come whole.
fn tools() -> Value {
    let mut steps = vec![
        json!({"tool": {"session": SESSION, "id": "edit", "title": "Edit src/lib.rs", "kind": "edit", "status": "pending",
            "tool_name": "Edit", "raw_input": {"file_path": "/work/src/lib.rs"},
            "locations": [{"path": "/work/src/lib.rs", "line": 2}]}}),
        json!({"tool": {"session": SESSION, "id": "edit", "update": true, "status": "completed", "content": [
            {"diff": {"path": "/work/src/lib.rs", "old": "fn main() {\n    old();\n}\n", "new": "fn main() {\n    new();\n    more();\n}\n"}},
        ]}}),
        json!({"tool": {"session": SESSION, "id": "run", "title": "cargo test", "kind": "execute", "status": "completed",
            "tool_name": "Bash", "raw_input": {"command": "cargo test"}, "content": [{"terminal": "term-1"}]}}),
        json!({"tool": {"session": SESSION, "id": "search", "title": "Search the web for CAF", "kind": "fetch", "status": "completed",
        "tool_name": "WebSearch", "raw_input": {"query": "CAF"}, "content": [
            {"text": "Two hits."},
            {"image": {"mime": "image/png", "data": PIXEL}},
            {"link": {"uri": "https://example.com/caf", "name": "CAF on the web"}},
            {"link": {"uri": "javascript:alert(1)", "name": "Not a link"}},
            {"raw": {"type": "hologram"}},
        ]}}),
    ];
    steps.extend(
        words("I used a tool of every kind.")
            .map(|word| json!({"say": {"session": SESSION, "text": word}})),
    );
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
    use std::time::{Duration, SystemTime};

    use caf_fake_agent::Script;
    use serde_json::Value;

    use super::{CLIMB, LIMIT, script, write};

    /// When a test's limits are made up from: 2026-11-28T12:00:00Z.
    const NOW: u64 = 1_795_867_200;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW)
    }

    /// Every way the script can be written.
    fn every() -> [(bool, Option<SystemTime>); 4] {
        [
            (false, None),
            (true, None),
            (false, Some(now())),
            (true, Some(now())),
        ]
    }

    /// The script's steps, as JSON.
    fn steps(text: &str) -> Vec<Value> {
        (text.lines())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The handlers answering any message, in the order they take one,
    /// each with whether it takes only one.
    fn answers(text: &str) -> Vec<(bool, Vec<Value>)> {
        (steps(text).into_iter())
            .filter_map(|step| step.get("on").cloned())
            .filter(|on| on["method"] == "session/prompt" && on["fallback"] == true)
            .map(|on| (on["once"] == true, on["do"].as_array().unwrap().clone()))
            .collect()
    }

    /// The limits a handler's steps say, Claude's `rate_limit_info`.
    fn limits(steps: &[Value]) -> Vec<&Value> {
        (steps.iter())
            .filter_map(|step| step.pointer("/update/update/_meta/_claude~1rateLimit"))
            .collect()
    }

    fn share(limits: &Value, window: &str) -> f64 {
        limits["unifiedWindows"][window]["utilization"]
            .as_f64()
            .unwrap()
    }

    #[test]
    fn the_message_tools_reports_a_call_of_every_kind_and_ends_the_turn() {
        for (usage, limits) in every() {
            let handler = (steps(&script(usage, limits)).into_iter())
                .filter_map(|step| step.get("on").cloned())
                .find(|on| on["match"] == super::prompt(super::TOOLS))
                .expect("a handler for tools");
            let done = handler["do"].as_array().unwrap();
            let content: Vec<String> = (done.iter())
                .filter_map(|step| step["tool"]["content"].as_array())
                .flatten()
                .map(|piece| piece.as_object().unwrap().keys().next().unwrap().clone())
                .collect();
            for kind in ["diff", "terminal", "text", "image", "link", "raw"] {
                assert!(content.iter().any(|c| c == kind), "no {kind}");
            }
            assert_eq!(
                done.last().unwrap()["respond"]["result"]["stopReason"],
                "end_turn"
            );
        }
    }

    #[test]
    fn the_fake_agents_script_reads() {
        for (usage, limits) in every() {
            Script::parse(&script(usage, limits)).expect("the script reads");
        }
    }

    #[test]
    fn the_script_that_says_what_turns_spent_or_limits_is_a_file_of_its_own() {
        let dir = std::env::temp_dir().join(format!("caf-debugger-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<_> = (every().into_iter())
            .map(|(usage, limits)| write(&dir, usage, limits).unwrap())
            .collect();
        // Each written after the one before, it leaves the others as they
        // were.
        let texts: Vec<_> = (paths.iter())
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect();
        std::fs::remove_dir_all(&dir).unwrap();
        let names: Vec<_> = (paths.iter())
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                super::FILE,
                super::USAGE_FILE,
                super::LIMITS_FILE,
                super::USAGE_LIMITS_FILE
            ]
        );
        for ((usage, limits), text) in every().into_iter().zip(texts) {
            assert_eq!(text, script(usage, limits));
        }
    }

    #[test]
    fn the_fake_says_what_a_turn_spent_only_if_asked() {
        // Made-up figures, which would count in the user's real usage.
        for limits in [None, Some(now())] {
            assert!(!script(false, limits).contains("inputTokens"));
            assert!(!script(false, limits).contains("\"cost\""));
            assert!(script(true, limits).contains("inputTokens"));
            assert!(script(true, limits).contains("\"cost\""));
        }
        assert!(!script(false, None).contains("usage_update"));
    }

    #[test]
    fn the_fake_says_its_limits_only_if_asked() {
        // Made up, and kept where every CAF app reads them.
        for usage in [false, true] {
            assert!(!script(usage, None).contains("rateLimit"));
            assert!(!script(usage, None).contains(&format!("\"{LIMIT}\"")));
            assert!(script(usage, Some(now())).contains("_claude/rateLimit"));
        }
    }

    #[test]
    fn the_fakes_limits_climb_turn_by_turn_then_stay() {
        let text = script(false, Some(now()));
        let answers = answers(&text);
        // One for each turn the limits climb, the last for every turn
        // after.
        assert_eq!(answers.len(), CLIMB as usize);
        let once: Vec<_> = answers.iter().map(|(once, _)| *once).collect();
        assert_eq!(
            once.iter().filter(|once| **once).count(),
            CLIMB as usize - 1
        );
        assert!(!once.last().unwrap());
        let said: Vec<_> = (answers.iter())
            .map(|(_, steps)| {
                let said = limits(steps);
                assert_eq!(said.len(), 1, "one report a turn");
                said[0].clone()
            })
            .collect();
        let five_hours: Vec<_> = said.iter().map(|each| share(each, "five_hour")).collect();
        let week: Vec<_> = said.iter().map(|each| share(each, "seven_day")).collect();
        let extra: Vec<_> = (said.iter())
            .map(|each| share(each, "seven_day_overage_included"))
            .collect();
        for turn in 1..said.len() {
            let climbed = five_hours[turn] - five_hours[turn - 1];
            assert!(
                (0.01..=0.10).contains(&climbed),
                "a few percent: {five_hours:?}"
            );
            assert!(
                week[turn] - week[turn - 1] < climbed,
                "the week climbs slower"
            );
            assert!(week[turn] > week[turn - 1]);
            assert!(extra[turn] > extra[turn - 1]);
        }
        // Extra usage counted in, the week is never less used.
        assert!(week.iter().zip(&extra).all(|(week, extra)| extra >= week));
        // From fine, through low, to out: under 10% left at the last.
        assert!(five_hours[0] < 0.8 && *five_hours.last().unwrap() > 0.9);
        for each in &said {
            let warned = each["unifiedWindows"]["five_hour"]["status"] == "allowed_warning";
            assert_eq!(warned, share(each, "five_hour") >= 0.8, "{each}");
            assert_eq!(each["unifiedWindows"]["seven_day"]["status"], "allowed");
            let extra = &each["unifiedWindows"]["seven_day_overage_included"];
            assert_eq!(extra["status"], "allowed");
            assert_eq!(extra["resetsAt"], NOW + 4 * 86_400);
            // Opus's week is the limit itself.
            assert_eq!(each["rateLimitType"], "seven_day_opus");
            assert!(each["utilization"].as_f64().unwrap() > 0.0);
            assert_eq!(each["status"], "allowed");
            assert_eq!(each["isUsingOverage"], false);
            // In seconds: the five hours in three hours, the weeks in four
            // days.
            assert_eq!(
                each["unifiedWindows"]["five_hour"]["resetsAt"],
                NOW + 3 * 3600
            );
            assert_eq!(
                each["unifiedWindows"]["seven_day"]["resetsAt"],
                NOW + 4 * 86_400
            );
            assert_eq!(each["resetsAt"], NOW + 4 * 86_400);
        }
    }

    #[test]
    fn the_fakes_limits_come_after_the_first_words_of_its_answer() {
        // As Claude's adapter passes them on only then.
        for (_, steps) in answers(&script(true, Some(now()))) {
            let kinds: Vec<_> = (steps.iter())
                .map(|step| {
                    if step.pointer("/update/update/_meta").is_some() {
                        "limits"
                    } else {
                        step.as_object().unwrap().keys().next().unwrap().as_str()
                    }
                })
                .collect();
            assert_eq!(kinds[..3], ["think", "say", "limits"], "{kinds:?}");
            assert!(kinds[3..].contains(&"say"));
            assert_eq!(kinds.iter().filter(|kind| **kind == "limits").count(), 1);
        }
    }

    #[test]
    fn the_message_limit_is_refused_for_its_limits_once() {
        let text = script(false, Some(now()));
        let handler = (steps(&text).into_iter())
            .filter_map(|step| step.get("on").cloned())
            .find(|on| on.pointer("/match/params/prompt/0/text") == Some(&LIMIT.into()))
            .expect("a handler for `limit`");
        assert_eq!(handler["once"], true);
        assert_ne!(handler["fallback"], true);
        let steps = handler["do"].as_array().unwrap();
        let said = limits(steps);
        assert_eq!(said.len(), 1);
        // Only the five hours: the week said again as the first turn's
        // would go backwards after a few turns.
        let windows: Vec<_> = said[0]["unifiedWindows"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(windows, ["five_hour"]);
        let five_hours = &said[0]["unifiedWindows"]["five_hour"];
        assert_eq!(five_hours["status"], "rejected");
        assert_eq!(five_hours["utilization"], 1.0);
        assert_eq!(said[0]["status"], "rejected");
        assert_eq!(said[0]["rateLimitType"], "five_hour");
        // After some words, and the turn fails, as Claude's does.
        assert!(steps[0].get("say").is_some());
        let error = &steps.last().unwrap()["respond"]["error"];
        assert_eq!(error["data"]["errorKind"], "rate_limit");
    }
}
