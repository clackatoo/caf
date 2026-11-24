//! Tests of the one shape every CAF type is written in (see the crate
//! docs): one of every type, written and read back through JSON and
//! through MessagePack in named mode, and the exact shape of some. Its
//! sample values are other tests' too.

use crate::directive::{RefusedCall, ToolDirective};
use crate::event::{RelayChange, Serving, ToolEvent};
use crate::ids::{CallId, JobId};
use crate::mcp::Key;
use crate::tool::{
    Action, CafTool, Cut, DecidedBy, Decision, Diff, DiffLine, Ending, Hunk, Image, LineChange,
    Lines, Link, Location, Origin, Output, Piece, ServedTool, StoppedBy, Text, Tools,
};
use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::acp::{Frame, Message, RequestId, RpcError, Shape};
use crate::agent::{
    AgentSpec, AgentStatus, CannotInstall, EnvChange, InstallFailure, InstallState, Launch,
    McpServer, SignInLink, SignInState, SpawnCommand,
};
use crate::change::{Change, Mismatch, Numbered};
use crate::chat::{
    Chat, ChatState, Compacted, Cost, Failure, FailureReason, Incompatible, Item, ItemKind, Lost,
    NoAppData, NoAppItems, Note, Part, Remedy, Starter, ToolCall, ToolKind, ToolStatus, Turn,
    TurnLife, Unknown, Usage, UserMessage,
};
use crate::core::{ConnectionInfo, OpenChat, Refused, SessionChoice};
use crate::directive::{
    AgentDirective, Directive, Level, PermissionRequest, PolicyDirective, Report, SetupDirective,
    Signal, StorageDirective, TimerDirective, UsageDirective, ViewerDirective,
};
use crate::event::{AgentEvent, Event, Exit, HostCommand, PolicyAnswer, SetupEvent, StorageEvent};
use crate::host::{Grouping, Host, Limits, Shows, Timers};
use crate::ids::{
    AgentName, AgentSessionId, ChangeNo, ChatId, ConnectionId, ItemId, QuestionId, Shelf, Ticket,
    TurnId,
};
use crate::install::{
    Artifact, Bundled, Edit, EnvValue, FilePatch, Install, LinkFrom, Needs, NpmPackage, Patch,
    PatchError, Platform, Program, Recipe, SignIn, SignedInAs, Status, StatusCommand, Unpack,
};
use crate::limits::{
    AccountLimits, ExtraUsage, LimitState, LimitWindow, ProfileLimits, WindowKind,
};
use crate::profiles::{AdapterInfo, Builtin, Checked, ProfileChoice, ProfileReport};
use crate::question::{
    Answer, OptionId, OptionKind, Permission, PermissionOption, Question, QuestionKind, SlowChoice,
    SlowToOpen,
};
use crate::save::{CannotRestore, Entry, Header, RestoreChat, Saved, SavedChat, SavedState};
use crate::settings::{
    Choice, KnownSettings, LearnFailure, ModelUsed, Setting, SettingChoice, SettingForm,
    SettingKind, SettingValue, Settings,
};
use crate::time::{End, Mono, Now, Wall, Window};
use crate::usage::{
    BadQuery, Bucket, ChatCounts, Estimate, LedgerLine, ModelPrice, ModelTokens, PriceLayer,
    PriceTable, PricedModel, Resolution, Spend, Summary, Tokens, TurnUsage, UsageQuery,
};
use crate::viewer::{
    AgentAction, ChatSummary, FromViewer, OpenRequest, Outcome, RequestNo, Rights, ToViewer,
};

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
        origin: Origin::Caf(CafTool::Read),
        action: Some(Action::Read {
            path: "/home/user/project/lib.rs".into(),
            lines: None,
        }),
        input: Some(json!({ "path": "lib.rs" })),
        input_cut: None,
        result: vec![Piece::Text(Text {
            text: "fn main() {}".into(),
            cut: None,
        })],
        locations: vec![Location {
            path: "/home/user/project/lib.rs".into(),
            line: None,
        }],
        locations_cut: 0,
        decision: Decision::Allowed(DecidedBy::User {
            option: "Allow".into(),
        }),
        app: None,
        stopped_by: None,
        job: None,
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
        model: Some(ModelUsed {
            id: "sonnet".into(),
            name: Some("Sonnet 4.6".into()),
        }),
        spent: None,
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

/// Tokens of every kind, thinking said.
pub(crate) fn tokens() -> Tokens {
    Tokens {
        input: 12,
        cache_read: 3000,
        cache_write: 400,
        output: 56,
        thinking: Some(7),
    }
}

/// What a turn spent: on two models, its cost reported.
pub(crate) fn turn_usage() -> TurnUsage {
    TurnUsage {
        models: vec![
            ModelTokens {
                model: "claude-sonnet-5-5".into(),
                name: Some("Sonnet 5.5".into()),
                tokens: tokens(),
            },
            ModelTokens {
                model: "claude-haiku-5-5".into(),
                name: None,
                tokens: Tokens {
                    thinking: None,
                    ..tokens()
                },
            },
        ],
        cost: Some(Cost {
            amount: 0.0125,
            currency: "USD".into(),
        }),
        fast: false,
    }
}

/// What a chat has spent.
pub(crate) fn spend() -> Spend {
    Spend {
        tokens: tokens(),
        reported: 0.0125,
        estimated: 0.5,
        unpriced: 80,
        turns: 2,
    }
}

/// A model's price, with a fast rate.
pub(crate) fn model_price() -> ModelPrice {
    let mut price = ModelPrice::new(3.0, 15.0);
    price.cache_read = 0.3;
    price.cache_write = 3.75;
    price.fast = Some(2.0);
    price
}

/// The prices of two models.
pub(crate) fn price_table() -> PriceTable {
    PriceTable {
        models: vec![
            PricedModel::new(
                "anthropic/claude-haiku-5-5",
                ModelPrice::new(1.0, 5.0),
                PriceLayer::Fetched,
            ),
            PricedModel::new(
                "anthropic/claude-sonnet-5-5",
                model_price(),
                PriceLayer::Own,
            ),
        ],
        fetched: Some(Wall(1_800_000_000_000)),
    }
}

/// A usage ledger's line.
fn ledger_line() -> LedgerLine {
    LedgerLine {
        at: Wall(1_800_000_000_000),
        app: "clackatoo".into(),
        chat: ChatId::from_number(1),
        turn: TurnId(2),
        agent: AgentName("claude".into()),
        profile: Some("claude-agent-acp".into()),
        usage: turn_usage(),
    }
}

/// What a usage summary covers: one app, by hour.
pub(crate) fn usage_query() -> UsageQuery {
    let mut query = UsageQuery::new(
        Wall(1_800_000_000_000),
        Wall(1_800_086_400_000),
        "Europe/London",
    );
    query.by = Resolution::Hour;
    query.apps = vec!["clackatoo".into()];
    query
}

/// A usage summary, of one bucket.
pub(crate) fn summary() -> Summary {
    let mut bucket = Bucket::new(
        Wall(1_800_000_000_000),
        "clackatoo",
        "claude-agent-acp",
        "claude-sonnet-5-5",
        true,
    );
    bucket.tokens = tokens();
    bucket.reported = 0.0125;
    bucket.estimated = 0.5;
    bucket.unpriced = 80;
    bucket.turns = 2;
    let mut summary = Summary::new(usage_query());
    summary.buckets = vec![bucket];
    summary.chats = ChatCounts {
        all: 1,
        by_app: vec![("clackatoo".into(), 1)],
        by_agent: vec![("claude-agent-acp".into(), 1)],
    };
    summary.prices = price_table().models;
    summary.matched = vec![(
        "claude-sonnet-5-5".into(),
        "anthropic/claude-sonnet-5-5".into(),
    )];
    summary.fetched = Some(Wall(1_800_000_000_000));
    summary.unreadable = vec!["eyespy/2026-10.jsonl.zst".into()];
    summary.names = vec![("claude-sonnet-5-5".into(), "Sonnet 5.5".into())];
    summary
}

pub(crate) fn chat() -> Chat {
    Chat {
        id: ChatId::from_number(1),
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
        state: ChatState::Open {
            session: AgentSessionId("s1".into()),
        },
        turns: vec![turn()],
        usage: Some(usage()),
        questions: vec![question()],
        settings: settings(),
        spend: Box::default(),
        archived: None,
    }
}

/// A chat's settings: the host's model, waiting, and effort and fast mode
/// chosen for the chat itself.
pub(crate) fn settings() -> Settings {
    Settings {
        offered: offered(),
        chosen: vec![
            SettingChoice::model("sonnet"),
            SettingChoice::effort("high"),
            SettingChoice::fast(true),
        ],
        own: vec![SettingKind::Effort, SettingKind::Fast],
    }
}

/// What Claude offers, as CAF shows it.
fn offered() -> Vec<Setting> {
    let choice = |id: &str, name: &str| Choice {
        id: id.into(),
        name: name.into(),
        description: None,
        group: None,
    };
    vec![
        Setting {
            kind: SettingKind::Model,
            name: "Model".into(),
            form: SettingForm::Choices(vec![
                Choice {
                    description: Some("Opus 4.7, for most work".into()),
                    ..choice("default", "Default")
                },
                choice("sonnet", "Sonnet"),
                Choice {
                    group: Some("Older".into()),
                    ..choice("haiku", "Haiku")
                },
            ]),
            current: Some(SettingValue::Choice("default".into())),
        },
        Setting {
            kind: SettingKind::Effort,
            name: "Effort".into(),
            form: SettingForm::Choices(vec![choice("low", "Low"), choice("high", "High")]),
            current: Some(SettingValue::Choice("high".into())),
        },
        Setting {
            kind: SettingKind::Fast,
            name: "Fast mode".into(),
            form: SettingForm::Switch,
            current: Some(SettingValue::Switch(false)),
        },
    ]
}

fn command() -> SpawnCommand {
    let mut command = SpawnCommand::new("/opt/claude-agent-acp");
    command.args = vec!["--acp".into()];
    command.env = vec![EnvChange::Set("A".into(), "b".into())];
    command
}

/// A program that comes with an agent: its adapter, with `args`.
fn adapter(args: &[&str]) -> Bundled {
    Bundled {
        program: Program::Adapter,
        args: args.iter().map(|&arg| arg.to_owned()).collect(),
        env: Vec::new(),
    }
}

/// A patch of one file, by two edits.
fn patch() -> Patch {
    Patch::new(
        1,
        vec![FilePatch::new(
            "node_modules/@agentclientprotocol/claude-agent-acp/dist/acp-agent.js",
            "6b1bb4e7e9caaf4aeca1042ae4aec2dc2433b0b0e4b1614b38a4d41699902aaa",
            "0f5a8e9a7c3b2d1e4f6a8b0c2d4e6f8a1b3c5d7e9f0a2b4c6d8e0f1a3b5c7d9e",
            vec![
                Edit::new(
                    "version: packageJson.version,",
                    "version: packageJson.version + \"+caf.1\",",
                ),
                Edit::new(
                    "let lastAssistantUsage = null;",
                    "let lastAssistantUsage = null;\n        let heldLimits = [];",
                ),
            ],
        )],
    )
}

/// A recipe with one of everything: an npm install, a patch, a sign-in, a
/// sign-out and a status command.
fn recipe() -> Recipe {
    Recipe {
        version: "1.2.3".into(),
        patch: Some(patch()),
        install: Install::Npm(NpmPackage {
            manifest: r#"{"dependencies":{"some-acp":"1.2.3"}}"#.into(),
            lockfile: r#"{"lockfileVersion":3}"#.into(),
        }),
        size: 1_000_000,
        needs: Needs {
            node: Some(22),
            npm: true,
        },
        adapter: Bundled {
            program: Program::InInstall("node_modules/.bin/some-acp".into()),
            args: vec!["--acp".into()],
            env: vec![
                ("MODE".into(), EnvValue::Text("quiet".into())),
                ("SOME_HOME".into(), EnvValue::PrivateFolder),
            ],
        },
        sign_in: Some(SignIn {
            command: adapter(&["login"]),
            link: LinkFrom::Browser,
            code_after: Some("Your code: ".into()),
            paste: true,
            sites: vec!["example.com".into()],
        }),
        sign_out: Some(adapter(&["logout"])),
        status: Status {
            pushed: true,
            command: Some(StatusCommand {
                command: adapter(&["status", "--json"]),
                signed_in: "/loggedIn".into(),
                how: Some((
                    "/authMethod".into(),
                    vec![("key".into(), SignedInAs::ApiKey)],
                )),
            }),
        },
    }
}

/// An agent's status: installed, and signing in.
fn agent_status() -> AgentStatus {
    AgentStatus {
        settings: KnownSettings::Known(offered()),
        name: AgentName("claude".into()),
        install: InstallState::Installed {
            version: "0.98.0".into(),
        },
        sign_in: SignInState::SigningIn(SignInLink {
            url: Some("https://claude.com/cai/oauth/authorize".into()),
            code: None,
            paste: true,
        }),
        can_sign_in: true,
        own_tools_off: true,
        limits: account_limits(),
    }
}

/// An account's limits, with a window of every kind.
pub(crate) fn account_limits() -> AccountLimits {
    let heard = Wall(1_795_000_000_000);
    let window = |kind, used, state| LimitWindow {
        kind,
        used,
        resets: Some(Wall(1_795_010_000_000)),
        state,
        heard,
    };
    AccountLimits {
        windows: vec![
            window(WindowKind::FiveHours, 42, LimitState::Fine),
            window(WindowKind::Week, 81, LimitState::Warning),
            window(
                WindowKind::ModelWeek("opus".into()),
                100,
                LimitState::Refused,
            ),
            window(WindowKind::ExtraWeek, 12, LimitState::Fine),
            LimitWindow {
                resets: None,
                ..window(WindowKind::Other("thirty_day".into()), 3, LimitState::Fine)
            },
        ],
        extra_usage: Some(ExtraUsage {
            in_use: false,
            heard,
        }),
        cleared: Some(Wall(1_794_000_000_000)),
    }
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

/// A saved chat's header.
fn header() -> Header {
    Header {
        chat: ChatId::from_number(1),
        agent: AgentName("claude".into()),
        profile: Some("claude-agent-acp".into()),
        cwd: "/work".into(),
        made: Wall(1),
        changed: Wall(2),
        label: Some("Fix the tests".into()),
        first: Some("Fix the tests".into()),
        turns: 1,
        closed: false,
        archived: Some(Wall(2)),
    }
}

/// A saved chat, whole.
fn saved() -> Saved {
    Saved {
        header: header(),
        chat: chat(),
        session: Some(AgentSessionId("s1".into())),
        data: NoAppData,
    }
}

/// Adds one of every type CAF writes to `s`, but for those that have no
/// values (`NoAppItems`).
pub(crate) fn every_type(s: &mut Samples) {
    // time
    s.both(&now());
    s.both(&Wall(1));
    s.both(&Mono(1));
    s.both(&Window::unknown());
    s.both(&End::At(Wall(1)));
    // ids
    s.both(&ChatId::from_number(1));
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
    s.both(&Incompatible::OwnTools);
    s.both(&turn());
    s.both(&Starter::OtherChat {
        from: ChatId::from_number(9),
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
    s.both(&ItemKind::<crate::chat::NoAppItems>::Tool(Box::new(tool())));
    s.both(&tool());
    s.both(&ToolKind::Execute);
    s.both(&ToolStatus::Pending);
    s.both(&Origin::Agent("WebSearch".into()));
    s.both(&CafTool::Edit);
    s.both(&Action::Edit {
        path: "/home/user/project/lib.rs".into(),
    });
    s.both(&Piece::Output(Output {
        lines: vec!["ok".into()],
        dropped: 0,
        ending: Some(Ending::Unknown),
    }));
    s.both(&LineChange::Added);
    s.both(&Text {
        text: "ok".into(),
        cut: None,
    });
    s.both(&Location {
        path: "/home/user/project/lib.rs".into(),
        line: Some(3),
    });
    s.both(&Output {
        lines: vec!["ok".into()],
        dropped: 2,
        ending: None,
    });
    s.both(&DiffLine {
        change: LineChange::Kept,
        old: Some(1),
        new: Some(1),
        text: "fn main() {".into(),
    });
    s.both(&Hunk { lines: Vec::new() });
    s.both(&Lines {
        first: 1,
        last: None,
    });
    s.both(&Cut { kept: 1, total: 2 });
    s.both(&Diff {
        path: "/home/user/project/lib.rs".into(),
        new_file: true,
        added: 1,
        removed: 0,
        hunks: vec![Hunk {
            lines: vec![DiffLine {
                change: LineChange::Added,
                old: None,
                new: Some(1),
                text: "fn main() {}".into(),
            }],
        }],
        more: 0,
    });
    s.both(&Image {
        mime: "image/png".into(),
        data: None,
        size: 3_000_000,
    });
    s.both(&Link {
        uri: "https://example.com".into(),
        name: "example".into(),
    });
    s.both(&Ending::Exited(Exit::Code(1)));
    s.written(
        &ToolDirective::Run {
            call: CallId(4),
            action: Action::Run {
                command: "ls".into(),
                folder: "/home/user/project".into(),
                background: false,
            },
            input: json!({ "command": "ls", "timeout": 5000 }),
        },
        true,
    );
    s.both(&RefusedCall::BadInput("no path".into()));
    s.written(
        &ToolDirective::Serve {
            chat: ChatId::from_number(1),
            tools: vec![CafTool::Read, CafTool::Bash],
            app: vec!["word_count".into()],
            cwd: "/home/user/project".into(),
            instructions: "Read files and run commands.".into(),
        },
        true,
    );
    s.written(
        &ToolDirective::Unserve {
            chat: ChatId::from_number(1),
        },
        true,
    );
    s.both(&StoppedBy::Agent);
    s.both(&Tools::caf([CafTool::Edit]));
    let mut relay = command();
    relay.env.clear();
    let serving = Serving::new(relay, "/run/user/1000/caf/app-1.sock", Key([7; 32]));
    s.both(&serving);
    s.written(&Event::<crate::chat::NoAppItems>::Serving(serving), true);
    s.written(
        &Event::<crate::chat::NoAppItems>::Relay {
            chat: ChatId::from_number(1),
            change: RelayChange::Lost,
        },
        true,
    );
    s.both(&RelayChange::Connected);
    s.both(&CallId(4));
    s.written(
        &Event::<crate::chat::NoAppItems>::Tool {
            call: CallId(4),
            event: ToolEvent::Called {
                chat: ChatId::from_number(1),
                tool: ServedTool::Caf(CafTool::Bash),
                input: json!({ "command": "ls" }),
                agent_call: Some("toolu_01".into()),
                title: None,
                bad_input: None,
            },
        },
        true,
    );
    s.written(
        &ToolEvent::<crate::chat::NoAppItems>::Finished {
            done: true,
            pieces: Vec::new(),
            folder: Some("/home/user/project/src".into()),
            notice: Some("<task-notification>…</task-notification>".into()),
            item: None,
        },
        true,
    );
    s.written(
        &ToolEvent::<crate::chat::NoAppItems>::Detached { job: JobId(2) },
        true,
    );
    s.written(&ToolEvent::<crate::chat::NoAppItems>::Stopped, true);
    s.both(&JobId(2));
    s.both(&Action::Stop { job: JobId(2) });
    s.both(&CafTool::TaskStop);
    s.written(
        &HostCommand::<crate::chat::NoAppItems>::Notify {
            chat: ChatId::from_number(1),
            turn: TurnId(5),
            text: "The build failed".into(),
        },
        true,
    );
    s.both(&Decision::NotAsked);
    s.both(&DecidedBy::Policy { reason: None });
    s.both(&Change::<crate::chat::NoAppItems>::OutputChanged {
        item: ItemId(3),
        from: 0,
        lines: vec!["ok".into()],
        dropped: 0,
    });
    s.both(&Compacted {});
    s.both(&Note::SessionGone);
    s.both(&Note::ToolsLost);
    s.both(&Unknown {
        what: "hologram".into(),
        raw: json!({ "sessionUpdate": "hologram" }),
        summary: None,
        cut: None,
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
    s.both(&Mismatch::NoOutput(ItemId(3)));
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
    let mut host = Host::new("caf", "1.0.0");
    // Its seed is fresh each run: the samples stay the same.
    host.seed = 42;
    s.both(&host);
    s.both(&Shows::default());
    s.both(&Grouping::PerChat);
    s.both(&Limits::default());
    s.both(&Timers::default());
    // agent
    let agent = AgentSpec::command("claude", command(), ProfileChoice::Cautious);
    s.both(&agent);
    s.both(&command());
    s.both(&EnvChange::Remove("HOME".into()));
    s.both(&AgentSpec::installed("claude", Builtin::ClaudeAgentAcp));
    s.both(&Launch::Installed);
    s.both(&agent_status());
    s.both(&InstallState::Installing {
        done: 10,
        size: 100,
    });
    s.both(&CannotInstall::Node {
        needs: 22,
        has: Some(20),
    });
    s.both(&InstallFailure::Other("EACCES".into()));
    s.both(&InstallFailure::Patch(
        "acp-agent.js isn't the file this patch was written for".into(),
    ));
    s.both(&patch());
    s.both(&patch().files[0]);
    s.both(&patch().files[0].edits[0]);
    s.both(&PatchError::NotAsPinned);
    s.both(&PatchError::Found { edit: 1, times: 2 });
    s.both(&PatchError::NotAsTested);
    s.both(&SignInState::SignedIn(SignedInAs::Account));
    s.both(&SignInLink {
        url: None,
        code: Some("ABCD-1234".into()),
        paste: false,
    });
    // install
    s.both(&recipe());
    s.both(&Install::Download(vec![Artifact {
        platform: Platform {
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        url: "https://example.com/agent.zip".into(),
        sha256: "00".repeat(32),
        unpack: Unpack::Zip,
    }]));
    s.both(&NpmPackage {
        manifest: "{}".into(),
        lockfile: "{}".into(),
    });
    s.both(&Artifact {
        platform: Platform {
            os: "macos".into(),
            arch: "aarch64".into(),
        },
        url: "https://example.com/agent".into(),
        sha256: "ff".repeat(32),
        unpack: Unpack::No,
    });
    s.both(&Platform {
        os: "linux".into(),
        arch: "aarch64".into(),
    });
    s.both(&Unpack::TarGz);
    s.both(&Needs::default());
    s.both(&adapter(&["--cli"]));
    s.both(&Program::InInstall("bin/agent".into()));
    s.both(&EnvValue::PrivateFolder);
    s.both(&SignIn {
        command: adapter(&["login"]),
        link: LinkFrom::Output("Open: ".into()),
        code_after: None,
        paste: false,
        sites: vec!["example.com".into()],
    });
    s.both(&LinkFrom::Browser);
    s.both(&Status::default());
    s.both(&StatusCommand {
        command: adapter(&["status"]),
        signed_in: "/ok".into(),
        how: None,
    });
    s.both(&SignedInAs::Gateway);
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
    open.tools = Tools::caf([CafTool::Read, CafTool::Write]);
    s.both(&open);
    s.both(&SessionChoice::New);
    s.both(&Refused::QueueFull);
    s.both(&Refused::ToolsNotServed);
    s.both(&Refused::UnknownTool("trim_clip".into()));
    s.both(&CafTool::Edit.hints());
    s.both(&ServedTool::App("trim_clip".into()));
    s.both(&crate::tool::AppToolSpec {
        name: "trim_clip".into(),
        title: "Trim a clip".into(),
        hints: CafTool::Edit.hints(),
        previews: true,
    });
    s.both(&ConnectionInfo {
        id: ConnectionId(2),
        agent: AgentName("claude".into()),
        profile: Some(profile()),
        chats: vec![ChatId::from_number(1)],
        stderr: vec!["warming up".into()],
    });
    // settings
    s.both(&settings());
    for setting in offered() {
        s.both(&setting.kind);
        s.both(&setting.form);
        s.both(&setting);
    }
    s.both(&Choice {
        id: "opus[1m]".into(),
        name: "Opus (1M context)".into(),
        description: Some("For long sessions".into()),
        group: Some("Opus".into()),
    });
    s.both(&SettingValue::Switch(true));
    s.both(&SettingChoice::model("sonnet"));
    s.both(&KnownSettings::Learning);
    for why in [
        LearnFailure::NotInstalled,
        LearnFailure::NotLookedYet,
        LearnFailure::SigningIn,
        LearnFailure::SignInNeeded,
        LearnFailure::CouldNotStart {
            error: "No such file or directory (os error 2)".into(),
        },
        LearnFailure::Refused {
            message: "Internal error".into(),
        },
        LearnFailure::StartTimedOut,
        LearnFailure::OpenTimedOut,
        LearnFailure::Unreadable,
        LearnFailure::Restarted,
        LearnFailure::Stopped,
        LearnFailure::ShutDown,
        LearnFailure::Incompatible,
        LearnFailure::OwnTools,
    ] {
        s.both(&KnownSettings::Failed(why.clone()));
        s.both(&why);
    }
    s.both(&ModelUsed {
        id: "haiku".into(),
        name: None,
    });
    // usage
    s.both(&tokens());
    s.both(&turn_usage().models[0]);
    s.both(&turn_usage());
    s.both(&spend());
    s.both(&model_price());
    s.both(&PriceLayer::Bundled);
    s.both(&price_table().models[0]);
    s.both(&price_table());
    s.both(&Estimate {
        usd: 0.5,
        unpriced: 80,
    });
    s.both(&ledger_line());
    s.both(&usage_query());
    s.both(&Resolution::Day);
    s.both(&BadQuery::Zone("Mars/Olympus".into()));
    s.both(&summary());
    s.both(&summary().buckets[0]);
    s.both(&summary().chats);
    s.both(&Change::<crate::chat::NoAppItems>::TurnSpent {
        turn: TurnId(2),
        usage: Box::new(turn_usage()),
    });
    s.both(&Change::<crate::chat::NoAppItems>::Spend(Box::new(spend())));
    s.both(&FromViewer::Usage {
        request: RequestNo(3),
        query: usage_query(),
    });
    s.both(&FromViewer::SetPrice {
        request: RequestNo(4),
        model: "claude-sonnet-5-5".into(),
        price: Some(model_price()),
    });
    s.both(&ToViewer::<crate::chat::NoAppItems>::Usage {
        request: RequestNo(3),
        summary: Box::new(summary()),
    });
    s.both(&Outcome::BadQuery(BadQuery::Period));
    s.written(&UsageDirective::Record(Box::new(ledger_line())), true);
    s.written(
        &Directive::<crate::chat::NoAppItems>::Usage(UsageDirective::Record(Box::new(
            ledger_line(),
        ))),
        true,
    );
    s.written(
        &Event::<crate::chat::NoAppItems>::Prices(price_table()),
        true,
    );
    // limits
    let kept = ProfileLimits {
        profile: Builtin::ClaudeAgentAcp,
        limits: account_limits(),
    };
    s.both(&account_limits());
    for window in &account_limits().windows {
        s.both(window);
        s.both(&window.kind);
        s.both(&window.state);
    }
    s.both(&ExtraUsage {
        in_use: true,
        heard: Wall(1_795_000_000_000),
    });
    s.both(&kept);
    s.written(&UsageDirective::SaveLimits(Box::new(kept.clone())), true);
    s.written(&Event::<crate::chat::NoAppItems>::Limits(vec![kept]), true);
    // save
    s.both(&Shelf("work".into()));
    s.both(&NoAppData);
    s.both(&header());
    s.both(&saved());
    s.both(&Entry::<NoAppItems, NoAppData>::Session(Some(
        AgentSessionId("s2".into()),
    )));
    s.both(&RestoreChat::new(
        ChatId::from_number(1),
        Shelf("work".into()),
    ));
    s.both(
        &RestoreChat::new(ChatId::from_number(1), Shelf("work".into()))
            .with_tools(Tools::caf(CafTool::ALL)),
    );
    s.both(&SavedChat {
        shelf: Shelf("work".into()),
        header: header(),
        state: SavedState::Failed(CannotRestore::Newer { version: 2 }),
    });
    s.both(&SavedState::Loaded);
    s.both(&CannotRestore::Damaged("a checksum doesn't match".into()));
    s.written(
        &StorageDirective::<NoAppItems, NoAppData>::Append {
            ticket: Ticket(1),
            shelf: Shelf("work".into()),
            header: header(),
            entries: vec![Entry::Data(NoAppData)],
        },
        true,
    );
    s.written(
        &StorageEvent::<NoAppItems, NoAppData>::Loaded(Box::new(saved())),
        true,
    );
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
        chat: ChatId::from_number(1),
        changes: vec![Numbered {
            no: ChangeNo(6),
            change: Change::ItemAdded {
                turn: TurnId(2),
                item: item(),
            },
        }],
    });
    s.both(&ChatSummary {
        id: ChatId::from_number(1),
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
        label: Some("Fix the tests".into()),
        shelf: Some(Shelf("work".into())),
        archived: Some(Wall(1_700_000_000_000)),
    });
    s.both(&RequestNo(1));
    s.both(&Rights::new(true, false));
    s.both(&OpenRequest {
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
        session: SessionChoice::New,
        tools: Tools::none(),
    });
    s.both(&OpenRequest::new("claude", "/work").with_tools(Tools::caf(CafTool::ALL)));
    s.both(&Outcome::Refused(Refused::NoSuchChat));
    s.both(&FromViewer::Agent {
        request: RequestNo(2),
        agent: AgentName("claude".into()),
        action: AgentAction::Paste("code#state".into()),
    });
    s.both(&AgentAction::SignIn);
    s.both(&ToViewer::<crate::chat::NoAppItems>::Agents(vec![
        agent_status(),
    ]));
    // directive
    s.written(
        &Directive::<crate::chat::NoAppItems>::Agent(AgentDirective::Start {
            process: Ticket(3),
            command: command(),
            hold: Some("/home/me/.local/share/caf/agents/claude-agent-acp-0.98.0".into()),
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
            chat: ChatId::from_number(1),
            request: PermissionRequest {
                hints: Some(CafTool::Bash.hints()),
                item: Some(ItemId(7)),
                title: "Run ls".into(),
                origin: Origin::Caf(CafTool::Bash),
                action: Some(Action::Run {
                    command: "ls".into(),
                    folder: "/home/user/project".into(),
                    background: false,
                }),
                input: Some("ls".into()),
            },
        },
        true,
    );
    s.both(&PermissionRequest {
        hints: None,
        item: None,
        title: "Run ls".into(),
        origin: Origin::NotRecorded,
        action: None,
        input: None,
    });
    s.written(
        &ViewerDirective::Changed {
            chat: ChatId::from_number(1),
            latest: ChangeNo(5),
        },
        true,
    );
    s.both(&Report {
        level: Level::Warn,
        message: "odd".into(),
    });
    s.both(&Level::Debug);
    let setup = [
        SetupDirective::Survey { ticket: Ticket(6) },
        SetupDirective::Install {
            ticket: Ticket(7),
            folder: "claude-agent-acp-0.98.0".into(),
            install: recipe().install,
            patch: Some(patch()),
            private: Some("claude-agent-acp-private".into()),
            size: 626_797_331,
        },
        SetupDirective::Prepare {
            ticket: Ticket(9),
            folder: "claude-agent-acp-0.98.0+caf.2".into(),
            from: "claude-agent-acp-0.98.0+caf.1".into(),
            patch: patch(),
            private: Some("claude-agent-acp-private".into()),
        },
        SetupDirective::Remove {
            ticket: Ticket(8),
            folder: "claude-agent-acp-0.97.0".into(),
            unused_since: Wall(1_800_000_000_000),
        },
        SetupDirective::SignIn {
            ticket: Ticket(9),
            command: command(),
            link: LinkFrom::Browser,
            code_after: None,
            paste: true,
        },
        SetupDirective::Paste {
            ticket: Ticket(9),
            text: "code#state".into(),
        },
        SetupDirective::Run {
            ticket: Ticket(10),
            command: command(),
        },
        SetupDirective::Status {
            ticket: Ticket(11),
            command: command(),
            signed_in: "/loggedIn".into(),
            how: Some((
                "/authMethod".into(),
                vec![("claude.ai".into(), SignedInAs::Account)],
            )),
        },
        SetupDirective::Stop { ticket: Ticket(9) },
    ];
    for directive in setup {
        s.written(&directive, true);
        s.written(
            &Directive::<crate::chat::NoAppItems>::Setup(directive),
            true,
        );
    }
    // event
    s.written(
        &Event::<crate::chat::NoAppItems>::Host(HostCommand::OpenChat {
            chat: ChatId::from_number(1),
            open,
        }),
        true,
    );
    s.written(
        &HostCommand::<crate::chat::NoAppItems>::Send {
            chat: ChatId::from_number(1),
            turn: TurnId(2),
            message: message(),
        },
        true,
    );
    s.written(&AgentEvent::Stderr(b"hi\n".to_vec()), true);
    let agent = AgentName("claude".into());
    let commands = [
        HostCommand::CheckAgents,
        HostCommand::Install(agent.clone()),
        HostCommand::StopInstall(agent.clone()),
        HostCommand::SignIn(agent.clone()),
        HostCommand::Paste {
            agent: agent.clone(),
            text: "code#state".into(),
        },
        HostCommand::StopSignIn(agent.clone()),
        HostCommand::SignOut(agent),
    ];
    for command in commands {
        s.written(&Event::<crate::chat::NoAppItems>::Host(command), true);
    }
    let reports = [
        SetupEvent::Surveyed {
            root: "/home/me/.local/share/caf/agents".into(),
            installed: vec!["claude-agent-acp-0.98.0".into()],
            node: Some(22),
            npm: true,
            platform: Platform {
                os: "linux".into(),
                arch: "x86_64".into(),
            },
            used: vec![("claude-agent-acp-0.98.0".into(), Wall(1_800_000_000_000))],
        },
        SetupEvent::Progress { done: 1000 },
        SetupEvent::Installed,
        SetupEvent::InstallFailed(InstallFailure::Offline),
        SetupEvent::Removed { removed: false },
        SetupEvent::Link("https://claude.com/cai/oauth/authorize".into()),
        SetupEvent::Code("ABCD-1234".into()),
        SetupEvent::Status(Some(SignedInAs::ApiKey)),
        SetupEvent::Status(None),
        SetupEvent::Ended { ok: true },
        SetupEvent::Failed("npm: not found".into()),
    ];
    for event in reports {
        s.written(&event, true);
        s.written(
            &Event::<crate::chat::NoAppItems>::Setup {
                ticket: Ticket(7),
                event,
            },
            true,
        );
    }
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
    // Ids are plain numbers and strings, a chat's a UUID's text, and
    // std's Duration and Option keep their shapes.
    assert_eq!(
        both(&ChatId::from_number(5)),
        json!("00000000-0000-0000-0000-000000000005")
    );
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
            "imported": false,
            "model": { "id": "sonnet", "name": "Sonnet 4.6" },
            "spent": null
        } })
    );
    assert_eq!(
        both(&ItemKind::<crate::chat::NoAppItems>::Tool(Box::new(tool()))),
        json!({ "type": "Tool", "value": {
            "title": "Read lib.rs",
            "kind": { "type": "Read" },
            "status": { "type": "Done" },
            "origin": { "type": "Caf", "value": { "type": "Read" } },
            "action": { "type": "Read", "value": { "path": "/home/user/project/lib.rs", "lines": null } },
            "input": { "path": "lib.rs" },
            "input_cut": null,
            "result": [{ "type": "Text", "value": { "text": "fn main() {}", "cut": null } }],
            "locations": [{ "path": "/home/user/project/lib.rs", "line": null }],
            "locations_cut": 0,
            "decision": { "type": "Allowed", "value": { "type": "User", "value": { "option": "Allow" } } },
            "app": null,
            "stopped_by": null,
            "job": null
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
        &format!(
            "{{ caf_recording: {}, host: T.Host, agents: Array<T.AgentSpec> }}",
            crate::event::RECORDING_VERSION
        ),
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
    let agents = vec![AgentSpec::command(
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

/// What recordings made before chats had UUIDs hold still reads: a chat id
/// as a number, an `OpenChat` with no shelf, instructions or settings, an
/// agent with no default settings, a status with nothing of settings, and
/// a host with no seed.
#[test]
fn recordings_from_before_saving_still_read() {
    let line = r#"{"type":"Host","value":{"type":"OpenChat","value":{"chat":1,"open":{"agent":"claude","cwd":"/work","session":{"type":"New"},"mcp_servers":[]}}}}"#;
    let event: Event = serde_json::from_str(line).expect("an old recording's event reads");
    let Event::Host(HostCommand::OpenChat { chat, open }) = event else {
        panic!("an OpenChat: {event:?}");
    };
    assert_eq!(chat, ChatId::from_number(1));
    assert_eq!(open.shelf, None);
    assert_eq!(open.instructions, None);
    assert!(open.settings.is_empty());
    assert_eq!(open.tools, Tools::none());
    // An agent from before the host could choose its settings.
    let line = r#"{"name":"claude","launch":{"type":"Installed"},"profile":{"type":"Builtin","value":{"type":"ClaudeAgentAcp"}}}"#;
    let agent: AgentSpec = serde_json::from_str(line).expect("an old agent reads");
    assert!(agent.settings.is_empty());
    // A status from a CAF that didn't turn agents' tools off.
    let status = serde_json::to_value(agent_status()).expect("a status");
    let mut old = status.clone();
    old.as_object_mut()
        .expect("an object")
        .remove("own_tools_off");
    let old: AgentStatus = serde_json::from_value(old).expect("an old status reads");
    assert!(!old.own_tools_off);
    // And one from before CAF knew agents' settings.
    let mut old = status;
    old.as_object_mut().expect("an object").remove("settings");
    let old: AgentStatus = serde_json::from_value(old).expect("an old status reads");
    assert_eq!(old.settings, KnownSettings::Unknown);
    let mut host = serde_json::to_value(Host::new("caf", "0.0.0")).expect("a host");
    host.as_object_mut().expect("an object").remove("seed");
    let host: Host = serde_json::from_value(host).expect("a host with no seed reads");
    assert_eq!(host.seed, 0);
}
