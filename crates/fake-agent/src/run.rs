//! Running a script: the fake's side of the pipes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::pattern::matches;
use crate::script::{Await, LongLine, On, Output, Respond, Script, SpawnHolder, Step, Wait};
use crate::{EXIT_BAD_SCRIPT, EXIT_CHECK_FAILED};

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
    let mut fake = Fake {
        out: Some(out),
        err: Some(err),
        inbox: read_stdin(record.clone(), gate.clone()),
        gate,
        record,
        main: &script.steps,
        at: 0,
        closed: false,
        queue: VecDeque::new(),
        answers: HashMap::new(),
        received: HashMap::new(),
        last_received: None,
        handling: 0,
        answered: HashSet::new(),
        sent: HashMap::new(),
        last_sent: None,
        next_id: 0,
        handlers: Vec::new(),
        holders: Vec::new(),
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
fn read_stdin(record: Record, gate: Arc<Gate>) -> Receiver<Heard> {
    let (send, inbox) = mpsc::channel();
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
    inbox
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
    /// Whether a handler is running: more than 0 if so.
    handling: usize,
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
            Step::Hang => loop {
                self.handle()?;
                self.hear(None);
            },
            Step::SpawnHolder(holder) => self.spawn_holder(holder),
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
            self.handling += 1;
            let done = self.steps(&on.steps);
            self.handling -= 1;
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
    /// that matches, else the first fallback that does, if the rest of the
    /// script never waits for it.
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
            let waited = self.main[self.at.min(self.main.len())..].iter().any(|step| {
                matches!(step, Step::Wait(wait) if wanted(wait.method.as_ref(), wait.matches.as_ref(), message))
            });
            if waited { None } else { matching(true) }
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
                        None if self.handling > 0 => {
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
            command
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
    /// Stderr is written without blocking, so a client that never reads it
    /// still sees the fake exit.
    fn fail<T>(&mut self, why: &str) -> Result<T, End> {
        self.record.write(&json!({"failed": why}));
        if let Some(err) = &mut self.err {
            let fd = err.as_raw_fd();
            // SAFETY: fcntl on a descriptor this owns has no memory
            // effects.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags >= 0 {
                    libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
            }
            let _ = err.write_all(format!("caf-fake-agent: {why}\n").as_bytes());
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
