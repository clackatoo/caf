//! The scripted-event harness every core test runs on.
//!
//! A test is a script: events at times in, and the directives they give
//! out, rendered as JSON, plus the core's whole state
//! ([`Core::inspect`]). JSON, because recorded traces are JSON: every
//! event a script gives the core is recorded as JSON, and
//! [`Script::replay`] feeds the recording to a fresh core through
//! [`Core::handle`], which must give the same directives and end in the
//! same state. That's how a bug becomes a recorded trace.
//!
//! Ids start at 1000, and the clock is always given in: `at` sets it in
//! milliseconds since the script started.
//!
//! Every script also checks, whatever the test:
//! - each event gives out at most one wake-up time, and once woken the
//!   core never waits to be woken at a time that has passed, which would
//!   have the timer driver wake it over and over;
//! - no request of the agent's is answered twice, and by the end each is
//!   answered once, unless its process went before it could be;
//! - its recording replays to the same directives and state;
//! - its chats' conformance fixtures are current (see [`fixtures`]).

use std::collections::BTreeMap;

use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::acp::{Frame, Message, RequestId, RpcError};
use crate::agent::{AgentSpec, EnvChange, SpawnCommand};
use crate::chat::UserMessage;
use crate::core::{Core, OpenChat, Refused};
use crate::directive::{
    AgentDirective, Directive, PolicyDirective, SetupDirective, TimerDirective, ViewerDirective,
};
use crate::event::{AgentEvent, Event, HostCommand, PolicyAnswer, SetupEvent};
use crate::fixtures::{self, After};
use crate::host::Host;
use crate::ids::{AgentName, ChatId, QuestionId, Ticket, TurnId};
use crate::profiles::{Builtin, ProfileChoice};
use crate::question::Answer;
use crate::time::{Mono, Now, Wall};

/// The first id a test's core gives out.
pub(crate) const FIRST: u64 = 1000;

/// The wall clock when a script starts.
const EPOCH: i64 = 1_800_000_000_000;

/// A script being run.
pub(crate) struct Script {
    /// The core.
    pub(crate) core: Core,
    /// The host it was made for.
    host: Host,
    /// The time now, in milliseconds since the start.
    ms: u64,
    /// Every event given so far, as JSON, with its time.
    trace: Vec<(u64, String)>,
    /// What each event gave out, rendered.
    outs: Vec<Vec<Value>>,
    /// The agent's requests, by process and id, and how often each was
    /// answered.
    answers: BTreeMap<(u64, String), usize>,
    /// Whether its chats' conformance fixtures are checked when it ends.
    fixtures: bool,
}

/// The agents a test offers: Claude, as the Claude profile expects it.
pub(crate) fn agents() -> Vec<AgentSpec> {
    let claude = SpawnCommand::new("/opt/caf/claude-agent-acp");
    let mut npx = SpawnCommand::new("/usr/bin/npx");
    npx.args = vec![
        "-y".into(),
        "@agentclientprotocol/claude-agent-acp@0.98.0".into(),
    ];
    npx.env = vec![EnvChange::Set("NPM_CONFIG_CACHE".into(), "/tmp/npm".into())];
    vec![
        AgentSpec::command(
            "claude",
            claude,
            ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
        ),
        AgentSpec::command(
            "plain",
            SpawnCommand::new("/opt/caf/plain-acp"),
            ProfileChoice::Cautious,
        ),
        // Two made-up agents CAF installs, with recipes of different
        // shapes, so nothing assumes Claude.
        AgentSpec::installed("npm-agent", Builtin::TestNpm),
        AgentSpec::installed("dl-agent", Builtin::TestDownload),
        // Another name for the npm agent, as another host might call it:
        // it shares its install.
        AgentSpec::installed("npm-twin", Builtin::TestNpm),
        // The npm agent, started by a host's own command.
        AgentSpec::command(
            "npm-host",
            SpawnCommand::new("/opt/npm-host/test-npm-acp"),
            ProfileChoice::Builtin(Builtin::TestNpm),
        ),
        // The downloaded agent, started by a host's own command: its adapter
        // doesn't say by itself whether it's signed in.
        AgentSpec::command(
            "dl-host",
            SpawnCommand::new("/opt/dl-host/test-download"),
            ProfileChoice::Builtin(Builtin::TestDownload),
        ),
        // Claude, run by a host as `npx -y <the adapter>`.
        AgentSpec::command(
            "claude-npx",
            npx,
            ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
        ),
    ]
}

/// A host for tests: every timer is its default, set out here so tests
/// can count on it.
pub(crate) fn host() -> Host {
    Host::new("caf-test", "1.2.3")
}

impl Script {
    /// A script for a core made for `host`, offering [`agents`].
    pub(crate) fn new(host: Host) -> Self {
        Self {
            core: Core::starting_ids_at(host.clone(), agents(), FIRST),
            host,
            ms: 0,
            trace: Vec::new(),
            outs: Vec::new(),
            answers: BTreeMap::new(),
            fixtures: true,
        }
    }

    /// Writes no conformance fixtures: for a script that only makes a
    /// recording, of a scenario another script already runs.
    pub(crate) fn without_fixtures(&mut self) -> &mut Self {
        self.fixtures = false;
        self
    }

    /// Sets the clock to `ms` milliseconds since the start.
    pub(crate) fn at(&mut self, ms: u64) -> &mut Self {
        assert!(ms >= self.ms, "the clock never goes back");
        self.ms = ms;
        self
    }

    /// The time now.
    pub(crate) fn now(&self) -> Now {
        Now {
            wall: Wall(EPOCH + i64::try_from(self.ms).unwrap_or(0)),
            mono: Mono(self.ms * 1_000_000),
        }
    }

    /// Records `event`, as a recording would hold it.
    fn record(&mut self, event: &Event) {
        let json = serde_json::to_string(event).expect("events serialise");
        self.trace.push((self.ms, json));
    }

    /// Gives the core `event`, and returns what it gave out.
    pub(crate) fn event(&mut self, event: Event) -> Vec<Value> {
        self.record(&event);
        let mut out = Vec::new();
        self.core.handle(self.now(), event, &mut out);
        self.done(&out)
    }

    /// Keeps and returns what an event gave out, rendered.
    fn done(&mut self, out: &[Directive]) -> Vec<Value> {
        let rendered: Vec<Value> = out.iter().map(render).collect();
        assert!(
            with(&rendered, "wake_at").len() <= 1,
            "one wake-up at most: {rendered:#?}"
        );
        for directive in &rendered {
            let Some(proc) = directive.get("write").and_then(Value::as_u64) else {
                continue;
            };
            let message = &directive["message"];
            if message.get("method").is_none() {
                let count = self
                    .answers
                    .entry((proc, message["id"].to_string()))
                    .or_default();
                *count += 1;
                assert_eq!(
                    *count, 1,
                    "the agent's request {} answered twice",
                    message["id"]
                );
            }
        }
        self.outs.push(rendered.clone());
        rendered
    }

    /// Runs a host method that, if it succeeds, the recording holds as
    /// `command`.
    fn host_call<T>(
        &mut self,
        call: impl FnOnce(&mut Core, Now, &mut Vec<Directive>) -> Result<T, Refused>,
        command: impl FnOnce(&T) -> HostCommand,
    ) -> (Result<T, Refused>, Vec<Value>) {
        let mut out = Vec::new();
        let now = self.now();
        let result = call(&mut self.core, now, &mut out);
        match &result {
            Ok(done) => {
                let event = Event::Host(command(done));
                self.record(&event);
                let rendered = self.done(&out);
                (result, rendered)
            }
            Err(_) => {
                assert!(out.is_empty(), "a refused command gives nothing out");
                (result, Vec::new())
            }
        }
    }

    /// Opens a chat.
    pub(crate) fn open(&mut self, open: OpenChat) -> (Result<ChatId, Refused>, Vec<Value>) {
        let recorded = open.clone();
        self.host_call(
            |core, now, out| core.open_chat(now, open, out),
            |&chat| HostCommand::OpenChat {
                chat,
                open: recorded,
            },
        )
    }

    /// Sends `text`.
    pub(crate) fn send(
        &mut self,
        chat: ChatId,
        text: &str,
    ) -> (Result<TurnId, Refused>, Vec<Value>) {
        self.send_message(chat, UserMessage::text(text))
    }

    /// Sends `message`.
    pub(crate) fn send_message(
        &mut self,
        chat: ChatId,
        message: UserMessage,
    ) -> (Result<TurnId, Refused>, Vec<Value>) {
        let recorded = message.clone();
        self.host_call(
            |core, now, out| core.send(now, chat, message, out),
            |&turn| HostCommand::Send {
                chat,
                turn,
                message: recorded,
            },
        )
    }

    /// Stops `chat`.
    pub(crate) fn stop(&mut self, chat: ChatId) -> (Result<(), Refused>, Vec<Value>) {
        self.host_call(
            |core, now, out| core.stop(now, chat, out),
            |()| HostCommand::Stop { chat },
        )
    }

    /// Answers `question`.
    pub(crate) fn answer(
        &mut self,
        question: QuestionId,
        answer: Answer,
    ) -> (Result<(), Refused>, Vec<Value>) {
        let recorded = answer.clone();
        self.host_call(
            |core, now, out| core.answer(now, question, answer, out),
            |()| HostCommand::Answer {
                question,
                answer: recorded,
            },
        )
    }

    /// Closes `chat`.
    pub(crate) fn close(&mut self, chat: ChatId) -> (Result<(), Refused>, Vec<Value>) {
        self.host_call(
            |core, now, out| core.close_chat(now, chat, out),
            |()| HostCommand::CloseChat { chat },
        )
    }

    /// Shuts everything down.
    pub(crate) fn shutdown(&mut self) -> Vec<Value> {
        let (_, out) = self.host_call(
            |core, now, out| {
                core.shutdown(now, out);
                Ok(())
            },
            |()| HostCommand::Shutdown,
        );
        out
    }

    /// Looks at what's installed.
    pub(crate) fn check_agents(&mut self) -> Vec<Value> {
        let (_, out) = self.host_call(
            |core, now, out| {
                core.check_agents(now, out);
                Ok(())
            },
            |()| HostCommand::CheckAgents,
        );
        out
    }

    /// Installs `agent`.
    pub(crate) fn install(&mut self, agent: &str) -> (Result<(), Refused>, Vec<Value>) {
        let name = AgentName(agent.to_owned());
        self.host_call(
            |core, now, out| core.install(now, &name, out),
            |()| HostCommand::Install(AgentName(agent.to_owned())),
        )
    }

    /// Stops installing `agent`.
    pub(crate) fn stop_install(&mut self, agent: &str) -> (Result<(), Refused>, Vec<Value>) {
        let name = AgentName(agent.to_owned());
        self.host_call(
            |core, now, out| core.stop_install(now, &name, out),
            |()| HostCommand::StopInstall(AgentName(agent.to_owned())),
        )
    }

    /// Signs in to `agent`.
    pub(crate) fn sign_in(&mut self, agent: &str) -> (Result<(), Refused>, Vec<Value>) {
        let name = AgentName(agent.to_owned());
        self.host_call(
            |core, now, out| core.sign_in(now, &name, out),
            |()| HostCommand::SignIn(AgentName(agent.to_owned())),
        )
    }

    /// Pastes `text` to `agent`'s sign-in.
    pub(crate) fn paste(&mut self, agent: &str, text: &str) -> (Result<(), Refused>, Vec<Value>) {
        let name = AgentName(agent.to_owned());
        self.host_call(
            |core, now, out| core.paste(now, &name, text.to_owned(), out),
            |()| HostCommand::Paste {
                agent: AgentName(agent.to_owned()),
                text: text.to_owned(),
            },
        )
    }

    /// Stops signing in to `agent`.
    pub(crate) fn stop_sign_in(&mut self, agent: &str) -> (Result<(), Refused>, Vec<Value>) {
        let name = AgentName(agent.to_owned());
        self.host_call(
            |core, now, out| core.stop_sign_in(now, &name, out),
            |()| HostCommand::StopSignIn(AgentName(agent.to_owned())),
        )
    }

    /// Signs out of `agent`.
    pub(crate) fn sign_out(&mut self, agent: &str) -> (Result<(), Refused>, Vec<Value>) {
        let name = AgentName(agent.to_owned());
        self.host_call(
            |core, now, out| core.sign_out(now, &name, out),
            |()| HostCommand::SignOut(AgentName(agent.to_owned())),
        )
    }

    /// The setup driver reports `event` on `ticket`.
    pub(crate) fn setup(&mut self, ticket: u64, event: SetupEvent) -> Vec<Value> {
        self.event(Event::Setup {
            ticket: Ticket(ticket),
            event,
        })
    }

    /// Where every agent stands, as JSON.
    pub(crate) fn agents(&self) -> Value {
        serde_json::to_value(self.core.agents()).expect("statuses serialise")
    }

    /// Where `agent` stands, as JSON.
    pub(crate) fn agent_status(&self, agent: &str) -> Value {
        let all = self.agents();
        (all.as_array().into_iter().flatten())
            .find(|each| each["name"] == agent)
            .cloned()
            .unwrap_or_else(|| panic!("no agent {agent} in {all:#}"))
    }

    /// Something happens to process `proc`.
    pub(crate) fn agent(&mut self, proc: u64, event: AgentEvent) -> Vec<Value> {
        self.event(Event::Agent {
            process: Ticket(proc),
            event,
        })
    }

    /// Process `proc` writes `message`, a JSON-RPC message.
    pub(crate) fn says(&mut self, proc: u64, message: &Value) -> Vec<Value> {
        if let (Some(id), Some(_)) = (message.get("id"), message.get("method")) {
            self.answers.entry((proc, id.to_string())).or_default();
        }
        self.agent(proc, AgentEvent::Frame(frame(message)))
    }

    /// Process `proc` answers CAF's request `id` with `result`.
    pub(crate) fn answers(&mut self, proc: u64, id: u64, result: &Value) -> Vec<Value> {
        self.says(
            proc,
            &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        )
    }

    /// Process `proc` answers CAF's request `id` with an error.
    pub(crate) fn fails(&mut self, proc: u64, id: u64, error: &Value) -> Vec<Value> {
        self.says(proc, &json!({ "jsonrpc": "2.0", "id": id, "error": error }))
    }

    /// Process `proc` sends `update` for session `session`.
    pub(crate) fn update(&mut self, proc: u64, session: &str, update: &Value) -> Vec<Value> {
        let params = json!({ "sessionId": session, "update": update });
        self.says(
            proc,
            &json!({ "jsonrpc": "2.0", "method": "session/update", "params": params }),
        )
    }

    /// The timer driver wakes the core.
    pub(crate) fn woken(&mut self) -> Vec<Value> {
        let out = self.event(Event::Woken);
        let now = self.now().mono;
        assert!(
            self.core.wake.is_none_or(|wake| wake > now),
            "woken, it waits for a time already past: {:?} at {now:?}",
            self.core.wake
        );
        out
    }

    /// The policy answers.
    pub(crate) fn policy(&mut self, ticket: u64, answer: PolicyAnswer) -> Vec<Value> {
        self.event(Event::Policy {
            ticket: Ticket(ticket),
            answer,
        })
    }

    /// The core's state, as JSON.
    pub(crate) fn state(&self) -> Value {
        self.core.inspect()
    }

    /// A chat's state, as JSON.
    pub(crate) fn chat(&self, chat: ChatId) -> Value {
        serde_json::to_value(self.core.chat(chat).expect("the chat")).expect("chats serialise")
    }

    /// Feeds the recording to a fresh core, checking it gives the same
    /// directives at every step and ends in the same state.
    pub(crate) fn replay(&self) {
        let mut core = Core::starting_ids_at(self.host.clone(), agents(), FIRST);
        for (step, ((ms, json), expected)) in self.trace.iter().zip(&self.outs).enumerate() {
            let event: Event = serde_json::from_str(json).expect("recorded events read back");
            let now = Now {
                wall: Wall(EPOCH + i64::try_from(*ms).unwrap_or(0)),
                mono: Mono(ms * 1_000_000),
            };
            let mut out = Vec::new();
            core.handle(now, event, &mut out);
            let got: Vec<Value> = out.iter().map(render).collect();
            assert_eq!(&got, expected, "step {step} of the replay, {json}");
        }
        assert_eq!(
            core.inspect(),
            self.state(),
            "the replay ends in the same state"
        );
    }

    /// Checks every request of the agent's was answered once, unless its
    /// process went before it could be.
    fn check_answers(&self) {
        for ((proc, id), &count) in &self.answers {
            let writable = self
                .core
                .procs
                .get(&Ticket(*proc))
                .is_some_and(|each| each.writable());
            assert!(
                count == 1 || !writable,
                "the agent's request {id} to {proc} was never answered"
            );
        }
    }

    /// A script that gives a core the recording `json` (see
    /// [`Script::recording`]), made for `host`.
    pub(crate) fn from_recording(host: Host, json: &str) -> Self {
        #[derive(serde::Deserialize)]
        struct Step {
            at: u64,
            event: Box<RawValue>,
        }
        let steps: Vec<Step> = serde_json::from_str(json).expect("a recording");
        let mut script = Self::new(host);
        for step in steps {
            let event: Event = serde_json::from_str(step.event.get()).expect("a recorded event");
            script.at(step.at);
            script.event(event);
        }
        script
    }

    /// The recording, as one JSON document.
    pub(crate) fn recording(&self) -> Value {
        let steps: Vec<Value> = (self.trace.iter())
            .map(|(ms, json)| json!({ "at": ms, "event": serde_json::from_str::<Value>(json).expect("JSON") }))
            .collect();
        Value::Array(steps)
    }
}

impl Drop for Script {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.check_answers();
            self.replay();
            if self.fixtures {
                check_fixtures(&self.core);
            }
        }
    }
}

/// Checks the conformance fixtures of `core`'s chats (see
/// [`fixtures`]): each chat's record, change by change, as its tests saw
/// it.
pub(crate) fn check_fixtures(core: &Core) {
    let chats = core.applied.iter().map(|(chat, applied)| {
        let steps = (applied.steps.iter()).map(|(change, chat)| (change, After::Chat(chat)));
        (
            chat.0,
            fixtures::text(&applied.picture, applied.latest, steps),
        )
    });
    fixtures::check_script(&fixtures::next_script(), chats);
}

/// `message`, a JSON-RPC message, as the agent driver hands it over.
pub(crate) fn frame(message: &Value) -> Frame {
    let raw = |value: &Value| RawValue::from_string(value.to_string()).expect("JSON");
    let id = message.get("id").map(|id| match id {
        Value::Number(number) => RequestId::Number(number.as_i64().expect("a whole number")),
        Value::String(text) => RequestId::String(text.clone()),
        _ => RequestId::Null,
    });
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let params = message.get("params").map(raw);
    Frame::Message(match (id, method) {
        (Some(id), Some(method)) => Message::Request { id, method, params },
        (None, Some(method)) => Message::Notification { method, params },
        (Some(id), None) => Message::Response {
            id,
            outcome: match (message.get("result"), message.get("error")) {
                (_, Some(error)) => Err(RpcError {
                    code: error["code"].as_i64().expect("a code"),
                    message: error["message"].as_str().unwrap_or_default().to_owned(),
                    data: error.get("data").map(raw),
                }),
                (Some(result), None) => Ok(raw(result)),
                (None, None) => panic!("an answer needs a result or an error"),
            },
        },
        (None, None) => panic!("not JSON-RPC"),
    })
}

/// A directive, as tests read it.
pub(crate) fn render(directive: &Directive) -> Value {
    match directive {
        Directive::Agent(agent) => match agent {
            AgentDirective::Start {
                process,
                command,
                hold,
            } => {
                let mut start = json!({ "start": process.0, "program": command.program });
                // From CAF's install: how it's started is the recipe's.
                if let Some(hold) = hold {
                    start["hold"] = json!(hold);
                    start["command"] = spawn(command);
                }
                start
            }
            AgentDirective::Write { process, bytes } => {
                assert_eq!(bytes.last(), Some(&b'\n'), "each message ends its line");
                let message: Value = serde_json::from_slice(bytes).expect("CAF writes JSON");
                json!({ "write": process.0, "message": message })
            }
            AgentDirective::CloseInput { process } => json!({ "close_input": process.0 }),
            AgentDirective::Signal { process, signal } => {
                json!({ "signal": process.0, "is": format!("{signal:?}") })
            }
            AgentDirective::CheckGroup { process } => json!({ "check_group": process.0 }),
            AgentDirective::Forget { process } => json!({ "forget": process.0 }),
        },
        Directive::Timer(TimerDirective::WakeAt(at)) => {
            json!({ "wake_at": at.map(|at| at.0 / 1_000_000) })
        }
        Directive::Policy(PolicyDirective::Decide {
            ticket,
            chat,
            request,
        }) => {
            json!({ "decide": ticket.0, "chat": chat.0, "title": request.title, "input": request.input })
        }
        Directive::Policy(PolicyDirective::Withdrawn { ticket }) => {
            json!({ "withdrawn": ticket.0 })
        }
        Directive::Viewer(ViewerDirective::Changed { chat, latest }) => {
            json!({ "changed": chat.0, "latest": latest.0 })
        }
        Directive::Storage(storage) => match *storage {},
        Directive::Setup(setup) => render_setup(setup),
        Directive::Report(report) => {
            json!({ "report": format!("{:?}", report.level), "message": report.message })
        }
    }
}

/// A directive for the setup driver, as tests read it.
fn render_setup(setup: &SetupDirective) -> Value {
    match setup {
        SetupDirective::Survey { ticket } => json!({ "survey": ticket.0 }),
        SetupDirective::Install {
            ticket,
            folder,
            install,
            private,
        } => {
            let how = serde_json::to_value(install).expect("installs serialise");
            json!({ "install": ticket.0, "folder": folder, "how": how["type"], "private": private })
        }
        SetupDirective::Remove {
            ticket,
            folder,
            unused_since,
        } => {
            json!({ "remove": ticket.0, "folder": folder, "unused_since": unused_since.0 })
        }
        SetupDirective::SignIn {
            ticket,
            command,
            link,
            code_after,
            paste,
        } => json!({
            "sign_in": ticket.0,
            "command": spawn(command),
            "link": link,
            "code_after": code_after,
            "paste": paste,
        }),
        SetupDirective::Paste { ticket, text } => json!({ "paste": ticket.0, "text": text }),
        SetupDirective::Run { ticket, command } => {
            json!({ "run": ticket.0, "command": spawn(command) })
        }
        SetupDirective::Status {
            ticket,
            command,
            signed_in,
            how,
        } => json!({
            "status": ticket.0,
            "command": spawn(command),
            "signed_in": signed_in,
            "how": how,
        }),
        SetupDirective::Stop { ticket } => json!({ "stop": ticket.0 }),
    }
}

/// A command, as tests read it: its program, arguments and variables as
/// text.
pub(crate) fn spawn(command: &SpawnCommand) -> Value {
    let text = |text: &std::ffi::OsString| text.to_string_lossy().into_owned();
    let env: Vec<String> = (command.env.iter())
        .map(|change| match change {
            EnvChange::Set(name, value) => format!("{}={}", text(name), text(value)),
            EnvChange::Remove(name) => format!("-{}", text(name)),
        })
        .collect();
    json!({
        "program": command.program,
        "args": command.args.iter().map(text).collect::<Vec<_>>(),
        "env": env,
    })
}

/// Whether `actual` has everything `expected` has: every key of an
/// object, arrays element by element, and equal values.
pub(crate) fn includes(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            expected.iter().all(|(key, expected)| {
                actual
                    .get(key)
                    .is_some_and(|actual| includes(actual, expected))
            })
        }
        (Value::Array(actual), Value::Array(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| includes(actual, expected))
        }
        _ => actual == expected,
    }
}

/// Asserts `actual` has everything `expected` has (see [`includes`]).
#[track_caller]
pub(crate) fn assert_has(actual: &Value, expected: &Value) {
    assert!(
        includes(actual, expected),
        "expected\n{}\nin\n{}",
        serde_json::to_string_pretty(expected).unwrap_or_default(),
        serde_json::to_string_pretty(actual).unwrap_or_default()
    );
}

/// The messages written to processes in `out`, as `(process, message)`.
pub(crate) fn writes(out: &[Value]) -> Vec<(u64, Value)> {
    (out.iter())
        .filter_map(|directive| {
            Some((
                directive.get("write")?.as_u64()?,
                directive["message"].clone(),
            ))
        })
        .collect()
}

/// The one message written in `out` with `method`.
#[track_caller]
pub(crate) fn sent(out: &[Value], method: &str) -> Value {
    let found: Vec<Value> = writes(out)
        .into_iter()
        .map(|(_, message)| message)
        .filter(|message| message["method"] == method)
        .collect();
    assert_eq!(found.len(), 1, "one {method} in {out:#?}");
    found.into_iter().next().unwrap_or_default()
}

/// The answers written in `out` to the agent's request `id`.
pub(crate) fn replies(out: &[Value], id: &Value) -> Vec<Value> {
    (writes(out).into_iter())
        .map(|(_, message)| message)
        .filter(|message| message.get("method").is_none() && message.get("id") == Some(id))
        .collect()
}

/// The directives in `out` that have `key`.
pub(crate) fn with(out: &[Value], key: &str) -> Vec<Value> {
    out.iter()
        .filter(|directive| directive.get(key).is_some())
        .cloned()
        .collect()
}
