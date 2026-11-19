//! ACP on the wire: splitting an adapter's output into messages, and
//! reading their JSON-RPC shape.
//!
//! This module has no state about sessions and does no I/O, so a driver
//! can use it wherever it likes. clackatoo parses messages of 256 KiB or
//! more on another thread, so a big tool output doesn't stall its loop.
//! What it produces, [`Frame`]s, is what the agent driver hands the core.
//!
//! Splitting and parsing are separate steps for that reason:
//! [`Framer::push`] only finds line ends, which is cheap, and [`parse`]
//! does the JSON, which may be slow.
//!
//! What the messages *mean* (sessions, prompts, updates) is the core's
//! business, read through each adapter's profile. Their types come from
//! `agent-client-protocol-schema`.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// Splits an adapter's output into lines, one JSON-RPC message each.
///
/// It never scans a line from its start again when more of it arrives, so
/// a huge line costs one pass. A line longer than its limit is dropped up
/// to its newline, but its first 64 KiB are kept, so the core can still
/// refuse the request it carried.
#[derive(Debug)]
pub struct Framer {
    biggest: usize,
}

/// One line from an adapter, not yet parsed.
#[non_exhaustive]
#[derive(Debug)]
pub enum Line {
    /// A whole line, without its newline.
    Whole(Vec<u8>),
    /// A line longer than the limit: its start, and how long it was.
    TooBig {
        /// Its first 64 KiB.
        head: Vec<u8>,
        /// Its whole length in bytes.
        len: usize,
    },
}

impl Framer {
    /// A framer that accepts lines of up to `biggest` bytes.
    #[must_use]
    pub fn new(biggest: usize) -> Self {
        Self { biggest }
    }

    /// Takes more bytes from the adapter and adds the lines they finish to
    /// `out`, in order. Bytes needn't be UTF-8, and needn't end on a line.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Line>) {
        todo!()
    }

    /// The adapter's output has ended: adds a last unfinished line, if
    /// there is one.
    pub fn finish(&mut self, out: &mut Vec<Line>) {
        todo!()
    }
}

/// Reads one line as a JSON-RPC message.
///
/// Lines must be handed to the core in the order they came, however long
/// each takes to parse.
#[must_use]
pub fn parse(line: Line) -> Frame {
    todo!()
}

/// One line from an adapter, read as far as JSON-RPC: what the agent
/// driver hands the core.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
pub enum Frame {
    /// A JSON-RPC message.
    Message(Message),
    /// A line too long to accept. Its id and method are recovered from its
    /// start, if they're there, so a request can be refused and an answer
    /// can end the request it answered.
    TooBig {
        /// Its id, if found.
        id: Option<RequestId>,
        /// Its method, if found.
        method: Option<String>,
        /// Its length in bytes.
        len: usize,
    },
    /// A line that isn't JSON-RPC: blank, or something the adapter printed
    /// by mistake. Skipped, and reported.
    Junk {
        /// Its length in bytes.
        len: usize,
    },
}

/// A JSON-RPC 2.0 message, sorted by shape. Its params and result stay
/// raw JSON until the core knows what they are.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    /// A request: a method with an id. The agent wants an answer.
    Request {
        /// Its id.
        id: RequestId,
        /// What it asks for, such as `session/request_permission`.
        method: String,
        /// Its params.
        params: Option<Box<RawValue>>,
    },
    /// A notification: a method with no id, such as `session/update`.
    Notification {
        /// What it says.
        method: String,
        /// Its params.
        params: Option<Box<RawValue>>,
    },
    /// An answer to one of CAF's requests: an id with no method.
    Response {
        /// The id of the request it answers.
        id: RequestId,
        /// What it says: a result, or an error. An answer with neither is
        /// read as an error.
        outcome: Result<Box<RawValue>, RpcError>,
    },
}

/// A JSON-RPC request id. CAF's own are always numbers; an agent's can be
/// anything JSON-RPC allows.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RequestId {
    /// A number.
    Number(i64),
    /// A string.
    String(String),
    /// `null`.
    Null,
}

/// A JSON-RPC error.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcError {
    /// Its code, such as -32000 (sign-in needed) or -32800 (cancelled).
    pub code: i64,
    /// What it says.
    pub message: String,
    /// Anything else it carries, such as Claude's `errorKind`.
    pub data: Option<Box<RawValue>>,
}
