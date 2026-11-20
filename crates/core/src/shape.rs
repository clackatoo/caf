//! Tests of the one shape every CAF type is written in (see the crate
//! docs): one of every type, written and read back through JSON and
//! through MessagePack in named mode, and the exact shape of some. Its
//! sample values are other tests' too.

use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::acp::{Frame, Message, RequestId, RpcError, Shape};
use crate::agent::{AgentSpec, EnvChange, McpServer, SpawnCommand};
use crate::change::{Change, Mismatch, Numbered};
use crate::chat::{
    Chat, ChatState, Compacted, Cost, Failure, FailureReason, Incompatible, Item, ItemKind, Lost,
    Note, Part, Remedy, Starter, ToolCall, ToolKind, ToolStatus, Turn, TurnLife, Unknown, Usage,
    UserMessage,
};
use crate::core::{ConnectionInfo, OpenChat, Refused, SessionChoice};
use crate::directive::{
    AgentDirective, Directive, Level, PermissionRequest, PolicyDirective, Report, Signal,
    TimerDirective, ViewerDirective,
};
use crate::event::{AgentEvent, Event, Exit, HostCommand, PolicyAnswer};
use crate::host::{Grouping, Host, Limits, Shows, Timers};
use crate::ids::{
    AgentName, AgentSessionId, ChangeNo, ChatId, ConnectionId, ItemId, QuestionId, Ticket, TurnId,
};
use crate::profiles::{AdapterInfo, Builtin, Checked, ProfileChoice, ProfileReport};
use crate::question::{
    Answer, OptionId, OptionKind, Permission, PermissionOption, Question, QuestionKind, SlowChoice,
    SlowToOpen,
};
use crate::time::{End, Mono, Now, Wall, Window};
use crate::viewer::{ChatSummary, FromViewer, OpenRequest, Outcome, RequestNo, ToViewer};

/// Values as CAF writes them, each with its TypeScript type: one of
/// every type, and everything from replaying the recorded traces. The
/// shape tests check each reads back, and `ts.rs` writes them all to
/// `ts/sdk/test/shapes.gen.ts`, where TypeScript checks its generated
/// types take them.
///
/// To add values, such as the viewer wire's sample messages, call
/// [`Samples::both`] or [`Samples::written`] with them (or
/// [`Samples::raw`] with text written by something else), from
/// [`every_type`] or a function like it that `ts.rs` also calls.
#[derive(Default)]
pub(crate) struct Samples {
    /// Each value's TypeScript type, naming the generated types as `T.…`,
    /// and its JSON.
    pub(crate) values: Vec<(String, String)>,
}

impl Samples {
    /// Adds `value`, as JSON, once it has read back equal from JSON and
    /// from MessagePack in named mode.
    #[track_caller]
    pub(crate) fn both<T: Serialize + DeserializeOwned + PartialEq + Debug>(
        &mut self,
        value: &T,
    ) -> Value {
        let text = serde_json::to_string(value).expect("it writes as JSON");
        let back: T = serde_json::from_str(&text).expect("it reads back from JSON");
        assert_eq!(&back, value, "through JSON: {text}");
        let packed = rmp_serde::to_vec_named(value).expect("it writes as MessagePack");
        let back: T = rmp_serde::from_slice(&packed).expect("it reads back from MessagePack");
        assert_eq!(&back, value, "through MessagePack: {text}");
        self.keep::<T>(text)
    }

    /// Adds `value`, as JSON, once what it reads back from JSON, and from
    /// MessagePack in named mode if `packed`, writes the same: for types
    /// without `PartialEq`.
    #[track_caller]
    pub(crate) fn written<T: Serialize + DeserializeOwned>(
        &mut self,
        value: &T,
        packed: bool,
    ) -> Value {
        let text = serde_json::to_string(value).expect("it writes as JSON");
        let back: T = serde_json::from_str(&text).expect("it reads back from JSON");
        assert_eq!(serde_json::to_string(&back).expect("JSON"), text);
        if packed {
            let bytes = rmp_serde::to_vec_named(value).expect("it writes as MessagePack");
            let back: T = rmp_serde::from_slice(&bytes).expect("it reads back from MessagePack");
            assert_eq!(serde_json::to_string(&back).expect("JSON"), text);
        }
        self.keep::<T>(text)
    }

    /// Adds JSON `text` written by something else, such as a recording's
    /// line, as TypeScript type `ty` (naming the generated types as
    /// `T.…`).
    pub(crate) fn raw(&mut self, ty: &str, text: &str) {
        self.values.push((ty.to_owned(), text.to_owned()));
    }

    /// Keeps `text`, written from a `T`, once its shape is checked.
    #[track_caller]
    fn keep<T>(&mut self, text: String) -> Value {
        let value = tagged(&text);
        // `caf_core::chat::Chat<caf_core::chat::NoAppItems>` is `Chat`.
        let name = std::any::type_name::<T>();
        let name = name.split('<').next().unwrap_or(name);
        let name = name.rsplit("::").next().unwrap_or(name);
        self.values.push((format!("T.{name}"), text));
        value
    }
}

/// `value` as JSON, once it has read back equal from JSON and from
/// MessagePack in named mode.
#[track_caller]
fn both<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) -> Value {
    Samples::default().both(value)
}

/// `value` as JSON, once it reads back from JSON, and from MessagePack in
/// named mode if `packed` (see [`Samples::written`]).
#[track_caller]
fn written<T: Serialize + DeserializeOwned>(value: &T, packed: bool) -> Value {
    Samples::default().written(value, packed)
}

/// `text` as JSON, once it's checked to hold no enum with something in it
/// in serde's default shape, `{"Variant": …}` (the std types' are
/// allowed). An enum of only unit variants in serde's default shape is a
/// bare string, which can't be told from any other here: the check of the
/// generated TypeScript in `ts.rs` finds those.
#[track_caller]
fn tagged(text: &str) -> Value {
    fn check(value: &Value, text: &str) {
        match value {
            Value::Object(map) => {
                if let [(key, _)] = map.iter().collect::<Vec<_>>()[..] {
                    assert!(
                        !key.starts_with(char::is_uppercase)
                            || ["Ok", "Err", "Unix", "Windows"].contains(&key.as_str()),
                        "{key} isn't in the one shape: {text}"
                    );
                }
                map.values().for_each(|value| check(value, text));
            }
            Value::Array(values) => values.iter().for_each(|value| check(value, text)),
            _ => {}
        }
    }
    let value = serde_json::from_str(text).expect("JSON");
    check(&value, text);
    value
}

fn raw(json: &str) -> Box<RawValue> {
    RawValue::from_string(json.to_owned()).expect("JSON")
}

pub(crate) fn now() -> Now {
    Now {
        wall: Wall(1_800_000_000_000),
        mono: Mono(5_000_000),
    }
}

pub(crate) fn message() -> UserMessage {
    UserMessage::text("hi")
}

pub(crate) fn tool() -> ToolCall {
    ToolCall {
        title: "Read lib.rs".into(),
        kind: ToolKind::Read,
        status: ToolStatus::Done,
        input: Some(json!({ "path": "lib.rs" })),
        output: "fn main() {}".into(),
        app: None,
    }
}

pub(crate) fn item() -> Item {
    Item {
        id: ItemId(3),
        window: Window::instant(now()),
        parent: None,
        kind: ItemKind::Words("On it.".into()),
    }
}

pub(crate) fn turn() -> Turn {
    Turn {
        id: TurnId(2),
        starter: Starter::User(message()),
        life: TurnLife::Finished,
        window: Window::open(now()),
        items: vec![item()],
        imported: false,
    }
}

pub(crate) fn question() -> Question {
    Question {
        id: QuestionId(4),
        window: Window::open(now()),
        kind: QuestionKind::Permission(Permission {
            tool: Some(ItemId(3)),
            title: "Run ls".into(),
            options: vec![PermissionOption {
                id: OptionId("allow".into()),
                label: "Allow".into(),
                kind: OptionKind::AllowOnce,
            }],
        }),
    }
}

pub(crate) fn usage() -> Usage {
    Usage {
        used: 5000,
        size: 200_000,
        cost: Some(Cost {
            amount: 0.25,
            currency: "USD".into(),
        }),
    }
}

pub(crate) fn chat() -> Chat {
    Chat {
        id: ChatId(1),
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
        state: ChatState::Open {
            session: AgentSessionId("s1".into()),
        },
        turns: vec![turn()],
        usage: Some(usage()),
        questions: vec![question()],
    }
}

fn command() -> SpawnCommand {
    let mut command = SpawnCommand::new("/opt/claude-agent-acp");
    command.args = vec!["--acp".into()];
    command.env = vec![EnvChange::Set("A".into(), "b".into())];
    command
}

fn profile() -> ProfileReport {
    ProfileReport {
        profile: ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
        adapter: Some(AdapterInfo {
            name: "@agentclientprotocol/claude-agent-acp".into(),
            version: "0.98.0".into(),
        }),
        checked: Checked::Yes,
    }
}

fn rpc_error() -> RpcError {
    RpcError {
        code: -32000,
        message: "sign in".into(),
        data: Some(raw(r#"{"errorKind":"authentication_failed"}"#)),
    }
}

fn answered() -> Message {
    Message::Response {
        id: RequestId::Number(7),
        outcome: Ok(raw(r#"{"stopReason":"end_turn"}"#)),
    }
}

/// Adds one of every type CAF writes to `s`, but for those that have no
/// values (`NoAppItems`, `StorageEvent`, `StorageDirective`).
pub(crate) fn every_type(s: &mut Samples) {
    // time
    s.both(&now());
    s.both(&Wall(1));
    s.both(&Mono(1));
    s.both(&Window::unknown());
    s.both(&End::At(Wall(1)));
    // ids
    s.both(&ChatId(1));
    s.both(&TurnId(1));
    s.both(&ItemId(1));
    s.both(&QuestionId(1));
    s.both(&ConnectionId(1));
    s.both(&Ticket(1));
    s.both(&ChangeNo(1));
    s.both(&AgentName("claude".into()));
    s.both(&AgentSessionId("s1".into()));
    // chat
    s.both(&chat());
    s.both(&usage());
    s.both(&Cost {
        amount: 1.5,
        currency: "EUR".into(),
    });
    s.both(&ChatState::Lost {
        reason: Lost::Incompatible(Incompatible::Protocol { version: Some(2) }),
    });
    s.both(&Lost::AgentStopped {
        stderr: vec!["out of memory".into()],
    });
    s.both(&Incompatible::HttpMcp);
    s.both(&turn());
    s.both(&Starter::OtherChat {
        from: ChatId(9),
        message: message(),
    });
    s.both(&message());
    s.both(&Part::Text("hi".into()));
    let failure = Failure {
        reason: FailureReason::UsageLimit,
        agent_said: Some("Rate limited".into()),
        remedy: Some(Remedy::SendAgain),
    };
    s.both(&TurnLife::Failed(failure.clone()));
    s.both(&failure);
    s.both(&FailureReason::DidNotStop);
    s.both(&Remedy::SignIn);
    s.both(&item());
    s.both(&ItemKind::<crate::chat::NoAppItems>::Tool(tool()));
    s.both(&tool());
    s.both(&ToolKind::Execute);
    s.both(&ToolStatus::Pending);
    s.both(&Compacted {});
    s.both(&Note::SessionGone);
    s.both(&Unknown {
        what: "hologram".into(),
        raw: json!({ "sessionUpdate": "hologram" }),
    });
    // change
    let change: Change = Change::TurnLife {
        turn: TurnId(2),
        life: TurnLife::Stopped,
        end: End::At(Wall(9)),
    };
    s.both(&Numbered {
        no: ChangeNo(5),
        change: change.clone(),
    });
    s.both(&change);
    s.both(&Mismatch::NotText(ItemId(3)));
    // question
    s.both(&question());
    s.both(&QuestionKind::SlowToOpen(SlowToOpen {
        adapter_answered: false,
    }));
    s.both(&Permission {
        tool: None,
        title: "Edit".into(),
        options: Vec::new(),
    });
    s.both(&PermissionOption {
        id: OptionId("no".into()),
        label: "No".into(),
        kind: OptionKind::Other,
    });
    s.both(&OptionId("allow".into()));
    s.both(&OptionKind::RejectAlways);
    s.both(&SlowToOpen {
        adapter_answered: true,
    });
    s.both(&Answer::SlowToOpen(SlowChoice::Retry));
    s.both(&SlowChoice::KeepWaiting);
    // host
    let host = Host::new("caf", "1.0.0");
    s.both(&host);
    s.both(&Shows::default());
    s.both(&Grouping::PerChat);
    s.both(&Limits::default());
    s.both(&Timers::default());
    // agent
    let agent = AgentSpec::new("claude", command(), ProfileChoice::Cautious);
    s.both(&agent);
    s.both(&command());
    s.both(&EnvChange::Remove("HOME".into()));
    s.both(&McpServer::Http {
        name: "map".into(),
        url: "http://localhost:9/mcp".into(),
        headers: vec![("Auth".into(), "x".into())],
    });
    // profiles
    s.both(&profile());
    s.both(&ProfileChoice::Builtin(Builtin::ClaudeAgentAcp));
    s.both(&Builtin::ClaudeAgentAcp);
    s.both(&AdapterInfo {
        name: "a".into(),
        version: "1".into(),
    });
    s.both(&Checked::Newer);
    // core
    let mut open = OpenChat::new("claude", "/work");
    open.session = SessionChoice::Continue(AgentSessionId("s1".into()));
    open.mcp_servers = vec![McpServer::Stdio {
        name: "tools".into(),
        command: command(),
    }];
    s.both(&open);
    s.both(&SessionChoice::New);
    s.both(&Refused::QueueFull);
    s.both(&ConnectionInfo {
        id: ConnectionId(2),
        agent: AgentName("claude".into()),
        profile: Some(profile()),
        chats: vec![ChatId(1)],
        stderr: vec!["warming up".into()],
    });
    // viewer
    s.both(&FromViewer::Answer {
        request: RequestNo(1),
        question: QuestionId(4),
        answer: Answer::Permission(OptionId("allow".into())),
    });
    s.both(&ToViewer::Picture {
        chat: chat(),
        latest: ChangeNo(5),
    });
    s.both(&ToViewer::Changes {
        chat: ChatId(1),
        changes: vec![Numbered {
            no: ChangeNo(6),
            change: Change::ItemAdded {
                turn: TurnId(2),
                item: item(),
            },
        }],
    });
    s.both(&ChatSummary {
        id: ChatId(1),
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
    });
    s.both(&RequestNo(1));
    s.both(&OpenRequest {
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
        session: SessionChoice::New,
    });
    s.both(&Outcome::Refused(Refused::NoSuchChat));
    // directive
    s.written(
        &Directive::Agent(AgentDirective::Start {
            process: Ticket(3),
            command: command(),
        }),
        true,
    );
    s.written(
        &AgentDirective::Signal {
            process: Ticket(3),
            signal: Signal::Term,
        },
        true,
    );
    s.both(&Signal::Kill);
    s.written(&TimerDirective::WakeAt(None), true);
    s.written(
        &PolicyDirective::Decide {
            ticket: Ticket(5),
            chat: ChatId(1),
            request: PermissionRequest {
                title: "Run ls".into(),
                input: Some("ls".into()),
            },
        },
        true,
    );
    s.both(&PermissionRequest {
        title: "Run ls".into(),
        input: None,
    });
    s.written(
        &ViewerDirective::Changed {
            chat: ChatId(1),
            latest: ChangeNo(5),
        },
        true,
    );
    s.both(&Report {
        level: Level::Warn,
        message: "odd".into(),
    });
    s.both(&Level::Debug);
    // event
    s.written(
        &Event::<crate::chat::NoAppItems>::Host(HostCommand::OpenChat {
            chat: ChatId(1),
            open,
        }),
        true,
    );
    s.written(
        &HostCommand::<crate::chat::NoAppItems>::Send {
            chat: ChatId(1),
            turn: TurnId(2),
            message: message(),
        },
        true,
    );
    s.written(&AgentEvent::Stderr(b"hi\n".to_vec()), true);
    s.both(&Exit::Signal(9));
    s.both(&PolicyAnswer::AskUser);
    // Raw ACP payloads are read back only from JSON: they only ever travel
    // as JSON (recordings, the debugger's inspector), never on the viewer
    // wire or in saved state.
    s.written(
        &Event::<crate::chat::NoAppItems>::Agent {
            process: Ticket(3),
            event: AgentEvent::Frame(Frame::Message(answered())),
        },
        false,
    );
    s.written(
        &Frame::TooBig {
            id: Some(RequestId::Null),
            method: Some("session/update".into()),
            shape: Shape::Call,
            len: 99,
        },
        true,
    );
    s.written(
        &Message::Request {
            id: RequestId::String("a".into()),
            method: "session/request_permission".into(),
            params: Some(raw(r#"{"sessionId":"s1"}"#)),
        },
        false,
    );
    s.written(&rpc_error(), false);
    s.both(&RequestId::Number(-1));
    s.both(&Shape::Answer);
}

#[test]
fn every_type_reads_back_from_json_and_messagepack() {
    every_type(&mut Samples::default());
}

#[test]
fn enums_are_type_then_value() {
    // Unit, newtype and struct variants.
    assert_eq!(both(&End::Open), json!({ "type": "Open" }));
    assert_eq!(both(&End::At(Wall(5))), json!({ "type": "At", "value": 5 }));
    assert_eq!(
        both(&ChatState::Lost {
            reason: Lost::Restarted
        }),
        json!({ "type": "Lost", "value": { "reason": { "type": "Restarted" } } })
    );
    // A tuple variant, holding std's OsString in its own shape.
    #[cfg(unix)]
    assert_eq!(
        both(&EnvChange::Set("A".into(), "b".into())),
        json!({ "type": "Set", "value": [{ "Unix": [65] }, { "Unix": [98] }] })
    );
    // Ids are plain numbers and strings, and std's Duration and Option
    // keep their shapes.
    assert_eq!(both(&ChatId(5)), json!(5));
    assert_eq!(both(&AgentName("claude".into())), json!("claude"));
    assert_eq!(
        both(&Timers::default())["group_poll"],
        json!({ "secs": 0, "nanos": 50_000_000 })
    );
    assert_eq!(
        written(&TimerDirective::WakeAt(None), true),
        json!({ "type": "WakeAt", "value": null })
    );
    // Nested, as a viewer reads it.
    assert_eq!(
        both(&Change::TurnAdded(turn())),
        json!({ "type": "TurnAdded", "value": {
            "id": 2,
            "starter": { "type": "User", "value": { "parts": [{ "type": "Text", "value": "hi" }] } },
            "life": { "type": "Finished" },
            "window": { "start": 1_800_000_000_000_i64, "end": { "type": "Open" } },
            "items": [{
                "id": 3,
                "window": {
                    "start": 1_800_000_000_000_i64,
                    "end": { "type": "At", "value": 1_800_000_000_000_i64 }
                },
                "parent": null,
                "kind": { "type": "Words", "value": "On it." }
            }],
            "imported": false
        } })
    );
    assert_eq!(
        both(&ItemKind::<crate::chat::NoAppItems>::Tool(tool())),
        json!({ "type": "Tool", "value": {
            "title": "Read lib.rs",
            "kind": { "type": "Read" },
            "status": { "type": "Done" },
            "input": { "path": "lib.rs" },
            "output": "fn main() {}",
            "app": null
        } })
    );
    // Nested in an event, with std's Result and the agent's own JSON kept
    // as they are.
    assert_eq!(
        written(
            &Event::<crate::chat::NoAppItems>::Agent {
                process: Ticket(3),
                event: AgentEvent::Frame(Frame::Message(answered())),
            },
            false
        ),
        json!({ "type": "Agent", "value": {
            "process": 3,
            "event": { "type": "Frame", "value": { "type": "Message", "value": {
                "type": "Response",
                "value": {
                    "id": { "type": "Number", "value": 7 },
                    "outcome": { "Ok": { "stopReason": "end_turn" } }
                }
            } } }
        } })
    );
    assert_eq!(
        written(&Event::<crate::chat::NoAppItems>::Woken, true),
        json!({ "type": "Woken" })
    );
}

#[test]
fn the_type_is_written_before_the_value() {
    // A raw payload reads back only when its enum's type comes first.
    let event = Event::<crate::chat::NoAppItems>::Agent {
        process: Ticket(3),
        event: AgentEvent::Frame(Frame::Message(answered())),
    };
    assert_eq!(
        serde_json::to_string(&event).expect("JSON"),
        concat!(
            r#"{"type":"Agent","value":{"process":3,"event":{"type":"Frame","value":"#,
            r#"{"type":"Message","value":{"type":"Response","value":"#,
            r#"{"id":{"type":"Number","value":7},"outcome":{"Ok":{"stopReason":"end_turn"}}}}}}}}"#,
        )
    );
}

#[test]
fn messagepack_is_written_with_names() {
    let packed = rmp_serde::to_vec_named(&chat()).expect("MessagePack");
    let has = |name: &str| {
        packed
            .windows(name.len())
            .any(|bytes| bytes == name.as_bytes())
    };
    for name in ["turns", "starter", "type", "value", "Finished"] {
        assert!(has(name), "{name} is written");
    }
}

/// Adds both recorded traces to `s`, as written and as CAF writes what
/// they hold: every event, every directive replaying them gives out, and
/// each chat, its changes and its connections as they end.
pub(crate) fn traces(s: &mut Samples) {
    traces_with(s, LIVE);
}

/// The tokio kit's recording of a live run on claude-agent-acp 0.98.0.
const LIVE: &str = include_str!("core/tests/traces/claude-0.98.0-live.jsonl");

/// [`traces`], with `live` as the kit's recording.
fn traces_with(s: &mut Samples, live: &str) {
    use crate::Core;
    use crate::change::Read;
    use crate::chat::NoAppItems;

    #[derive(serde::Deserialize)]
    struct Header {
        host: Host,
        agents: Vec<AgentSpec>,
    }
    #[derive(serde::Deserialize)]
    struct Line<'a> {
        #[serde(borrow)]
        now: &'a RawValue,
        #[serde(borrow)]
        event: &'a RawValue,
    }
    #[derive(serde::Deserialize)]
    struct Step<'a> {
        at: u64,
        #[serde(borrow)]
        event: &'a RawValue,
    }

    /// Gives `core` `event` at `now`, adding the event and what it gives out.
    fn feed(s: &mut Samples, core: &mut Core, now: Now, event: &RawValue) {
        s.raw("T.Event", event.get());
        let event: Event = serde_json::from_str(event.get()).expect("a recorded event");
        s.written(&event, false);
        let mut out = Vec::new();
        core.handle(now, event, &mut out);
        for directive in &out {
            s.written(directive, true);
        }
    }

    /// Adds how `core`'s chats and connections end.
    fn ending(s: &mut Samples, core: &Core, first: u64) {
        for chat in core.chats() {
            s.both(chat);
            if let Some(Read::Picture { latest, .. }) = core.read(chat.id, None) {
                s.both(&ToViewer::<NoAppItems>::Picture {
                    chat: chat.clone(),
                    latest,
                });
            }
            if let Some(Read::Changes(changes)) = core.read(chat.id, Some(ChangeNo(first - 1))) {
                for change in changes {
                    s.both(change);
                }
                s.both(&ToViewer::Changes {
                    chat: chat.id,
                    changes: changes.to_vec(),
                });
            }
        }
        for connection in core.connections() {
            s.both(&connection);
        }
    }

    // The tokio kit's recording of a live run: a header, then a line for
    // each event.
    let mut lines = live.lines();
    let header = lines.next().expect("a header");
    s.raw(
        "{ caf_recording: 2, host: T.Host, agents: Array<T.AgentSpec> }",
        header,
    );
    let header: Header = serde_json::from_str(header).expect("the header reads");
    let mut core = Core::new(header.host, header.agents);
    for line in lines {
        // A recording cut short says why, and ends there.
        if line.starts_with(r#"{"stopped":"#) {
            s.raw("{ stopped: string }", line);
            break;
        }
        s.raw("{ now: T.Now, event: T.Event }", line);
        let line: Line = serde_json::from_str(line).expect("a line reads");
        let now: Now = serde_json::from_str(line.now.get()).expect("a time");
        feed(s, &mut core, now, line.event);
    }
    ending(s, &core, 1);

    // The core tests' own recording, as the harness gives it: its ids
    // start at 1000.
    let agents = vec![AgentSpec::new(
        "claude",
        SpawnCommand::new("/opt/caf/claude-agent-acp"),
        ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
    )];
    let mut core = Core::starting_ids_at(Host::new("caf-test", "1.2.3"), agents, 1000);
    let steps: Vec<Step> = serde_json::from_str(include_str!("core/tests/traces/claude-turn.json"))
        .expect("a recording");
    for step in steps {
        let ms = i64::try_from(step.at).expect("a time");
        let now = Now {
            wall: Wall(1_800_000_000_000 + ms),
            mono: Mono(step.at * 1_000_000),
        };
        feed(s, &mut core, now, step.event);
    }
    ending(s, &core, 1000);
}

#[test]
fn a_recording_cut_short_is_read_as_far_as_it_goes() {
    let mut whole = Samples::default();
    traces_with(&mut whole, LIVE);
    let stopped = r#"{"stopped":"the disk has fallen behind"}"#;
    let mut cut = Samples::default();
    traces_with(
        &mut cut,
        &format!("{LIVE}{stopped}\n{{\"not\":\"read\"}}\n"),
    );
    // The same, with the line that says why it stopped, and nothing after.
    let why = ("{ stopped: string }".to_owned(), stopped.to_owned());
    assert_eq!(cut.values.iter().filter(|value| **value == why).count(), 1);
    cut.values.retain(|value| *value != why);
    assert_eq!(cut.values, whole.values);
}

#[test]
fn what_the_traces_hold_reads_back() {
    let mut s = Samples::default();
    traces(&mut s);
    // Both chats, with their changes.
    let chats = (s.values.iter()).filter(|(ty, _)| ty == "T.Chat").count();
    let changes = (s.values.iter())
        .filter(|(ty, _)| ty == "T.Numbered")
        .count();
    assert_eq!(chats, 2);
    assert!(changes > 10, "{changes} changes");
}
