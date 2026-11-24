//! The fake, run as a program: scripts in, behaviour out.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use caf_fake_agent::{EXIT_BAD_SCRIPT, EXIT_CHECK_FAILED};
use serde_json::{Value, json};

/// Long enough for anything that should happen, on a slow machine.
const SOON: Duration = Duration::from_secs(10);

/// A folder of its own for each test.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(1000);
        let dir = std::env::temp_dir().join(format!(
            "caf-fake-agent-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A running fake, with its output read on threads.
struct Fake {
    child: Child,
    stdin: Option<ChildStdin>,
    /// Its stdout, a line at a time, without the newline. Ends when stdout
    /// does.
    lines: Receiver<Vec<u8>>,
    stderr: Option<JoinHandle<String>>,
    record: PathBuf,
    /// Its folder, removed once it's gone, which ends any holder it
    /// started.
    _dir: Dir,
}

impl Fake {
    fn start(script: &str) -> Self {
        let dir = Dir::new();
        let path = dir.path("script.jsonl");
        std::fs::write(&path, script).unwrap();
        Self::start_in(dir, &path)
    }

    fn start_in(dir: Dir, script: &std::path::Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_caf-fake-agent"));
        command
            .arg(script)
            .arg("--record")
            .arg(dir.path("record.jsonl"));
        Self::spawn(dir, command)
    }

    /// Starts the fake by this command, whose record is `record.jsonl` in
    /// `dir`.
    fn spawn(dir: Dir, mut command: Command) -> Self {
        let record = dir.path("record.jsonl");
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match stdout.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let mut stderr = child.stderr.take().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            stderr: Some(stderr),
            record,
            _dir: dir,
        }
    }

    fn send(&mut self, message: &Value) {
        let mut line = serde_json::to_vec(message).unwrap();
        line.push(b'\n');
        self.stdin.as_mut().unwrap().write_all(&line).unwrap();
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// The next line on stdout.
    fn line(&self) -> Vec<u8> {
        self.lines.recv_timeout(SOON).expect("a line from the fake")
    }

    /// The next message on stdout.
    fn recv(&self) -> Value {
        let line = self.line();
        serde_json::from_slice(&line)
            .unwrap_or_else(|err| panic!("{err}: {}", String::from_utf8_lossy(&line)))
    }

    /// Whether stdout ends within `within`, with nothing more on it.
    fn stdout_ends(&self, within: Duration) -> bool {
        match self.lines.recv_timeout(within) {
            Err(RecvTimeoutError::Disconnected) => true,
            Err(RecvTimeoutError::Timeout) => false,
            Ok(line) => panic!("unexpected: {}", String::from_utf8_lossy(&line)),
        }
    }

    /// Waits for it to exit, killing it if it doesn't.
    fn exit(&mut self) -> ExitStatus {
        let until = Instant::now() + SOON;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() > until {
                let _ = self.child.kill();
                panic!("the fake didn't exit");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Everything it wrote to stderr. Only once it, and anything holding
    /// stderr, has gone.
    fn stderr(&mut self) -> String {
        self.stderr.take().unwrap().join().unwrap()
    }

    /// The record, but for its first line, which says how it started.
    fn record(&self) -> Vec<Value> {
        let mut record = self.whole_record();
        if !record.is_empty() {
            assert!(record.remove(0).get("started").is_some());
        }
        record
    }

    /// The record's lines from the script, not those the thread reading
    /// stdin writes as it reads, which may come between them.
    fn script_record(&self) -> Vec<Value> {
        let mut record = self.record();
        record.retain(|entry| entry.get("received").is_none() && entry.get("closed").is_none());
        record
    }

    /// The record's whole lines: one the fake is still writing may be
    /// read in part, without its newline.
    fn whole_record(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.record).unwrap_or_default();
        let whole = text.rfind('\n').map_or("", |end| &text[..end]);
        whole
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Waits for this line in the record.
    fn wait_for_record(&self, entry: &Value) {
        let until = Instant::now() + SOON;
        while !self.record().contains(entry) {
            assert!(Instant::now() < until, "{entry} never came");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn request(id: u64, method: &str, params: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

const HANDSHAKE: &str = r#"
# Start as Claude's adapter does.
{"wait": {"method": "initialize"}}
{"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true, "promptCapabilities": {"image": true, "embeddedContext": true}}, "agentInfo": {"name": "fake", "title": "Fake Agent", "version": "1.0.0"}, "authMethods": []}}}
{"wait": {"method": "session/new"}}
{"respond": {"result": {"sessionId": "s1"}}}
"#;

fn handshake(fake: &mut Fake) {
    fake.send(&request(
        0,
        "initialize",
        &json!({"protocolVersion": 1, "clientCapabilities": {}}),
    ));
    let init = fake.recv();
    assert_eq!(init["id"], 0);
    assert_eq!(init["result"]["agentInfo"]["name"], "fake");
    fake.send(&request(
        1,
        "session/new",
        &json!({"cwd": "/", "mcpServers": []}),
    ));
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": "s1"}})
    );
}

#[test]
fn a_prompt_streams_then_ends() {
    let mut fake = Fake::start(&format!(
        "{HANDSHAKE}{}",
        r#"
{"wait": {"method": "session/prompt", "as": "prompt"}}
{"think": {"session": "s1", "text": "Reading."}}
{"say": {"session": "s1", "text": "Hello"}}
{"update": {"session": "s1", "update": {"sessionUpdate": "usage_update", "used": 1200, "size": 200000}}}
{"update": {"session": "s1", "update": {"sessionUpdate": "brand_new_thing", "x": 1}}}
{"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}}
"#
    ));
    handshake(&mut fake);
    let prompt = json!({"sessionId": "s1", "prompt": [{"type": "text", "text": "hi"}]});
    fake.send(&request(2, "session/prompt", &prompt));
    let update = |update: Value| json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": update}});
    let text = |kind: &str, text: &str| {
        update(json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}}))
    };
    assert_eq!(fake.recv(), text("agent_thought_chunk", "Reading."));
    assert_eq!(fake.recv(), text("agent_message_chunk", "Hello"));
    assert_eq!(
        fake.recv(),
        update(json!({"sessionUpdate": "usage_update", "used": 1200, "size": 200000}))
    );
    assert_eq!(
        fake.recv(),
        update(json!({"sessionUpdate": "brand_new_thing", "x": 1}))
    );
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "end_turn"}})
    );
    // The script is over: closing stdin ends it, as it ends Claude's
    // adapter.
    fake.close_stdin();
    assert!(fake.stdout_ends(SOON));
    assert_eq!(fake.exit().code(), Some(0));
    let record = fake.record();
    assert_eq!(record[0]["received"]["method"], "initialize");
    assert_eq!(record[2]["received"]["params"], prompt);
    assert_eq!(record.last().unwrap(), &json!({"closed": "stdin"}));
}

#[test]
fn tool_calls_are_reported_as_claudes_adapter_reports_them() {
    let mut fake = Fake::start(&format!(
        "{HANDSHAKE}{}",
        r#"
{"wait": {"method": "session/prompt", "as": "prompt"}}
{"tool": {"session": "s1", "id": "t1", "title": "Edit lib.rs", "kind": "edit", "status": "pending", "tool_name": "mcp__eyespy__edit", "raw_input": {"path": "/work/lib.rs"}, "locations": [{"path": "/work/lib.rs", "line": 2}, {"path": "/work/a.rs"}]}}
{"tool": {"session": "s1", "id": "t1", "update": true, "status": "completed", "content": [{"diff": {"path": "/work/lib.rs", "old": "a\nb\n", "new": "a\nc\n"}}, {"diff": {"path": "/work/new.rs", "new": "x\n"}}, {"terminal": "term-1"}]}}
{"tool": {"session": "s1", "id": "t2", "title": "Search", "status": "completed", "tool_name": "WebSearch", "raw_output": "found", "content": [{"text": "two hits"}, {"image": {"mime": "image/png", "data": "aGVsbG8="}}, {"link": {"uri": "https://example.com", "name": "Example"}}, {"raw": {"type": "hologram"}}]}}
{"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}}
"#
    ));
    handshake(&mut fake);
    let prompt = json!({"sessionId": "s1", "prompt": [{"type": "text", "text": "hi"}]});
    fake.send(&request(2, "session/prompt", &prompt));
    let update = |update: Value| json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": update}});
    assert_eq!(
        fake.recv(),
        update(
            json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit lib.rs", "kind": "edit", "status": "pending",
            "_meta": {"claudeCode": {"toolName": "mcp__eyespy__edit"}}, "rawInput": {"path": "/work/lib.rs"},
            "locations": [{"path": "/work/lib.rs", "line": 2}, {"path": "/work/a.rs"}]})
        )
    );
    assert_eq!(
        fake.recv(),
        update(
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed", "content": [
                {"type": "diff", "path": "/work/lib.rs", "oldText": "a\nb\n", "newText": "a\nc\n"},
                {"type": "diff", "path": "/work/new.rs", "oldText": null, "newText": "x\n"},
                {"type": "terminal", "terminalId": "term-1"},
            ]})
        )
    );
    assert_eq!(
        fake.recv(),
        update(
            json!({"sessionUpdate": "tool_call", "toolCallId": "t2", "title": "Search", "status": "completed",
                "_meta": {"claudeCode": {"toolName": "WebSearch"}}, "rawOutput": "found", "content": [
                {"type": "content", "content": {"type": "text", "text": "two hits"}},
                {"type": "content", "content": {"type": "image", "mimeType": "image/png", "data": "aGVsbG8="}},
                {"type": "content", "content": {"type": "resource_link", "uri": "https://example.com", "name": "Example"}},
                {"type": "hologram"},
            ]})
        )
    );
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "end_turn"}})
    );
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
}

#[test]
fn a_prompt_says_what_it_spent_as_claudes_adapter_does() {
    let cost = json!({"sessionUpdate": "usage_update", "used": 24928, "size": 200000, "cost": {"amount": 0.0125, "currency": "USD"}});
    let result = json!({
        "stopReason": "end_turn",
        "usage": {"inputTokens": 2, "cachedReadTokens": 12799, "cachedWriteTokens": 12123, "outputTokens": 4, "totalTokens": 24928},
        "_meta": {"quota": {"model_usage": [{"model": "small", "token_count": {
            "totalTokens": 24928, "inputTokens": 2, "cachedInputTokens": 12799,
            "cachedWriteTokens": 12123, "outputTokens": 4, "reasoningOutputTokens": 0,
        }}]}},
    });
    let mut fake = Fake::start(&format!(
        "{HANDSHAKE}\n{}\n{}\n{}\n",
        json!({"wait": {"method": "session/prompt", "as": "prompt"}}),
        json!({"update": {"session": "s1", "update": cost}}),
        json!({"respond": {"to": "prompt", "result": result}}),
    ));
    handshake(&mut fake);
    let prompt = json!({"sessionId": "s1", "prompt": [{"type": "text", "text": "hi"}]});
    fake.send(&request(2, "session/prompt", &prompt));
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": cost}})
    );
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 2, "result": result})
    );
}

#[test]
fn errors_carry_their_data_and_unanswered_requests_stay_so() {
    let mut fake = Fake::start(
        r#"
{"wait": {"method": "initialize"}}
{"respond": {"error": {"code": -32603, "message": "Internal error", "data": {"errorKind": "overloaded"}}}}
{"wait": {"method": "session/new"}}
{"note": "never answered"}
"#,
    );
    fake.send(&request(5, "initialize", &json!({})));
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 5, "error": {"code": -32603, "message": "Internal error", "data": {"errorKind": "overloaded"}}})
    );
    fake.send(&request(6, "session/new", &json!({})));
    // Nothing comes, and closing stdin ends it.
    assert!(!fake.stdout_ends(Duration::from_millis(200)));
    fake.close_stdin();
    assert!(fake.stdout_ends(SOON));
    assert_eq!(fake.exit().code(), Some(0));
    assert!(fake.record().contains(&json!({"note": "never answered"})));
}

#[test]
fn stdin_closing_while_waiting_ends_it() {
    let mut fake = Fake::start(r#"{"wait": {"method": "initialize"}}"#);
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
    assert_eq!(
        fake.record().last().unwrap(),
        &json!({"ended": "stdin closed while waiting for initialize"})
    );
    let mut fake = Fake::start(
        r#"{"request": {"method": "fs/read_text_file", "params": {}}}
{"await": {"expect": {"result": {}}}}"#,
    );
    assert_eq!(fake.recv()["id"], 0);
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
    assert_eq!(
        fake.record().last().unwrap(),
        &json!({"ended": "stdin closed while waiting for the answer to 0"})
    );
}

#[test]
fn it_writes_junk_bad_bytes_long_lines_and_stderr() {
    let mut fake = Fake::start(
        r#"
{"line": "not json at all"}
{"bytes": [255, 254, 10]}
{"bytes": [123, 34]}
{"bytes": [97, 34, 58, 49, 125, 10]}
{"json": {"jsonrpc": "2.0", "id": 9}}
{"respond": {"id": "nobody-asked", "result": null}}
{"long_line": {"len": 300000, "message": {"jsonrpc": "2.0", "id": 7, "method": "session/update", "params": {}}}}
{"long_line": {"len": 1000}}
{"stderr": "something went wrong"}
{"exit": 5}
"#,
    );
    assert_eq!(fake.line(), b"not json at all");
    assert_eq!(fake.line(), [255, 254]);
    assert_eq!(fake.line(), br#"{"a":1}"#);
    assert_eq!(fake.recv(), json!({"jsonrpc": "2.0", "id": 9}));
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": "nobody-asked", "result": null})
    );
    let long = fake.line();
    assert_eq!(long.len(), 300_000);
    let long: Value = serde_json::from_slice(&long).unwrap();
    assert_eq!(long["id"], 7);
    assert_eq!(long["method"], "session/update");
    assert_eq!(fake.line(), vec![b'x'; 1000]);
    assert!(fake.stdout_ends(SOON));
    assert_eq!(fake.exit().code(), Some(5));
    assert_eq!(fake.stderr(), "something went wrong\n");
}

#[test]
fn closing_stdout_leaves_it_running() {
    let mut fake = Fake::start(
        r#"
{"say": {"session": "s1", "text": "bye"}}
"close_stdout"
{"say": {"session": "s1", "text": "never seen"}}
"wait_closed"
{"stderr": "still here"}
{"exit": 4}
"#,
    );
    assert_eq!(fake.recv()["params"]["update"]["content"]["text"], "bye");
    assert!(fake.stdout_ends(SOON));
    assert!(fake.child.try_wait().unwrap().is_none());
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(4));
    assert_eq!(fake.stderr(), "still here\n");
}

#[test]
fn a_handler_ends_a_prompt_on_cancel() {
    let mut fake = Fake::start(&format!(
        "{HANDSHAKE}{}",
        r#"
{"wait": {"method": "session/prompt", "as": "prompt"}}
{"on": {"method": "session/cancel", "once": true, "do": [{"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}}]}}
{"say": {"session": "s1", "text": "one "}}
{"wait": {"method": "_test/go"}}
{"say": {"session": "s1", "text": "two"}}
{"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}}
{"note": "done"}
"#
    ));
    handshake(&mut fake);
    fake.send(&request(
        2,
        "session/prompt",
        &json!({"sessionId": "s1", "prompt": []}),
    ));
    assert_eq!(fake.recv()["params"]["update"]["content"]["text"], "one ");
    fake.send(
        &json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "s1"}}),
    );
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "cancelled"}})
    );
    fake.send(&json!({"jsonrpc": "2.0", "method": "_test/go"}));
    // Words in flight still come, but the prompt isn't answered again.
    assert_eq!(fake.recv()["params"]["update"]["content"]["text"], "two");
    fake.close_stdin();
    assert!(fake.stdout_ends(SOON));
    assert_eq!(fake.exit().code(), Some(0));
    let record = fake.record();
    assert!(record.contains(&json!({"skipped": "answered already", "id": 2})));
    assert!(record.contains(&json!({"note": "done"})));
}

#[test]
fn a_message_that_came_early_waits_for_its_turn() {
    let mut fake = Fake::start(
        r#"
{"wait": {"method": "session/prompt"}}
{"respond": {"result": {"stopReason": "end_turn"}}}
{"wait": {"method": "session/cancel", "match": {"params": {"sessionId": "s1"}}, "timeout": 5000}}
{"note": "cancel seen"}
"#,
    );
    fake.send(
        &json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "s2"}}),
    );
    fake.send(
        &json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "s1"}}),
    );
    fake.send(&request(
        3,
        "session/prompt",
        &json!({"sessionId": "s1", "prompt": []}),
    ));
    assert_eq!(fake.recv()["id"], 3);
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
    assert!(fake.record().contains(&json!({"note": "cancel seen"})));
}

#[test]
fn requests_to_the_client_are_checked() {
    let mut fake = Fake::start(
        r#"
{"request": {"method": "session/request_permission", "as": "ask", "params": {"sessionId": "s1", "toolCall": {"toolCallId": "t1"}, "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}]}}}
{"await": {"to": "ask", "expect": {"result": {"outcome": {"outcome": "selected", "optionId": "allow"}}}, "timeout": 5000}}
{"note": "allowed"}
{"request": {"method": "elicitation/create", "params": {"sessionId": "s1", "mode": "form", "message": "Name?", "requestedSchema": {"type": "object", "properties": {}}}}}
{"cancel_request": {}}
{"request": {"method": "fs/read_text_file", "id": "read-1", "params": {"sessionId": "s1", "path": "/etc/hostname"}}}
{"await": {"expect": {"result": {"content": "box"}}, "timeout": 5000}}
{"note": "never reached"}
"#,
    );
    let ask = fake.recv();
    assert_eq!(ask["id"], 0);
    assert_eq!(ask["method"], "session/request_permission");
    fake.send(&json!({"jsonrpc": "2.0", "id": 0, "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}}));
    let elicit = fake.recv();
    assert_eq!(elicit["id"], 1);
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "method": "$/cancel_request", "params": {"requestId": 1}})
    );
    let read = fake.recv();
    assert_eq!(read["id"], "read-1");
    fake.send(&json!({"jsonrpc": "2.0", "id": "read-1", "error": {"code": -32601, "message": "Method not found"}}));
    assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
    let record = fake.record();
    assert!(record.contains(&json!({"note": "allowed"})));
    assert!(!record.contains(&json!({"note": "never reached"})));
    let failed = record.last().unwrap()["failed"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        failed.starts_with("the answer to \"read-1\" was"),
        "{failed}"
    );
    assert!(fake.stderr().contains(&failed));
}

#[test]
fn a_wait_can_time_out() {
    let mut fake = Fake::start(r#"{"wait": {"method": "initialize", "timeout": 50}}"#);
    assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
    assert_eq!(
        fake.record(),
        [json!({"failed": "timed out waiting for initialize"})]
    );
}

#[test]
fn includes_replay_a_history() {
    let dir = Dir::new();
    let history = dir.path("history.jsonl");
    std::fs::write(
        &history,
        r#"{"update": {"session": "past-1", "update": {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "Fix it"}}}}
{"say": {"session": "past-1", "text": "Fixed."}}
"#,
    )
    .unwrap();
    let script = dir.path("script.jsonl");
    std::fs::write(
        &script,
        format!(
            r#"{{"wait": {{"method": "session/load"}}}}
{{"include": {}}}
{{"respond": {{"result": {{}}}}}}
"#,
            json!(history)
        ),
    )
    .unwrap();
    let mut fake = Fake::start_in(dir, &script);
    fake.send(&request(
        4,
        "session/load",
        &json!({"sessionId": "past-1", "cwd": "/", "mcpServers": []}),
    ));
    assert_eq!(
        fake.recv()["params"]["update"]["sessionUpdate"],
        "user_message_chunk"
    );
    assert_eq!(fake.recv()["params"]["update"]["content"]["text"], "Fixed.");
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 4, "result": {}})
    );
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
}

#[test]
fn a_wrong_script_does_nothing() {
    let mut fake =
        Fake::start("{\"say\": {\"session\": \"s1\", \"text\": \"hi\"}}\n{\"respond\": {}}\n");
    assert_eq!(fake.exit().code(), Some(EXIT_BAD_SCRIPT));
    assert!(fake.stdout_ends(SOON));
    let stderr = fake.stderr();
    assert!(stderr.contains("script.jsonl:2: "), "{stderr}");
    assert!(fake.record().is_empty());
}

#[test]
fn a_relative_script_is_refused() {
    let output = Command::new(env!("CARGO_BIN_EXE_caf-fake-agent"))
        .arg("script.jsonl")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_BAD_SCRIPT));
    assert!(String::from_utf8_lossy(&output.stderr).contains("absolute"));
}

#[test]
fn a_holder_keeps_the_pipes_open_after_it_exits() {
    let dir = Dir::new();
    let pid = dir.path("holder.pid");
    let script = dir.path("script.jsonl");
    std::fs::write(
        &script,
        format!(
            "{}\n{{\"exit\": 0}}\n",
            json!({"spawn_holder": {"pid_file": pid, "secs": 30}})
        ),
    )
    .unwrap();
    let mut fake = Fake::start_in(dir, &script);
    assert_eq!(fake.exit().code(), Some(0));
    let holder: u32 = std::fs::read_to_string(&pid).unwrap().parse().unwrap();
    assert_ne!(holder, fake.child.id());
    // Exited, but its stdout is still open, until the holder goes, which
    // it does once its pid file does.
    assert!(!fake.stdout_ends(Duration::from_millis(300)));
    std::fs::remove_file(&pid).unwrap();
    assert!(fake.stdout_ends(SOON));
}

#[test]
fn it_exits_when_nobody_reads_stdout() {
    let dir = Dir::new();
    let script = dir.path("script.jsonl");
    std::fs::write(
        &script,
        "{\"wait\": {\"method\": \"go\"}}\n{\"long_line\": {\"len\": 1048576}}\n\"hang\"\n",
    )
    .unwrap();
    let record = dir.path("record.jsonl");
    let mut child = Command::new(env!("CARGO_BIN_EXE_caf-fake-agent"))
        .arg(&script)
        .arg("--record")
        .arg(&record)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Nobody reads stdout from before the fake writes to it. What it
    // writes is more than a pipe holds, so it can't land in the pipe
    // while another test's process, started meanwhile, briefly holds a
    // copy of the reading end.
    drop(child.stdout.take());
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"{\"jsonrpc\": \"2.0\", \"method\": \"go\"}\n")
        .unwrap();
    let until = Instant::now() + SOON;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > until {
            let _ = child.kill();
            panic!("the fake didn't exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(0));
    let record = std::fs::read_to_string(&record).unwrap();
    assert!(record.contains("\n{\"gone\":\"stdout: "), "{record}");
}

#[test]
fn a_handler_doesnt_take_the_scripts_names() {
    let mut fake = Fake::start(
        r#"
{"wait": {"method": "session/prompt"}}
{"on": {"method": "session/prompt", "do": [{"respond": {"error": {"code": -32600, "message": "busy"}}}]}}
"wait_closed"
{"respond": {"result": {"stopReason": "end_turn"}}}
{"respond": {"to": "session/prompt", "result": {"stopReason": "end_turn"}}}
"#,
    );
    let prompt = json!({"sessionId": "s1", "prompt": []});
    fake.send(&request(2, "session/prompt", &prompt));
    fake.send(&request(3, "session/prompt", &prompt));
    fake.close_stdin();
    // The handler refuses the second prompt, and the script answers the
    // first, both by default and by name: once.
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32600, "message": "busy"}})
    );
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "end_turn"}})
    );
    assert!(fake.stdout_ends(SOON));
    assert_eq!(fake.exit().code(), Some(0));
    assert!(
        fake.record()
            .contains(&json!({"skipped": "answered already", "id": 2}))
    );
}

#[test]
fn handlers_take_turns_in_the_order_they_were_added() {
    let mut fake = Fake::start(
        r#"
{"on": {"method": "ping", "do": [{"note": "first"}]}}
{"on": {"method": "ping", "do": [{"note": "second"}]}}
{"on": {"method": "pong", "once": true, "do": [{"note": "pong"}]}}
{"wait": {"method": "_test/go"}}
{"wait": {"method": "pong"}}
{"note": "the second pong waited"}
"#,
    );
    for method in ["ping", "ping", "pong", "pong", "_test/go"] {
        fake.send(&json!({"jsonrpc": "2.0", "method": method}));
    }
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
    let notes: Vec<Value> = fake
        .record()
        .into_iter()
        .filter_map(|entry| entry.get("note").cloned())
        .collect();
    assert_eq!(
        notes,
        [
            json!("first"),
            json!("first"),
            json!("pong"),
            json!("the second pong waited")
        ]
    );
}

#[test]
fn sleeping_takes_as_long_as_it_says() {
    let started = Instant::now();
    let mut fake = Fake::start("{\"sleep\": 200}\n{\"exit\": 6}\n");
    assert_eq!(fake.exit().code(), Some(6));
    assert!(started.elapsed() >= Duration::from_millis(200));
}

#[test]
fn a_sleep_or_timeout_too_long_to_say_is_forever() {
    for script in [
        r#"{"sleep": 18446744073709551615}"#,
        r#"{"wait": {"method": "initialize", "timeout": 18446744073709551615}}"#,
        r#"{"request": {"method": "x"}}
{"await": {"timeout": 18446744073709551615}}"#,
    ] {
        let mut fake = Fake::start(script);
        std::thread::sleep(Duration::from_millis(200));
        assert!(fake.child.try_wait().unwrap().is_none(), "{script}");
    }
}

#[test]
fn closing_stderr_drops_what_follows() {
    let mut fake = Fake::start(
        "{\"stderr\": \"kept\"}\n\"close_stderr\"\n{\"stderr\": \"lost\"}\n{\"exit\": 0}\n",
    );
    assert_eq!(fake.exit().code(), Some(0));
    assert_eq!(fake.stderr(), "kept\n");
}

#[test]
fn an_await_can_time_out() {
    let mut fake = Fake::start(
        r#"{"request": {"method": "session/request_permission", "params": {}}}
{"await": {"timeout": 50}}"#,
    );
    assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
    assert_eq!(
        fake.record().last().unwrap(),
        &json!({"failed": "timed out waiting for the answer to 0"})
    );
}

#[test]
fn a_wait_times_out_while_other_messages_flood_in() {
    let mut fake = Fake::start(r#"{"wait": {"method": "initialize", "timeout": 300}}"#);
    let started = Instant::now();
    let mut stdin = fake.stdin.take().unwrap();
    let flood = std::thread::spawn(move || {
        let line = b"{\"jsonrpc\": \"2.0\", \"method\": \"noise\"}\n";
        while started.elapsed() < SOON && stdin.write_all(line).is_ok() {}
    });
    assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
    assert!(started.elapsed() < Duration::from_secs(5));
    flood.join().unwrap();
}

#[test]
fn answering_a_notification_fails_but_not_in_a_fallback() {
    // In the script.
    let mut fake = Fake::start(
        r#"{"wait": {"method": "session/cancel"}}
{"respond": {"result": {}}}"#,
    );
    fake.send(&json!({"jsonrpc": "2.0", "method": "session/cancel"}));
    assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
    assert_eq!(
        fake.script_record(),
        [json!({"failed": "session/cancel is a notification"})]
    );
    // In a handler that isn't a fallback: the likeliest slip, an answer
    // to the cancel rather than to the prompt.
    let mut fake = Fake::start(
        r#"{"on": {"method": "session/cancel", "do": [{"respond": {"result": {"stopReason": "cancelled"}}}]}}
"wait_closed""#,
    );
    fake.send(&json!({"jsonrpc": "2.0", "method": "session/cancel"}));
    assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
    assert_eq!(
        fake.script_record(),
        [json!({"failed": "session/cancel is a notification"})]
    );
    // In a fallback, it's skipped: see
    // `a_fallback_takes_only_what_the_script_wont_wait_for`.
}

#[test]
fn a_handler_doesnt_answer_what_the_script_answered() {
    let mut fake = Fake::start(
        r#"{"wait": {"method": "session/prompt", "as": "prompt"}}
{"respond": {"result": {"stopReason": "end_turn"}}}
{"on": {"method": "session/cancel", "do": [{"respond": {"to": "prompt", "result": {"stopReason": "cancelled"}}}]}}
"wait_closed""#,
    );
    fake.send(&request(2, "session/prompt", &json!({})));
    assert_eq!(fake.recv()["result"]["stopReason"], "end_turn");
    fake.send(&json!({"jsonrpc": "2.0", "method": "session/cancel"}));
    fake.close_stdin();
    assert!(fake.stdout_ends(SOON));
    assert_eq!(fake.exit().code(), Some(0));
    assert!(
        fake.record()
            .contains(&json!({"skipped": "answered already", "id": 2}))
    );
}

#[test]
fn a_wait_can_expect_what_comes() {
    let mut fake = Fake::start(
        r#"{"wait": {"method": "session/new", "expect": {"params": {"cwd": "/work"}}}}
{"note": "never reached"}"#,
    );
    fake.send(&request(
        1,
        "session/new",
        &json!({"cwd": "/", "mcpServers": []}),
    ));
    assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
    let failed = fake.record().last().unwrap()["failed"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        failed.starts_with("waiting for session/new, {")
            && failed.ends_with("came, not {\"params\":{\"cwd\":\"/work\"}}"),
        "{failed}"
    );
}

#[test]
fn a_cancelled_request_can_be_answered_by_its_id() {
    let mut fake = Fake::start(
        r#"{"on": {"method": "$/cancel_request", "do": [{"respond": {"id_from": "/params/requestId", "error": {"code": -32800, "message": "Cancelled"}}}]}}
{"wait": {"method": "session/prompt"}}
"wait_closed""#,
    );
    fake.send(&request(7, "session/prompt", &json!({})));
    fake.send(&json!({"jsonrpc": "2.0", "method": "$/cancel_request", "params": {"requestId": 7}}));
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32800, "message": "Cancelled"}})
    );
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
}

#[test]
fn a_fallback_takes_only_what_the_script_wont_wait_for() {
    let mut fake = Fake::start(
        r#"{"on": {"fallback": true, "do": [{"respond": {"error": {"code": -32601, "message": "Method not found"}}}]}}
{"wait": {"method": "_test/go"}}
{"wait": {"method": "session/new"}}
{"respond": {"result": {"sessionId": "s1"}}}"#,
    );
    fake.send(&request(1, "session/set_mode", &json!({})));
    fake.send(&json!({"jsonrpc": "2.0", "method": "session/cancel"}));
    fake.send(&request(2, "session/new", &json!({})));
    fake.send(&json!({"jsonrpc": "2.0", "method": "_test/go"}));
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "Method not found"}})
    );
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "s1"}})
    );
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
    assert!(
        fake.record()
            .contains(&json!({"skipped": "a notification", "method": "session/cancel"}))
    );
}

#[test]
fn a_handler_can_replay_a_history() {
    let dir = Dir::new();
    let history = dir.path("history.jsonl");
    std::fs::write(
        &history,
        "{\"say\": {\"session\": \"past-1\", \"text\": \"Fixed.\"}}\n",
    )
    .unwrap();
    let script = dir.path("script.jsonl");
    std::fs::write(
        &script,
        format!(
            "{}\n",
            json!({"on": {"method": "session/load", "do": [{"include": history}, {"respond": {"result": {}}}]}})
        ),
    )
    .unwrap();
    let mut fake = Fake::start_in(dir, &script);
    for id in [1, 2] {
        fake.send(&request(
            id,
            "session/load",
            &json!({"sessionId": "past-1"}),
        ));
        assert_eq!(fake.recv()["params"]["update"]["content"]["text"], "Fixed.");
        assert_eq!(
            fake.recv(),
            json!({"jsonrpc": "2.0", "id": id, "result": {}})
        );
    }
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
}

#[test]
fn includes_cant_nest_forever() {
    let dir = Dir::new();
    let script = dir.path("script.jsonl");
    std::fs::write(&script, format!("{}\n", json!({"include": script}))).unwrap();
    let mut fake = Fake::start_in(dir, &script);
    assert_eq!(fake.exit().code(), Some(EXIT_BAD_SCRIPT));
    assert!(fake.stderr().contains("includes nest more than 16 deep"));
}

#[test]
fn a_long_line_can_go_to_stderr() {
    let mut fake =
        Fake::start("{\"long_line\": {\"len\": 100000, \"to\": \"stderr\"}}\n{\"exit\": 0}\n");
    assert_eq!(fake.exit().code(), Some(0));
    assert_eq!(fake.stderr(), format!("{}\n", "x".repeat(100_000)));
}

#[test]
fn a_holder_started_after_stdout_closes_doesnt_hold_it() {
    let dir = Dir::new();
    let pid = dir.path("holder.pid");
    let script = dir.path("script.jsonl");
    std::fs::write(
        &script,
        format!(
            "\"close_stdout\"\n{}\n\"hang\"\n",
            json!({"spawn_holder": {"pid_file": pid}})
        ),
    )
    .unwrap();
    let mut fake = Fake::start_in(dir, &script);
    assert!(fake.stdout_ends(SOON));
    assert!(fake.child.try_wait().unwrap().is_none());
    let until = Instant::now() + SOON;
    while !pid.exists() {
        assert!(Instant::now() < until, "no holder");
        std::thread::sleep(Duration::from_millis(5));
    }
    // It was given what took stdout's place, not nothing, nor a pipe. It
    // goes with the test's folder.
    let holder = std::fs::read_to_string(&pid).unwrap();
    for fd in [0, 1] {
        assert_eq!(
            std::fs::read_link(format!("/proc/{holder}/fd/{fd}")).unwrap(),
            PathBuf::from("/dev/null")
        );
    }
}

#[test]
fn it_can_stop_reading() {
    let dir = Dir::new();
    let go = dir.path("go");
    let script = dir.path("script.jsonl");
    std::fs::write(
        &script,
        format!(
            "\"stop_reading\"\n{{\"note\": \"stopped\"}}\n{}\n\"resume_reading\"\n{}\n{}\n",
            json!({"wait_file": {"path": go}}),
            json!({"wait": {"method": "_test/last"}}),
            json!({"note": "read it all"}),
        ),
    )
    .unwrap();
    let mut fake = Fake::start_in(dir, &script);
    fake.wait_for_record(&json!({"note": "stopped"}));
    let mut stdin = fake.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        // Far more than the pipe holds.
        let line = format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "method": "noise", "params": "x".repeat(1000)})
        );
        for _ in 0..2000 {
            stdin.write_all(line.as_bytes()).unwrap();
        }
        stdin
            .write_all(b"{\"jsonrpc\": \"2.0\", \"method\": \"_test/last\"}\n")
            .unwrap();
    });
    // It reads nothing until told to go on, so the writes can't finish
    // however long this takes.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!writer.is_finished(), "the client's writes didn't block");
    std::fs::write(&go, "").unwrap();
    writer.join().unwrap();
    fake.wait_for_record(&json!({"note": "read it all"}));
}

#[test]
fn it_can_die_by_a_signal() {
    use std::os::unix::process::ExitStatusExt as _;
    for signal in [9, 15] {
        let mut fake = Fake::start(&format!(
            "{{\"signal\": {signal}}}\n{{\"note\": \"lived\"}}\n"
        ));
        assert_eq!(fake.exit().signal(), Some(signal));
        assert!(fake.record().is_empty());
    }
}

#[test]
fn it_can_be_started_by_environment_variables() {
    let dir = Dir::new();
    let script = dir.path("script.jsonl");
    std::fs::write(
        &script,
        "{\"record_env\": [\"CAF_TEST_SET\", \"CAF_TEST_UNSET\"]}\n{\"exit\": 0}\n",
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_caf-fake-agent"));
    command
        .args(["--adapter-flag", "value"])
        .current_dir(&dir.0)
        .env(caf_fake_agent::SCRIPT_VAR, &script)
        .env(caf_fake_agent::RECORD_VAR, dir.path("record.jsonl"))
        .env("CAF_TEST_SET", "yes")
        .env_remove("CAF_TEST_UNSET");
    let cwd = dir.0.clone();
    let mut fake = Fake::spawn(dir, command);
    assert_eq!(fake.exit().code(), Some(0));
    let record = fake.whole_record();
    let started = &record[0]["started"];
    assert_eq!(
        started["args"].as_array().unwrap()[1..],
        [json!("--adapter-flag"), json!("value")]
    );
    assert_eq!(started["cwd"], json!(cwd));
    let names = started["env"].as_array().unwrap();
    assert!(names.contains(&json!("CAF_TEST_SET")));
    assert!(!names.contains(&json!("CAF_TEST_UNSET")));
    assert_eq!(
        record[1],
        json!({"env": {"CAF_TEST_SET": "yes", "CAF_TEST_UNSET": null}})
    );
}

#[test]
fn a_holder_started_by_environment_variables_only_holds() {
    let dir = Dir::new();
    let pid = dir.path("holder.pid");
    let script = dir.path("script.jsonl");
    std::fs::write(
        &script,
        format!(
            "{}\n{{\"say\": {{\"session\": \"s1\", \"text\": \"once\"}}}}\n{{\"exit\": 0}}\n",
            json!({"spawn_holder": {"pid_file": pid}})
        ),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_caf-fake-agent"));
    command
        .env(caf_fake_agent::SCRIPT_VAR, &script)
        .env(caf_fake_agent::RECORD_VAR, dir.path("record.jsonl"));
    let mut fake = Fake::spawn(dir, command);
    assert_eq!(fake.exit().code(), Some(0));
    // Said once, by the fake: the holder ran no script.
    assert_eq!(fake.recv()["params"]["update"]["content"]["text"], "once");
    assert!(!fake.stdout_ends(Duration::from_millis(300)));
    let record = fake.whole_record();
    assert_eq!(
        (record.iter())
            .filter(|entry| entry.get("started").is_some())
            .count(),
        1
    );
    assert!(!record.iter().any(|entry| entry.get("failed").is_some()));
    std::fs::remove_file(&pid).unwrap();
    assert!(fake.stdout_ends(SOON));
}

#[test]
fn a_fallback_leaves_what_a_later_handler_takes() {
    let mut fake = Fake::start(
        r#"{"on": {"fallback": true, "do": [{"respond": {"error": {"code": -32601, "message": "Method not found"}}}]}}
{"wait": {"method": "_test/go"}}
{"on": {"method": "session/set_mode", "do": [{"respond": {"result": {}}}]}}
"wait_closed""#,
    );
    fake.send(&request(1, "session/set_mode", &json!({})));
    fake.send(&json!({"jsonrpc": "2.0", "method": "_test/go"}));
    assert_eq!(
        fake.recv(),
        json!({"jsonrpc": "2.0", "id": 1, "result": {}})
    );
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
}

#[test]
fn waiting_for_a_file_can_time_out() {
    let dir = Dir::new();
    let never = dir.path("never");
    let script = dir.path("script.jsonl");
    std::fs::write(
        &script,
        format!("{}\n", json!({"wait_file": {"path": never, "timeout": 50}})),
    )
    .unwrap();
    let mut fake = Fake::start_in(dir, &script);
    assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
    assert_eq!(
        fake.record(),
        [json!({"failed": format!("timed out waiting for {}", never.display())})]
    );
}

/// An MCP server for the fake to start: the fake again, following
/// `server.jsonl` in `dir`, and recording to `server-record.jsonl`.
fn mcp_server(dir: &Dir, script: &str) -> Value {
    std::fs::write(dir.path("server.jsonl"), script).unwrap();
    json!({
        "name": "caf",
        "command": env!("CARGO_BIN_EXE_caf-fake-agent"),
        "args": ["--from-session"],
        "env": [
            {"name": caf_fake_agent::SCRIPT_VAR, "value": dir.path("server.jsonl")},
            {"name": caf_fake_agent::RECORD_VAR, "value": dir.path("server-record.jsonl")},
        ],
    })
}

/// Starts the fake with `script` after [`HANDSHAKE`], and opens a session
/// naming `server`.
fn mcp_fake(dir: Dir, script: &str, server: &Value) -> Fake {
    let path = dir.path("script.jsonl");
    std::fs::write(&path, format!("{HANDSHAKE}{script}")).unwrap();
    let mut fake = Fake::start_in(dir, &path);
    fake.send(&request(0, "initialize", &json!({"protocolVersion": 1})));
    assert_eq!(fake.recv()["id"], 0);
    fake.send(&request(
        1,
        "session/new",
        &json!({"cwd": "/", "mcpServers": [server]}),
    ));
    assert_eq!(fake.recv()["result"]["sessionId"], "s1");
    fake
}

/// The server's record, but for its first line.
fn server_record(fake: &Fake) -> Vec<Value> {
    let path = fake.record.with_file_name("server-record.jsonl");
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let whole = text.rfind('\n').map_or("", |end| &text[..end]);
    let mut record: Vec<Value> = (whole.lines())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    if !record.is_empty() {
        record.remove(0);
    }
    record
}

#[test]
fn it_calls_an_mcp_servers_tools_as_claude_does() {
    let dir = Dir::new();
    let server = mcp_server(
        &dir,
        r#"
{"record_env": ["CAF_TEST_EXTRA"]}
{"wait": {"method": "server/discover", "timeout": 10000}}
{"respond": {"error": {"code": -32601, "message": "Method not found"}}}
{"wait": {"method": "initialize", "expect": {"params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "caf-fake-agent"}}}, "timeout": 10000}}
{"respond": {"result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "caf", "version": "1"}}}}
{"wait": {"method": "notifications/initialized", "timeout": 10000}}
{"wait": {"method": "tools/list", "timeout": 10000}}
{"respond": {"result": {"tools": [{"name": "Read"}, {"name": "Bash"}]}}}
{"wait": {"method": "tools/call", "as": "read", "expect": {"id": 3, "params": {"name": "Read", "arguments": {"path": "/a"}, "_meta": {"claudecode/toolUseId": "toolu_1", "progressToken": 3}}}, "timeout": 10000}}
{"notify": {"method": "notifications/progress", "params": {"progressToken": 3, "progress": 1, "message": "Running"}}}
{"respond": {"to": "read", "result": {"content": [{"type": "text", "text": "hello"}], "isError": false}}}
{"wait": {"method": "tools/call", "as": "bash", "expect": {"params": {"name": "Bash", "arguments": {}, "_meta": {}}}, "timeout": 10000}}
{"wait": {"method": "notifications/cancelled", "expect": {"params": {"requestId": 4, "reason": "Stop"}}, "timeout": 10000}}
{"note": "cancelled"}
"wait_closed"
{"note": "dropped"}
"#,
    );
    let mut fake = mcp_fake(
        dir,
        r#"
{"mcp_start": {"from": "session/new", "env": {"CAF_TEST_EXTRA": "yes"}, "discover": true, "expect": {"result": {"serverInfo": {"name": "caf"}}}, "timeout": 10000}}
{"mcp_list": {"expect": ["Read", "Bash"], "timeout": 10000}}
{"mcp_call": {"tool": "Read", "arguments": {"path": "/a"}, "tool_use_id": "toolu_1", "report": "s1", "as": "read"}}
{"mcp_progress": {"expect": {"params": {"message": "Running"}}, "timeout": 10000}}
{"mcp_await": {"to": "read", "expect": {"result": {"content": [{"text": "hello"}], "isError": false}}, "timeout": 10000}}
{"mcp_call": {"tool": "Bash", "no_progress": true, "as": "bash"}}
{"mcp_cancel": {"reason": "Stop"}}
{"mcp_await": {"none": true, "timeout": 200}}
{"mcp_drop": {}}
{"note": "done"}
"#,
        &server,
    );
    let name = "mcp__caf__Read";
    assert_eq!(
        fake.recv()["params"],
        json!({"sessionId": "s1", "update": {
            "sessionUpdate": "tool_call", "toolCallId": "toolu_1", "title": name,
            "status": "pending", "_meta": {"claudeCode": {"toolName": name}},
            "rawInput": {"path": "/a"},
        }})
    );
    fake.wait_for_record(&json!({"note": "done"}));
    let until = Instant::now() + SOON;
    while !server_record(&fake).contains(&json!({"note": "dropped"})) {
        assert!(Instant::now() < until, "{:?}", server_record(&fake));
        std::thread::sleep(Duration::from_millis(5));
    }
    let server = server_record(&fake);
    assert_eq!(server[0], json!({"env": {"CAF_TEST_EXTRA": "yes"}}));
    assert!(server.contains(&json!({"note": "cancelled"})));
    let record = fake.record();
    let started = record.iter().find_map(|entry| entry.get("mcp_started"));
    assert_eq!(
        started,
        Some(&json!({
            "server": "caf",
            "command": env!("CARGO_BIN_EXE_caf-fake-agent"),
            "args": ["--from-session"],
            "env": ["CAF_FAKE_AGENT_RECORD", "CAF_FAKE_AGENT_SCRIPT", "CAF_TEST_EXTRA"],
        }))
    );
    assert!(record.contains(&json!({"mcp_dropped": "caf"})));
    fake.close_stdin();
    assert_eq!(fake.exit().code(), Some(0));
}

#[test]
fn an_mcp_servers_answers_are_checked() {
    let handshake = r#"
{"wait": {"method": "initialize", "timeout": 10000}}
{"respond": {"result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "caf", "version": "1"}}}}
{"wait": {"method": "tools/list", "timeout": 10000}}
{"respond": {"result": {"tools": [{"name": "Read"}]}}}
"#;
    let check = |server: &str, script: &str| {
        let dir = Dir::new();
        let server = mcp_server(&dir, &format!("{handshake}{server}"));
        let mut fake = mcp_fake(dir, script, &server);
        assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
        let record = fake.script_record();
        let failed = record.iter().find_map(|entry| entry.get("failed"));
        failed
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let start = r#"{"mcp_start": {"timeout": 10000}}"#;
    assert_eq!(
        check(
            "",
            &format!("{start}\n{}", r#"{"mcp_list": {"expect": ["Bash"]}}"#)
        ),
        r#"caf has the tools ["Read"], not ["Bash"]"#
    );
    // The answers in these are JSON, whose keys the fake may write in
    // either order, as serde_json's features say: so they're read back.
    let answer = |failed: String, before: &str, after: &str| {
        let rest = failed
            .strip_prefix(before)
            .unwrap_or_else(|| panic!("{failed}"));
        let answer = rest
            .strip_suffix(after)
            .unwrap_or_else(|| panic!("{failed}"));
        serde_json::from_str::<Value>(answer).unwrap()
    };
    assert_eq!(
        answer(
            check(
                r#"{"wait": {"method": "tools/call", "timeout": 10000}}
{"respond": {"result": {"content": [], "isError": true}}}"#,
                &format!(
                    "{start}\n{}",
                    r#"{"mcp_list": {}}
{"mcp_call": {"tool": "Read"}}
{"mcp_await": {"expect": {"result": {"isError": false}}, "timeout": 10000}}"#
                )
            ),
            "caf's answer to 2 was ",
            r#", not {"result":{"isError":false}}"#
        ),
        json!({"jsonrpc": "2.0", "id": 2, "result": {"content": [], "isError": true}})
    );
    assert_eq!(
        check(
            r#"{"wait": {"method": "tools/call", "timeout": 10000}}
{"exit": 0}"#,
            &format!(
                "{start}\n{}",
                r#"{"mcp_list": {}}
{"mcp_call": {"tool": "Read"}}
{"mcp_await": {"timeout": 10000}}"#
            )
        ),
        "caf closed without answering 2"
    );
    assert_eq!(
        answer(
            check(
                r#"{"wait": {"method": "tools/call", "timeout": 10000}}
{"respond": {"result": {"content": []}}}"#,
                &format!(
                    "{start}\n{}",
                    r#"{"mcp_list": {}}
{"mcp_call": {"tool": "Read"}}
{"mcp_await": {"none": true, "timeout": 5000}}"#
                )
            ),
            "caf answered 2, with ",
            ""
        ),
        json!({"jsonrpc": "2.0", "id": 2, "result": {"content": []}})
    );
    assert_eq!(
        check("", r#"{"mcp_start": {"server": "eyespy"}}"#),
        "session/new names no MCP server eyespy"
    );
    assert_eq!(
        check("", r#"{"mcp_list": {}}"#),
        "no MCP server started called caf"
    );
}

#[test]
fn it_answers_cafs_relay_check() {
    let output = Command::new(env!("CARGO_BIN_EXE_caf-fake-agent"))
        .env(caf_relay::CHECK_VAR, "1")
        .env_remove(caf_relay::RELAY_VAR)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        caf_relay::CHECK_ANSWER
    );
}
