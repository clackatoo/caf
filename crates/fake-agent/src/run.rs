//! Running a script: the fake's side of the pipes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ChildStdin;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::pattern::matches;
use crate::script::{
    Await, LongLine, McpAwait, McpCall, McpProgress, McpStart, On, Output, Respond, Script,
    SpawnHolder, Step, Tool, ToolContent, Wait,
};
use crate::{EXIT_BAD_SCRIPT, EXIT_CHECK_FAILED};

/// The MCP server, and connection, a step means if it doesn't say.
const CAF: &str = "caf";

/// How much of a long line is written at a time.
const CHUNK: usize = 1 << 20;

/// Runs a script to its end, and returns the code to exit with.
///
/// The script is checked first ([`Script::check`]). `record`, an absolute
/// path, if given, gets a line of JSON for each thing that happens, as the
/// crate's docs say. Call this once, from `main`: it takes over stdin,
/// stdout and stderr.
#[must_use]
pub fn run(script: &Script, record: Option<&Path>) -> i32 {
    if let Err(err) = script.check() {
        eprintln!("caf-fake-agent: {err}");
        return EXIT_BAD_SCRIPT;
    }
    let record = match record.map(Record::open).transpose() {
        Ok(record) => record.unwrap_or_default(),
        Err(err) => {
            eprintln!("caf-fake-agent: {err}");
            return EXIT_BAD_SCRIPT;
        }
    };
    record.write(&json!({"started": started()}));
    let gate = Arc::new(Gate::default());
    // SAFETY: fds 1 and 2 are open from the process's start, and nothing
    // else in this program writes to them, so these become their only
    // owners.
    let (out, err) = unsafe { (File::from_raw_fd(1), File::from_raw_fd(2)) };
    let (heard, inbox) = mpsc::channel();
    read_stdin(heard.clone(), record.clone(), gate.clone());
    let mut fake = Fake {
        out: Some(out),
        err: Some(err),
        inbox,
        heard,
        gate,
        record,
        main: &script.steps,
        at: 0,
        closed: false,
        queue: VecDeque::new(),
        answers: HashMap::new(),
        received: HashMap::new(),
        last_received: None,
        handling: None,
        answered: HashSet::new(),
        sent: HashMap::new(),
        last_sent: None,
        next_id: 0,
        handlers: Vec::new(),
        holders: Vec::new(),
        servers: HashMap::new(),
        generations: 0,
        calls: HashMap::new(),
        last_call: None,
    };
    match fake.main() {
        Ok(()) => 0,
        Err(End(code)) => code,
    }
}

/// How the fake was started: its arguments, working directory and the
/// names of its environment variables. Not their values, which may be
/// secrets, and a failing test prints its record: `record_env` asks for
/// the ones a test needs.
fn started() -> Value {
    let lossy = |text: std::ffi::OsString| text.to_string_lossy().into_owned();
    let mut env: Vec<String> = std::env::vars_os().map(|(name, _)| lossy(name)).collect();
    env.sort();
    json!({
        "args": std::env::args_os().map(lossy).collect::<Vec<_>>(),
        "cwd": std::env::current_dir().ok().map(|cwd| lossy(cwd.into_os_string())),
        "env": env,
    })
}

/// Whether the reader thread may read stdin.
#[derive(Default)]
struct Gate {
    stopped: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn set(&self, stopped: bool) {
        *self.stopped.lock().expect("never poisoned") = stopped;
        self.changed.notify_all();
    }

    /// Waits until reading is allowed.
    fn pass(&self) {
        let stopped = self.stopped.lock().expect("never poisoned");
        drop(
            self.changed
                .wait_while(stopped, |stopped| *stopped)
                .expect("never poisoned"),
        );
    }
}

/// The script is over: exit with this code.
struct End(i32);

/// Something the reader thread heard.
enum Heard {
    /// A JSON object from the client.
    Message(Value),
    /// The client closed stdin.
    Closed,
    /// A JSON object from an MCP server, by its connection's name and
    /// generation.
    Mcp(String, u64, Value),
    /// An MCP server closed its stdout, by its connection's name and
    /// generation.
    McpClosed(String, u64),
}

/// The record file, shared with the reader thread.
#[derive(Clone, Default)]
struct Record(Option<Arc<Mutex<File>>>);

impl Record {
    fn open(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err(format!(
                "{}: a record's path must be absolute",
                path.display()
            ));
        }
        let file = File::options()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        Ok(Self(Some(Arc::new(Mutex::new(file)))))
    }

    fn write(&self, entry: &Value) {
        if let Some(file) = &self.0 {
            let mut line = serde_json::to_vec(entry).expect("JSON is always JSON");
            line.push(b'\n');
            // A test reads the record, so a record it can't write is a
            // test that fails anyway, with what was written so far.
            let _ = file.lock().expect("never poisoned").write_all(&line);
        }
    }
}

/// Reads stdin on a thread of its own, so the fake never stops reading
/// while it writes, and both pipes can't fill at once, unless the script
/// stops it.
fn read_stdin(send: Sender<Heard>, record: Record, gate: Arc<Gate>) {
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        let mut line = Vec::new();
        loop {
            gate.pass();
            line.clear();
            match input.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = line.trim_ascii();
            if text.is_empty() {
                continue;
            }
            match serde_json::from_slice::<Value>(text) {
                Ok(message) if message.is_object() => {
                    record.write(&json!({"received": message}));
                    if send.send(Heard::Message(message)).is_err() {
                        return;
                    }
                }
                _ => record.write(&json!({"unreadable": String::from_utf8_lossy(text)})),
            }
        }
        record.write(&json!({"closed": "stdin"}));
        let _ = send.send(Heard::Closed);
    });
}

/// Reads an MCP server's stdout on a thread of its own. `stop_reading`
/// is about the client's stdin, so this never stops.
/// Its stderr is read on another, a line at a time into the record, as
/// Claude logs its servers' stderr. Once stdout closes, the server is
/// waited for, and how it exited recorded.
fn read_server(
    name: String,
    generation: u64,
    mut child: Child,
    send: Sender<Heard>,
    record: Record,
) {
    let stdout = child.stdout.take().expect("it's piped");
    if let Some(stderr) = child.stderr.take() {
        let (name, record) = (name.clone(), record.clone());
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr).split(b'\n') {
                let Ok(line) = line else { return };
                let line = String::from_utf8_lossy(&line);
                record.write(&json!({"mcp_stderr": {"server": name, "line": line.trim_end()}}));
            }
        });
    }
    std::thread::spawn(move || {
        let mut input = std::io::BufReader::new(stdout);
        let mut line = Vec::new();
        loop {
            line.clear();
            match input.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = line.trim_ascii();
            if text.is_empty() {
                continue;
            }
            match serde_json::from_slice::<Value>(text) {
                Ok(message) if message.is_object() => {
                    record.write(&json!({"mcp_received": {"server": name, "message": message}}));
                    // The fake gone, this thread goes too, leaving the
                    // server to exit when its stdin closes.
                    if send
                        .send(Heard::Mcp(name.clone(), generation, message))
                        .is_err()
                    {
                        return;
                    }
                }
                _ => record.write(&json!({"mcp_unreadable": {
                    "server": name,
                    "text": String::from_utf8_lossy(text),
                }})),
            }
        }
        record.write(&json!({"mcp_closed": name}));
        let _ = send.send(Heard::McpClosed(name.clone(), generation));
        // A server that closes its stdout but carries on is waited for
        // still: this thread waits with it, and nothing else.
        match child.wait() {
            Ok(status) => {
                let how = match (status.code(), status.signal()) {
                    (Some(code), _) => json!({"server": name, "code": code}),
                    (None, Some(signal)) => json!({"server": name, "signal": signal}),
                    (None, None) => json!({"server": name}),
                };
                record.write(&json!({"mcp_exited": how}));
            }
            Err(err) => {
                record.write(&json!({"mcp_exited": {"server": name, "error": err.to_string()}}))
            }
        }
    });
}

/// An MCP server the fake started, as Claude starts the servers it's
/// given.
struct Server {
    /// Its name in `mcpServers`, for the tools' names.
    name: String,
    /// Which of the servers started under its connection's name it is:
    /// what one started before it sends is ignored.
    generation: u64,
    /// Its stdin, until `mcp_drop`.
    stdin: Option<ChildStdin>,
    /// Whether it closed its stdout.
    closed: bool,
    /// The id of the fake's next request to it.
    next_id: u64,
    /// Its answers not yet awaited, by id.
    answers: HashMap<String, Value>,
    /// Its progress notifications not yet waited for, in order.
    progress: VecDeque<Value>,
}

/// A call the fake made: its connection, its request's id, and its
/// progress token, if it gave one.
struct Call {
    server: String,
    /// The generation of the server it was made to.
    generation: u64,
    id: Value,
    token: Option<Value>,
}

/// A handler, and whether it's done.
struct Handler {
    on: On,
    done: bool,
}

struct Fake<'s> {
    /// Stdout, until the script closes it.
    out: Option<File>,
    /// Stderr, until the script closes it.
    err: Option<File>,
    inbox: Receiver<Heard>,
    /// The inbox's sender, for MCP servers' readers.
    heard: Sender<Heard>,
    gate: Arc<Gate>,
    record: Record,
    /// The script's own steps, not its handlers'.
    main: &'s [Step],
    /// Which of them it's at.
    at: usize,
    /// Whether the client closed stdin.
    closed: bool,
    /// Requests and notifications from the client not yet waited for, in
    /// the order they came.
    queue: VecDeque<Value>,
    /// The client's answers not yet awaited, by id.
    answers: HashMap<String, Value>,
    /// Requests and notifications waited for, by name.
    received: HashMap<String, Value>,
    /// The one last waited for: within a handler, the one it handles.
    last_received: Option<Value>,
    /// The handler running, if any: whether it's a fallback.
    handling: Option<bool>,
    /// The ids of the client's requests answered so far.
    answered: HashSet<String>,
    /// The ids of the fake's requests, by name.
    sent: HashMap<String, Value>,
    /// The name of the request last sent.
    last_sent: Option<String>,
    /// The id of the fake's next request, unless the script gives one.
    next_id: u64,
    handlers: Vec<Handler>,
    /// Processes holding the pipes. Never waited for: they're meant to
    /// outlive the fake.
    holders: Vec<Child>,
    /// The MCP servers started, by their connections' names.
    servers: HashMap<String, Server>,
    /// How many MCP servers have been started.
    generations: u64,
    /// The MCP calls made, by name.
    calls: HashMap<String, Call>,
    /// The name of the call last made.
    last_call: Option<String>,
}

/// An id as a key: its JSON.
fn key(id: &Value) -> String {
    id.to_string()
}

impl Fake<'_> {
    /// Runs the script's steps, then reads on.
    fn main(&mut self) -> Result<(), End> {
        let main = self.main;
        for (at, step) in main.iter().enumerate() {
            self.at = at;
            self.step(step)?;
        }
        self.at = main.len();
        self.finish()
    }

    fn steps(&mut self, steps: &[Step]) -> Result<(), End> {
        steps.iter().try_for_each(|step| self.step(step))
    }

    fn step(&mut self, step: &Step) -> Result<(), End> {
        match step {
            Step::Wait(wait) => self.wait(wait),
            Step::Respond(respond) => self.respond(respond),
            Step::Notify(notify) => {
                let mut message = json!({"jsonrpc": "2.0", "method": notify.method});
                if let Some(params) = &notify.params {
                    message["params"] = params.clone();
                }
                self.send(&message)
            }
            Step::Update(update) => self.update(&update.session, &update.update),
            Step::Say(chunk) => self.update(
                &chunk.session,
                &json!({
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": chunk.text},
                }),
            ),
            Step::Think(chunk) => self.update(
                &chunk.session,
                &json!({
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {"type": "text", "text": chunk.text},
                }),
            ),
            Step::Tool(tool) => self.update(&tool.session, &tool_update(tool)),
            Step::Request(request) => {
                let id = request.id.clone().unwrap_or_else(|| {
                    self.next_id += 1;
                    json!(self.next_id - 1)
                });
                let label = request
                    .label
                    .clone()
                    .unwrap_or_else(|| request.method.clone());
                let mut message = json!({"jsonrpc": "2.0", "id": id, "method": request.method});
                if let Some(params) = &request.params {
                    message["params"] = params.clone();
                }
                self.sent.insert(label.clone(), id);
                self.last_sent = Some(label);
                self.send(&message)
            }
            Step::Await(wait) => self.await_answer(wait),
            Step::CancelRequest(cancel) => {
                let id = self.sent_id(cancel.to.as_ref())?;
                self.send(&json!({
                    "jsonrpc": "2.0",
                    "method": "$/cancel_request",
                    "params": {"requestId": id},
                }))
            }
            Step::On(on) => {
                self.handlers.push(Handler {
                    on: on.clone(),
                    done: false,
                });
                Ok(())
            }
            Step::Sleep(ms) => {
                // None if it's too far off to say: forever.
                let until = deadline(Some(*ms));
                loop {
                    self.handle()?;
                    // Checked apart from hearing, so a flood can't make
                    // it sleep longer.
                    if past(until) || !self.hear(until) {
                        return self.handle();
                    }
                }
            }
            Step::Line(text) => self.write_out(format!("{text}\n").as_bytes()),
            Step::Json(value) => self.send(value),
            Step::Bytes(bytes) => self.write_out(bytes),
            Step::LongLine(line) => self.long_line(line),
            Step::Stderr(text) => {
                self.write_err(format!("{text}\n").as_bytes());
                Ok(())
            }
            Step::Note(text) => {
                self.record.write(&json!({"note": text}));
                Ok(())
            }
            Step::RecordEnv(names) => {
                let env: serde_json::Map<String, Value> = (names.iter())
                    .map(|name| {
                        let value = std::env::var_os(name)
                            .map(|value| Value::String(value.to_string_lossy().into_owned()));
                        (name.clone(), value.unwrap_or(Value::Null))
                    })
                    .collect();
                self.record.write(&json!({"env": env}));
                Ok(())
            }
            Step::Exit(code) => Err(End(*code)),
            Step::Signal(signal) => {
                // SAFETY: kill has no memory effects; a signal that's
                // caught or ignored returns here.
                if unsafe { libc::kill(libc::getpid(), *signal) } != 0 {
                    let err = std::io::Error::last_os_error();
                    return self.fail(&format!("couldn't send signal {signal}: {err}"));
                }
                Ok(())
            }
            Step::StopReading => {
                self.gate.set(true);
                Ok(())
            }
            Step::ResumeReading => {
                self.gate.set(false);
                Ok(())
            }
            Step::CloseStdout => {
                close(&mut self.out);
                Ok(())
            }
            Step::CloseStderr => {
                close(&mut self.err);
                Ok(())
            }
            Step::WaitClosed => {
                while !self.closed {
                    self.handle()?;
                    self.hear(None);
                }
                self.handle()
            }
            Step::WaitFile(wait) => {
                let until = deadline(wait.timeout);
                // Polled, as files give no notice, reading meanwhile.
                while !wait.path.exists() {
                    if past(until) {
                        return self
                            .fail(&format!("timed out waiting for {}", wait.path.display()));
                    }
                    self.handle()?;
                    self.hear(deadline(Some(10)));
                }
                self.handle()
            }
            Step::Hang => loop {
                self.handle()?;
                self.hear(None);
            },
            Step::SpawnHolder(holder) => self.spawn_holder(holder),
            Step::McpStart(start) => self.mcp_start(start),
            Step::McpList(list) => {
                let server = list.server.as_deref().unwrap_or(CAF);
                let id = self.mcp_request(server, "tools/list", json!({}))?;
                let answer = self.mcp_answer(server, &id, deadline(list.timeout))?;
                let Some(tools) = answer.pointer("/result/tools").and_then(Value::as_array) else {
                    return self.fail(&format!("{server} answered tools/list with {answer}"));
                };
                let mut names: Vec<&str> = (tools.iter())
                    .map(|tool| tool.get("name").and_then(Value::as_str).unwrap_or_default())
                    .collect();
                names.sort_unstable();
                if let Some(expect) = &list.expect {
                    let mut want: Vec<&str> = expect.iter().map(String::as_str).collect();
                    want.sort_unstable();
                    if names != want {
                        return self
                            .fail(&format!("{server} has the tools {names:?}, not {want:?}"));
                    }
                }
                Ok(())
            }
            Step::McpCall(call) => self.mcp_call(call),
            Step::McpAwait(wait) => self.mcp_await(wait),
            Step::McpProgress(wait) => self.mcp_progress(wait),
            Step::McpCancel(cancel) => {
                let (server, id) = {
                    let call = self.call(cancel.to.as_ref())?;
                    (call.server.clone(), call.id.clone())
                };
                let mut params = json!({"requestId": id});
                if let Some(reason) = &cancel.reason {
                    params["reason"] = json!(reason);
                }
                self.mcp_send(
                    &server,
                    &json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": params}),
                )
            }
            Step::McpDrop(drop) => {
                let server = drop.server.as_deref().unwrap_or(CAF);
                match self.servers.get_mut(server) {
                    Some(found) => {
                        found.stdin = None;
                        self.record.write(&json!({"mcp_dropped": server}));
                        Ok(())
                    }
                    None => self.fail(&format!("no MCP server started called {server}")),
                }
            }
            Step::Include(path) => self.fail(&format!(
                "{} wasn't included when the script was read",
                path.display()
            )),
        }
    }

    /// After the last step: reads on, running handlers, until stdin
    /// closes.
    fn finish(&mut self) -> Result<(), End> {
        loop {
            self.handle()?;
            if self.closed {
                return Ok(());
            }
            self.hear(None);
        }
    }

    /// Takes in what the client sent, waiting until `until` (forever if
    /// `None`) for something to come. Returns whether anything did.
    fn hear(&mut self, until: Option<Instant>) -> bool {
        if self.closed {
            match until {
                Some(until) => std::thread::sleep(until.saturating_duration_since(Instant::now())),
                // Only handlers could do anything now, and nothing more
                // will come for them.
                None => loop {
                    std::thread::park();
                },
            }
            return false;
        }
        let heard = match until {
            Some(until) => {
                match self
                    .inbox
                    .recv_timeout(until.saturating_duration_since(Instant::now()))
                {
                    Ok(heard) => heard,
                    Err(RecvTimeoutError::Timeout) => return false,
                    Err(RecvTimeoutError::Disconnected) => Heard::Closed,
                }
            }
            None => self.inbox.recv().unwrap_or(Heard::Closed),
        };
        self.take(heard);
        // Take whatever else came meanwhile, so it's all queued in order.
        while let Ok(heard) = self.inbox.try_recv() {
            self.take(heard);
        }
        true
    }

    /// Sorts what the client sent: requests and notifications are queued,
    /// answers kept by id.
    fn take(&mut self, heard: Heard) {
        match heard {
            Heard::Message(message) if message.get("method").is_some() => {
                self.queue.push_back(message);
            }
            Heard::Message(message) => {
                let id = message.get("id").map_or_else(|| "null".into(), key);
                self.answers.insert(id, message);
            }
            Heard::Closed => self.closed = true,
            Heard::Mcp(name, generation, message) => {
                let server = self.servers.get_mut(&name);
                // From a server replaced by one started under its name
                // since: the record shows it, and nothing else heeds it.
                let Some(server) = server.filter(|server| server.generation == generation) else {
                    return;
                };
                if message.get("method").and_then(Value::as_str) == Some("notifications/progress") {
                    server.progress.push_back(message);
                } else if message.get("method").is_none() {
                    let id = message.get("id").map_or_else(|| "null".into(), key);
                    server.answers.insert(id, message);
                }
                // Anything else a server sends, the record shows, and the
                // fake lets be.
            }
            Heard::McpClosed(name, generation) => {
                let server = self.servers.get_mut(&name);
                if let Some(server) = server.filter(|server| server.generation == generation) {
                    server.closed = true;
                }
            }
        }
    }

    /// Runs the handlers on queued messages they match, in order.
    fn handle(&mut self) -> Result<(), End> {
        let mut at = 0;
        while at < self.queue.len() {
            let Some(found) = self.handler_for(&self.queue[at]) else {
                at += 1;
                continue;
            };
            let message = self.queue.remove(at).expect("it's there");
            let handler = &mut self.handlers[found];
            handler.done = handler.on.once;
            let on = handler.on.clone();
            // The handler's names are its own: the script's come back
            // once it's done.
            let label = on.label.clone().unwrap_or_else(|| method(&message));
            let shadowed = self.received.insert(label.clone(), message.clone());
            let last = self.last_received.replace(message);
            self.handling = Some(on.fallback);
            let done = self.steps(&on.steps);
            self.handling = None;
            self.last_received = last;
            match shadowed {
                Some(shadowed) => self.received.insert(label, shadowed),
                None => self.received.remove(&label),
            };
            done?;
        }
        Ok(())
    }

    /// The handler that takes this message, if any: the first ordinary one
    /// that matches, else the first fallback that does, if nothing later
    /// in the script, a `wait` or a handler that isn't a fallback, would
    /// take it.
    fn handler_for(&self, message: &Value) -> Option<usize> {
        let matching = |fallback: bool| {
            self.handlers.iter().position(|handler| {
                !handler.done
                    && handler.on.fallback == fallback
                    && wanted(
                        handler.on.method.as_ref(),
                        handler.on.matches.as_ref(),
                        message,
                    )
            })
        };
        matching(false).or_else(|| {
            let fallbacks =
                (self.handlers.iter()).any(|handler| !handler.done && handler.on.fallback);
            if !fallbacks {
                return None;
            }
            // What a later `wait`, or a later handler that isn't a
            // fallback, would take is left for it.
            let later = self.main[self.at.min(self.main.len())..]
                .iter()
                .any(|step| match step {
                    Step::Wait(wait) => {
                        wanted(wait.method.as_ref(), wait.matches.as_ref(), message)
                    }
                    Step::On(on) => {
                        !on.fallback && wanted(on.method.as_ref(), on.matches.as_ref(), message)
                    }
                    _ => false,
                });
            if later { None } else { matching(true) }
        })
    }

    fn wait(&mut self, wait: &Wait) -> Result<(), End> {
        let until = deadline(wait.timeout);
        let what = match (&wait.method, &wait.matches) {
            (Some(method), Some(pattern)) => format!("{method} matching {pattern}"),
            (Some(method), None) => method.clone(),
            (None, Some(pattern)) => format!("a message matching {pattern}"),
            (None, None) => "a message".into(),
        };
        loop {
            self.handle()?;
            let found = self
                .queue
                .iter()
                .position(|message| wanted(wait.method.as_ref(), wait.matches.as_ref(), message));
            if let Some(found) = found {
                let message = self.queue.remove(found).expect("it's there");
                if let Some(expect) = &wait.expect
                    && !matches(expect, &message)
                {
                    return self.fail(&format!("waiting for {what}, {message} came, not {expect}"));
                }
                let label = wait.label.clone().unwrap_or_else(|| method(&message));
                self.received.insert(label, message.clone());
                self.last_received = Some(message);
                return Ok(());
            }
            // The client is gone, so what's waited for can't come: exit,
            // as an adapter does when its stdin closes.
            if self.closed {
                return self.ended(&format!("waiting for {what}"));
            }
            if past(until) || !self.hear(until) {
                return self.fail(&format!("timed out waiting for {what}"));
            }
        }
    }

    fn respond(&mut self, respond: &Respond) -> Result<(), End> {
        let id = match &respond.id {
            Some(id) => id.clone(),
            None => {
                let message = match &respond.to {
                    Some(label) => self.received.get(label),
                    None => self.last_received.as_ref(),
                };
                let Some(message) = message else {
                    return self.fail(&format!(
                        "nothing to answer called {}",
                        respond.to.as_deref().unwrap_or("anything")
                    ));
                };
                let id = match &respond.id_from {
                    Some(pointer) => message
                        .pointer(pointer)
                        .cloned()
                        .ok_or_else(|| format!("{} has nothing at {pointer}", method(message))),
                    None => match message.get("id") {
                        Some(id) => Ok(id.clone()),
                        None if self.handling == Some(true) => {
                            let method = method(message);
                            self.record
                                .write(&json!({"skipped": "a notification", "method": method}));
                            return Ok(());
                        }
                        None => Err(format!("{} is a notification", method(message))),
                    },
                };
                let id = match id {
                    Ok(id) => id,
                    Err(why) => return self.fail(&why),
                };
                if !self.answered.insert(key(&id)) {
                    self.record
                        .write(&json!({"skipped": "answered already", "id": id}));
                    return Ok(());
                }
                id
            }
        };
        let mut message = json!({"jsonrpc": "2.0", "id": id});
        if let Some(error) = &respond.error {
            message["error"] = serde_json::to_value(error).expect("errors are JSON");
        } else {
            message["result"] = respond.result.clone().unwrap_or(Value::Null);
        }
        self.send(&message)
    }

    fn update(&mut self, session: &str, update: &Value) -> Result<(), End> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": session, "update": update},
        }))
    }

    fn await_answer(&mut self, wait: &Await) -> Result<(), End> {
        let id = self.sent_id(wait.to.as_ref())?;
        let until = deadline(wait.timeout);
        loop {
            self.handle()?;
            if let Some(answer) = self.answers.remove(&key(&id)) {
                return match &wait.expect {
                    Some(expect) if !matches(expect, &answer) => {
                        self.fail(&format!("the answer to {id} was {answer}, not {expect}"))
                    }
                    _ => Ok(()),
                };
            }
            if self.closed {
                return self.ended(&format!("waiting for the answer to {id}"));
            }
            if past(until) || !self.hear(until) {
                return self.fail(&format!("timed out waiting for the answer to {id}"));
            }
        }
    }

    /// The id of one of the fake's requests: the one named, or the last.
    fn sent_id(&mut self, label: Option<&String>) -> Result<Value, End> {
        let label = label.or(self.last_sent.as_ref());
        match label.and_then(|label| self.sent.get(label)) {
            Some(id) => Ok(id.clone()),
            None => self.fail(&format!(
                "no request sent called {}",
                label.map_or("anything", String::as_str)
            )),
        }
    }

    fn long_line(&mut self, line: &LongLine) -> Result<(), End> {
        let (head, tail) = match &line.message {
            Some(message) => {
                let mut text = serde_json::to_string(message).expect("JSON is always JSON");
                // An object ends with its brace: the spaces go before it.
                text.pop();
                (text.into_bytes(), b"}\n".as_slice())
            }
            None => (Vec::new(), b"\n".as_slice()),
        };
        let fill = if line.message.is_some() { b' ' } else { b'x' };
        let mut left = line.len - head.len() - (tail.len() - 1);
        self.write_to(line.to, &head)?;
        let chunk = vec![fill; left.min(CHUNK)];
        while left > 0 {
            let now = left.min(CHUNK);
            self.write_to(line.to, &chunk[..now])?;
            left -= now;
        }
        self.write_to(line.to, tail)
    }

    fn spawn_holder(&mut self, holder: &SpawnHolder) -> Result<(), End> {
        if let Some(path) = &holder.pid_file {
            // So the file found below is this holder's, not an old one.
            let _ = std::fs::remove_file(path);
        }
        let spawned = std::env::current_exe().and_then(|program| {
            let mut command = Command::new(program);
            command
                .arg("--hold")
                .arg(holder.secs.unwrap_or(60).to_string());
            if let Some(path) = &holder.pid_file {
                command.arg(path);
            }
            // Not told the script, so even an old holder that read the
            // environment first couldn't run it again.
            command
                .env_remove(crate::SCRIPT_VAR)
                .env_remove(crate::RECORD_VAR)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
        });
        let child = match spawned {
            Ok(child) => child,
            Err(err) => return self.fail(&format!("couldn't start a holder: {err}")),
        };
        self.holders.push(child);
        // The holder writes its pid file itself, so it knows the file was
        // there; the script goes on once it has.
        if let Some(path) = &holder.pid_file {
            let until = deadline(Some(10_000));
            while !path.exists() {
                if past(until) {
                    return self.fail(&format!("the holder never wrote {}", path.display()));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }

    /// Starts an MCP server named in a message's `mcpServers`, and does
    /// the handshake.
    fn mcp_start(&mut self, start: &McpStart) -> Result<(), End> {
        let name = start.server.as_deref().unwrap_or(CAF);
        let label = start.label.clone().unwrap_or_else(|| name.to_owned());
        let message = match &start.from {
            Some(from) => self.received.get(from),
            None => self.last_received.as_ref(),
        };
        let Some(message) = message else {
            return self.fail(&format!(
                "nothing waited for called {}",
                start.from.as_deref().unwrap_or("anything")
            ));
        };
        let entry = (message
            .pointer("/params/mcpServers")
            .and_then(Value::as_array))
        .and_then(|servers| servers.iter().find(|server| server["name"] == name))
        .cloned();
        let Some(entry) = entry else {
            return self.fail(&format!(
                "{} names no MCP server {name}",
                self::method(message)
            ));
        };
        let Some(program) = entry.get("command").and_then(Value::as_str) else {
            return self.fail(&format!("the MCP server {name} isn't a command: {entry}"));
        };
        let strings = |at: &str| -> Vec<&Value> {
            entry
                .get(at)
                .and_then(Value::as_array)
                .map(|items| items.iter().collect())
                .unwrap_or_default()
        };
        let args: Vec<String> = strings("args")
            .into_iter()
            .map(|arg| arg.as_str().map_or_else(|| arg.to_string(), str::to_owned))
            .collect();
        let mut env: Vec<(String, String)> = strings("env")
            .into_iter()
            .filter_map(|var| {
                Some((
                    var.get("name")?.as_str()?.to_owned(),
                    var.get("value")?.as_str()?.to_owned(),
                ))
            })
            .collect();
        env.extend(
            start
                .env
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        let mut command = Command::new(program);
        // Not told the fake's own script, so a server that's the fake
        // again, without a script of its own, can't run this one.
        command
            .args(&args)
            .env_remove(crate::SCRIPT_VAR)
            .env_remove(crate::RECORD_VAR)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => return self.fail(&format!("couldn't start the MCP server {name}: {err}")),
        };
        let mut names: Vec<&str> = env.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        self.record.write(&json!({"mcp_started": {
            "server": label,
            "command": program,
            "args": args,
            "env": names,
        }}));
        self.generations += 1;
        let generation = self.generations;
        let stdin = child.stdin.take();
        read_server(
            label.clone(),
            generation,
            child,
            self.heard.clone(),
            self.record.clone(),
        );
        // One started under this name before is replaced: its stdin closes,
        // so it exits, as Claude closes a server it's done with.
        self.servers.insert(
            label.clone(),
            Server {
                name: name.to_owned(),
                generation,
                stdin,
                closed: false,
                next_id: 0,
                answers: HashMap::new(),
                progress: VecDeque::new(),
            },
        );
        if start.discover {
            let id = self.mcp_request(&label, "server/discover", json!({}))?;
            let answer = self.mcp_answer(&label, &id, deadline(start.timeout))?;
            if !matches(&json!({"error": {"code": -32601}}), &answer) {
                return self.fail(&format!(
                    "{label} answered server/discover with {answer}, not -32601"
                ));
            }
        }
        let id = self.mcp_request(
            &label,
            "initialize",
            json!({
                "protocolVersion": start.version.as_deref().unwrap_or("2025-06-18"),
                "capabilities": {},
                "clientInfo": {"name": "caf-fake-agent", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        let answer = self.mcp_answer(&label, &id, deadline(start.timeout))?;
        if answer.get("result").is_none() {
            return self.fail(&format!("{label} answered initialize with {answer}"));
        }
        if let Some(expect) = &start.expect
            && !matches(expect, &answer)
        {
            return self.fail(&format!(
                "{label} answered initialize with {answer}, not {expect}"
            ));
        }
        self.mcp_send(
            &label,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
    }

    fn mcp_call(&mut self, call: &McpCall) -> Result<(), End> {
        let server = call.server.as_deref().unwrap_or(CAF);
        let Some(found) = self.servers.get(server) else {
            return self.fail(&format!("no MCP server started called {server}"));
        };
        let generation = found.generation;
        let qualified = format!("mcp__{}__{}", found.name, call.tool);
        // The id the request will have, as the MCP SDK makes its token.
        let token = (!call.no_progress).then(|| json!(found.next_id));
        let arguments = call.arguments.clone().unwrap_or_else(|| json!({}));
        if let (Some(session), Some(tool_use_id)) = (&call.report, &call.tool_use_id) {
            let report = Tool {
                session: session.clone(),
                id: tool_use_id.clone(),
                title: Some(qualified.clone()),
                status: Some("pending".into()),
                tool_name: Some(qualified),
                raw_input: Some(arguments.clone()),
                ..Tool::default()
            };
            self.update(session, &tool_update(&report))?;
        }
        let mut meta = serde_json::Map::new();
        if let Some(tool_use_id) = &call.tool_use_id {
            meta.insert("claudecode/toolUseId".into(), json!(tool_use_id));
        }
        if let Some(token) = &token {
            meta.insert("progressToken".into(), token.clone());
        }
        let id = self.mcp_request(
            server,
            "tools/call",
            json!({"name": call.tool, "arguments": arguments, "_meta": meta}),
        )?;
        let label = call.label.clone().unwrap_or_else(|| call.tool.clone());
        self.calls.insert(
            label.clone(),
            Call {
                server: server.to_owned(),
                generation,
                id,
                token,
            },
        );
        self.last_call = Some(label);
        Ok(())
    }

    fn mcp_await(&mut self, wait: &McpAwait) -> Result<(), End> {
        let (server, id) = {
            let call = self.call(wait.to.as_ref())?;
            (call.server.clone(), call.id.clone())
        };
        let until = deadline(wait.timeout);
        if !wait.none {
            let answer = self.mcp_answer(&server, &id, until)?;
            return match &wait.expect {
                Some(expect) if !matches(expect, &answer) => self.fail(&format!(
                    "{server}'s answer to {id} was {answer}, not {expect}"
                )),
                _ => Ok(()),
            };
        }
        loop {
            self.handle()?;
            if let Some(answer) = self.server(&server)?.answers.remove(&key(&id)) {
                return self.fail(&format!("{server} answered {id}, with {answer}"));
            }
            if self.server(&server)?.closed || past(until) {
                return Ok(());
            }
            if self.closed {
                return self.ended(&format!("checking {server} doesn't answer {id}"));
            }
            self.hear(until);
        }
    }

    fn mcp_progress(&mut self, wait: &McpProgress) -> Result<(), End> {
        let (server, id, token) = {
            let call = self.call(wait.to.as_ref())?;
            (call.server.clone(), call.id.clone(), call.token.clone())
        };
        let Some(token) = token else {
            return self.fail(&format!("the call {id} to {server} asked for no progress"));
        };
        let until = deadline(wait.timeout);
        loop {
            self.handle()?;
            let progress = &mut self.server(&server)?.progress;
            let found = (progress.iter())
                .position(|message| message.pointer("/params/progressToken") == Some(&token));
            if let Some(found) = found {
                let message = progress.remove(found).expect("it's there");
                return match &wait.expect {
                    Some(expect) if !matches(expect, &message) => self.fail(&format!(
                        "{server}'s progress on {id} was {message}, not {expect}"
                    )),
                    _ => Ok(()),
                };
            }
            if self.server(&server)?.closed {
                return self.fail(&format!("{server} closed, with no progress on {id}"));
            }
            if self.closed {
                return self.ended(&format!("waiting for {server}'s progress on {id}"));
            }
            if past(until) || !self.hear(until) {
                return self.fail(&format!(
                    "timed out waiting for {server}'s progress on {id}"
                ));
            }
        }
    }

    /// A call the fake made: the one named, or the last.
    fn call(&mut self, label: Option<&String>) -> Result<&Call, End> {
        let label = label.or(self.last_call.as_ref()).cloned();
        let label = match label {
            Some(label) if self.calls.contains_key(&label) => label,
            label => {
                return self.fail(&format!(
                    "no MCP call made called {}",
                    label.as_deref().unwrap_or("anything")
                ));
            }
        };
        // Its server started again under its name since: the new one
        // never heard of it.
        let call = &self.calls[&label];
        let current = (self.servers.get(&call.server)).map(|server| server.generation);
        if current != Some(call.generation) {
            return self.fail(&format!("the call {label} was to a server since replaced"));
        }
        Ok(&self.calls[&label])
    }

    /// A server the fake started, by its connection's name.
    fn server(&mut self, name: &str) -> Result<&mut Server, End> {
        if self.servers.contains_key(name) {
            Ok(self.servers.get_mut(name).expect("it's there"))
        } else {
            self.fail(&format!("no MCP server started called {name}"))
        }
    }

    /// Sends a server a request, and gives its id.
    fn mcp_request(&mut self, server: &str, method: &str, params: Value) -> Result<Value, End> {
        let found = self.server(server)?;
        let id = json!(found.next_id);
        found.next_id += 1;
        self.mcp_send(
            server,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )?;
        Ok(id)
    }

    /// Waits until `until` for a server's answer to one of the fake's
    /// requests.
    fn mcp_answer(
        &mut self,
        server: &str,
        id: &Value,
        until: Option<Instant>,
    ) -> Result<Value, End> {
        loop {
            self.handle()?;
            if let Some(answer) = self.server(server)?.answers.remove(&key(id)) {
                return Ok(answer);
            }
            if self.server(server)?.closed {
                return self.fail(&format!("{server} closed without answering {id}"));
            }
            if self.closed {
                return self.ended(&format!("waiting for {server}'s answer to {id}"));
            }
            if past(until) || !self.hear(until) {
                return self.fail(&format!("timed out waiting for {server}'s answer to {id}"));
            }
        }
    }

    /// Writes a message to a server's stdin.
    fn mcp_send(&mut self, server: &str, message: &Value) -> Result<(), End> {
        let mut line = serde_json::to_vec(message).expect("JSON is always JSON");
        line.push(b'\n');
        let written = match &mut self.server(server)?.stdin {
            Some(stdin) => stdin.write_all(&line).map_err(|err| err.to_string()),
            None => Err("its stdin is closed".into()),
        };
        match written {
            Ok(()) => Ok(()),
            Err(why) => self.fail(&format!("couldn't write to {server}: {why}")),
        }
    }

    fn send(&mut self, message: &Value) -> Result<(), End> {
        let mut line = serde_json::to_vec(message).expect("JSON is always JSON");
        line.push(b'\n');
        self.write_out(&line)
    }

    fn write_to(&mut self, to: Output, bytes: &[u8]) -> Result<(), End> {
        match to {
            Output::Stdout => self.write_out(bytes),
            Output::Stderr => {
                self.write_err(bytes);
                Ok(())
            }
        }
    }

    /// Writes to stderr, if it's open. Nobody reading it is the client's
    /// business.
    fn write_err(&mut self, bytes: &[u8]) {
        if let Some(err) = &mut self.err {
            let _ = err.write_all(bytes);
        }
    }

    /// Writes to stdout. Once the script closes it, nothing is written. If
    /// the client stops reading, the fake exits, as an adapter whose
    /// client is gone.
    fn write_out(&mut self, bytes: &[u8]) -> Result<(), End> {
        let Some(out) = &mut self.out else {
            return Ok(());
        };
        match out.write_all(bytes) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.record
                    .write(&json!({"gone": format!("stdout: {err}")}));
                Err(End(0))
            }
        }
    }

    /// A check failed: says so in the record and on stderr, and ends.
    /// Stderr is written on a thread, waited for at most 5 s, so a client
    /// that never reads it still sees the fake exit; the fake ends either
    /// way, and the thread with it. The wait costs nothing unless stderr
    /// is full.
    fn fail<T>(&mut self, why: &str) -> Result<T, End> {
        self.record.write(&json!({"failed": why}));
        if let Some(mut err) = self.err.take() {
            let line = format!("caf-fake-agent: {why}\n");
            let (done, written) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = err.write_all(line.as_bytes());
                let _ = done.send(());
            });
            let _ = written.recv_timeout(Duration::from_secs(5));
        }
        Err(End(EXIT_CHECK_FAILED))
    }

    /// Stdin closed while the script waited: says so in the record, and
    /// exits as an adapter does.
    fn ended<T>(&mut self, waiting: &str) -> Result<T, End> {
        self.record
            .write(&json!({"ended": format!("stdin closed while {waiting}")}));
        Err(End(0))
    }
}

/// When a wait of so many milliseconds ends: never, if not given or too
/// far off to say.
fn deadline(ms: Option<u64>) -> Option<Instant> {
    ms.and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)))
}

/// Whether a deadline has passed.
fn past(until: Option<Instant>) -> bool {
    until.is_some_and(|until| Instant::now() >= until)
}

/// Whether a message has this method, if given, and matches this pattern,
/// if given.
fn wanted(method: Option<&String>, pattern: Option<&Value>, message: &Value) -> bool {
    method.is_none_or(|method| message.get("method").and_then(Value::as_str) == Some(method))
        && pattern.is_none_or(|pattern| matches(pattern, message))
}

/// A message's method.
fn method(message: &Value) -> String {
    message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Closes stdout or stderr, and puts `/dev/null` in its place, so no file
/// opened later takes its number and gets written to by mistake, and a
/// holder started later gets `/dev/null` too.
fn close(file: &mut Option<File>) {
    let Some(file) = file.take() else {
        return;
    };
    let fd = file.into_raw_fd();
    match File::options().write(true).open("/dev/null") {
        // SAFETY: both descriptors are open and this owns them. dup2
        // closes `fd` and makes it `/dev/null`, in one step.
        Ok(null) => unsafe {
            libc::dup2(null.as_raw_fd(), fd);
        },
        // SAFETY: this owns `fd`, and nothing else uses it.
        Err(_) => drop(unsafe { File::from_raw_fd(fd) }),
    }
}

/// The `session/update` that reports `tool`.
fn tool_update(tool: &Tool) -> Value {
    let kind = if tool.update {
        "tool_call_update"
    } else {
        "tool_call"
    };
    let mut update = json!({ "sessionUpdate": kind, "toolCallId": tool.id });
    let fields = [
        ("title", tool.title.as_ref().map(|title| json!(title))),
        ("kind", tool.kind.as_ref().map(|kind| json!(kind))),
        ("status", tool.status.as_ref().map(|status| json!(status))),
        (
            "_meta",
            (tool.tool_name.as_ref()).map(|name| json!({ "claudeCode": { "toolName": name } })),
        ),
        (
            "content",
            (tool.content.as_ref()).map(|content| content.iter().map(tool_content).collect()),
        ),
        (
            "locations",
            (tool.locations.as_ref()).map(|locations| json!(locations)),
        ),
        ("rawInput", tool.raw_input.clone()),
        ("rawOutput", tool.raw_output.clone()),
    ];
    for (key, value) in fields {
        if let Some(value) = value {
            update[key] = value;
        }
    }
    update
}

/// A piece of a tool call's content, as ACP says it.
fn tool_content(content: &ToolContent) -> Value {
    match content {
        ToolContent::Text(text) => {
            json!({ "type": "content", "content": { "type": "text", "text": text } })
        }
        ToolContent::Diff(diff) => {
            json!({ "type": "diff", "path": diff.path, "oldText": diff.old, "newText": diff.new })
        }
        ToolContent::Terminal(id) => json!({ "type": "terminal", "terminalId": id }),
        ToolContent::Image(image) => json!({ "type": "content",
            "content": { "type": "image", "mimeType": image.mime, "data": image.data } }),
        ToolContent::Link(link) => json!({ "type": "content",
            "content": { "type": "resource_link", "uri": link.uri, "name": link.name } }),
        ToolContent::Raw(raw) => raw.clone(),
    }
}
