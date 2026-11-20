//! Tests of the one shape every CAF type is written in (see the crate
//! docs): one of every type, written and read back through JSON and
//! through MessagePack in named mode, and the exact shape of some.

use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::acp::{Frame, Message, RequestId, RpcError, Shape};
use crate::agent::{AgentSpec, EnvChange, McpServer, SpawnCommand};
use crate::change::{Change, Numbered};
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

/// `value` as JSON, once it has read back equal from JSON and from
/// MessagePack in named mode.
#[track_caller]
fn both<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) -> Value {
    let text = serde_json::to_string(value).expect("it writes as JSON");
    let back: T = serde_json::from_str(&text).expect("it reads back from JSON");
    assert_eq!(&back, value, "through JSON: {text}");
    let packed = rmp_serde::to_vec_named(value).expect("it writes as MessagePack");
    let back: T = rmp_serde::from_slice(&packed).expect("it reads back from MessagePack");
    assert_eq!(&back, value, "through MessagePack: {text}");
    tagged(&text)
}

/// `value` as JSON, once what it reads back from JSON, and from
/// MessagePack in named mode if `packed`, writes the same: for types
/// without `PartialEq`.
#[track_caller]
fn written<T: Serialize + DeserializeOwned>(value: &T, packed: bool) -> Value {
    let text = serde_json::to_string(value).expect("it writes as JSON");
    let back: T = serde_json::from_str(&text).expect("it reads back from JSON");
    assert_eq!(serde_json::to_string(&back).expect("JSON"), text);
    if packed {
        let bytes = rmp_serde::to_vec_named(value).expect("it writes as MessagePack");
        let back: T = rmp_serde::from_slice(&bytes).expect("it reads back from MessagePack");
        assert_eq!(serde_json::to_string(&back).expect("JSON"), text);
    }
    tagged(&text)
}

/// `text` as JSON, once it's checked to hold no enum in serde's default
/// shape, `{"Variant": …}`. The std types' are allowed.
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

fn now() -> Now {
    Now {
        wall: Wall(1_800_000_000_000),
        mono: Mono(5_000_000),
    }
}

fn message() -> UserMessage {
    UserMessage::text("hi")
}

fn tool() -> ToolCall {
    ToolCall {
        title: "Read lib.rs".into(),
        kind: ToolKind::Read,
        status: ToolStatus::Done,
        input: Some(json!({ "path": "lib.rs" })),
        output: "fn main() {}".into(),
        app: None,
    }
}

fn item() -> Item {
    Item {
        id: ItemId(3),
        window: Window::instant(now()),
        parent: None,
        kind: ItemKind::Words("On it.".into()),
    }
}

fn turn() -> Turn {
    Turn {
        id: TurnId(2),
        starter: Starter::User(message()),
        life: TurnLife::Finished,
        window: Window::open(now()),
        items: vec![item()],
        imported: false,
    }
}

fn question() -> Question {
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

fn usage() -> Usage {
    Usage {
        used: 5000,
        size: 200_000,
        cost: Some(Cost {
            amount: 0.25,
            currency: "USD".into(),
        }),
    }
}

fn chat() -> Chat {
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

#[test]
fn every_type_reads_back_from_json_and_messagepack() {
    // time
    both(&now());
    both(&Wall(1));
    both(&Mono(1));
    both(&Window::unknown());
    both(&End::At(Wall(1)));
    // ids
    both(&ChatId(1));
    both(&TurnId(1));
    both(&ItemId(1));
    both(&QuestionId(1));
    both(&ConnectionId(1));
    both(&Ticket(1));
    both(&ChangeNo(1));
    both(&AgentName("claude".into()));
    both(&AgentSessionId("s1".into()));
    // chat
    both(&chat());
    both(&usage());
    both(&Cost {
        amount: 1.5,
        currency: "EUR".into(),
    });
    both(&ChatState::Lost {
        reason: Lost::Incompatible(Incompatible::Protocol { version: Some(2) }),
    });
    both(&Lost::AgentStopped {
        stderr: vec!["out of memory".into()],
    });
    both(&Incompatible::HttpMcp);
    both(&turn());
    both(&Starter::OtherChat {
        from: ChatId(9),
        message: message(),
    });
    both(&message());
    both(&Part::Text("hi".into()));
    let failure = Failure {
        reason: FailureReason::UsageLimit,
        agent_said: Some("Rate limited".into()),
        remedy: Some(Remedy::SendAgain),
    };
    both(&TurnLife::Failed(failure.clone()));
    both(&failure);
    both(&FailureReason::DidNotStop);
    both(&Remedy::SignIn);
    both(&item());
    both(&ItemKind::<crate::chat::NoAppItems>::Tool(tool()));
    both(&tool());
    both(&ToolKind::Execute);
    both(&ToolStatus::Pending);
    both(&Compacted {});
    both(&Note::SessionGone);
    both(&Unknown {
        what: "hologram".into(),
        raw: json!({ "sessionUpdate": "hologram" }),
    });
    // change
    let change: Change = Change::TurnLife {
        turn: TurnId(2),
        life: TurnLife::Stopped,
        end: End::At(Wall(9)),
    };
    both(&Numbered {
        no: ChangeNo(5),
        change: change.clone(),
    });
    both(&change);
    // question
    both(&question());
    both(&QuestionKind::SlowToOpen(SlowToOpen {
        adapter_answered: false,
    }));
    both(&Permission {
        tool: None,
        title: "Edit".into(),
        options: Vec::new(),
    });
    both(&PermissionOption {
        id: OptionId("no".into()),
        label: "No".into(),
        kind: OptionKind::Other,
    });
    both(&OptionId("allow".into()));
    both(&OptionKind::RejectAlways);
    both(&SlowToOpen {
        adapter_answered: true,
    });
    both(&Answer::SlowToOpen(SlowChoice::Retry));
    both(&SlowChoice::KeepWaiting);
    // host
    let host = Host::new("caf", "1.0.0");
    both(&host);
    both(&Shows::default());
    both(&Grouping::PerChat);
    both(&Limits::default());
    both(&Timers::default());
    // agent
    let agent = AgentSpec::new("claude", command(), ProfileChoice::Cautious);
    both(&agent);
    both(&command());
    both(&EnvChange::Remove("HOME".into()));
    both(&McpServer::Http {
        name: "map".into(),
        url: "http://localhost:9/mcp".into(),
        headers: vec![("Auth".into(), "x".into())],
    });
    // profiles
    both(&profile());
    both(&ProfileChoice::Builtin(Builtin::ClaudeAgentAcp));
    both(&Builtin::ClaudeAgentAcp);
    both(&AdapterInfo {
        name: "a".into(),
        version: "1".into(),
    });
    both(&Checked::Newer);
    // core
    let mut open = OpenChat::new("claude", "/work");
    open.session = SessionChoice::Continue(AgentSessionId("s1".into()));
    open.mcp_servers = vec![McpServer::Stdio {
        name: "tools".into(),
        command: command(),
    }];
    both(&open);
    both(&SessionChoice::New);
    both(&Refused::QueueFull);
    both(&ConnectionInfo {
        id: ConnectionId(2),
        agent: AgentName("claude".into()),
        profile: Some(profile()),
        chats: vec![ChatId(1)],
        stderr: vec!["warming up".into()],
    });
    // viewer
    both(&FromViewer::Answer {
        request: RequestNo(1),
        question: QuestionId(4),
        answer: Answer::Permission(OptionId("allow".into())),
    });
    both(&ToViewer::Picture {
        chat: chat(),
        latest: ChangeNo(5),
    });
    both(&ToViewer::Changes {
        chat: ChatId(1),
        changes: vec![Numbered {
            no: ChangeNo(6),
            change: Change::ItemAdded {
                turn: TurnId(2),
                item: item(),
            },
        }],
    });
    both(&ChatSummary {
        id: ChatId(1),
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
    });
    both(&RequestNo(1));
    both(&OpenRequest {
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
        session: SessionChoice::New,
    });
    both(&Outcome::Refused(Refused::NoSuchChat));
    // directive
    written(
        &Directive::Agent(AgentDirective::Start {
            process: Ticket(3),
            command: command(),
        }),
        true,
    );
    written(
        &AgentDirective::Signal {
            process: Ticket(3),
            signal: Signal::Term,
        },
        true,
    );
    both(&Signal::Kill);
    written(&TimerDirective::WakeAt(None), true);
    written(
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
    both(&PermissionRequest {
        title: "Run ls".into(),
        input: None,
    });
    written(
        &ViewerDirective::Changed {
            chat: ChatId(1),
            latest: ChangeNo(5),
        },
        true,
    );
    both(&Report {
        level: Level::Warn,
        message: "odd".into(),
    });
    both(&Level::Debug);
    // event
    written(
        &Event::<crate::chat::NoAppItems>::Host(HostCommand::OpenChat {
            chat: ChatId(1),
            open,
        }),
        true,
    );
    written(
        &HostCommand::<crate::chat::NoAppItems>::Send {
            chat: ChatId(1),
            turn: TurnId(2),
            message: message(),
        },
        true,
    );
    written(&AgentEvent::Stderr(b"hi\n".to_vec()), true);
    both(&Exit::Signal(9));
    both(&PolicyAnswer::AskUser);
    // Raw ACP payloads are read back only from JSON: they only ever travel
    // as JSON (recordings, the debugger's inspector), never on the viewer
    // wire or in saved state.
    written(
        &Event::<crate::chat::NoAppItems>::Agent {
            process: Ticket(3),
            event: AgentEvent::Frame(Frame::Message(answered())),
        },
        false,
    );
    written(
        &Frame::TooBig {
            id: Some(RequestId::Null),
            method: Some("session/update".into()),
            shape: Shape::Call,
            len: 99,
        },
        true,
    );
    written(
        &Message::Request {
            id: RequestId::String("a".into()),
            method: "session/request_permission".into(),
            params: Some(raw(r#"{"sessionId":"s1"}"#)),
        },
        false,
    );
    written(&rpc_error(), false);
    both(&RequestId::Number(-1));
    both(&Shape::Answer);
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
