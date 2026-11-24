//! Serving CAF's tools over MCP: the protocol, with no I/O.
//!
//! The agent starts CAF's relay as its MCP server `caf`, and the relay
//! connects to the tools driver's socket, says which chat it's for and
//! proves it with that chat's [`token`], then carries MCP's lines both
//! ways. The tools driver runs a [`Connection`] for each: it hands it each
//! line the agent sent, and does what comes [`Out`].
//!
//! *Being written: the protocol's rules come with the next commit.*

use std::fmt;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::Sha256;

use crate::ids::ChatId;
use crate::tool::CafTool;

/// The longest line a connection takes from the agent, in bytes: one
/// longer closes it ([`Out::Close`]). The driver enforces it while
/// reading, so it never holds more.
pub const MOST_LINE: usize = 4 << 20;

/// One relay's MCP connection, from its handshake on: a state machine.
#[derive(Debug)]
pub struct Connection {
    tools: Vec<CafTool>,
    instructions: String,
}

impl Connection {
    /// A connection serving `tools` with `instructions` (one line).
    #[must_use]
    pub fn new(tools: &[CafTool], instructions: &str) -> Self {
        Self {
            tools: tools.to_vec(),
            instructions: instructions.to_owned(),
        }
    }

    /// One line the agent sent (without its newline). What to do about it.
    pub fn take(&mut self, line: &[u8]) -> Vec<Out> {
        let _ = (line, &self.tools, &self.instructions);
        todo!("the protocol comes with the next commit")
    }

    /// Answers a call. `None` if it was already answered, or cancelled.
    pub fn answer(&mut self, request: &Request, answer: &Answer) -> Option<Vec<u8>> {
        let _ = (request, answer);
        todo!("the protocol comes with the next commit")
    }

    /// A progress notification for an open call, if the agent gave a
    /// token. `None` otherwise, or once the call has ended.
    pub fn progress(&mut self, request: &Request, message: &str) -> Option<Vec<u8>> {
        let _ = (request, message);
        todo!("the protocol comes with the next commit")
    }

    /// The calls still open, for ending them all when the connection drops.
    #[must_use]
    pub fn open(&self) -> Vec<Request> {
        Vec::new()
    }
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

/// The tool definitions `tools/list` gives, for `tools`.
#[must_use]
pub fn tool_list(tools: &[CafTool]) -> serde_json::Value {
    let _ = tools;
    serde_json::Value::Array(Vec::new())
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
