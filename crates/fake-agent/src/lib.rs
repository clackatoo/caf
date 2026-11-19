//! A fake ACP adapter for tests, which does what a script says.
//!
//! An *adapter* is the program CAF starts to talk to an agent, such as
//! `claude-agent-acp`. It speaks ACP, the [Agent Client
//! Protocol](https://agentclientprotocol.com), over its stdin and stdout:
//! one JSON-RPC 2.0 message per line. JSON-RPC has three kinds of
//! message: a *request* (a method and an id) wants an *answer* (the same
//! id, with a result or an error), and a *notification* (a method, no id)
//! wants nothing. CAF is the adapter's *client*.
//!
//! `caf-fake-agent` is an adapter with no model behind it. It follows a
//! script, so a test can make it behave as Claude's adapter does, or
//! misbehave on purpose: never answer, answer with errors, write junk,
//! exit, or hold its pipes open after it exits.
//!
//! ```text
//! caf-fake-agent SCRIPT [--record RECORD]
//! ```
//!
//! Both are absolute paths: the fake never reads a path against its own
//! working directory, which a test may not know.
//!
//! # Scripts
//!
//! A script is a file of JSON lines, one [`Step`] each, done in order.
//! Blank lines and lines starting with `#` are skipped. A step is an
//! object named by its only key, or a bare string if it has no settings:
//!
//! ```text
//! # Start as Claude's adapter does, then answer one prompt.
//! {"wait": {"method": "initialize"}}
//! {"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}, "agentInfo": {"name": "fake", "version": "1.0.0"}}}}
//! {"wait": {"method": "session/new"}}
//! {"respond": {"result": {"sessionId": "s1"}}}
//! {"wait": {"method": "session/prompt", "as": "prompt"}}
//! {"think": {"session": "s1", "text": "Reading the tests."}}
//! {"say": {"session": "s1", "text": "Hello"}}
//! {"update": {"session": "s1", "update": {"sessionUpdate": "usage_update", "used": 1200, "size": 200000}}}
//! {"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}}
//! ```
//!
//! | Step | What it does |
//! |---|---|
//! | `{"wait": {"method", "match", "as", "timeout"}}` | Waits for a request or notification ([`Wait`]) |
//! | `{"respond": {"to", "id", "result" or "error"}}` | Answers a request ([`Respond`]) |
//! | `{"notify": {"method", "params"}}` | Sends a notification ([`Notify`]) |
//! | `{"update": {"session", "update"}}` | Sends `session/update` ([`Update`]) |
//! | `{"say": {"session", "text"}}`, `{"think": …}` | Sends an `agent_message_chunk` or `agent_thought_chunk` ([`Chunk`]) |
//! | `{"request": {"method", "params", "id", "as"}}` | Sends a request ([`Request`]) |
//! | `{"await": {"to", "expect", "timeout"}}` | Waits for the answer to one, and checks it ([`Await`]) |
//! | `{"cancel_request": {"to"}}` | Withdraws one, with `$/cancel_request` ([`CancelRequest`]) |
//! | `{"on": {"method", "match", "as", "once", "do"}}` | Adds a handler ([`On`]) |
//! | `{"sleep": MS}` | Sleeps, reading meanwhile |
//! | `{"line": TEXT}` | Writes a line of junk to stdout |
//! | `{"json": VALUE}` | Writes any JSON as a line |
//! | `{"bytes": [N, …]}` | Writes these bytes, exactly: invalid UTF-8, half a line |
//! | `{"long_line": {"len", "message"}}` | Writes a line of exactly `len` bytes ([`LongLine`]) |
//! | `{"stderr": TEXT}` | Writes a line to stderr |
//! | `{"note": TEXT}` | Writes a note to the record |
//! | `{"exit": CODE}` | Exits |
//! | `"close_stdout"`, `"close_stderr"` | Closes one, and carries on |
//! | `"wait_closed"` | Waits until stdin closes, and carries on |
//! | `"hang"` | Does nothing more, ever, but run handlers |
//! | `{"spawn_holder": {"pid_file", "secs"}}` | Starts a process holding stdout and stderr, before any `close_stdout` ([`SpawnHolder`]) |
//! | `{"include": PATH}` | Puts another script's steps here |
//!
//! Settings left out take their defaults, but a step that has settings
//! still needs its object: `{"cancel_request": {}}`, not
//! `"cancel_request"`.
//!
//! The whole script, with what it includes, is read and checked before
//! anything happens: a wrong one makes the fake exit with
//! [`EXIT_BAD_SCRIPT`], having done nothing.
//!
//! ## Names
//!
//! Steps refer to messages by name. A request or notification waited for
//! is named by its method, or by `as`; so is a request the fake sends.
//! `respond` answers the one named `to`, or the one last waited for;
//! `await` and `cancel_request` take the one named `to`, or the one last
//! sent. A request is answered once: answering it again does nothing, so
//! a handler and the script can both end a prompt. `respond` with an `id`
//! answers that id, as is, whatever the client sent, for answers to
//! nothing and answers given twice.
//!
//! The fake's own requests count up from 0, unless a request gives its
//! `id`.
//!
//! ## Patterns
//!
//! `match` and `expect` are patterns. A pattern matches a message that
//! has everything it has: an object matches if each of its keys is there
//! and matches, whatever else is there; an array matches an array of the
//! same length, item by item; anything else must be equal. So `{"params":
//! {"sessionId": "s1"}}` matches any message about session `s1`, and
//! `{"error": {"code": -32800}}` any answer cancelled.
//!
//! ## Reading, waiting and handlers
//!
//! The fake reads stdin all the time, on a thread of its own, so it never
//! stops reading while it writes. What comes is kept in order:
//! - a request or notification waits in a queue until a `wait` or a
//!   handler takes it. A `wait` takes the first that matches, so one that
//!   came early isn't lost, and those it passes over stay queued;
//! - an answer waits, by its id, until an `await` takes it.
//!
//! Handlers run only while the script waits, sleeps or is over, so a
//! script's writes are never interleaved with a handler's. A handler
//! takes every queued message it matches, before any `wait` sees it, and
//! the first handler added wins. Within a handler, `respond` answers the
//! message that set it off. A handler's steps can't wait.
//!
//! ## The end
//!
//! - **The script ends:** the fake reads on, running handlers, until
//!   stdin closes, then exits with 0.
//! - **Stdin closes while a `wait` or `await` waits:** the fake exits with
//!   0, as Claude's adapter does when its client closes its input.
//!   `"wait_closed"` then `"hang"` makes one that doesn't.
//! - **Stdout can't be written,** because nobody reads it: the fake exits
//!   with 0. After `"close_stdout"`, writes to stdout are skipped.
//! - **A check fails** (an `await` whose answer doesn't match, a timeout,
//!   a name never given): the fake says why on stderr and in the record,
//!   and exits with [`EXIT_CHECK_FAILED`].
//!
//! # The record
//!
//! Given `--record`, the fake appends a line of JSON to that file for
//! each thing that happens, as it happens, so a test can see what its
//! client sent even if the fake never ends:
//!
//! | Line | When |
//! |---|---|
//! | `{"received": MESSAGE}` | A message from the client |
//! | `{"unreadable": TEXT}` | A line from the client that isn't a JSON object |
//! | `{"closed": "stdin"}` | The client closed stdin |
//! | `{"note": TEXT}` | A `note` step |
//! | `{"skipped": "answered already", "id": ID}` | A `respond` to a request already answered |
//! | `{"failed": WHY}` | A check failed |
//! | `{"gone": WHY}` | Stdout couldn't be written |
//!
//! # Examples
//!
//! Stop: the prompt ends `cancelled` when the client sends
//! `session/cancel`, even mid-stream. The script's own answer then does
//! nothing.
//!
//! ```text
//! {"wait": {"method": "session/prompt", "as": "prompt"}}
//! {"on": {"method": "session/cancel", "once": true, "do": [{"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}}]}}
//! {"say": {"session": "s1", "text": "one "}}
//! {"sleep": 1000}
//! {"say": {"session": "s1", "text": "two"}}
//! {"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}}
//! ```
//!
//! Asking permission, and checking the client allowed it:
//!
//! ```text
//! {"request": {"method": "session/request_permission", "as": "ask", "params": {"sessionId": "s1", "toolCall": {"toolCallId": "t1", "title": "Run ls"}, "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}, {"optionId": "reject", "name": "Reject", "kind": "reject_once"}]}}}
//! {"await": {"to": "ask", "expect": {"result": {"outcome": {"outcome": "selected", "optionId": "allow"}}}, "timeout": 5000}}
//! ```
//!
//! Loading a session replays its history before the answer, from a file
//! of steps:
//!
//! ```text
//! {"wait": {"method": "session/load", "match": {"params": {"sessionId": "past-1"}}}}
//! {"include": "/tmp/test-1234/past-1.jsonl"}
//! {"respond": {"result": {}}}
//! ```
//!
//! A sign-in error with data, then a crash, leaving a process that holds
//! the pipes:
//!
//! ```text
//! {"wait": {"method": "session/new"}}
//! {"respond": {"error": {"code": -32000, "message": "Authentication required", "data": {"errorKind": "auth"}}}}
//! {"spawn_holder": {"pid_file": "/tmp/test-1234/holder.pid"}}
//! {"stderr": "Please run /login"}
//! {"exit": 1}
//! ```
//!
//! A test can also build a script in Rust, and write it with
//! [`Script::to_text`]:
//!
//! ```
//! use caf_fake_agent::{Respond, Script, Step, Wait};
//! use serde_json::json;
//!
//! let script = Script::new(vec![
//!     Step::Wait(Wait { method: Some("initialize".into()), ..Wait::default() }),
//!     Step::Respond(Respond { result: Some(json!({"protocolVersion": 1})), ..Respond::default() }),
//! ]);
//! assert_eq!(Script::parse(&script.to_text()), Ok(script));
//! ```
//!
//! Its types aren't `#[non_exhaustive]`, unlike CAF's, so other crates
//! can build them.

mod pattern;
mod run;
mod script;

pub use run::run;
pub use script::{
    Await, CancelRequest, Chunk, LongLine, Notify, On, Request, Respond, RpcError, Script,
    ScriptError, SpawnHolder, Step, Update, Wait,
};

/// The exit code when the script or the arguments are wrong.
pub const EXIT_BAD_SCRIPT: i32 = 2;

/// The exit code when one of the script's checks fails.
pub const EXIT_CHECK_FAILED: i32 = 3;
