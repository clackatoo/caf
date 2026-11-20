//! ACP on the wire: splitting an adapter's output into messages, and
//! reading their JSON-RPC shape.
//!
//! This module has no state about sessions and does no I/O, so a driver
//! can use it wherever it likes. What it produces, [`Frame`]s, is what the
//! agent driver hands the core.
//!
//! Splitting and parsing are separate steps: [`Framer::push`] only finds
//! line ends, which is cheap, and [`parse`] does the JSON, which may be
//! slow. Parsing a few megabytes of tool output takes milliseconds, so a
//! driver can parse big lines on another thread, and a big message
//! doesn't hold up everything else it does.
//!
//! What the messages *mean* (sessions, prompts, updates) is the core's
//! business, read through each adapter's profile. Their types come from
//! `agent-client-protocol-schema`.

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// How much of a line too long to accept is kept, to find its id and
/// method in.
const HEAD: usize = 64 << 10;

/// The code an answer is given when it has neither a result nor an
/// error, or an error that isn't one: JSON-RPC's internal error, as the
/// agent's side broke, not CAF's.
const BROKEN_ANSWER: i64 = -32603;

/// Splits an adapter's output into lines, one JSON-RPC message each.
///
/// It never scans a line from its start again when more of it arrives, so
/// a huge line costs one pass. A line longer than its limit is dropped up
/// to its newline, but its first 64 KiB are kept, so the core can still
/// refuse the request it carried.
#[derive(Debug)]
pub struct Framer {
    biggest: usize,
    /// What's come of a line not yet ended. All of it has been looked
    /// through for a newline already, so it never is again.
    line: Vec<u8>,
    /// A line too long to accept, being dropped up to its newline.
    dropping: Option<Dropping>,
}

/// A line too long to accept: its start, and how long it's been so far.
#[derive(Debug)]
struct Dropping {
    head: Vec<u8>,
    len: usize,
}

impl Dropping {
    /// Counts `bytes`, more of the line, keeping them if the head isn't
    /// full yet.
    fn take(&mut self, bytes: &[u8]) {
        let room = HEAD - self.head.len();
        self.head.extend_from_slice(&bytes[..room.min(bytes.len())]);
        self.len += bytes.len();
    }
}

/// One line from an adapter, not yet parsed.
#[non_exhaustive]
#[derive(Debug)]
pub enum Line {
    /// A whole line, without its newline. A `\r` before the newline is
    /// kept: [`parse`] reads it as space.
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
        Self {
            biggest,
            line: Vec::new(),
            dropping: None,
        }
    }

    /// Takes more bytes from the adapter and adds the lines they finish to
    /// `out`, in order. Bytes needn't be UTF-8, and needn't end on a line.
    ///
    /// Only the new bytes are looked through. A line that grows past the
    /// limit is let go of as soon as it does, so the framer never holds
    /// more than the limit and the head of one line.
    pub fn push(&mut self, mut bytes: &[u8], out: &mut Vec<Line>) {
        while !bytes.is_empty() {
            let end = memchr::memchr(b'\n', bytes);
            let (piece, rest) = match end {
                Some(at) => (&bytes[..at], &bytes[at + 1..]),
                None => (bytes, &[][..]),
            };
            bytes = rest;
            if let Some(dropping) = &mut self.dropping {
                dropping.take(piece);
            } else if piece.len() > self.biggest - self.line.len() {
                let mut dropping = Dropping {
                    head: Vec::new(),
                    len: 0,
                };
                dropping.take(&std::mem::take(&mut self.line));
                dropping.take(piece);
                self.dropping = Some(dropping);
            } else if end.is_some() && self.line.is_empty() {
                // A line all in this piece: copied once, straight out.
                out.push(Line::Whole(piece.to_vec()));
                continue;
            } else {
                self.line.extend_from_slice(piece);
            }
            if end.is_some() {
                self.end_line(out);
            }
        }
    }

    /// The adapter's output has ended: adds a last unfinished line, if
    /// there is one.
    pub fn finish(&mut self, out: &mut Vec<Line>) {
        if self.dropping.is_some() || !self.line.is_empty() {
            self.end_line(out);
        }
    }

    /// Adds the line being read to `out`, whole or too big, and starts
    /// the next.
    fn end_line(&mut self, out: &mut Vec<Line>) {
        out.push(match self.dropping.take() {
            Some(Dropping { head, len }) => Line::TooBig { head, len },
            None => Line::Whole(std::mem::take(&mut self.line)),
        });
    }
}

/// Reads one line as a JSON-RPC message.
///
/// Lines must be handed to the core in the order they came, however long
/// each takes to parse.
///
/// How it reads them:
/// - **By shape**: a method with an id is a request, a method without one
///   a notification, and an id without a method an answer. Anything else
///   is [`Frame::Junk`].
/// - **Bad UTF-8 is mended, not refused**: each broken sequence becomes
///   U+FFFD, so a message with one bad byte in a tool's output still
///   arrives. Bad bytes outside a string break the JSON, and make junk.
/// - **`jsonrpc`** may be missing, as some adapters leave it out, but if
///   it's there it must be `"2.0"`.
/// - **Ids** are whole numbers that fit an `i64`, strings or `null`.
///   Numbers are read from their text, exactly. One written with a
///   fraction of only zeros (`5.0`) is read as the whole number, as a
///   JavaScript adapter can't tell the two apart. Any other id (`1.5`,
///   `1.0000000000000001`, `5e0`, 2⁶³, `true`, an object) can't be
///   answered faithfully, so its message is junk.
/// - **Batches** (arrays of messages) are junk: no adapter sends them, so
///   they aren't read yet.
/// - **Too-big lines** give their id and method if their head holds them:
///   a number is believed only when what follows it was read too, since
///   `12` cut off may have been `123`. Their [`Shape`] says whether the
///   head showed an answer's keys or a request's.
#[must_use]
pub fn parse(line: Line) -> Frame {
    match line {
        Line::Whole(bytes) => read_whole(&bytes),
        Line::TooBig { head, len } => {
            let head = Head::of(&head);
            let shape = match (head.answer, head.call) {
                (true, false) => Shape::Answer,
                (false, true) => Shape::Call,
                _ => Shape::Unknown,
            };
            Frame::TooBig {
                id: head.id,
                method: head.method,
                shape,
                len,
            }
        }
    }
}

/// Reads a whole line, as [`parse`] says.
fn read_whole(bytes: &[u8]) -> Frame {
    let junk = Frame::Junk { len: bytes.len() };
    let text = String::from_utf8_lossy(bytes);
    let Ok(wire) = serde_json::from_str::<Wire>(&text) else {
        return junk;
    };
    if wire
        .jsonrpc
        .as_deref()
        .is_some_and(|version| version != "2.0")
    {
        return junk;
    }
    let id = match wire.id.as_deref().map(request_id) {
        Some(None) => return junk,
        Some(id) => id,
        None => None,
    };
    Frame::Message(match (wire.method, id) {
        (Some(method), Some(id)) => Message::Request {
            id,
            method,
            params: wire.params,
        },
        (Some(method), None) => Message::Notification {
            method,
            params: wire.params,
        },
        (None, Some(id)) => Message::Response {
            id,
            outcome: outcome(wire.result, wire.error),
        },
        (None, None) => return junk,
    })
}

/// A JSON-RPC message as it comes, before it's sorted by shape. Fields
/// whose `null` means something are told apart from missing ones.
#[derive(Deserialize)]
struct Wire {
    jsonrpc: Option<String>,
    #[serde(default, deserialize_with = "present")]
    id: Option<Box<RawValue>>,
    method: Option<String>,
    params: Option<Box<RawValue>>,
    #[serde(default, deserialize_with = "present")]
    result: Option<Box<RawValue>>,
    error: Option<Box<RawValue>>,
}

/// A field that's there, `null` or not.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    field: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(field).map(Some)
}

/// An error as JSON-RPC says it must be.
#[derive(Deserialize)]
struct WireError {
    code: i64,
    message: String,
    data: Option<Box<RawValue>>,
}

/// What an answer says: its error if it has one, even beside a result,
/// else its result. One with neither, or an error that isn't one, is a
/// [`BROKEN_ANSWER`], a broken error kept whole in its `data`.
fn outcome(
    result: Option<Box<RawValue>>,
    error: Option<Box<RawValue>>,
) -> Result<Box<RawValue>, RpcError> {
    match (result, error) {
        (_, Some(error)) => Err(match serde_json::from_str::<WireError>(error.get()) {
            Ok(WireError {
                code,
                message,
                data,
            }) => RpcError {
                code,
                message,
                data,
            },
            Err(_) => RpcError {
                code: BROKEN_ANSWER,
                message: "the agent's answer had an error that wasn't a JSON-RPC error".to_owned(),
                data: Some(error),
            },
        }),
        (Some(result), None) => Ok(result),
        (None, None) => Err(RpcError {
            code: BROKEN_ANSWER,
            message: "the agent's answer had neither a result nor an error".to_owned(),
            data: None,
        }),
    }
}

/// `id` as a request id, if it's one CAF can answer faithfully. Numbers
/// are read from their text, never through a float, which can't hold
/// every `i64` and may be read a little off.
fn request_id(id: &RawValue) -> Option<RequestId> {
    let text = id.get();
    match text.as_bytes().first()? {
        b'n' => Some(RequestId::Null),
        b'"' => serde_json::from_str(text).ok().map(RequestId::String),
        b'-' | b'0'..=b'9' => whole_number(text).map(RequestId::Number),
        _ => None,
    }
}

/// `number`, JSON number text, if it's a whole number that fits an
/// `i64`: written plainly (`5`), or with a fraction of only zeros
/// (`5.0`). An exponent (`5e0`) isn't read.
fn whole_number(number: &str) -> Option<i64> {
    let (whole, fraction) = number.split_once('.').unwrap_or((number, "0"));
    if !fraction.bytes().all(|digit| digit == b'0') {
        return None;
    }
    whole.parse().ok()
}

/// What the head of a line too long to accept says, read until it's cut
/// off or breaks.
#[derive(Debug, Default)]
struct Head {
    id: Option<RequestId>,
    method: Option<String>,
    /// It had a `result` or `error` key.
    answer: bool,
    /// It had a `method` or `params` key.
    call: bool,
}

impl Head {
    /// What `head`, the start of a message, says.
    fn of(head: &[u8]) -> Self {
        let text = String::from_utf8_lossy(head);
        let mut found = Self::default();
        // An error is where it's cut off: what came before is kept.
        let _ = (&mut found).deserialize(&mut serde_json::Deserializer::from_str(&text));
        found
    }
}

impl<'de> DeserializeSeed<'de> for &mut Head {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, reading: D) -> Result<(), D::Error> {
        reading.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for &mut Head {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON-RPC message")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        // A number is believed only once the next key is read: cut off,
        // `12` may have been `123`.
        let mut number: Option<Box<RawValue>> = None;
        while let Some(key) = map.next_key::<String>()? {
            if let Some(id) = number.take() {
                self.id = request_id(&id);
            }
            self.answer |= matches!(key.as_str(), "result" | "error");
            self.call |= matches!(key.as_str(), "method" | "params");
            match key.as_str() {
                "id" => {
                    let id: Box<RawValue> = map.next_value()?;
                    if id
                        .get()
                        .starts_with(['-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9'])
                    {
                        number = Some(id);
                    } else {
                        self.id = request_id(&id);
                    }
                }
                "method" => self.method = Some(map.next_value()?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if let Some(id) = number {
            self.id = request_id(&id);
        }
        Ok(())
    }
}

/// One line from an adapter, read as far as JSON-RPC: what the agent
/// driver hands the core.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Frame {
    /// A JSON-RPC message.
    Message(Message),
    /// A line too long to accept. Its id and method are recovered from its
    /// start, if they're there, so a request can be refused and an answer
    /// can end the request it answered.
    ///
    /// It may gain fields, so patterns on it outside CAF need `..`.
    #[non_exhaustive]
    TooBig {
        /// Its id, if found.
        id: Option<RequestId>,
        /// Its method, if found.
        method: Option<String>,
        /// What its head showed it to be. An id without a method may be an
        /// answer, or a request whose method came later in the line.
        shape: Shape,
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

/// What the head of a line too long to accept showed it to be, by the
/// keys found there, whatever their values.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Shape {
    /// It had `result` or `error`, and neither `method` nor `params`: an
    /// answer.
    Answer,
    /// It had `method` or `params`, and neither `result` nor `error`: a
    /// request if it has an id, else a notification.
    Call,
    /// It had none of them, or both kinds.
    Unknown,
}

/// A JSON-RPC 2.0 message, sorted by shape. Its params and result stay
/// raw JSON until the core knows what they are.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Message {
    /// A request: a method with an id. The agent wants an answer.
    Request {
        /// Its id.
        id: RequestId,
        /// What it asks for, such as `session/request_permission`.
        method: String,
        /// Its params.
        #[cfg_attr(feature = "ts", ts(as = "Option<serde_json::Value>"))]
        params: Option<Box<RawValue>>,
    },
    /// A notification: a method with no id, such as `session/update`.
    Notification {
        /// What it says.
        method: String,
        /// Its params.
        #[cfg_attr(feature = "ts", ts(as = "Option<serde_json::Value>"))]
        params: Option<Box<RawValue>>,
    },
    /// An answer to one of CAF's requests: an id with no method.
    Response {
        /// The id of the request it answers.
        id: RequestId,
        /// What it says: a result, or an error. An error wins if it has
        /// both. An answer with neither, or with an error that isn't a
        /// JSON-RPC error, is read as error -32603 (internal error), the
        /// broken error kept whole in its `data`.
        #[cfg_attr(feature = "ts", ts(as = "Result<serde_json::Value, RpcError>"))]
        outcome: Result<Box<RawValue>, RpcError>,
    },
}

/// A JSON-RPC request id. CAF's own are always numbers; an agent's can be
/// anything JSON-RPC allows.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RpcError {
    /// Its code, such as -32000 (sign-in needed) or -32800 (cancelled).
    pub code: i64,
    /// What it says.
    pub message: String,
    /// Anything else it carries, such as Claude's `errorKind`.
    #[cfg_attr(feature = "ts", ts(as = "Option<serde_json::Value>"))]
    pub data: Option<Box<RawValue>>,
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    /// A line, in a form tests can compare.
    #[derive(Debug, PartialEq, Eq)]
    enum Seen {
        Whole(Vec<u8>),
        TooBig(Vec<u8>, usize),
    }

    fn seen(lines: Vec<Line>) -> Vec<Seen> {
        (lines.into_iter())
            .map(|line| match line {
                Line::Whole(bytes) => Seen::Whole(bytes),
                Line::TooBig { head, len } => Seen::TooBig(head, len),
            })
            .collect()
    }

    /// What `pieces`, pushed one after another and then finished, make.
    fn frame(biggest: usize, pieces: &[&[u8]]) -> Vec<Seen> {
        let mut framer = Framer::new(biggest);
        let mut out = Vec::new();
        for piece in pieces {
            framer.push(piece, &mut out);
        }
        framer.finish(&mut out);
        seen(out)
    }

    fn whole(bytes: &[u8]) -> Seen {
        Seen::Whole(bytes.to_vec())
    }

    fn parsed(line: &str) -> Frame {
        parse(Line::Whole(line.as_bytes().to_vec()))
    }

    #[test]
    fn lines_are_split_at_newlines() {
        assert_eq!(
            frame(100, &[b"a\nbc\n\nd"]),
            [whole(b"a"), whole(b"bc"), whole(b""), whole(b"d")]
        );
        assert_eq!(frame(100, &[b"a\n"]), [whole(b"a")]);
        assert_eq!(frame(100, &[b""]), []);
        assert_eq!(frame(100, &[b"\n"]), [whole(b"")]);
    }

    #[test]
    fn a_stream_split_anywhere_makes_the_same_lines() {
        let stream: &[u8] = b"{\"id\":1}\r\n\n\xff\xfe junk\nabcdefghij\nabcdefghijk\nlong line past the limit\nend";
        let biggest = 10;
        let expected = frame(biggest, &[stream]);
        assert_eq!(
            expected,
            [
                whole(b"{\"id\":1}\r"),
                whole(b""),
                whole(b"\xff\xfe junk"),
                whole(b"abcdefghij"),
                Seen::TooBig(b"abcdefghijk".to_vec(), 11),
                Seen::TooBig(b"long line past the limit".to_vec(), 24),
                whole(b"end"),
            ]
        );
        for split in 0..=stream.len() {
            let (a, b) = stream.split_at(split);
            assert_eq!(frame(biggest, &[a, b]), expected, "split at {split}");
            for second in split..=stream.len() {
                let (b, c) = stream[split..].split_at(second - split);
                assert_eq!(
                    frame(biggest, &[a, b, c]),
                    expected,
                    "split at {split} and {second}"
                );
            }
        }
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(frame(biggest, &bytes), expected, "a byte at a time");
    }

    #[test]
    fn a_line_of_exactly_the_limit_is_accepted() {
        assert_eq!(
            frame(3, &[b"abc\nabcd\n"]),
            [whole(b"abc"), Seen::TooBig(b"abcd".to_vec(), 4)]
        );
        assert_eq!(frame(3, &[b"ab", b"c", b"\n"]), [whole(b"abc")]);
        assert_eq!(
            frame(3, &[b"ab", b"cd", b"\n"]),
            [Seen::TooBig(b"abcd".to_vec(), 4)]
        );
        assert_eq!(
            frame(0, &[b"\na\n"]),
            [whole(b""), Seen::TooBig(b"a".to_vec(), 1)]
        );
    }

    #[test]
    fn a_line_too_big_keeps_its_first_64_kib_and_its_length() {
        let len = HEAD * 3 + 17;
        let mut stream = vec![b'x'; len];
        stream.extend_from_slice(b"\nnext\n");
        for piece in [1000, HEAD - 1, HEAD, HEAD + 1, stream.len()] {
            let pieces: Vec<&[u8]> = stream.chunks(piece).collect();
            let lines = frame(HEAD / 2, &pieces);
            assert_eq!(
                lines,
                [Seen::TooBig(vec![b'x'; HEAD], len), whole(b"next")],
                "pieces of {piece}"
            );
        }
    }

    #[test]
    fn a_line_too_big_lets_go_of_what_it_held() {
        let mut framer = Framer::new(100);
        let mut out = Vec::new();
        framer.push(&[b'x'; 99], &mut out);
        assert_eq!(framer.line.len(), 99);
        framer.push(&[b'x'; 2], &mut out);
        assert!(framer.line.is_empty() && framer.line.capacity() == 0);
        framer.push(&[b'x'; 1000], &mut out);
        assert!(out.is_empty() && framer.line.capacity() == 0);
        framer.push(b"\n", &mut out);
        assert_eq!(seen(out), [Seen::TooBig(vec![b'x'; 1101], 1101)]);
    }

    #[test]
    fn an_unfinished_line_too_big_is_finished_as_too_big() {
        assert_eq!(frame(2, &[b"abc"]), [Seen::TooBig(b"abc".to_vec(), 3)]);
    }

    /// Fed in small pieces, a huge line costs one pass. If it were looked
    /// through from its start on every push, this would take hours.
    #[test]
    fn a_huge_line_is_never_looked_through_again() {
        let mut framer = Framer::new(64 << 20);
        let mut out = Vec::new();
        let piece = [b'x'; 1024];
        for _ in 0..(32 << 10) {
            framer.push(&piece, &mut out);
        }
        framer.push(b"\n", &mut out);
        assert!(matches!(&out[..], [Line::Whole(line)] if line.len() == 32 << 20));
    }

    #[test]
    fn requests_notifications_and_answers_are_sorted_by_shape() {
        let Frame::Message(Message::Request { id, method, params }) = parsed(
            r#"{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{"a":[1, 2]}}"#,
        ) else {
            panic!("not a request");
        };
        assert_eq!(
            (id, method.as_str()),
            (RequestId::Number(7), "session/request_permission")
        );
        assert_eq!(params.unwrap().get(), r#"{"a":[1, 2]}"#);

        let Frame::Message(Message::Notification { method, params }) =
            parsed(r#"{"jsonrpc":"2.0","method":"session/update"}"#)
        else {
            panic!("not a notification");
        };
        assert_eq!(method, "session/update");
        assert!(params.is_none());

        let Frame::Message(Message::Response { id, outcome }) =
            parsed(r#"{"jsonrpc":"2.0","id":"a","result":{"x":1}}"#)
        else {
            panic!("not an answer");
        };
        assert_eq!(id, RequestId::String("a".to_owned()));
        assert_eq!(outcome.unwrap().get(), r#"{"x":1}"#);
    }

    #[test]
    fn a_null_result_is_a_result() {
        let Frame::Message(Message::Response { outcome, .. }) =
            parsed(r#"{"jsonrpc":"2.0","id":1,"result":null}"#)
        else {
            panic!("not an answer");
        };
        assert_eq!(outcome.unwrap().get(), "null");
        let Frame::Message(Message::Response { outcome, .. }) =
            parsed(r#"{"jsonrpc":"2.0","id":1,"result":null,"error":null}"#)
        else {
            panic!("not an answer");
        };
        assert_eq!(outcome.unwrap().get(), "null");
    }

    #[test]
    fn errors_are_read_with_their_data() {
        let Frame::Message(Message::Response { outcome, .. }) = parsed(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"Authentication required","data":{"errorKind":"auth"}}}"#,
        ) else {
            panic!("not an answer");
        };
        let error = outcome.unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (-32000, "Authentication required")
        );
        assert_eq!(error.data.unwrap().get(), r#"{"errorKind":"auth"}"#);
    }

    #[test]
    fn an_error_beside_a_result_wins() {
        let Frame::Message(Message::Response { outcome, .. }) = parsed(
            r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-32800,"message":"cancelled"}}"#,
        ) else {
            panic!("not an answer");
        };
        assert_eq!(outcome.unwrap_err().code, -32800);
    }

    #[test]
    fn an_answer_with_neither_result_nor_error_is_a_broken_answer() {
        let Frame::Message(Message::Response { id, outcome }) =
            parsed(r#"{"jsonrpc":"2.0","id":3}"#)
        else {
            panic!("not an answer");
        };
        assert_eq!(id, RequestId::Number(3));
        let error = outcome.unwrap_err();
        assert_eq!(error.code, -32603);
        assert!(error.data.is_none());
    }

    #[test]
    fn a_broken_error_is_kept_whole() {
        for broken in [
            r#""no""#,
            r#"{"code":"x","message":"m"}"#,
            r#"{"code":1.5,"message":"m"}"#,
            r#"{"message":"m"}"#,
            "[]",
        ] {
            let Frame::Message(Message::Response { outcome, .. }) =
                parsed(&format!(r#"{{"jsonrpc":"2.0","id":3,"error":{broken}}}"#))
            else {
                panic!("not an answer: {broken}");
            };
            let error = outcome.unwrap_err();
            assert_eq!(error.code, -32603, "{broken}");
            assert_eq!(error.data.unwrap().get(), broken);
        }
    }

    #[test]
    fn ids_may_be_numbers_strings_or_null() {
        let id = |id: &str| match parsed(&format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"m"}}"#))
        {
            Frame::Message(Message::Request { id, .. }) => Some(id),
            Frame::Junk { .. } => None,
            other => panic!("{other:?}"),
        };
        assert_eq!(id("0"), Some(RequestId::Number(0)));
        assert_eq!(id("-5"), Some(RequestId::Number(-5)));
        assert_eq!(id("9223372036854775807"), Some(RequestId::Number(i64::MAX)));
        assert_eq!(
            id("-9223372036854775808"),
            Some(RequestId::Number(i64::MIN))
        );
        assert_eq!(id(r#""x\"y""#), Some(RequestId::String("x\"y".to_owned())));
        assert_eq!(id(r#""""#), Some(RequestId::String(String::new())));
        assert_eq!(id("null"), Some(RequestId::Null));
        // A whole number written with a fraction of zeros is the number.
        assert_eq!(id("5.0"), Some(RequestId::Number(5)));
        assert_eq!(id("5.000"), Some(RequestId::Number(5)));
        assert_eq!(id("-0.0"), Some(RequestId::Number(0)));
        assert_eq!(
            id("9223372036854775807.0"),
            Some(RequestId::Number(i64::MAX))
        );
        // Anything else can't be answered faithfully.
        assert_eq!(id("1.5"), None);
        assert_eq!(id("9223372036854775808"), None);
        assert_eq!(id("18446744073709551615"), None);
        assert_eq!(id("1e300"), None);
        // Read from the text, never through a float.
        assert_eq!(
            id("9007199254740991.0"),
            Some(RequestId::Number(9_007_199_254_740_991))
        );
        assert_eq!(
            id("9007199254740993.0"),
            Some(RequestId::Number(9_007_199_254_740_993))
        );
        assert_eq!(
            id("9007199254740993"),
            Some(RequestId::Number(9_007_199_254_740_993))
        );
        assert_eq!(id("1.0000000000000001"), None);
        assert_eq!(id("1e3"), None);
        assert_eq!(id("1.0e0"), None);
        assert_eq!(id("9223372036854775808.0"), None);
        assert_eq!(id("true"), None);
        assert_eq!(id("{}"), None);
        assert_eq!(id("[1]"), None);
    }

    #[test]
    fn an_answer_with_a_null_id_is_an_answer() {
        let Frame::Message(Message::Response { id, outcome }) = parsed(
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#,
        ) else {
            panic!("not an answer");
        };
        assert_eq!(id, RequestId::Null);
        assert_eq!(outcome.unwrap_err().code, -32700);
    }

    #[test]
    fn lines_that_arent_json_rpc_are_junk() {
        for line in [
            "",
            "   ",
            "\r",
            "hello",
            "Debugger listening on ws://127.0.0.1:9229",
            "{",
            "{}",
            r#"{"jsonrpc":"2.0"}"#,
            r#"{"level":"info","msg":"started"}"#,
            "42",
            r#""text""#,
            "null",
            // A batch: not read yet.
            r#"[{"jsonrpc":"2.0","method":"a"},{"jsonrpc":"2.0","method":"b"}]"#,
            r#"{"jsonrpc":"1.0","id":1,"method":"m"}"#,
            r#"{"jsonrpc":2.0,"id":1,"method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":5}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"m","method":"n"}"#,
            r#"{"jsonrpc":"2.0","method":"m"} trailing"#,
        ] {
            assert!(
                matches!(parsed(line), Frame::Junk { len } if len == line.len()),
                "{line:?}"
            );
        }
    }

    #[test]
    fn jsonrpc_may_be_missing() {
        assert!(matches!(
            parsed(r#"{"method":"m"}"#),
            Frame::Message(Message::Notification { .. })
        ));
    }

    #[test]
    fn a_carriage_return_before_the_newline_is_space() {
        let frames = frame(100, &[b"{\"jsonrpc\":\"2.0\",\"method\":\"m\"}\r\n"]);
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            parsed("{\"jsonrpc\":\"2.0\",\"method\":\"m\"}\r"),
            Frame::Message(Message::Notification { .. })
        ));
        assert!(matches!(
            parsed("  {\"jsonrpc\":\"2.0\",\"method\":\"m\"} \t"),
            Frame::Message(Message::Notification { .. })
        ));
    }

    #[test]
    fn bad_utf8_in_a_string_is_mended() {
        let mut line =
            br#"{"jsonrpc":"2.0","method":"session/update","params":{"text":"a"#.to_vec();
        line.extend_from_slice(b"\xff\xc3");
        line.extend_from_slice(br#"b"}}"#);
        let Frame::Message(Message::Notification { params, .. }) = parse(Line::Whole(line)) else {
            panic!("not a notification");
        };
        let params: Value = serde_json::from_str(params.unwrap().get()).unwrap();
        assert_eq!(params["text"], "a\u{fffd}\u{fffd}b");

        let mut line = br#"{"jsonrpc":"2.0","id":"#.to_vec();
        line.extend_from_slice(b"\"\xe2\x82\"");
        line.extend_from_slice(br#","result":1}"#);
        let Frame::Message(Message::Response { id, .. }) = parse(Line::Whole(line)) else {
            panic!("not an answer");
        };
        assert_eq!(id, RequestId::String("\u{fffd}".to_owned()));
    }

    #[test]
    fn bad_utf8_outside_a_string_is_junk() {
        let line = b"{\"jsonrpc\":\"2.0\",\xff\"method\":\"m\"}".to_vec();
        let len = line.len();
        assert!(matches!(parse(Line::Whole(line)), Frame::Junk { len: junk } if junk == len));
    }

    fn head(head: &str) -> (Option<RequestId>, Option<String>) {
        match parse(Line::TooBig {
            head: head.as_bytes().to_vec(),
            len: 1 << 30,
        }) {
            Frame::TooBig {
                id, method, len, ..
            } => {
                assert_eq!(len, 1 << 30);
                (id, method)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_too_big_line_gives_its_id_and_method_if_its_head_has_them() {
        let number = Some(RequestId::Number(12));
        let fs = Some("fs/write_text_file".to_owned());
        assert_eq!(
            head(
                r#"{"jsonrpc":"2.0","id":12,"method":"fs/write_text_file","params":{"content":"aaa"#
            ),
            (number.clone(), fs.clone())
        );
        assert_eq!(
            head(r#"{"method":"fs/write_text_file","id":12,"params":{"#),
            (number.clone(), fs.clone())
        );
        assert_eq!(
            head(r#"{"jsonrpc":"2.0","id":12,"result":{"content":"aaa"#),
            (number.clone(), None)
        );
        assert_eq!(
            head(r#"{"id":"abc","method":"m","params":["#),
            (
                Some(RequestId::String("abc".to_owned())),
                Some("m".to_owned())
            )
        );
        assert_eq!(
            head(r#"{"id":null,"result":"#),
            (Some(RequestId::Null), None)
        );
        // Its id isn't taken from inside its params.
        assert_eq!(
            head(r#"{"method":"m","params":{"id":5,"x":"aaa"#),
            (None, Some("m".to_owned()))
        );
        assert_eq!(
            head(r#"{"method":"m","params":{"id":5},"id":6,"x":"aaa"#),
            (Some(RequestId::Number(6)), Some("m".to_owned()))
        );
    }

    #[test]
    fn a_head_cut_off_mid_value_keeps_only_what_it_finished() {
        // `12` may have been `123`.
        assert_eq!(
            head(r#"{"method":"m","id":12"#),
            (None, Some("m".to_owned()))
        );
        assert_eq!(
            head(r#"{"method":"m","id":12,"#),
            (None, Some("m".to_owned()))
        );
        assert_eq!(
            head(r#"{"method":"m","id":12,"p"#),
            (None, Some("m".to_owned()))
        );
        assert_eq!(
            head(r#"{"method":"m","id":12,"p":"#),
            (Some(RequestId::Number(12)), Some("m".to_owned()))
        );
        assert_eq!(head(r#"{"id":"ab"#), (None, None));
        assert_eq!(
            head(r#"{"id":1,"method":"fs/wri"#),
            (Some(RequestId::Number(1)), None)
        );
        assert_eq!(
            head(r#"{"id":1.5,"method":"m","p":"#),
            (None, Some("m".to_owned()))
        );
    }

    #[test]
    fn a_head_that_isnt_json_rpc_gives_nothing() {
        assert_eq!(head(""), (None, None));
        assert_eq!(head("xxxxxxxx"), (None, None));
        assert_eq!(head(r#"[{"id":1,"method":"m","params":"#), (None, None));
    }

    #[test]
    fn a_head_cut_mid_character_is_still_read() {
        let mut bytes = br#"{"id":4,"method":"m","params":{"text":""#.to_vec();
        bytes.extend_from_slice("é".as_bytes().get(..1).unwrap());
        let Frame::TooBig { id, method, .. } = parse(Line::TooBig {
            head: bytes,
            len: 1 << 30,
        }) else {
            panic!("not too big");
        };
        assert_eq!(
            (id, method),
            (Some(RequestId::Number(4)), Some("m".to_owned()))
        );
    }

    #[test]
    fn a_framed_too_big_line_is_parsed_from_its_head() {
        let mut stream =
            br#"{"jsonrpc":"2.0","id":9,"method":"fs/write_text_file","params":{"content":""#
                .to_vec();
        stream.resize(HEAD * 2, b'a');
        stream.extend_from_slice(b"\"}}\n");
        let mut framer = Framer::new(HEAD);
        let mut out = Vec::new();
        for piece in stream.chunks(4096) {
            framer.push(piece, &mut out);
        }
        let [line] = <[Line; 1]>::try_from(out).unwrap();
        let Frame::TooBig {
            id,
            method,
            shape,
            len,
        } = parse(line)
        else {
            panic!("not too big");
        };
        assert_eq!(id, Some(RequestId::Number(9)));
        assert_eq!(method.as_deref(), Some("fs/write_text_file"));
        assert_eq!(len, HEAD * 2 + 3);
        assert_eq!(shape, Shape::Call);
    }

    fn shape(head: &str) -> Shape {
        match parse(Line::TooBig {
            head: head.as_bytes().to_vec(),
            len: 1 << 30,
        }) {
            Frame::TooBig { shape, .. } => shape,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_too_big_line_says_whether_its_head_looked_like_an_answer_or_a_call() {
        assert_eq!(
            shape(r#"{"jsonrpc":"2.0","id":3,"result":{"content":"aaa"#),
            Shape::Answer
        );
        assert_eq!(
            shape(r#"{"jsonrpc":"2.0","id":3,"error":{"code":"#),
            Shape::Answer
        );
        assert_eq!(
            shape(r#"{"jsonrpc":"2.0","id":3,"method":"m","params":{"#),
            Shape::Call
        );
        assert_eq!(
            shape(r#"{"jsonrpc":"2.0","method":"session/update","params":{"#),
            Shape::Call
        );
        // A request whose method comes after its params, past the head.
        assert_eq!(
            shape(r#"{"jsonrpc":"2.0","id":3,"params":{"content":"aaa"#),
            Shape::Call
        );
        assert_eq!(shape(r#"{"jsonrpc":"2.0","id":3,"x":"aaa"#), Shape::Unknown);
        assert_eq!(shape(r#"{"id":3,"result":1,"params":"#), Shape::Unknown);
        assert_eq!(shape("xxxx"), Shape::Unknown);
        // Keys inside params don't count.
        assert_eq!(
            shape(r#"{"id":3,"params":{"result":1,"error":"#),
            Shape::Call
        );
        // A key whose value was cut off counts.
        assert_eq!(shape(r#"{"id":3,"result""#), Shape::Answer);
    }
}
