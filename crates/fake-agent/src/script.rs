//! Scripts: what the fake does, step by step.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// How deep `include`s may nest.
const MOST_INCLUDES: usize = 16;

/// A script: the steps the fake takes, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    /// Its steps, with every [`Step::Include`] already replaced by the
    /// steps of the file it names.
    pub steps: Vec<Step>,
}

/// One thing the fake does. Written as one JSON object per line, named by
/// its only key, such as `{"sleep": 100}`. Steps without settings are a
/// bare string, such as `"hang"`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Waits for a request or notification from the client.
    Wait(Wait),
    /// Answers a request from the client.
    Respond(Respond),
    /// Sends the client a notification.
    Notify(Notify),
    /// Sends a `session/update` notification.
    Update(Update),
    /// Sends an `agent_message_chunk` update with this text.
    Say(Chunk),
    /// Sends an `agent_thought_chunk` update with this text.
    Think(Chunk),
    /// Sends the client a request.
    Request(Request),
    /// Waits for the client's answer to one of the fake's requests.
    Await(Await),
    /// Withdraws one of the fake's requests, with `$/cancel_request`.
    CancelRequest(CancelRequest),
    /// From now on, does some steps whenever a matching message comes.
    On(On),
    /// Sleeps for this many milliseconds, reading meanwhile.
    Sleep(u64),
    /// Writes this text and a newline to stdout, as is: for junk lines.
    Line(String),
    /// Writes any JSON value as one line, such as an answer with neither a
    /// result nor an error.
    Json(Value),
    /// Writes these bytes to stdout, exactly, with nothing added: for
    /// invalid UTF-8 or a line split across writes.
    Bytes(Vec<u8>),
    /// Writes a line of exactly so many bytes.
    LongLine(LongLine),
    /// Writes this text and a newline to stderr.
    Stderr(String),
    /// Writes this text to the record, to show how far the script got.
    Note(String),
    /// Exits with this code.
    Exit(i32),
    /// Closes stdout, and carries on.
    CloseStdout,
    /// Closes stderr, and carries on.
    CloseStderr,
    /// Waits until the client closes stdin, and carries on.
    WaitClosed,
    /// Does nothing more, ever, but run handlers: not even exit when stdin
    /// closes.
    Hang,
    /// Starts a process that holds stdout and stderr open after the fake
    /// exits, as `npx` leaves node behind.
    SpawnHolder(SpawnHolder),
    /// Puts the steps of another script file here: an absolute path.
    Include(PathBuf),
}

/// Waits for a request or notification from the client: the first one
/// that matches, among those that came already and those still to come.
/// Those that don't match are kept for later steps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wait {
    /// Its method, such as `session/prompt`. Any, if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// A pattern the whole message must match: see the crate's docs.
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub matches: Option<Value>,
    /// The name later steps call it by. Its method, if left out.
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How many milliseconds to wait before the check fails. Forever, if
    /// left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// Answers a request from the client, with a result or an error. A
/// request already answered isn't answered again, unless by `id`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Respond {
    /// The name of the request it answers. The one last waited for, if
    /// left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// An id to answer instead, as is, whether or not the client sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// The result. `null` is a result too.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<Value>,
    /// The error, instead of a result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// A JSON-RPC error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    /// Its code, such as -32000 (sign-in needed).
    pub code: i64,
    /// What it says.
    pub message: String,
    /// Anything else it carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A notification for the client.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notify {
    /// Its method.
    pub method: String,
    /// Its params, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A `session/update` notification.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    /// The session it's about.
    pub session: String,
    /// The update, as is, such as
    /// `{"sessionUpdate": "usage_update", "used": 1000, "size": 200000}`.
    pub update: Value,
}

/// Text streamed into a session, as an `agent_message_chunk` or an
/// `agent_thought_chunk`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    /// The session it's in.
    pub session: String,
    /// The text.
    pub text: String,
}

/// A request for the client, such as `session/request_permission`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Its method.
    pub method: String,
    /// Its params, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Its id, as is. The fake's own count, from 0, if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// The name later steps call it by. Its method, if left out.
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Waits for the client's answer to one of the fake's requests, and
/// checks it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Await {
    /// The name of the request. The one last sent, if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// A pattern the whole answer must match, or the check fails, such as
    /// `{"error": {"code": -32800}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
    /// How many milliseconds to wait before the check fails. Forever, if
    /// left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// Withdraws one of the fake's requests.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    /// The name of the request. The one last sent, if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// A handler: steps done whenever a matching request or notification
/// comes, while the script waits or sleeps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct On {
    /// The method it handles. Any, if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// A pattern the whole message must match.
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub matches: Option<Value>,
    /// The name its steps call the message by. Its method, if left out.
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Whether it handles only the first such message.
    #[serde(default, skip_serializing_if = "is_false")]
    pub once: bool,
    /// Its steps. None may wait: not `wait`, `await`, `wait_closed`,
    /// `hang`, `on` or `include`.
    #[serde(rename = "do")]
    pub steps: Vec<Step>,
}

/// A line of an exact length.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LongLine {
    /// Its length in bytes, not counting its newline.
    pub len: usize,
    /// A JSON object to pad with spaces to that length, so it's still a
    /// message, with its id and method near the start. `x`s, if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
}

/// A process holding the fake's stdout and stderr. It holds what they
/// are when it starts, so it must start before `close_stdout`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnHolder {
    /// A file to write its pid to: an absolute path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_file: Option<PathBuf>,
    /// How many seconds it lives. 60, if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secs: Option<u64>,
}

/// Why a script can't be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    /// Where: a file and line, if it came from one.
    pub at: String,
    /// What's wrong.
    pub message: String,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.at.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.at, self.message)
        }
    }
}

impl std::error::Error for ScriptError {}

impl Script {
    /// A script of these steps.
    #[must_use]
    pub fn new(steps: Vec<Step>) -> Self {
        Self { steps }
    }

    /// Reads a script from a file, an absolute path.
    ///
    /// # Errors
    ///
    /// If the path isn't absolute, the file can't be read, or the script
    /// is wrong.
    pub fn load(path: &Path) -> Result<Self, ScriptError> {
        load(path, 0)
    }

    /// Reads a script from its text.
    ///
    /// # Errors
    ///
    /// If the script is wrong, or a file it includes is.
    pub fn parse(text: &str) -> Result<Self, ScriptError> {
        parse(text, "", 0)
    }

    /// The script as text, one step per line, as [`Script::parse`] reads
    /// it.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for step in &self.steps {
            text.push_str(&serde_json::to_string(step).expect("steps are always JSON"));
            text.push('\n');
        }
        text
    }
}

/// Deserialises a value that's there, even `null`, as `Some`.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes a reference"
)]
fn is_false(value: &bool) -> bool {
    !value
}

fn load(path: &Path, depth: usize) -> Result<Script, ScriptError> {
    let at = path.display().to_string();
    let error = |message: String| ScriptError {
        at: at.clone(),
        message,
    };
    if !path.is_absolute() {
        return Err(error("a script's path must be absolute".into()));
    }
    let text = std::fs::read_to_string(path).map_err(|err| error(err.to_string()))?;
    parse(&text, &at, depth)
}

fn parse(text: &str, file: &str, depth: usize) -> Result<Script, ScriptError> {
    let mut steps = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = if file.is_empty() {
            format!("line {}", number + 1)
        } else {
            format!("{file}:{}", number + 1)
        };
        let error = |message: String| ScriptError {
            at: at.clone(),
            message,
        };
        let step: Step = serde_json::from_str(line).map_err(|err| error(err.to_string()))?;
        if let Step::Include(path) = &step {
            if depth >= MOST_INCLUDES {
                return Err(error(format!(
                    "includes nest more than {MOST_INCLUDES} deep"
                )));
            }
            let included = load(path, depth + 1)?;
            steps.extend(included.steps);
            continue;
        }
        check(&step, false).map_err(error)?;
        steps.push(step);
    }
    Ok(Script { steps })
}

/// Checks what serde can't: absolute paths, handlers that don't wait, and
/// lines that fit their length.
fn check(step: &Step, in_handler: bool) -> Result<(), String> {
    match step {
        Step::Wait(_) | Step::Await(_) | Step::WaitClosed | Step::Hang | Step::Include(_)
            if in_handler =>
        {
            Err("a handler's steps can't wait".into())
        }
        Step::On(_) if in_handler => Err("a handler can't add handlers".into()),
        Step::On(on) => on.steps.iter().try_for_each(|step| check(step, true)),
        Step::Respond(respond) if respond.result.is_some() == respond.error.is_some() => {
            Err("an answer has either a result or an error".into())
        }
        Step::SpawnHolder(SpawnHolder {
            pid_file: Some(path),
            ..
        }) if !path.is_absolute() => Err("a pid file's path must be absolute".into()),
        Step::LongLine(LongLine {
            len,
            message: Some(message),
        }) => {
            if !message.is_object() {
                return Err("a long line's message must be an object".into());
            }
            let short = serde_json::to_string(message)
                .expect("JSON is always JSON")
                .len();
            if short > *len {
                return Err(format!("the message is {short} bytes, longer than {len}"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Respond, Script, Step};

    #[test]
    fn steps_without_settings_are_strings_or_null() {
        let script = Script::parse("\"hang\"\n{\"close_stdout\": null}\n").unwrap();
        assert_eq!(script.steps, [Step::Hang, Step::CloseStdout]);
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let script = Script::parse("# a comment\n\n  {\"sleep\": 5}\n").unwrap();
        assert_eq!(script.steps, [Step::Sleep(5)]);
    }

    #[test]
    fn a_null_result_is_a_result() {
        let script = Script::parse(r#"{"respond": {"result": null}}"#).unwrap();
        assert_eq!(
            script.steps,
            [Step::Respond(Respond {
                result: Some(json!(null)),
                ..Respond::default()
            })]
        );
    }

    #[test]
    fn mistakes_are_refused_with_their_line() {
        let wrong = |text: &str| Script::parse(text).unwrap_err().to_string();
        assert!(wrong("\n{\"slep\": 5}").starts_with("line 2: "));
        assert!(wrong(r#"{"respond": {"reslt": {}}}"#).contains("unknown field"));
        assert!(wrong(r#"{"respond": {}}"#).contains("either a result or an error"));
        assert!(wrong(r#"{"include": "relative.jsonl"}"#).contains("absolute"));
        assert!(wrong(r#"{"spawn_holder": {"pid_file": "pid"}}"#).contains("absolute"));
        assert!(wrong(r#"{"on": {"do": ["hang"]}}"#).contains("can't wait"));
        assert!(wrong(r#"{"long_line": {"len": 2, "message": {"id": 1}}}"#).contains("longer"));
    }

    #[test]
    fn text_reads_back_as_the_same_script() {
        let script = Script::parse(
            r#"{"wait": {"method": "initialize", "as": "init"}}
{"respond": {"to": "init", "result": {"protocolVersion": 1}}}
{"on": {"method": "session/cancel", "once": true, "do": [{"note": "cancelled"}]}}
"hang""#,
        )
        .unwrap();
        assert_eq!(Script::parse(&script.to_text()).unwrap(), script);
    }
}
