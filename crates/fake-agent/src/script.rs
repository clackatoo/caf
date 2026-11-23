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
    /// Its steps. A script read by [`Script::load`] or [`Script::parse`]
    /// has every [`Step::Include`] replaced by the steps of the file it
    /// names, in its handlers too; [`run`](crate::run) refuses one that
    /// still has any.
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
    /// Sends a `tool_call` or `tool_call_update` update, as Claude's
    /// adapter reports a tool call.
    Tool(Tool),
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
    /// Writes the values of these environment variables to the record,
    /// `null` for one that isn't set.
    RecordEnv(Vec<String>),
    /// Exits with this code.
    Exit(i32),
    /// Sends the fake this signal, such as 9 (`SIGKILL`) or 15
    /// (`SIGTERM`), to die by it. One that doesn't kill it lets it carry
    /// on. Tests should prefer those two: 6 (`SIGABRT`) may write a core
    /// dump.
    Signal(i32),
    /// Stops reading stdin, so the pipe fills and the client's writes
    /// block. What it read already still counts, and so may the line it's
    /// reading, and up to 8 KiB after it. Stdin closing isn't noticed
    /// until it reads again.
    StopReading,
    /// Reads stdin again.
    ResumeReading,
    /// Closes stdout, and carries on.
    CloseStdout,
    /// Closes stderr, and carries on.
    CloseStderr,
    /// Waits until the client closes stdin, and carries on.
    WaitClosed,
    /// Waits until a file is there, reading meanwhile: so a test can say
    /// when the script goes on, even while the fake isn't reading stdin.
    WaitFile(WaitFile),
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
    /// A pattern the message, once taken, must match, or the check
    /// fails: so a wrong message fails at once rather than leaving the
    /// fake waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
    /// The name later steps call it by. Its method, if left out.
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How many milliseconds to wait before the check fails. Forever, if
    /// left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// Waits until a file is there.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitFile {
    /// The file: an absolute path.
    pub path: PathBuf,
    /// How many milliseconds to wait before the check fails. Forever, if
    /// left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// Answers a request from the client, with a result or an error. A
/// request already answered isn't answered again, unless by `id`.
///
/// It answers the message named `to`, or the one last waited for, unless
/// `id_from` says to answer the request whose id that message holds.
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
    /// Where in the message the id to answer is, as a JSON pointer, such
    /// as `/params/requestId` in a `$/cancel_request`, to answer the
    /// request it cancels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_from: Option<String>,
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

/// A tool call reported, as a `tool_call` update, or a change to one, as a
/// `tool_call_update`: only what's given is sent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    /// The session it's in.
    pub session: String,
    /// The call's id, `toolCallId`.
    pub id: String,
    /// Whether it's a change to a call reported already: a
    /// `tool_call_update`, else a `tool_call`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub update: bool,
    /// Its title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Its kind, as ACP says it, such as `read` or `execute`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Its status, as ACP says it: `pending`, `in_progress`, `completed`
    /// or `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The tool's name, as Claude's adapter gives it in
    /// `_meta.claudeCode.toolName`: `WebSearch` for one of the agent's
    /// own, `mcp__eyespy__look` for one of an MCP server's,
    /// `mcp__caf__Read` for one of CAF's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// What it gave back, `content`, in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolContent>>,
    /// The files it's about, `locations`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<ToolLocation>>,
    /// What it was given, `rawInput`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
    /// What it gave back, raw, `rawOutput`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<Value>,
}

/// One piece of what a tool call gave back, as ACP's `ToolCallContent`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolContent {
    /// Text.
    Text(String),
    /// A file's change: `old` left out for a new file.
    Diff(ToolDiff),
    /// A terminal, by its id.
    Terminal(String),
    /// A picture: its type, and its bytes in base64.
    Image(ToolImage),
    /// A file or web address, `resource_link`.
    Link(ToolLink),
    /// Any content, as is: for kinds CAF can't read.
    Raw(Value),
}

/// A file's change, in [`ToolContent::Diff`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDiff {
    /// The file, as an absolute path.
    pub path: String,
    /// What it held before, if there was a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// What it holds after.
    pub new: String,
}

/// A picture, in [`ToolContent::Image`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolImage {
    /// Its type, such as `image/png`.
    pub mime: String,
    /// Its bytes, in base64.
    pub data: String,
}

/// A file or web address, in [`ToolContent::Link`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolLink {
    /// The address.
    pub uri: String,
    /// What it's called.
    pub name: String,
}

/// A file a tool call is about, and the line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolLocation {
    /// The file, as an absolute path.
    pub path: String,
    /// The line, numbered from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
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
/// comes, while the script waits, sleeps or is over.
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
    /// Whether it takes only what the rest of the script never waits for,
    /// such as requests a real adapter would refuse. Other handlers come
    /// first.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fallback: bool,
    /// Its steps. None may wait or sleep: not `wait`, `await`, `sleep`,
    /// `wait_closed`, `wait_file`, `hang`, `spawn_holder` (which waits for
    /// its pid file) or `on`. An `include` is replaced by its file's
    /// steps.
    #[serde(rename = "do")]
    pub steps: Vec<Step>,
}

/// A line of an exact length.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LongLine {
    /// Its length in bytes, not counting its newline.
    pub len: usize,
    /// Where it goes: stdout, if left out.
    #[serde(default, skip_serializing_if = "Output::is_stdout")]
    pub to: Output,
    /// A JSON object to pad with spaces to that length, so it's still a
    /// message, with its id and method near the start. `x`s, if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
}

/// Where the fake writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// Stdout.
    #[default]
    Stdout,
    /// Stderr.
    Stderr,
}

impl Output {
    fn is_stdout(&self) -> bool {
        *self == Self::Stdout
    }
}

/// A process holding the fake's stdout and stderr. It holds what they
/// are when it starts, so it must start before `close_stdout`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnHolder {
    /// A file for its pid: an absolute path. The holder writes it, and
    /// goes as soon as the file does, so a test that deletes its folder
    /// leaves nothing behind. Without one, it lives its `secs` whatever
    /// happens, so tests should give one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_file: Option<PathBuf>,
    /// How many seconds it lives at most. 60, if left out.
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
    /// A script of these steps. They're checked when it runs, or by
    /// [`Script::check`].
    #[must_use]
    pub fn new(steps: Vec<Step>) -> Self {
        Self { steps }
    }

    /// Checks what [`Script::load`] and [`Script::parse`] check: paths
    /// absolute, handlers that don't wait, lines that fit their length,
    /// pointers that are pointers, and no `include` left.
    ///
    /// # Errors
    ///
    /// If a step is wrong, saying which.
    pub fn check(&self) -> Result<(), ScriptError> {
        self.steps.iter().enumerate().try_for_each(|(at, step)| {
            let wrong = |message: String| ScriptError {
                at: format!("step {}", at + 1),
                message,
            };
            check(step, false).map_err(wrong)
        })
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
        let mut step: Step = serde_json::from_str(line).map_err(|err| error(err.to_string()))?;
        if let Step::Include(path) = &step {
            steps.extend(include(path, depth).map_err(error)?);
            continue;
        }
        if let Step::On(on) = &mut step {
            let mut expanded = Vec::new();
            for step in on.steps.drain(..) {
                match step {
                    Step::Include(path) => expanded.extend(include(&path, depth).map_err(error)?),
                    step => expanded.push(step),
                }
            }
            on.steps = expanded;
        }
        check(&step, false).map_err(error)?;
        steps.push(step);
    }
    Ok(Script { steps })
}

/// The steps of an included file.
fn include(path: &Path, depth: usize) -> Result<Vec<Step>, String> {
    if depth >= MOST_INCLUDES {
        return Err(format!("includes nest more than {MOST_INCLUDES} deep"));
    }
    load(path, depth + 1)
        .map(|script| script.steps)
        .map_err(|err| err.to_string())
}

/// Checks what serde can't: absolute paths, handlers that don't wait, and
/// lines that fit their length.
fn check(step: &Step, in_handler: bool) -> Result<(), String> {
    match step {
        // Reading a script replaces these, so only one built in Rust has
        // any.
        Step::Include(path) => Err(format!("{} isn't included", path.display())),
        Step::Wait(_)
        | Step::Await(_)
        | Step::Sleep(_)
        | Step::WaitClosed
        | Step::WaitFile(_)
        | Step::Hang
        | Step::SpawnHolder(_)
            if in_handler =>
        {
            Err("a handler's steps can't wait or sleep".into())
        }
        Step::WaitFile(WaitFile { path, .. }) if !path.is_absolute() => {
            Err("a file to wait for must be an absolute path".into())
        }
        Step::On(_) if in_handler => Err("a handler can't add handlers".into()),
        Step::On(on) => on.steps.iter().try_for_each(|step| check(step, true)),
        Step::Respond(respond) if respond.result.is_some() == respond.error.is_some() => {
            Err("an answer has either a result or an error".into())
        }
        Step::Respond(respond) if respond.id.is_some() && respond.id_from.is_some() => {
            Err("an answer has an id or says where to find one, not both".into())
        }
        Step::Respond(Respond {
            id_from: Some(pointer),
            ..
        }) if !pointer.starts_with('/') => Err(format!(
            "{pointer} isn't a JSON pointer: it starts with /, as /params/requestId"
        )),
        Step::SpawnHolder(SpawnHolder {
            pid_file: Some(path),
            ..
        }) if !path.is_absolute() => Err("a pid file's path must be absolute".into()),
        Step::LongLine(LongLine {
            len,
            message: Some(message),
            ..
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

    use super::{LongLine, On, Respond, Script, Step, Tool, ToolContent, ToolDiff, ToolLocation};

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
    fn a_tool_call_reads_and_writes_back_the_same() {
        let text = r#"{"tool": {"session": "s1", "id": "t1", "update": true, "tool_name": "mcp__caf__Read", "content": [{"diff": {"path": "/a", "new": "x"}}, {"raw": {"type": "hologram"}}], "locations": [{"path": "/a", "line": 3}]}}"#;
        let script = Script::parse(text).unwrap();
        assert_eq!(
            script.steps,
            [Step::Tool(Tool {
                session: "s1".into(),
                id: "t1".into(),
                update: true,
                tool_name: Some("mcp__caf__Read".into()),
                content: Some(vec![
                    ToolContent::Diff(ToolDiff {
                        path: "/a".into(),
                        old: None,
                        new: "x".into(),
                    }),
                    ToolContent::Raw(json!({ "type": "hologram" })),
                ]),
                locations: Some(vec![ToolLocation {
                    path: "/a".into(),
                    line: Some(3),
                }]),
                ..Tool::default()
            })]
        );
        assert_eq!(Script::parse(&script.to_text()), Ok(script));
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
        assert!(wrong(r#"{"on": {"do": [{"sleep": 5}]}}"#).contains("can't wait or sleep"));
        assert!(wrong(r#"{"on": {"do": [{"spawn_holder": {}}]}}"#).contains("can't wait"));
        assert!(wrong(r#"{"wait_file": {"path": "go"}}"#).contains("absolute"));
        assert!(
            wrong(r#"{"respond": {"id_from": "params/requestId", "result": 1}}"#)
                .contains("isn't a JSON pointer")
        );
        assert!(
            wrong(r#"{"respond": {"id": 1, "id_from": "/id", "result": 1}}"#).contains("not both")
        );
        assert!(wrong(r#"{"long_line": {"len": 2, "message": {"id": 1}}}"#).contains("longer"));
    }

    #[test]
    fn scripts_built_in_rust_are_checked_too() {
        let included = Script::new(vec![Step::Sleep(1), Step::Include("/x.jsonl".into())]);
        assert_eq!(
            included.check().unwrap_err().to_string(),
            "step 2: /x.jsonl isn't included"
        );
        let short = Script::new(vec![Step::LongLine(LongLine {
            len: 3,
            message: Some(json!({"id": 1})),
            ..LongLine::default()
        })]);
        assert!(short.check().unwrap_err().message.contains("longer than 3"));
        let in_handler = Script::new(vec![Step::On(On {
            steps: vec![Step::Include("/y.jsonl".into())],
            ..On::default()
        })]);
        assert_eq!(
            in_handler.check().unwrap_err().to_string(),
            "step 1: /y.jsonl isn't included"
        );
        assert_eq!(Script::new(vec![Step::Hang]).check(), Ok(()));
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
