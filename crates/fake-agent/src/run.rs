//! Running a script: the fake's side of the pipes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::pattern::matches;
use crate::script::{Await, LongLine, On, Respond, Script, SpawnHolder, Step, Wait};
use crate::{EXIT_BAD_SCRIPT, EXIT_CHECK_FAILED};

/// How much of a long line is written at a time.
const CHUNK: usize = 1 << 20;

/// Runs a script to its end, and returns the code to exit with.
///
/// `record`, an absolute path, if given, gets a line of JSON for each
/// message the client sends and each check that fails: see the crate's
/// docs. Call this once, from `main`: it takes over stdin, stdout and
/// stderr.
#[must_use]
pub fn run(script: &Script, record: Option<&Path>) -> i32 {
    let record = match record.map(Record::open).transpose() {
        Ok(record) => record.unwrap_or_default(),
        Err(err) => {
            eprintln!("caf-fake-agent: {err}");
            return EXIT_BAD_SCRIPT;
        }
    };
    // SAFETY: fds 1 and 2 are open from the process's start, and nothing
    // else in this program writes to them, so these become their only
    // owners.
    let (out, err) = unsafe { (File::from_raw_fd(1), File::from_raw_fd(2)) };
    let mut fake = Fake {
        out: Some(out),
        err: Some(err),
        inbox: read_stdin(record.clone()),
        record,
        closed: false,
        queue: VecDeque::new(),
        answers: HashMap::new(),
        received: HashMap::new(),
        last_received: None,
        answered: HashSet::new(),
        sent: HashMap::new(),
        last_sent: None,
        next_id: 0,
        handlers: Vec::new(),
        holders: Vec::new(),
    };
    match fake.steps(&script.steps).and_then(|()| fake.finish()) {
        Ok(()) => 0,
        Err(End(code)) => code,
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
/// while it writes, and both pipes can't fill at once.
fn read_stdin(record: Record) -> Receiver<Heard> {
    let (send, inbox) = mpsc::channel();
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
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

struct Fake {
    /// Stdout, until the script closes it.
    out: Option<File>,
    /// Stderr, until the script closes it.
    err: Option<File>,
    inbox: Receiver<Heard>,
    record: Record,
    /// Whether the client closed stdin.
    closed: bool,
    /// Requests and notifications from the client not yet waited for, in
    /// the order they came.
    queue: VecDeque<Value>,
    /// The client's answers not yet awaited, by id.
    answers: HashMap<String, Value>,
    /// Requests and notifications waited for, by name.
    received: HashMap<String, Value>,
    /// The name of the one last waited for.
    last_received: Option<String>,
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

impl Fake {
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
                let until = Instant::now() + Duration::from_millis(*ms);
                loop {
                    self.handle()?;
                    // Checked apart from hearing, so a flood can't make
                    // it sleep longer.
                    if Instant::now() >= until || !self.hear(Some(until)) {
                        return self.handle();
                    }
                }
            }
            Step::Line(text) => self.write_out(format!("{text}\n").as_bytes()),
            Step::Json(value) => self.send(value),
            Step::Bytes(bytes) => self.write_out(bytes),
            Step::LongLine(line) => self.long_line(line),
            Step::Stderr(text) => {
                if let Some(err) = &mut self.err {
                    // Nobody reading stderr is the client's business.
                    let _ = err.write_all(format!("{text}\n").as_bytes());
                }
                Ok(())
            }
            Step::Note(text) => {
                self.record.write(&json!({"note": text}));
                Ok(())
            }
            Step::Exit(code) => Err(End(*code)),
            Step::CloseStdout => {
                close(&mut self.out, 1);
                Ok(())
            }
            Step::CloseStderr => {
                close(&mut self.err, 2);
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
            let found = self.handlers.iter().position(|handler| {
                !handler.done
                    && wanted(
                        handler.on.method.as_ref(),
                        handler.on.matches.as_ref(),
                        &self.queue[at],
                    )
            });
            let Some(found) = found else {
                at += 1;
                continue;
            };
            let message = self.queue.remove(at).expect("it's there");
            let handler = &mut self.handlers[found];
            handler.done = handler.on.once;
            let on = handler.on.clone();
            let label = on.label.clone().unwrap_or_else(|| method(&message));
            self.received.insert(label.clone(), message);
            let last = self.last_received.replace(label);
            let done = self.steps(&on.steps);
            self.last_received = last;
            done?;
        }
        Ok(())
    }

    fn wait(&mut self, wait: &Wait) -> Result<(), End> {
        let until = wait
            .timeout
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        loop {
            self.handle()?;
            let found = self
                .queue
                .iter()
                .position(|message| wanted(wait.method.as_ref(), wait.matches.as_ref(), message));
            if let Some(found) = found {
                let message = self.queue.remove(found).expect("it's there");
                let label = wait.label.clone().unwrap_or_else(|| method(&message));
                self.received.insert(label.clone(), message);
                self.last_received = Some(label);
                return Ok(());
            }
            // The client is gone, so what's waited for can't come: exit,
            // as an adapter does when its stdin closes.
            if self.closed {
                return Err(End(0));
            }
            if until.is_some_and(|until| Instant::now() >= until) || !self.hear(until) {
                return self.fail(&format!(
                    "timed out waiting for {}",
                    wait.method.as_deref().unwrap_or("a message")
                ));
            }
        }
    }

    fn respond(&mut self, respond: &Respond) -> Result<(), End> {
        let id = match &respond.id {
            Some(id) => id.clone(),
            None => {
                let label = respond.to.as_ref().or(self.last_received.as_ref());
                let Some(message) = label.and_then(|label| self.received.get(label)) else {
                    return self.fail(&format!(
                        "nothing to answer called {}",
                        label.map_or("anything", String::as_str)
                    ));
                };
                let Some(id) = message.get("id").cloned() else {
                    return self.fail(&format!("{} is a notification", method(message)));
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
        let until = wait
            .timeout
            .map(|ms| Instant::now() + Duration::from_millis(ms));
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
                return Err(End(0));
            }
            if until.is_some_and(|until| Instant::now() >= until) || !self.hear(until) {
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
        self.write_out(&head)?;
        let chunk = vec![fill; left.min(CHUNK)];
        while left > 0 {
            let now = left.min(CHUNK);
            self.write_out(&chunk[..now])?;
            left -= now;
        }
        self.write_out(tail)
    }

    fn spawn_holder(&mut self, holder: &SpawnHolder) -> Result<(), End> {
        let spawned = std::env::current_exe().and_then(|program| {
            Command::new(program)
                .arg("--hold")
                .arg(holder.secs.unwrap_or(60).to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
        });
        let child = match spawned {
            Ok(child) => child,
            Err(err) => return self.fail(&format!("couldn't start a holder: {err}")),
        };
        if let Some(path) = &holder.pid_file {
            // Renamed into place, so it's never read half written.
            let new = path.with_extension("new");
            let written = std::fs::write(&new, child.id().to_string())
                .and_then(|()| std::fs::rename(&new, path));
            if let Err(err) = written {
                return self.fail(&format!("{}: {err}", path.display()));
            }
        }
        self.holders.push(child);
        Ok(())
    }

    fn send(&mut self, message: &Value) -> Result<(), End> {
        let mut line = serde_json::to_vec(message).expect("JSON is always JSON");
        line.push(b'\n');
        self.write_out(&line)
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
    fn fail<T>(&mut self, why: &str) -> Result<T, End> {
        self.record.write(&json!({"failed": why}));
        if let Some(err) = &mut self.err {
            let _ = err.write_all(format!("caf-fake-agent: {why}\n").as_bytes());
        }
        Err(End(EXIT_CHECK_FAILED))
    }
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
/// opened later takes its number and gets written to by mistake.
fn close(file: &mut Option<File>, fd: i32) {
    if file.take().is_some()
        && let Ok(null) = File::options().write(true).open("/dev/null")
        && null.as_raw_fd() == fd
    {
        // Open for the rest of the process's life.
        let _ = null.into_raw_fd();
    }
}
