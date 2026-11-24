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
//! A test that starts the fake through a driver, which may add arguments
//! of its own, can give both in the environment instead:
//! [`SCRIPT_VAR`] (`CAF_FAKE_AGENT_SCRIPT`) and [`RECORD_VAR`]
//! (`CAF_FAKE_AGENT_RECORD`). Then every argument is ignored, and the
//! record's first line says how it was started.
//!
//! Tests in this crate find the program in `CARGO_BIN_EXE_caf-fake-agent`.
//! Cargo gives that only to the crate's own tests, so tests elsewhere call
//! [`binary`], which finds it, and builds it first if it's missing or
//! stale; or set [`BIN_VAR`] (`CAF_FAKE_AGENT_BIN`) to use a program
//! built already.
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
//! | `{"wait": {"method", "match", "expect", "as", "timeout"}}` | Waits for a request or notification ([`Wait`]) |
//! | `{"respond": {"to", "id", "id_from", "result" or "error"}}` | Answers a request ([`Respond`]) |
//! | `{"notify": {"method", "params"}}` | Sends a notification ([`Notify`]) |
//! | `{"update": {"session", "update"}}` | Sends `session/update` ([`Update`]) |
//! | `{"say": {"session", "text"}}`, `{"think": …}` | Sends an `agent_message_chunk` or `agent_thought_chunk` ([`Chunk`]) |
//! | `{"tool": {"session", "id", "update", "title", "kind", "status", "tool_name", "content", "locations", "raw_input", "raw_output"}}` | Sends a `tool_call`, or a `tool_call_update` ([`Tool`]) |
//! | `{"request": {"method", "params", "id", "as"}}` | Sends a request ([`Request`]) |
//! | `{"await": {"to", "expect", "timeout"}}` | Waits for the answer to one, and checks it ([`Await`]) |
//! | `{"cancel_request": {"to"}}` | Withdraws one, with `$/cancel_request` ([`CancelRequest`]) |
//! | `{"on": {"method", "match", "as", "once", "fallback", "do"}}` | Adds a handler ([`On`]) |
//! | `{"sleep": MS}` | Sleeps, reading meanwhile |
//! | `{"line": TEXT}` | Writes a line of junk to stdout |
//! | `{"json": VALUE}` | Writes any JSON as a line |
//! | `{"bytes": [N, …]}` | Writes these bytes, exactly: invalid UTF-8, half a line |
//! | `{"long_line": {"len", "message", "to"}}` | Writes a line of exactly `len` bytes, to stdout or stderr ([`LongLine`]) |
//! | `{"stderr": TEXT}` | Writes a line to stderr |
//! | `{"note": TEXT}` | Writes a note to the record |
//! | `{"record_env": [NAME, …]}` | Writes these environment variables' values to the record |
//! | `{"exit": CODE}` | Exits |
//! | `{"signal": N}` | Sends itself signal N, such as 9 to die by `SIGKILL` or 15 by `SIGTERM` |
//! | `"stop_reading"`, `"resume_reading"` | Stops reading stdin, so the client's writes block, and starts again |
//! | `"close_stdout"`, `"close_stderr"` | Closes one, and carries on |
//! | `"wait_closed"` | Waits until stdin closes, and carries on |
//! | `{"wait_file": {"path", "timeout"}}` | Waits until a file is there, so the test says when to go on ([`WaitFile`]) |
//! | `"hang"` | Does nothing more, ever, but run handlers |
//! | `{"spawn_holder": {"pid_file", "secs"}}` | Starts a process holding stdout and stderr, before any `close_stdout`, until its pid file goes ([`SpawnHolder`]) |
//! | `{"include": PATH}` | Puts another script's steps here, or a handler's |
//! | `{"mcp_start": {"from", "server", "as", "env", "discover", "version", "expect", "timeout"}}` | Starts an MCP server the client named, and does the handshake ([`McpStart`]) |
//! | `{"mcp_list": {"server", "expect", "timeout"}}` | Asks a server for its tools, and checks their names ([`McpList`]) |
//! | `{"mcp_call": {"server", "tool", "arguments", "tool_use_id", "no_progress", "report", "as"}}` | Calls a server's tool ([`McpCall`]) |
//! | `{"mcp_await": {"to", "expect", "none", "timeout"}}` | Waits for a call's answer, and checks it, or checks none comes ([`McpAwait`]) |
//! | `{"mcp_progress": {"to", "expect", "timeout"}}` | Waits for a call's progress ([`McpProgress`]) |
//! | `{"mcp_cancel": {"to", "reason"}}` | Cancels a call ([`McpCancel`]) |
//! | `{"mcp_drop": {"server"}}` | Closes a server's stdin ([`McpDrop`]) |
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
//! nothing and answers given twice. `respond` with `id_from` answers the
//! request whose id the message holds there: a handler on
//! `$/cancel_request` answers the request the client cancelled with
//! `{"respond": {"id_from": "/params/requestId", "error": {"code":
//! -32800, "message": "Cancelled"}}}`.
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
//! stops reading while it writes, unless the script says
//! `"stop_reading"`. What comes is kept in order:
//! - a request or notification waits in a queue until a `wait` or a
//!   handler takes it. A `wait` takes the first that matches, so one that
//!   came early isn't lost, and those it passes over stay queued;
//! - an answer waits, by its id, until an `await` takes it.
//!
//! Handlers run only while the script waits or sleeps (in `wait`,
//! `await`, `sleep`, `wait_closed`, `wait_file` and `hang`), or is over,
//! so a
//! script's writes are never interleaved with a handler's. A handler
//! takes every queued message it matches, before any `wait` sees it, and
//! the first handler added wins. A *fallback* handler comes after the
//! others, and takes only what nothing later in the script would take,
//! neither a `wait` nor a handler that isn't a fallback: so `{"on":
//! {"fallback": true, "do": [...]}}` can refuse whatever the script
//! doesn't expect, as a real adapter refuses methods it doesn't know. A
//! later `{"wait": {}}` would take anything, so until it's done,
//! fallbacks take nothing.
//!
//! Within a handler, `respond` answers the message that set it off, and
//! its name is the handler's own: once the handler is done, the script's
//! names mean what they meant before. In a fallback, a `respond` to a
//! notification does nothing, so one fallback can refuse requests and let
//! notifications be; anywhere else, it's a failed check. A handler's
//! steps can't wait or sleep.
//!
//! The client's answers are kept by id: if it answers one id twice
//! before an `await` takes it, the second replaces the first, and only
//! the record shows both.
//!
//! ## MCP servers
//!
//! The fake is an MCP client too, as Claude is. Claude starts each server
//! a session names, and calls its tools as the model asks; the fake does
//! when the script says. `mcp_start` starts a stdio server named in the
//! `mcpServers` of a `session/new`, `session/load` or `session/resume`
//! waited for: its command, with its arguments, and the fake's own
//! environment with the server's `env` added, then the script's `env`.
//! Then it does MCP's handshake: `initialize`, then
//! `notifications/initialized`. With `discover`, it first sends
//! `server/discover`, as Claude probes a server, and checks that's
//! refused with -32601.
//!
//! Each server's stdout is read on a thread of its own, all the time, as
//! stdin is. A step waiting for a server runs handlers as `wait` does, and
//! fails if the server closes its stdout first; stdin closing ends the
//! fake meanwhile, as it does a `wait`.
//!
//! A server's connection is named by `as`, or by the server's name, which
//! is `caf` unless a step says: `mcp_start` can start the same server
//! twice, under two names, as Claude may while it opens a session again.
//! A call is named by `as`, or by its tool; `mcp_await`, `mcp_progress`
//! and `mcp_cancel` take the one named `to`, or the one last made. Each
//! connection's requests count up from 0.
//!
//! `mcp_call` sends `tools/call` as Claude does: the tool's name and
//! arguments, and in `_meta` the model's id for the call,
//! `claudecode/toolUseId`, if it has a `tool_use_id`, and a
//! `progressToken`, the request's id, unless `no_progress`. With
//! `report`, it first reports the call in that session as Claude's adapter
//! does, so CAF can join the two: a `tool_call`, pending, whose
//! `toolCallId` is the `tool_use_id`, and whose title and
//! `_meta.claudeCode.toolName` are `mcp__caf__Read` for `caf`'s `Read`.
//!
//! `mcp_await` with `none` checks no answer comes in its `timeout`, as
//! none should once a call is cancelled; the server closing passes it
//! too. `mcp_drop` closes a server's stdin, as Claude does when a session
//! ends, and doesn't wait for it to go. Anything else a server sends is
//! in the record, and otherwise let be.
//!
//! Calling CAF's `Read`, then calling `Bash` and cancelling it:
//!
//! ```text
//! {"wait": {"method": "session/new"}}
//! {"respond": {"result": {"sessionId": "s1"}}}
//! {"mcp_start": {"discover": true, "expect": {"result": {"serverInfo": {"name": "caf"}}}, "timeout": 10000}}
//! {"mcp_list": {"expect": ["Read", "Bash"], "timeout": 10000}}
//! {"wait": {"method": "session/prompt", "as": "prompt"}}
//! {"mcp_call": {"tool": "Read", "arguments": {"path": "/work/a.rs"}, "tool_use_id": "toolu_1", "report": "s1"}}
//! {"mcp_await": {"expect": {"result": {"isError": false}}, "timeout": 10000}}
//! {"mcp_call": {"tool": "Bash", "arguments": {"command": "ls"}, "tool_use_id": "toolu_2", "report": "s1"}}
//! {"mcp_cancel": {}}
//! {"mcp_await": {"none": true, "timeout": 500}}
//! {"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}}
//! ```
//!
//! A fake started as an MCP server, by a test's session naming it with
//! [`SCRIPT_VAR`] in its `env`, follows a script as it does as an
//! adapter: MCP is JSON-RPC by lines too.
//!
//! ## The end
//!
//! - **The script ends:** the fake reads on, running handlers, until
//!   stdin closes, then exits with 0.
//! - **Stdin closes while a `wait` or `await` waits:** the fake says so in
//!   the record and exits with 0, as Claude's adapter does when its client
//!   closes its input. `"wait_closed"` then `"hang"` makes one that
//!   doesn't.
//! - **Stdout can't be written,** because nobody reads it: the fake exits
//!   with 0. After `"close_stdout"`, writes to stdout are skipped.
//! - **A check fails** (a `wait` or `await` whose message doesn't match
//!   its `expect`, a timeout of a `wait`, `await` or `wait_file`, a name
//!   never given): the fake says why on
//!   stderr, without waiting for anyone to read it, and in the record,
//!   and exits with [`EXIT_CHECK_FAILED`].
//!
//! # The record
//!
//! Given a record file, the fake appends a line of JSON to it for each
//! thing that happens, as it happens, so a test can see what its client
//! sent even if the fake never ends. It never holds an environment
//! variable's value unless the script asks for it, since values can be
//! secrets and a failing test prints its record. `received`,
//! `unreadable` and `closed` lines are written as stdin is read, so they
//! can fall between the script's own lines either way.
//!
//! | Line | When |
//! |---|---|
//! | `{"started": {"args", "cwd", "env"}}` | It started: its arguments, working directory, and the names of its environment variables |
//! | `{"received": MESSAGE}` | A message from the client |
//! | `{"unreadable": TEXT}` | A line from the client that isn't a JSON object |
//! | `{"closed": "stdin"}` | The client closed stdin |
//! | `{"note": TEXT}` | A `note` step |
//! | `{"env": {NAME: VALUE or null, …}}` | A `record_env` step |
//! | `{"skipped": "answered already", "id": ID}` | A `respond` to a request already answered |
//! | `{"skipped": "a notification", "method": METHOD}` | A fallback's `respond` to a notification |
//! | `{"failed": WHY}` | A check failed |
//! | `{"ended": WHY}` | Stdin closed while the script waited |
//! | `{"gone": WHY}` | Stdout couldn't be written |
//! | `{"mcp_started": {"server", "command", "args", "env"}}` | An MCP server started: its connection's name, its command and arguments, and the names of the environment variables it was given |
//! | `{"mcp_received": {"server", "message"}}` | A message from an MCP server |
//! | `{"mcp_unreadable": {"server", "text"}}` | A line from an MCP server that isn't a JSON object |
//! | `{"mcp_closed": SERVER}` | An MCP server closed its stdout |
//! | `{"mcp_dropped": SERVER}` | An `mcp_drop` step |
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
//! What a turn spent, as Claude's adapter says it: the session's cost so
//! far in a `usage_update`, just before the prompt's answer, and the
//! turn's tokens in the answer, as ACP's `usage` and split by model in
//! `_meta.quota.model_usage`. CAF's fake profile reads both as it reads
//! Claude's; any other profile reads only `usage`. Counts are whole
//! tokens, input uncached, thinking part of output.
//!
//! ```text
//! {"update": {"session": "s1", "update": {"sessionUpdate": "usage_update", "used": 24928, "size": 200000, "cost": {"amount": 0.0125, "currency": "USD"}}}}
//! {"respond": {"to": "prompt", "result": {"stopReason": "end_turn", "usage": {"inputTokens": 2, "cachedReadTokens": 12799, "cachedWriteTokens": 12123, "outputTokens": 4, "totalTokens": 24928}, "_meta": {"quota": {"model_usage": [{"model": "small", "token_count": {"totalTokens": 24928, "inputTokens": 2, "cachedInputTokens": 12799, "cachedWriteTokens": 12123, "outputTokens": 4, "reasoningOutputTokens": 0}}]}}}}}
//! ```
//!
//! Tool calls, as Claude's adapter reports them: an edit, its diff
//! (`old` left out for a new file) and the file it's about, then a web
//! search of the agent's own, giving back a picture, a link, and content
//! of a kind CAF can't read. `tool_name` is what says whose tool it is,
//! as `_meta.claudeCode.toolName`: `mcp__eyespy__look` for an MCP
//! server's, `mcp__caf__Read` for one of CAF's own. A `content` item is
//! one of `{"text": TEXT}`, `{"diff": {"path", "old", "new"}}`,
//! `{"terminal": ID}`, `{"image": {"mime", "data"}}`, `{"link": {"uri",
//! "name"}}`, or `{"raw": CONTENT}`, sent as is.
//!
//! ```text
//! {"tool": {"session": "s1", "id": "t1", "title": "Edit lib.rs", "kind": "edit", "status": "pending", "tool_name": "mcp__eyespy__edit", "raw_input": {"path": "/work/lib.rs"}, "locations": [{"path": "/work/lib.rs", "line": 2}]}}
//! {"tool": {"session": "s1", "id": "t1", "update": true, "status": "completed", "content": [{"diff": {"path": "/work/lib.rs", "old": "a\nb\n", "new": "a\nc\n"}}]}}
//! {"tool": {"session": "s1", "id": "t2", "title": "Search the web", "kind": "fetch", "status": "completed", "tool_name": "WebSearch", "content": [{"image": {"mime": "image/png", "data": "aGVsbG8="}}, {"link": {"uri": "https://example.com", "name": "Example"}}, {"raw": {"type": "hologram"}}]}}
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
//! of steps; and a fallback refuses anything else, as an adapter refuses
//! a method it doesn't know:
//!
//! ```text
//! {"on": {"fallback": true, "do": [{"respond": {"error": {"code": -32601, "message": "Method not found"}}}]}}
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
//!
//! # Signing in
//!
//! ```text
//! caf-fake-agent [SCRIPT] [--cli] auth login|status|logout
//! ```
//!
//! The fake also has the sign-in commands an agent's own command line
//! has, as `claude-agent-acp --cli auth login` does, for testing CAF's
//! setup driver and the views of a sign-in. These come before the
//! script: an adapter's sign-in is run with its environment, which may
//! name one. A script given as the first argument is passed over, as a
//! host that starts the fake with one keeps it when CAF runs its sign-in,
//! adding the recipe's arguments after it. What they do is set by a JSON file, [`Auth`], that
//! [`AUTH_VAR`] (`CAF_FAKE_AGENT_AUTH`) names:
//!
//! ```text
//! {"url": "https://example.com/sign-in", "browser": true, "code": "ABCD-1234",
//!  "paste": true, "expect": "pasted-code", "state": "/tmp/test-1234/signed-in"}
//! ```
//!
//! - **`auth login`**, in this order:
//!   1. prints a token and an email, [`AUTH_SECRET`] and [`AUTH_EMAIL`],
//!      which nothing CAF keeps may ever hold;
//!   2. prints the code, on its error output, after `code_after` (`Your
//!      code: ` unless it says);
//!   3. prints the link after `marker`, if there is one, on its standard
//!      output;
//!   4. runs `$BROWSER` with the link, if `browser`;
//!   5. if `paste`, prompts for a code, and waits for a line on its
//!      standard input, which must be `expect` if that's set;
//!   6. waits for `wait_file`, if there is one, as a sign-in waits for the
//!      user to approve it, up to `timeout` milliseconds;
//!   7. then writes `method` (`claude.ai` unless it says) to `state`, and
//!      exits with `exit` (0 unless it says). A step that fails exits with
//!      1, and signs nobody in.
//! - **`auth status`** prints, as Claude's `auth status --json` does,
//!   `{"loggedIn": true, "authMethod": METHOD, "email": …}` and exits with
//!   0, or `{"loggedIn": false, …}` and exits with 1, as `state` says.
//! - **`auth logout`** removes `state`.

mod auth;
mod binary;
mod pattern;
mod run;
mod script;

pub use auth::{AUTH_EMAIL, AUTH_SECRET, Auth, auth};
pub use binary::binary;
pub use run::run;
pub use script::{
    Await, CancelRequest, Chunk, LongLine, McpAwait, McpCall, McpCancel, McpDrop, McpList,
    McpProgress, McpStart, Notify, On, Output, Request, Respond, RpcError, Script, ScriptError,
    SpawnHolder, Step, Tool, ToolContent, ToolDiff, ToolImage, ToolLink, ToolLocation, Update,
    Wait, WaitFile,
};

/// The environment variable that names the program for [`binary`] to
/// give, an absolute path, so it builds nothing: for test runners that
/// start each test in a process of its own, such as nextest, or builds
/// [`binary`] can't repeat.
pub const BIN_VAR: &str = "CAF_FAKE_AGENT_BIN";

/// The environment variable that names the script, an absolute path. When
/// it's set, the fake's arguments are ignored.
pub const SCRIPT_VAR: &str = "CAF_FAKE_AGENT_SCRIPT";

/// The environment variable that names the record file, an absolute path,
/// when [`SCRIPT_VAR`] is set.
pub const RECORD_VAR: &str = "CAF_FAKE_AGENT_RECORD";

/// The environment variable that names the settings for `auth login`,
/// `auth status` and `auth logout`, a JSON file of an [`Auth`].
pub const AUTH_VAR: &str = "CAF_FAKE_AGENT_AUTH";

/// The exit code when the script or the arguments are wrong.
pub const EXIT_BAD_SCRIPT: i32 = 2;

/// The exit code when one of the script's checks fails.
pub const EXIT_CHECK_FAILED: i32 = 3;
