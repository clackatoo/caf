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

    fn whole_record(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.record)
            .unwrap_or_default()
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
fn answering_a_notification_fails_but_not_in_a_handler() {
    let mut fake = Fake::start(
        r#"{"on": {"method": "ping", "do": [{"respond": {"result": {}}}]}}
{"wait": {"method": "session/cancel"}}
{"respond": {"result": {}}}"#,
    );
    fake.send(&json!({"jsonrpc": "2.0", "method": "ping"}));
    fake.send(&json!({"jsonrpc": "2.0", "method": "session/cancel"}));
    assert_eq!(fake.exit().code(), Some(EXIT_CHECK_FAILED));
    assert_eq!(
        fake.script_record(),
        [
            json!({"skipped": "a notification", "method": "ping"}),
            json!({"failed": "session/cancel is a notification"}),
        ]
    );
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
    // The holder goes with the test's folder.
    let until = Instant::now() + SOON;
    while !pid.exists() {
        assert!(Instant::now() < until, "no holder");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn it_can_stop_reading() {
    let mut fake = Fake::start(
        r#""stop_reading"
{"note": "stopped"}
{"sleep": 2000}
"resume_reading"
{"wait": {"method": "_test/last"}}
{"note": "read it all"}"#,
    );
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
    std::thread::sleep(Duration::from_millis(300));
    assert!(!writer.is_finished(), "the client's writes didn't block");
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
fn other_crates_find_the_program() {
    let found = caf_fake_agent::binary();
    let built = PathBuf::from(env!("CARGO_BIN_EXE_caf-fake-agent"));
    assert_eq!(found.canonicalize().unwrap(), built.canonicalize().unwrap());
}
