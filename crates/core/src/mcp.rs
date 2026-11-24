//! Serving CAF's tools over MCP: the protocol, with no I/O.
//!
//! The agent starts CAF's relay as its MCP server `caf`, and the relay
//! connects to the tools driver's socket, says which chat it's for and
//! proves it with that chat's [`token`], then carries MCP's lines both
//! ways. The tools driver runs a [`Connection`] for each: it hands it each
//! line the agent sent, and does what comes [`Out`].
//!
//! # The protocol
//!
//! MCP over stdio: JSON-RPC 2.0, a message a line. CAF's server only
//! answers; it never asks the agent anything.
//! - **`initialize`** takes MCP's versions 2024-11-05, 2025-03-26,
//!   2025-06-18 and 2025-11-25, and answers with the agent's, or the
//!   newest if it asked for another. It offers tools only
//!   (`{"tools": {}}`), as server `caf`, with the connection's
//!   instructions.
//! - **`notifications/initialized`** is taken, and **`ping`** answered
//!   `{}`.
//! - **`tools/list`** gives the chat's tools, on one page ([`tool_list`]).
//! - **`tools/call`** is a call ([`Out::Call`]). One to a tool CAF doesn't
//!   have, or didn't give this chat, is answered at once as an error, and
//!   never reaches the driver; one naming no tool is answered -32602.
//! - **`notifications/cancelled`** for an open call ends it
//!   ([`Out::Cancelled`]): it's never answered after.
//! - **Any other request**, `server/discover` included, is answered at
//!   once that there's no such method (-32601). Other notifications, and
//!   answers, as CAF asks nothing, are ignored.
//! - **What isn't JSON** is answered -32700, and a batch, or anything else
//!   that isn't one request, -32600.
//! - **A line longer than [`MOST_LINE`]**, or a call reusing the id of
//!   one still open, closes the connection ([`Out::Close`]).
//! - **A call is answered at most once**, and never once it's cancelled.
//!
//! A call that comes before `initialize` is served all the same: the
//! relay belongs to the chat its token says, whatever order the agent
//! speaks in.

use std::collections::BTreeMap;
use std::fmt;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use sha2::Sha256;

use crate::ids::ChatId;
use crate::tool::CafTool;

/// The longest line a connection takes from the agent, in bytes: one
/// longer closes it ([`Out::Close`]). The driver enforces it while
/// reading, so it never holds more.
pub const MOST_LINE: usize = 4 << 20;

/// The versions of MCP CAF speaks, oldest first.
const VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// JSON-RPC's errors CAF answers with.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const NO_METHOD: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// One relay's MCP connection, from its handshake on: a state machine.
#[derive(Debug)]
pub struct Connection {
    /// The tools it serves.
    tools: Vec<CafTool>,
    /// What it tells the agent as it starts.
    instructions: String,
    /// The calls not yet answered or cancelled.
    open: BTreeMap<Request, Open>,
}

/// A call not yet answered or cancelled.
#[derive(Debug)]
struct Open {
    /// The agent's token for progress on it, if it gave one.
    token: Option<Value>,
    /// How many progress notifications it has had.
    progress: u64,
}

impl Connection {
    /// A connection serving `tools` with `instructions` (one line).
    #[must_use]
    pub fn new(tools: &[CafTool], instructions: &str) -> Self {
        Self {
            tools: tools.to_vec(),
            instructions: instructions.to_owned(),
            open: BTreeMap::new(),
        }
    }

    /// One line the agent sent (without its newline). What to do about it.
    pub fn take(&mut self, line: &[u8]) -> Vec<Out> {
        if line.len() > MOST_LINE {
            return vec![Out::Close(format!(
                "a line of {} bytes, longer than {MOST_LINE}",
                line.len()
            ))];
        }
        let message: Value = match serde_json::from_slice(line) {
            Ok(message) => message,
            Err(error) => {
                return vec![error_line(
                    None,
                    PARSE_ERROR,
                    &format!("Parse error: {error}"),
                )];
            }
        };
        let Value::Object(message) = message else {
            let why = if message.is_array() {
                "Batches aren't supported"
            } else {
                "Invalid request"
            };
            return vec![error_line(None, INVALID_REQUEST, why)];
        };
        let id = match message.get("id") {
            None => None,
            Some(id @ (Value::Number(_) | Value::String(_))) => Some(Request(id.to_string())),
            Some(_) => return vec![error_line(None, INVALID_REQUEST, "Invalid request id")],
        };
        let Some(method) = message.get("method") else {
            // An answer, to nothing CAF asked.
            return Vec::new();
        };
        let Some(method) = method.as_str() else {
            return vec![error_line(id.as_ref(), INVALID_REQUEST, "Invalid method")];
        };
        let params = message.get("params").unwrap_or(&Value::Null);
        let Some(id) = id else {
            return self.notified(method, params);
        };
        match method {
            "initialize" => vec![result_line(&id, &self.initialized(params))],
            "ping" => vec![result_line(&id, &json!({}))],
            "tools/list" => vec![result_line(
                &id,
                &json!({ "tools": tool_list(&self.tools) }),
            )],
            "tools/call" => self.called(id, params),
            _ => vec![error_line(
                Some(&id),
                NO_METHOD,
                &format!("Method not found: {}", crate::core::agent_text(method, 256)),
            )],
        }
    }

    /// Answers a call. `None` if it was already answered, or cancelled.
    pub fn answer(&mut self, request: &Request, answer: &Answer) -> Option<Vec<u8>> {
        self.open.remove(request)?;
        Some(answer_bytes(request, answer))
    }

    /// A progress notification for an open call, if the agent gave a
    /// token. `None` otherwise, or once the call has ended.
    pub fn progress(&mut self, request: &Request, message: &str) -> Option<Vec<u8>> {
        let open = self.open.get_mut(request)?;
        let token = open.token.clone()?;
        open.progress += 1;
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": { "progressToken": token, "progress": open.progress, "message": message },
        });
        Some(line(&notification))
    }

    /// The calls still open, for ending them all when the connection drops.
    #[must_use]
    pub fn open(&self) -> Vec<Request> {
        self.open.keys().cloned().collect()
    }

    /// The answer to `initialize`, given `params`.
    fn initialized(&self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = (VERSIONS.iter())
            .find(|version| Some(**version) == asked)
            .unwrap_or(&VERSIONS[VERSIONS.len() - 1]);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "caf", "version": env!("CARGO_PKG_VERSION") },
            "instructions": self.instructions,
        })
    }

    /// A notification, `method` with `params`.
    fn notified(&mut self, method: &str, params: &Value) -> Vec<Out> {
        if method != "notifications/cancelled" {
            return Vec::new();
        }
        let request = match params.get("requestId") {
            Some(id @ (Value::Number(_) | Value::String(_))) => Request(id.to_string()),
            _ => return Vec::new(),
        };
        match self.open.remove(&request) {
            Some(_) => vec![Out::Cancelled(request)],
            None => Vec::new(),
        }
    }

    /// A call, `id`, with `params`.
    fn called(&mut self, id: Request, params: &Value) -> Vec<Out> {
        if self.open.contains_key(&id) {
            return vec![Out::Close(format!(
                "a call reused the id {} of one still open",
                crate::core::agent_text(&id.0, 256)
            ))];
        }
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return vec![error_line(
                Some(&id),
                INVALID_PARAMS,
                "Invalid params: `name` must be a string",
            )];
        };
        let Some(tool) = CafTool::named(name).filter(|tool| self.tools.contains(tool)) else {
            let name = crate::core::agent_text(name, 256);
            let answer = Answer::error(format!("There's no tool named `{name}` here."));
            return vec![Out::Write(answer_bytes(&id, &answer))];
        };
        let input = match params.get("arguments") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(input) => input.clone(),
        };
        let meta = params.get("_meta");
        let token = (meta.and_then(|meta| meta.get("progressToken")))
            .filter(|token| token.is_string() || token.is_number())
            .cloned();
        let agent_call = (meta.and_then(|meta| meta.get("claudecode/toolUseId")))
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.open.insert(id.clone(), Open { token, progress: 0 });
        vec![Out::Call {
            request: id,
            tool,
            input,
            agent_call,
        }]
    }
}

/// `message` as a line: its JSON, and a newline.
fn line(message: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(message).expect("JSON writes");
    bytes.push(b'\n');
    bytes
}

/// The answer to `id`: `result`, as a line.
fn result_bytes(id: &Request, result: &Value) -> Vec<u8> {
    let id: Value = serde_json::from_str(&id.0).expect("an id is JSON");
    line(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// The answer to call `id`: `answer`, as a line.
fn answer_bytes(id: &Request, answer: &Answer) -> Vec<u8> {
    let content: Vec<Value> = (answer.content.iter())
        .map(|part| match part {
            AnswerPart::Text(text) => json!({ "type": "text", "text": text }),
            AnswerPart::Image { mime, data } => {
                json!({ "type": "image", "data": data, "mimeType": mime })
            }
        })
        .collect();
    result_bytes(
        id,
        &json!({ "content": content, "isError": answer.is_error }),
    )
}

/// [`result_bytes`], to write.
fn result_line(id: &Request, result: &Value) -> Out {
    Out::Write(result_bytes(id, result))
}

/// An error answering `id`, or no request CAF could read (`null`), to
/// write.
fn error_line(id: Option<&Request>, code: i64, message: &str) -> Out {
    let id: Value = id.map_or(Value::Null, |id| {
        serde_json::from_str(&id.0).expect("an id is JSON")
    });
    Out::Write(line(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })))
}

/// What to do about a line the agent sent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    /// Write these bytes (one line, with its newline) to the relay.
    Write(Vec<u8>),
    /// The agent called a tool.
    Call {
        /// The call, to answer it by.
        request: Request,
        /// The tool.
        tool: CafTool,
        /// What it was given, as the model wrote it.
        input: serde_json::Value,
        /// The agent's id for the call, if it sent one, such as Claude's
        /// `_meta["claudecode/toolUseId"]`.
        agent_call: Option<String>,
    },
    /// The agent cancelled a call: stop its work and send nothing for it.
    Cancelled(Request),
    /// Close the connection: the reason, for the log.
    Close(String),
}

/// A call's JSON-RPC id, as the agent gave it (opaque).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Request(String);

/// What the model is answered.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// What it's told, in order.
    pub content: Vec<AnswerPart>,
    /// Whether the call failed.
    pub is_error: bool,
}

impl Answer {
    /// An answer of `content`, an error if `is_error`.
    #[must_use]
    pub fn new(content: Vec<AnswerPart>, is_error: bool) -> Self {
        Self { content, is_error }
    }

    /// An answer of one piece of text.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![AnswerPart::Text(text.into())], false)
    }

    /// A failure, saying why.
    #[must_use]
    pub fn error(why: impl Into<String>) -> Self {
        Self::new(vec![AnswerPart::Text(why.into())], true)
    }
}

/// A piece of what the model is answered.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnswerPart {
    /// Text.
    Text(String),
    /// A picture.
    Image {
        /// Its type, such as `image/png`.
        mime: String,
        /// Its bytes, in base64.
        data: String,
    },
}

/// The tool definitions `tools/list` gives, for `tools`, in their order:
/// each one's name, title, description (at most 4096 characters), input
/// schema, which is Claude Code's own for its tool of that name, as the
/// model is trained on it, but for the fields CAF doesn't read, which a
/// call may still send, as nothing refuses them, its annotations (`readOnlyHint`,
/// `destructiveHint`, `openWorldHint`), and `_meta`
/// `{"anthropic/alwaysLoad": true}`, so Claude Code loads it at once
/// rather than searching for it.
#[must_use]
pub fn tool_list(tools: &[CafTool]) -> Value {
    Value::Array(tools.iter().map(|&tool| definition(tool)).collect())
}

/// One tool's definition, for [`tool_list`]. The descriptions are short
/// placeholders, until the tools are built.
fn definition(tool: CafTool) -> Value {
    let text = |description: &str| json!({ "type": "string", "description": description });
    let whole = |description: &str, minimum: u64| json!({ "type": "integer", "minimum": minimum, "description": description });
    let flag = |description: &str| json!({ "type": "boolean", "description": description });
    let (title, description, properties, required, read_only, open_world) = match tool {
        CafTool::Read => (
            "Read file",
            "Reads a file from the local filesystem. `file_path` is absolute. By default it \
             reads the whole file; `offset` (the line to start at) and `limit` (how many \
             lines) read part of a long one. Lines come back numbered from 1.",
            json!({
                "file_path": text("The absolute path of the file to read"),
                "offset": whole("The line number to start reading from", 0),
                "limit": whole("The number of lines to read", 1),
            }),
            vec!["file_path"],
            true,
            false,
        ),
        CafTool::Edit => (
            "Edit file",
            "Replaces exact text in a file. `old_string` must be found in the file exactly, \
             and only once, unless `replace_all` is true, which replaces every occurrence. \
             Read the file first.",
            json!({
                "file_path": text("The absolute path of the file to change"),
                "old_string": text("The text to replace"),
                "new_string": text("The text to replace it with (must differ from old_string)"),
                "replace_all": flag("Replace every occurrence of old_string (default false)"),
            }),
            vec!["file_path", "old_string", "new_string"],
            false,
            false,
        ),
        CafTool::Write => (
            "Write file",
            "Writes a whole file, making it if it isn't there, and replacing what was there \
             if it is. Read an existing file first. Prefer Edit for changing part of a file.",
            json!({
                "file_path": text("The absolute path of the file to write"),
                "content": text("What to write to the file"),
            }),
            vec!["file_path", "content"],
            false,
            false,
        ),
        CafTool::Bash => (
            "Run command",
            "Runs a shell command in the chat's folder, and gives back its output. \
             `timeout` is in milliseconds. `run_in_background` starts it and returns at \
             once. Give a short `description` of what it does.",
            json!({
                "command": text("The command to run"),
                "timeout": whole("How long it may run, in milliseconds", 0),
                "description": text("What the command does, in a few words"),
                "run_in_background": flag("Run it in the background, returning at once"),
            }),
            vec!["command"],
            false,
            true,
        ),
    };
    json!({
        "name": tool.name(),
        "title": title,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        },
        "annotations": {
            "title": title,
            "readOnlyHint": read_only,
            "destructiveHint": !read_only,
            "openWorldHint": open_world,
        },
        "_meta": { "anthropic/alwaysLoad": true },
    })
}

/// The per-run key, 32 bytes, that each chat's [`token`] is made with.
/// Its `Debug` never prints it. Written as hex; `[hidden]`, which
/// recordings and the debugger write in its place, reads as all zeros, so
/// a replay's tokens are consistent within it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Key(pub [u8; 32]);

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key([hidden])")
    }
}

/// What recordings and the debugger write in a key's place.
pub const HIDDEN: &str = "[hidden]";

impl Serialize for Key {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text == HIDDEN {
            return Ok(Self([0; 32]));
        }
        let bytes = unhex(&text)
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .ok_or_else(|| serde::de::Error::custom("a key is 64 hex digits, or `[hidden]`"))?;
        Ok(Self(bytes))
    }
}

/// The relay's secret for `chat`: hex of HMAC-SHA256 under `key` of the
/// chat id's text, as [`ChatId`] writes it (`CAF_RELAY_CHAT`).
#[must_use]
pub fn token(key: &Key, chat: ChatId) -> String {
    hex(&mac(key, chat).finalize().into_bytes())
}

/// Whether `token` is `chat`'s, compared in constant time.
#[must_use]
pub fn token_is(key: &Key, chat: ChatId, token: &str) -> bool {
    unhex(token).is_some_and(|given| mac(key, chat).verify_slice(&given).is_ok())
}

/// HMAC-SHA256 under `key`, of `chat`'s id's text.
fn mac(key: &Key, chat: ChatId) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(&key.0).expect("HMAC takes a key of any length");
    mac.update(chat.to_string().as_bytes());
    mac
}

/// `bytes` in lowercase hex.
fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// The bytes `text` is in hex, either case: `None` if it isn't.
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (text.as_bytes().chunks(2))
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A connection serving Read and Bash.
    fn connection() -> Connection {
        Connection::new(&[CafTool::Read, CafTool::Bash], "CAF's tools.")
    }

    /// What `out` writes, each line as JSON.
    fn written(out: &[Out]) -> Vec<Value> {
        (out.iter())
            .map(|each| match each {
                Out::Write(bytes) => {
                    assert_eq!(bytes.last(), Some(&b'\n'), "a whole line");
                    assert_eq!(
                        bytes.iter().filter(|byte| **byte == b'\n').count(),
                        1,
                        "one line"
                    );
                    serde_json::from_slice(bytes).expect("JSON")
                }
                other => panic!("not a write: {other:?}"),
            })
            .collect()
    }

    /// What taking `message` writes, as one line of JSON.
    fn answer_to(connection: &mut Connection, message: &Value) -> Value {
        let out = connection.take(message.to_string().as_bytes());
        let mut lines = written(&out);
        assert_eq!(lines.len(), 1, "{out:?}");
        lines.remove(0)
    }

    /// A call to `tool`, as Claude Code makes one.
    fn call(id: Value, tool: &str, arguments: Value, meta: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
            "name": tool, "arguments": arguments, "_meta": meta,
        }})
    }

    /// The request a call `take` gave.
    fn called(out: Vec<Out>) -> (Request, CafTool, Value, Option<String>) {
        match <[Out; 1]>::try_from(out) {
            Ok(
                [
                    Out::Call {
                        request,
                        tool,
                        input,
                        agent_call,
                    },
                ],
            ) => (request, tool, input, agent_call),
            other => panic!("not one call: {other:?}"),
        }
    }

    #[test]
    fn initialize_answers_the_agents_version_or_the_newest() {
        let mut connection = connection();
        for version in VERSIONS {
            let answer = answer_to(
                &mut connection,
                &json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                    "protocolVersion": version, "capabilities": {}, "clientInfo": { "name": "claude-code", "version": "2" },
                }}),
            );
            assert_eq!(
                answer,
                json!({ "jsonrpc": "2.0", "id": 0, "result": {
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "caf", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "CAF's tools.",
                }})
            );
        }
        let answer = answer_to(
            &mut connection,
            &json!({ "jsonrpc": "2.0", "id": "a", "method": "initialize", "params": { "protocolVersion": "2099-01-01" } }),
        );
        assert_eq!(answer["id"], "a");
        assert_eq!(answer["result"]["protocolVersion"], "2025-11-25");
        let answer = answer_to(
            &mut connection,
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
        );
        assert_eq!(answer["result"]["protocolVersion"], "2025-11-25");
    }

    #[test]
    fn notifications_are_taken_and_ping_answered() {
        let mut connection = connection();
        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(connection.take(initialized.to_string().as_bytes()), []);
        let other =
            json!({ "jsonrpc": "2.0", "method": "notifications/roots/list_changed", "params": {} });
        assert_eq!(connection.take(other.to_string().as_bytes()), []);
        // An answer to nothing CAF asked.
        let answer = json!({ "jsonrpc": "2.0", "id": 4, "result": {} });
        assert_eq!(connection.take(answer.to_string().as_bytes()), []);
        assert_eq!(
            answer_to(
                &mut connection,
                &json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" })
            ),
            json!({ "jsonrpc": "2.0", "id": 7, "result": {} })
        );
    }

    #[test]
    fn tools_list_gives_the_chats_tools_on_one_page() {
        let mut connection = connection();
        let answer = answer_to(
            &mut connection,
            &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
        );
        assert_eq!(
            answer,
            json!({ "jsonrpc": "2.0", "id": 2, "result": { "tools": tool_list(&[CafTool::Read, CafTool::Bash]) } })
        );
        let names: Vec<&str> = (answer["result"]["tools"].as_array().expect("tools").iter())
            .map(|tool| tool["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(names, ["Read", "Bash"]);
        assert!(answer["result"].get("nextCursor").is_none());
    }

    #[test]
    fn tool_definitions_are_claude_codes_with_their_hints() {
        let list = tool_list(&CafTool::ALL);
        let tools = list.as_array().expect("a list");
        assert_eq!(tools.len(), 4);
        for (tool, definition) in CafTool::ALL.into_iter().zip(tools) {
            assert_eq!(definition["name"], tool.name());
            assert!(definition["title"].is_string());
            let description = definition["description"].as_str().expect("a description");
            assert!(!description.is_empty() && description.chars().count() <= 4096);
            assert_eq!(definition["inputSchema"]["type"], "object");
            assert_eq!(definition["_meta"], json!({ "anthropic/alwaysLoad": true }));
            let hints = &definition["annotations"];
            let read_only = tool == CafTool::Read;
            assert_eq!(hints["readOnlyHint"], read_only, "{tool:?}");
            assert_eq!(hints["destructiveHint"], !read_only, "{tool:?}");
            assert_eq!(hints["openWorldHint"], tool == CafTool::Bash, "{tool:?}");
        }
        let fields = |at: usize| -> Vec<String> {
            let mut fields: Vec<String> = (tools[at]["inputSchema"]["properties"]
                .as_object()
                .expect("properties")
                .keys())
            .cloned()
            .collect();
            fields.sort();
            fields
        };
        assert_eq!(fields(0), ["file_path", "limit", "offset"]);
        assert_eq!(
            fields(1),
            ["file_path", "new_string", "old_string", "replace_all"]
        );
        assert_eq!(fields(2), ["content", "file_path"]);
        assert_eq!(
            fields(3),
            ["command", "description", "run_in_background", "timeout"]
        );
    }

    #[test]
    fn a_call_is_handed_on_with_the_agents_id_for_it() {
        let mut connection = connection();
        let (request, tool, input, agent_call) = called(
            connection.take(
                call(
                    json!(5),
                    "Read",
                    json!({ "file_path": "/a" }),
                    json!({ "claudecode/toolUseId": "toolu_1", "progressToken": 9 }),
                )
                .to_string()
                .as_bytes(),
            ),
        );
        assert_eq!(tool, CafTool::Read);
        assert_eq!(input, json!({ "file_path": "/a" }));
        assert_eq!(agent_call.as_deref(), Some("toolu_1"));
        assert_eq!(connection.open(), std::slice::from_ref(&request));
        // No arguments are an empty object; no `_meta`, no id.
        let line = json!({ "jsonrpc": "2.0", "id": "x", "method": "tools/call", "params": { "name": "Bash" } });
        let (other, tool, input, agent_call) = called(connection.take(line.to_string().as_bytes()));
        assert_eq!((tool, input, agent_call), (CafTool::Bash, json!({}), None));
        assert_eq!(connection.open().len(), 2);
        // Answered once, and never again.
        let answer = connection
            .answer(&request, &Answer::text("one\ntwo"))
            .expect("answered");
        assert_eq!(
            written(&[Out::Write(answer)]),
            [
                json!({ "jsonrpc": "2.0", "id": 5, "result": { "content": [{ "type": "text", "text": "one\ntwo" }], "isError": false } })
            ]
        );
        assert_eq!(connection.answer(&request, &Answer::text("again")), None);
        assert_eq!(connection.open(), std::slice::from_ref(&other));
        let answer = connection
            .answer(
                &other,
                &Answer::new(
                    vec![
                        AnswerPart::Text("see".into()),
                        AnswerPart::Image {
                            mime: "image/png".into(),
                            data: "iVBO".into(),
                        },
                    ],
                    true,
                ),
            )
            .expect("answered");
        assert_eq!(
            written(&[Out::Write(answer)]),
            [
                json!({ "jsonrpc": "2.0", "id": "x", "result": { "content": [
                { "type": "text", "text": "see" },
                { "type": "image", "data": "iVBO", "mimeType": "image/png" },
            ], "isError": true } })
            ]
        );
        assert!(connection.open().is_empty());
    }

    #[test]
    fn a_call_to_a_tool_not_given_is_answered_as_an_error_at_once() {
        let mut connection = connection();
        for name in ["Write", "Grep", ""] {
            let answer = answer_to(
                &mut connection,
                &call(json!(3), name, json!({ "file_path": "/a" }), json!({})),
            );
            assert_eq!(answer["id"], 3);
            assert_eq!(answer["result"]["isError"], true, "{name}");
            assert!(
                answer["result"]["content"][0]["text"]
                    .as_str()
                    .expect("why")
                    .contains("no tool")
            );
        }
        assert!(connection.open().is_empty());
        // One naming no tool has bad params.
        for params in [json!({}), json!({ "name": 3 }), Value::Null] {
            let answer = answer_to(
                &mut connection,
                &json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": params }),
            );
            assert_eq!(answer["id"], 4);
            assert_eq!(answer["error"]["code"], -32602, "{params}");
        }
        assert!(connection.open().is_empty());
        // Nothing is left to answer.
        assert_eq!(
            connection.answer(&Request("3".into()), &Answer::text("x")),
            None
        );
    }

    #[test]
    fn a_cancelled_call_is_never_answered() {
        let mut connection = connection();
        let (request, ..) = called(
            connection.take(
                call(
                    json!(8),
                    "Bash",
                    json!({ "command": "sleep 9" }),
                    json!({ "progressToken": "p" }),
                )
                .to_string()
                .as_bytes(),
            ),
        );
        let cancel = |id: Value| {
            json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": id, "reason": "user" } })
                .to_string()
        };
        // Another id, or the same in another type, cancels nothing.
        assert_eq!(connection.take(cancel(json!("8")).as_bytes()), []);
        assert_eq!(connection.take(cancel(json!(9)).as_bytes()), []);
        assert_eq!(
            connection.take(cancel(json!(8)).as_bytes()),
            [Out::Cancelled(request.clone())]
        );
        assert_eq!(connection.take(cancel(json!(8)).as_bytes()), []);
        assert_eq!(
            connection.answer(&request, &Answer::error("Cancelled")),
            None
        );
        assert_eq!(connection.progress(&request, "Running"), None);
        assert!(connection.open().is_empty());
    }

    #[test]
    fn progress_goes_only_to_an_open_call_with_a_token() {
        let mut connection = connection();
        let (with, ..) = called(
            connection.take(
                call(
                    json!(1),
                    "Read",
                    json!({ "file_path": "/a" }),
                    json!({ "progressToken": "t1" }),
                )
                .to_string()
                .as_bytes(),
            ),
        );
        let (without, ..) = called(
            connection.take(
                call(json!(2), "Read", json!({ "file_path": "/b" }), json!({}))
                    .to_string()
                    .as_bytes(),
            ),
        );
        let first = connection
            .progress(&with, "Waiting for permission")
            .expect("progress");
        let second = connection.progress(&with, "Running").expect("progress");
        assert_eq!(
            written(&[Out::Write(first), Out::Write(second)]),
            [
                json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": { "progressToken": "t1", "progress": 1, "message": "Waiting for permission" } }),
                json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": { "progressToken": "t1", "progress": 2, "message": "Running" } }),
            ]
        );
        assert_eq!(connection.progress(&without, "Running"), None);
        assert!(connection.answer(&with, &Answer::text("done")).is_some());
        assert_eq!(connection.progress(&with, "Running"), None);
        assert_eq!(connection.progress(&Request("77".into()), "Running"), None);
    }

    #[test]
    fn other_requests_have_no_such_method() {
        let mut connection = connection();
        for method in [
            "server/discover",
            "resources/list",
            "prompts/list",
            "completion/complete",
        ] {
            let answer = answer_to(
                &mut connection,
                &json!({ "jsonrpc": "2.0", "id": 11, "method": method, "params": {} }),
            );
            assert_eq!(answer["id"], 11);
            assert_eq!(answer["error"]["code"], -32601, "{method}");
            assert!(answer.get("result").is_none());
        }
    }

    #[test]
    fn what_isnt_one_request_is_refused() {
        let mut connection = connection();
        let parse = connection.take(b"{\"jsonrpc\": \"2.0\", \"id\": 1,");
        let answer = &written(&parse)[0];
        assert_eq!(answer["id"], Value::Null);
        assert_eq!(answer["error"]["code"], -32700);
        let not_utf8 = connection.take(b"\xff\xfe");
        assert_eq!(written(&not_utf8)[0]["error"]["code"], -32700);
        let batch = json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }]);
        let answer = answer_to(&mut connection, &batch);
        assert_eq!(answer["error"]["code"], -32600);
        assert_eq!(answer["id"], Value::Null);
        for odd in [
            json!(3),
            json!("ping"),
            json!({ "jsonrpc": "2.0", "id": {}, "method": "ping" }),
            json!({ "jsonrpc": "2.0", "id": null, "method": "ping" }),
            json!({ "jsonrpc": "2.0", "id": true, "method": "ping" }),
        ] {
            let answer = answer_to(&mut connection, &odd);
            assert_eq!(answer["error"]["code"], -32600, "{odd}");
            assert_eq!(answer["id"], Value::Null, "{odd}");
        }
        let answer = answer_to(
            &mut connection,
            &json!({ "jsonrpc": "2.0", "id": 4, "method": 7 }),
        );
        assert_eq!(answer["error"]["code"], -32600);
        assert_eq!(answer["id"], 4);
    }

    #[test]
    fn an_overlong_line_or_a_reused_id_closes_the_connection() {
        let mut connection = connection();
        let long = vec![b' '; MOST_LINE + 1];
        assert!(matches!(connection.take(&long)[..], [Out::Close(_)]));
        // At the limit, it's read.
        let mut ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })
            .to_string()
            .into_bytes();
        ping.resize(MOST_LINE, b' ');
        assert_eq!(written(&connection.take(&ping))[0]["result"], json!({}));
        let line = call(json!(1), "Read", json!({ "file_path": "/a" }), json!({})).to_string();
        called(connection.take(line.as_bytes()));
        assert!(matches!(
            connection.take(line.as_bytes())[..],
            [Out::Close(_)]
        ));
        // Once answered, its id may come again.
        let request = connection.open().remove(0);
        assert!(connection.answer(&request, &Answer::text("x")).is_some());
        called(connection.take(line.as_bytes()));
    }

    #[test]
    fn a_call_before_initialize_is_served() {
        let mut connection = connection();
        let line = call(json!(1), "Bash", json!({ "command": "ls" }), json!({})).to_string();
        let (_, tool, ..) = called(connection.take(line.as_bytes()));
        assert_eq!(tool, CafTool::Bash);
    }

    #[test]
    fn tokens_are_each_chats_and_compared_whole() {
        let key = Key([1; 32]);
        let (one, two) = (ChatId::from_number(1), ChatId::from_number(2));
        let token = token(&key, one);
        assert_eq!(token.len(), 64);
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert!(token_is(&key, one, &token));
        assert!(token_is(&key, one, &token.to_ascii_uppercase()));
        assert!(!token_is(&key, two, &token));
        assert!(!token_is(&Key([2; 32]), one, &token));
        assert!(!token_is(&key, one, &token[..62]));
        assert!(!token_is(&key, one, ""));
        assert!(!token_is(&key, one, &format!("+{}", &token[1..])));
        assert!(!token_is(&key, one, &"zz".repeat(32)));
        // HMAC-SHA256 of the id's text, as OpenSSL computes it:
        // `printf %s 00000000-0000-0000-0000-000000001234 |
        // openssl dgst -sha256 -hmac 0123456789abcdef0123456789abcdef`.
        let known = Key(*b"0123456789abcdef0123456789abcdef");
        assert_eq!(
            super::token(&known, ChatId::from_number(0x1234)),
            "53bee8321fd1504dffb431dc24021f24a5101d258b6e0a7e0b6a3dd11ada916c"
        );
    }

    #[test]
    fn a_key_is_hex_never_printed_and_hidden_reads_as_zeros() {
        let key = Key([0xab; 32]);
        assert_eq!(format!("{key:?}"), "Key([hidden])");
        let json = serde_json::to_string(&key).expect("JSON");
        assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
        assert_eq!(serde_json::from_str::<Key>(&json).expect("reads"), key);
        assert_eq!(
            serde_json::from_str::<Key>("\"[hidden]\"").expect("reads"),
            Key([0; 32])
        );
        assert!(serde_json::from_str::<Key>("\"abab\"").is_err());
        assert!(serde_json::from_str::<Key>(&format!("\"{}\"", "g".repeat(64))).is_err());
        assert!(serde_json::from_str::<Key>("7").is_err());
    }
}
