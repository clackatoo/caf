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
//! - each call to a tool CAF serves is run at most once, refused or
//!   cancelled at most once and never once run (but for a cancel), told
//!   nothing once it ended, and by the end has ended;
//! - its recording replays to the same directives and state;
//! - its chats' conformance fixtures are current (see [`fixtures`]).

use std::collections::BTreeMap;

use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::acp::{Frame, Message, RequestId, RpcError};
use crate::agent::{AgentSpec, EnvChange, SpawnCommand};
use crate::chat::{NoAppData, UserMessage};
use crate::core::{Core, OpenChat, Refused};
use crate::directive::{
    AgentDirective, Directive, PolicyDirective, SetupDirective, TimerDirective, ViewerDirective,
};
use crate::event::{
    AgentEvent, Event, HostCommand, PolicyAnswer, SetupEvent, StorageEvent, ToolEvent,
};
use crate::fixtures::{self, After};
use crate::host::Host;
use crate::ids::{AgentName, CallId, ChatId, QuestionId, Shelf, Ticket, TurnId};
use crate::profiles::{Builtin, ProfileChoice};
use crate::question::Answer;
use crate::save::RestoreChat;
use crate::settings::SettingChoice;
use crate::time::{Mono, Now, Wall};

/// The first id a test's core gives out.
pub(crate) const FIRST: u64 = 1000;

/// The wall clock when a script starts.
pub(crate) const EPOCH: i64 = 1_800_000_000_000;

/// A script being run.
pub(crate) struct Script {
    /// The core.
    pub(crate) core: Core,
    /// The host it was made for.
    host: Host,
    /// The agents it offers.
    agents: Vec<AgentSpec>,
    /// The time now, in milliseconds since the start.
    ms: u64,
    /// Every event given so far, as JSON, with its time.
    trace: Vec<(u64, String)>,
    /// What each event gave out, rendered.
    pub(crate) outs: Vec<Vec<Value>>,
    /// The agent's requests, by process and id, and how often each was
    /// answered.
    answers: BTreeMap<(u64, String), usize>,
    /// Whether its chats' conformance fixtures are checked when it ends.
    fixtures: bool,
    /// The calls to tools CAF serves the driver reported, by its number
    /// for each: whether it was run, and whether it has ended: refused or
    /// cancelled by the core, finished by the driver, or given up on by
    /// the agent.
    calls: BTreeMap<u64, (bool, bool)>,
    /// The calls that became jobs: answered as they did, and stopped, not
    /// answered, by a cancel after.
    jobs: std::collections::BTreeSet<u64>,
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
        // A plain ACP adapter, with no tools of its own.
        AgentSpec::command(
            "plain",
            SpawnCommand::new("/opt/caf/plain-acp"),
            ProfileChoice::Builtin(Builtin::TestPlain),
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
        Self::offering(host, agents())
    }

    /// A script for a core made for `host`, offering `agents`.
    pub(crate) fn offering(host: Host, agents: Vec<AgentSpec>) -> Self {
        Self {
            core: Core::starting_ids_at(host.clone(), agents.clone(), FIRST),
            host,
            agents,
            ms: 0,
            trace: Vec::new(),
            outs: Vec::new(),
            answers: BTreeMap::new(),
            fixtures: true,
            calls: BTreeMap::new(),
            jobs: std::collections::BTreeSet::new(),
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
        if let Event::Tool { call, event } = &event {
            match event {
                ToolEvent::Called { .. } => {
                    self.calls.entry(call.0).or_default();
                }
                // Finished only if the core had it run.
                ToolEvent::Finished { .. } => {
                    if self.core.calls.contains_key(call)
                        && let Some(seen) = self.calls.get_mut(&call.0)
                    {
                        seen.1 = true;
                    }
                }
                ToolEvent::Gone => {
                    if let Some(seen) = self.calls.get_mut(&call.0) {
                        seen.1 = true;
                    }
                }
                // The driver answered the agent as it went on.
                ToolEvent::Detached { .. } => {
                    if self
                        .core
                        .calls
                        .get(call)
                        .is_some_and(|each| each.job.is_none())
                        && let Some(seen) = self.calls.get_mut(&call.0)
                        && seen.0
                    {
                        seen.1 = true;
                        self.jobs.insert(call.0);
                    }
                }
                _ => {}
            }
        }
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
        for directive in &rendered {
            let Some(tool) = directive.get("tool") else {
                continue;
            };
            // What's served is no call's.
            if ["Preview", "Serve", "Unserve"].contains(&tool["type"].as_str().unwrap_or_default())
            {
                continue;
            }
            let call = tool["value"]["call"].as_u64().expect("a call");
            // A job's stopped, not answered.
            if tool["type"] == "Cancel" && self.jobs.remove(&call) {
                continue;
            }
            let seen = self.calls.get_mut(&call);
            let seen = seen.unwrap_or_else(|| panic!("call {call} answered, never called"));
            assert!(
                !seen.1,
                "call {call} told more once it ended: {rendered:#?}"
            );
            if tool["type"] == "Run" {
                assert!(!seen.0, "call {call} run twice");
                seen.0 = true;
            } else {
                assert!(
                    tool["type"] == "Cancel" || !seen.0,
                    "call {call} refused once run"
                );
                seen.1 = true;
            }
        }
        self.outs.push(rendered.clone());
        rendered
    }

    /// The tools driver reports `event` of `call`.
    pub(crate) fn tool(&mut self, call: u64, event: ToolEvent) -> Vec<Value> {
        self.event(Event::Tool {
            call: CallId(call),
            event,
        })
    }

    /// The policy answers `ticket`, giving `reason`.
    pub(crate) fn policy_saying(
        &mut self,
        ticket: u64,
        answer: PolicyAnswer,
        reason: &str,
    ) -> Vec<Value> {
        self.event(Event::Policy {
            ticket: Ticket(ticket),
            answer,
            reason: Some(reason.to_owned()),
        })
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

    /// Tells `chat`'s agent `text`, as a notice.
    pub(crate) fn notify(
        &mut self,
        chat: ChatId,
        text: &str,
    ) -> (Result<TurnId, Refused>, Vec<Value>) {
        let recorded = text.to_owned();
        self.host_call(
            |core, now, out| core.notify(now, chat, text, out),
            |&turn| HostCommand::Notify {
                chat,
                turn,
                text: recorded,
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

    /// Unarchives `chat`.
    pub(crate) fn unarchive(&mut self, chat: ChatId) -> (Result<(), Refused>, Vec<Value>) {
        self.host_call(
            |core, now, out| core.unarchive(now, chat, out),
            |()| HostCommand::Unarchive { chat },
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

    /// Lists `shelf`'s saved chats.
    pub(crate) fn list_saved(&mut self, shelf: &str) -> (Result<(), Refused>, Vec<Value>) {
        let shelf = Shelf(shelf.to_owned());
        let recorded = shelf.clone();
        self.host_call(
            |core, now, out| core.list_saved(now, shelf, out),
            |()| HostCommand::ListSaved(recorded),
        )
    }

    /// Restores a saved chat.
    pub(crate) fn restore(&mut self, restore: RestoreChat) -> (Result<(), Refused>, Vec<Value>) {
        let recorded = restore.clone();
        self.host_call(
            |core, now, out| core.restore(now, restore, out),
            |()| HostCommand::Restore(recorded),
        )
    }

    /// Puts `chat` away.
    pub(crate) fn put_away(&mut self, chat: ChatId) -> (Result<(), Refused>, Vec<Value>) {
        self.host_call(
            |core, now, out| core.put_away(now, chat, out),
            |()| HostCommand::PutAway { chat },
        )
    }

    /// Deletes `chat`, kept on `shelf`.
    pub(crate) fn delete(
        &mut self,
        chat: ChatId,
        shelf: &str,
    ) -> (Result<(), Refused>, Vec<Value>) {
        let shelf = Shelf(shelf.to_owned());
        let recorded = shelf.clone();
        self.host_call(
            |core, now, out| core.delete(now, chat, shelf, out),
            |()| HostCommand::Delete {
                chat,
                shelf: recorded,
            },
        )
    }

    /// Moves every chat on `from` to `to`.
    pub(crate) fn move_shelf(&mut self, from: &str, to: &str) -> (Result<(), Refused>, Vec<Value>) {
        let (from, to) = (Shelf(from.to_owned()), Shelf(to.to_owned()));
        let recorded = (from.clone(), to.clone());
        self.host_call(
            |core, now, out| core.move_shelf(now, from, to, out),
            |()| HostCommand::MoveShelf {
                from: recorded.0,
                to: recorded.1,
            },
        )
    }

    /// Sets `chat`'s data slot.
    pub(crate) fn set_data(&mut self, chat: ChatId) -> (Result<(), Refused>, Vec<Value>) {
        self.host_call(
            |core, now, out| core.set_data(now, chat, NoAppData, out),
            |()| HostCommand::SetData {
                chat,
                data: NoAppData,
            },
        )
    }

    /// Chooses a setting of `chat`'s agent.
    pub(crate) fn set_setting(
        &mut self,
        chat: ChatId,
        choice: SettingChoice,
    ) -> (Result<(), Refused>, Vec<Value>) {
        let recorded = choice.clone();
        self.host_call(
            |core, now, out| core.set_setting(now, chat, choice, out),
            |()| HostCommand::SetSetting {
                chat,
                choice: recorded,
            },
        )
    }

    /// Learns what settings `agent` offers.
    pub(crate) fn learn_settings(&mut self, agent: &str) -> (Result<(), Refused>, Vec<Value>) {
        let name = AgentName(agent.to_owned());
        self.host_call(
            |core, now, out| core.learn_settings(now, &name, out),
            |()| HostCommand::LearnSettings(AgentName(agent.to_owned())),
        )
    }

    /// The storage driver reports `event` on `ticket`.
    pub(crate) fn storage(&mut self, ticket: u64, event: StorageEvent) -> Vec<Value> {
        self.event(Event::Storage {
            ticket: Ticket(ticket),
            event,
        })
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
            reason: None,
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

    /// A core of its own, as this one is now: a fresh one given every
    /// event so far. What's done to it changes nothing here.
    pub(crate) fn copy(&self) -> Core {
        let mut core = Core::starting_ids_at(self.host.clone(), self.agents.clone(), FIRST);
        for (ms, json) in &self.trace {
            let event: Event = serde_json::from_str(json).expect("recorded events read back");
            let now = Now {
                wall: Wall(EPOCH + i64::try_from(*ms).unwrap_or(0)),
                mono: Mono(ms * 1_000_000),
            };
            core.handle(now, event, &mut Vec::new());
        }
        core
    }

    /// A core of its own for an app with items of type `A`, as this one
    /// is now: a fresh one given every event so far, which hold none.
    pub(crate) fn copy_as<A: crate::chat::AppItem>(&self) -> (Core<A>, Now) {
        let mut core = Core::<A>::starting_ids_at(self.host.clone(), self.agents.clone(), FIRST);
        for (ms, json) in &self.trace {
            let event: Event<A> = serde_json::from_str(json).expect("recorded events read back");
            let now = Now {
                wall: Wall(EPOCH + i64::try_from(*ms).unwrap_or(0)),
                mono: Mono(ms * 1_000_000),
            };
            core.handle(now, event, &mut Vec::new());
        }
        (core, self.now())
    }

    /// Feeds the recording to a fresh core, checking it gives the same
    /// directives at every step and ends in the same state.
    pub(crate) fn replay(&self) {
        let mut core = Core::starting_ids_at(self.host.clone(), self.agents.clone(), FIRST);
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
        for (call, (_, ended)) in &self.calls {
            assert!(*ended, "call {call} never ended");
        }
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
            // Tests' chats are numbered ([`ChatId::from_number`]).
            u64::try_from(chat.0.as_u128()).expect("a test's chat id"),
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
            json!({
                "decide": ticket.0, "chat": chat, "item": request.item, "title": request.title,
                "origin": request.origin, "action": request.action, "input": request.input,
                "hints": request.hints,
            })
        }
        Directive::Policy(PolicyDirective::Withdrawn { ticket }) => {
            json!({ "withdrawn": ticket.0 })
        }
        Directive::Viewer(ViewerDirective::Changed { chat, latest }) => {
            json!({ "changed": chat, "latest": latest.0 })
        }
        Directive::Storage(storage) => json!({ "storage": storage }),
        Directive::Setup(setup) => render_setup(setup),
        Directive::Usage(usage) => {
            json!({ "usage": serde_json::to_value(usage).expect("usage directives serialise") })
        }
        Directive::Tool(tool) => {
            json!({ "tool": serde_json::to_value(tool).expect("tool directives serialise") })
        }
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
            size,
            ..
        } => {
            let how = serde_json::to_value(install).expect("installs serialise");
            json!({ "install": ticket.0, "folder": folder, "how": how["type"], "private": private, "size": size })
        }
        SetupDirective::Prepare {
            ticket,
            folder,
            from,
            patch,
            private,
        } => {
            json!({ "prepare": ticket.0, "folder": folder, "from": from, "level": patch.level, "private": private })
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
