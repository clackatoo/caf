//! The sample that pins version 1 of the viewer wire:
//! `samples/viewer/v1.json`, one of every message each way, and of every
//! variant of every enum inside them.
//!
//! The file is what's pinned, not the values below. The test reads it,
//! writes it back out, and must get the same bytes, so a change to how
//! any of it is written fails. The values below only write the file, and
//! only under `CAF_BLESS_SAMPLE=1`, which is for a version not yet
//! released. `CAF_BLESS`, which regenerates the fixtures, never touches
//! it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::{
    AgentStatus, CannotInstall, InstallFailure, InstallState, SignInLink, SignInState,
};
use crate::change::{Change, Numbered};
use crate::chat::{
    Chat, ChatState, Compacted, Cost, Failure, FailureReason, Incompatible, Item, ItemKind, Lost,
    Note, Remedy, Starter, ToolCall, ToolKind, ToolStatus, Turn, TurnLife, Unknown, Usage,
    UserMessage,
};
use crate::core::{Refused, SessionChoice};
use crate::ids::{AgentName, AgentSessionId, ChangeNo, ChatId, ItemId, QuestionId, Shelf, TurnId};
use crate::install::SignedInAs;
use crate::kinds::Seen;
use crate::question::{
    Answer, OptionId, OptionKind, Permission, PermissionOption, Question, QuestionKind, SlowChoice,
    SlowToOpen,
};
use crate::save::{CannotRestore, Header, SavedChat, SavedState};
use crate::time::{End, Wall, Window};
use crate::viewer::{
    AgentAction, ChatSummary, FromViewer, OpenRequest, Outcome, RequestNo, Rights, ToViewer,
};

/// What the sample holds.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    version: u32,
    from_viewer: Vec<FromViewer>,
    to_viewer: Vec<ToViewer>,
}

/// Where it's kept.
fn path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/viewer/v1.json")
}

/// `sample`, as it's kept.
fn text(sample: &Sample) -> String {
    serde_json::to_string_pretty(sample).expect("it writes as JSON") + "\n"
}

/// `ms` milliseconds into the sample's time.
fn wall(ms: i64) -> Wall {
    Wall(1_800_000_000_000 + ms)
}

/// Something that started at `ms` and is still going.
fn open(ms: i64) -> Window {
    Window {
        start: Some(wall(ms)),
        end: End::Open,
    }
}

/// Something that started at `start` and ended at `end`.
fn ended(start: i64, end: i64) -> Window {
    Window {
        start: Some(wall(start)),
        end: End::At(wall(end)),
    }
}

/// An item with no parent.
fn item(id: u64, window: Window, kind: ItemKind) -> Item {
    Item {
        id: ItemId(id),
        window,
        parent: None,
        kind,
    }
}

/// A turn of CAF's own, not imported.
fn turn(id: u64, starter: Starter, life: TurnLife, window: Window, items: Vec<Item>) -> Turn {
    Turn {
        id: TurnId(id),
        starter,
        life,
        window,
        items,
        imported: false,
    }
}

/// A tool call, `kind`, as `status`.
fn tool(kind: ToolKind, status: ToolStatus) -> ToolCall {
    ToolCall {
        title: format!("{kind:?} lib.rs"),
        kind,
        status,
        input: Some(json!({ "path": "lib.rs" })),
        output: "fn main() {}".into(),
        app: None,
    }
}

/// The chat whole: every starter, item, tool and note, and both kinds
/// of question.
fn chat() -> Chat {
    let kinds = [
        ToolKind::Read,
        ToolKind::Edit,
        ToolKind::Delete,
        ToolKind::Move,
        ToolKind::Search,
        ToolKind::Execute,
        ToolKind::Think,
        ToolKind::Fetch,
        ToolKind::Other,
    ];
    let statuses = [
        ToolStatus::Pending,
        ToolStatus::Running,
        ToolStatus::Done,
        ToolStatus::Failed,
    ];
    let tools = (kinds.into_iter().zip(statuses.into_iter().cycle()).zip(6..))
        .map(|((kind, status), id)| item(id, ended(110, 120), ItemKind::Tool(tool(kind, status))));
    let mut thinking = item(
        5,
        ended(105, 106),
        ItemKind::Thinking("Reading the tests.".into()),
    );
    thinking.parent = Some(ItemId(4));
    let mut items = vec![
        item(4, ended(100, 104), ItemKind::Words("On it.".into())),
        thinking,
    ];
    items.extend(tools);
    items.push(item(15, ended(130, 130), ItemKind::Compacted(Compacted {})));
    items.push(item(
        16,
        ended(140, 140),
        ItemKind::Unknown(Unknown {
            what: "an update of a kind CAF doesn't know".into(),
            raw: json!({ "sessionUpdate": "plan", "entries": [] }),
            summary: None,
        }),
    ));
    let notes = [
        Note::SessionGone,
        Note::CannotContinue,
        Note::SessionStuck,
        Note::NotInstalled,
        Note::InstallFailed,
        Note::SignedIn,
    ];
    // 24 to 34 are taken, so the newer notes follow.
    let notes = (notes.into_iter().zip([21, 22, 23, 41, 42, 43]))
        .map(|(note, id)| item(id, ended(300, 300), ItemKind::Note(note)))
        .collect();
    let mut imported = turn(
        2,
        Starter::User(UserMessage::text("Earlier")),
        TurnLife::Finished,
        Window::unknown(),
        vec![item(
            3,
            Window::unknown(),
            ItemKind::Words("From before.".into()),
        )],
    );
    imported.imported = true;
    Chat {
        id: ChatId::from_number(1),
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
        state: ChatState::Open {
            session: AgentSessionId("s1".into()),
        },
        turns: vec![
            imported,
            turn(
                17,
                Starter::User(UserMessage::text("Fix the tests")),
                TurnLife::Finished,
                ended(100, 200),
                items,
            ),
            turn(18, Starter::Caf, TurnLife::Finished, ended(300, 300), notes),
            turn(
                19,
                Starter::Agent,
                TurnLife::Running,
                open(400),
                vec![item(20, open(400), ItemKind::Words("Also,".into()))],
            ),
            turn(
                24,
                Starter::OtherChat {
                    from: ChatId::from_number(9),
                    message: UserMessage::text("Review this"),
                },
                TurnLife::Waiting,
                open(500),
                Vec::new(),
            ),
            turn(
                25,
                Starter::Notice("The build failed".into()),
                TurnLife::Waiting,
                open(600),
                Vec::new(),
            ),
        ],
        usage: Some(Usage {
            used: 5000,
            size: 200_000,
            cost: Some(Cost {
                amount: 0.25,
                currency: "USD".into(),
            }),
        }),
        questions: vec![
            Question {
                id: QuestionId(26),
                window: open(410),
                kind: QuestionKind::Permission(Permission {
                    tool: Some(ItemId(11)),
                    title: "Run cargo test".into(),
                    options: [
                        ("allow", OptionKind::AllowOnce),
                        ("allow-always", OptionKind::AllowAlways),
                        ("reject", OptionKind::RejectOnce),
                        ("reject-always", OptionKind::RejectAlways),
                        ("other", OptionKind::Other),
                    ]
                    .into_iter()
                    .map(|(id, kind)| PermissionOption {
                        id: OptionId(id.into()),
                        label: id.into(),
                        kind,
                    })
                    .collect(),
                }),
            },
            Question {
                id: QuestionId(27),
                window: open(420),
                kind: QuestionKind::SlowToOpen(SlowToOpen {
                    adapter_answered: true,
                }),
            },
        ],
    }
}

/// One of every change, and of every state, life and failure in them.
/// Not a story: they needn't apply in order.
fn changes() -> Vec<Change> {
    let lost = [
        Lost::CouldNotStart {
            error: "No such file or directory".into(),
        },
        Lost::AgentStopped {
            stderr: vec!["out of memory".into()],
        },
        Lost::Restarted,
        Lost::SignInNeeded,
        Lost::Refused {
            message: "Session not found".into(),
        },
        Lost::Stopped,
        Lost::Incompatible(Incompatible::Protocol { version: Some(2) }),
        Lost::Incompatible(Incompatible::Protocol { version: None }),
        Lost::Incompatible(Incompatible::HttpMcp),
        Lost::Incompatible(Incompatible::BadAnswer),
        Lost::Incompatible(Incompatible::OwnTools),
        Lost::InUse,
        Lost::ShutDown,
    ];
    let mut states = vec![
        ChatState::Opening,
        ChatState::Open {
            session: AgentSessionId("s2".into()),
        },
    ];
    states.extend(lost.into_iter().map(|reason| ChatState::Lost { reason }));
    states.push(ChatState::Closed);
    states.push(ChatState::WaitingForAgent);
    states.push(ChatState::Idle);
    let reasons = [
        FailureReason::TooLong,
        FailureReason::TooManySteps,
        FailureReason::Refused,
        FailureReason::SignInNeeded,
        FailureReason::AccessDenied,
        FailureReason::BadRequest,
        FailureReason::UsageLimit,
        FailureReason::ProviderError,
        FailureReason::AgentStopped,
        FailureReason::DidNotStop,
        FailureReason::BadAnswer,
        FailureReason::Other,
    ];
    let remedies = [
        Some(Remedy::SendAgain),
        Some(Remedy::SignIn),
        Some(Remedy::NewChat),
        None,
    ];
    let failures = (reasons
        .into_iter()
        .zip(remedies.into_iter().cycle())
        .enumerate())
    .map(|(at, (reason, remedy))| {
        TurnLife::Failed(Failure {
            reason,
            agent_said: (at % 2 == 0).then(|| "Try again at 5pm".into()),
            remedy,
        })
    });
    let ending = |life: TurnLife| Change::TurnLife {
        turn: TurnId(19),
        life,
        end: End::At(wall(900)),
    };
    let mut changes: Vec<Change> = states.into_iter().map(Change::State).collect();
    changes.push(Change::TurnAdded(turn(
        30,
        Starter::User(UserMessage::text("And the docs")),
        TurnLife::Waiting,
        open(700),
        Vec::new(),
    )));
    changes.push(Change::TurnInserted {
        before: TurnId(30),
        turn: turn(
            31,
            Starter::Caf,
            TurnLife::Finished,
            ended(710, 710),
            vec![item(32, ended(710, 710), ItemKind::Note(Note::SessionGone))],
        ),
    });
    for life in [TurnLife::Waiting, TurnLife::Running, TurnLife::Stopping] {
        changes.push(Change::TurnLife {
            turn: TurnId(19),
            life,
            end: End::Open,
        });
    }
    changes.extend(
        [TurnLife::Finished, TurnLife::Stopped, TurnLife::CutOff]
            .into_iter()
            .chain(failures)
            .map(ending),
    );
    changes.extend([
        Change::ItemAdded {
            turn: TurnId(19),
            item: item(
                33,
                open(800),
                ItemKind::Tool(tool(ToolKind::Execute, ToolStatus::Pending)),
            ),
        },
        Change::Appended {
            item: ItemId(20),
            text: " the docs.".into(),
        },
        Change::ToolChanged {
            item: ItemId(33),
            tool: tool(ToolKind::Execute, ToolStatus::Done),
        },
        Change::ItemEnded {
            item: ItemId(20),
            end: End::At(wall(810)),
        },
        Change::Usage(Usage {
            used: 6000,
            size: 200_000,
            cost: None,
        }),
        Change::QuestionAsked(Question {
            id: QuestionId(34),
            window: open(820),
            kind: QuestionKind::Permission(Permission {
                tool: None,
                title: "Fetch docs.rs".into(),
                options: vec![PermissionOption {
                    id: OptionId("allow".into()),
                    label: "Allow".into(),
                    kind: OptionKind::AllowOnce,
                }],
            }),
        }),
        Change::QuestionGone(QuestionId(26)),
    ]);
    changes
}

/// One of every install and sign-in state, as agents' statuses.
fn agents() -> Vec<AgentStatus> {
    let installs = [
        InstallState::Host,
        InstallState::Unknown,
        InstallState::Missing { size: 626_797_331 },
        InstallState::Installing {
            done: 1_000_000,
            size: 626_797_331,
        },
        InstallState::Installed {
            version: "0.98.0".into(),
        },
        InstallState::Cannot(CannotInstall::Node {
            needs: 22,
            has: Some(20),
        }),
        InstallState::Cannot(CannotInstall::Node {
            needs: 22,
            has: None,
        }),
        InstallState::Cannot(CannotInstall::Npm),
        InstallState::Cannot(CannotInstall::Platform),
        InstallState::Cannot(CannotInstall::NoRecipe),
        InstallState::Cannot(CannotInstall::Failed(InstallFailure::Offline)),
        InstallState::Cannot(CannotInstall::Failed(InstallFailure::Corrupt)),
        InstallState::Cannot(CannotInstall::Failed(InstallFailure::NoSpace)),
        InstallState::Cannot(CannotInstall::Failed(InstallFailure::Stopped)),
        InstallState::Cannot(CannotInstall::Failed(InstallFailure::Other(
            "npm ERR! code EACCES".into(),
        ))),
    ];
    let sign_ins = [
        SignInState::Unknown,
        SignInState::SignedOut,
        SignInState::SigningIn(SignInLink {
            url: None,
            code: None,
            paste: false,
        }),
        SignInState::SigningIn(SignInLink {
            url: Some("https://claude.com/cai/oauth/authorize".into()),
            code: Some("ABCD-1234".into()),
            paste: true,
        }),
        SignInState::SignedIn(SignedInAs::Account),
        SignInState::SignedIn(SignedInAs::ApiKey),
        SignInState::SignedIn(SignedInAs::Gateway),
        SignInState::SignedIn(SignedInAs::Unknown),
    ];
    (installs
        .into_iter()
        .zip(sign_ins.into_iter().cycle())
        .enumerate())
    .map(|(at, (install, sign_in))| AgentStatus {
        name: AgentName(format!("agent-{at}")),
        // Both ways, and always while signing in, as the core says it.
        can_sign_in: matches!(sign_in, SignInState::SigningIn(_)) || at % 2 == 0,
        own_tools_off: at % 3 != 0,
        install,
        sign_in,
    })
    .collect()
}

/// Version 1: one of every message each way, and of everything in them.
fn version_1() -> Sample {
    let summary = ChatSummary {
        id: ChatId::from_number(1),
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
        label: Some("Fix the tests".into()),
        shelf: Some(Shelf("work".into())),
    };
    let open = |request, session| FromViewer::Open {
        request: RequestNo(request),
        open: OpenRequest {
            agent: AgentName("claude".into()),
            cwd: "/work".into(),
            session,
        },
    };
    let answer = |request, question, answer| FromViewer::Answer {
        request: RequestNo(request),
        question: QuestionId(question),
        answer,
    };
    let mut from_viewer = vec![
        FromViewer::Hello { min: 1, max: 1 },
        FromViewer::Watch {
            chat: ChatId::from_number(1),
            since: None,
        },
        FromViewer::Watch {
            chat: ChatId::from_number(1),
            since: Some(ChangeNo(5)),
        },
        FromViewer::Unwatch {
            chat: ChatId::from_number(1),
        },
        open(1, SessionChoice::New),
        open(2, SessionChoice::Continue(AgentSessionId("s1".into()))),
        FromViewer::Send {
            request: RequestNo(3),
            chat: ChatId::from_number(1),
            message: UserMessage::text("hi"),
        },
        FromViewer::Stop {
            request: RequestNo(4),
            chat: ChatId::from_number(1),
        },
        answer(5, 26, Answer::Permission(OptionId("allow".into()))),
        answer(6, 27, Answer::SlowToOpen(SlowChoice::KeepWaiting)),
        answer(7, 27, Answer::SlowToOpen(SlowChoice::Retry)),
        answer(8, 27, Answer::SlowToOpen(SlowChoice::Stop)),
        FromViewer::Close {
            request: RequestNo(9),
            chat: ChatId::from_number(1),
        },
    ];
    let actions = [
        AgentAction::Install,
        AgentAction::StopInstall,
        AgentAction::SignIn,
        AgentAction::Paste("code#state".into()),
        AgentAction::StopSignIn,
        AgentAction::SignOut,
    ];
    from_viewer.extend((actions.into_iter().zip(10..)).map(|(action, request)| {
        FromViewer::Agent {
            request: RequestNo(request),
            agent: AgentName("claude".into()),
            action,
        }
    }));
    from_viewer.push(FromViewer::Restore {
        request: RequestNo(20),
        chat: ChatId::from_number(2),
        shelf: Shelf("work".into()),
    });
    let refused = [
        Refused::NoSuchChat,
        Refused::Closed,
        Refused::NoSuchAgent,
        Refused::RelativeFolder,
        Refused::Unsendable,
        Refused::QueueFull,
        Refused::Unsupported,
        Refused::CommandNotAlone,
        Refused::NoSuchQuestion,
        Refused::WrongAnswer,
        Refused::NothingToStop,
        Refused::NotInstallable,
        Refused::AlreadyInstalled,
        Refused::CannotSignIn,
        Refused::NotSigningIn,
        Refused::NotSaved,
        Refused::Loaded,
        Refused::BadShelf,
        Refused::OwnTools,
    ];
    let outcomes = [
        Outcome::Opened(ChatId::from_number(1)),
        Outcome::Sent(TurnId(30)),
        Outcome::Done,
        Outcome::NotAllowed,
        Outcome::Gone,
        Outcome::CannotRestore(CannotRestore::InUse),
    ]
    .into_iter()
    .chain(refused.into_iter().map(Outcome::Refused));
    let mut to_viewer = vec![
        ToViewer::Welcome {
            version: 1,
            agents: vec![AgentName("claude".into())],
            chats: vec![summary.clone()],
            run: "7f3a9c2e".into(),
            may: Rights::new(true, false),
        },
        ToViewer::NoCommonVersion { min: 1, max: 1 },
        ToViewer::Chats(vec![summary]),
        ToViewer::Picture {
            chat: chat(),
            latest: ChangeNo(5),
        },
        ToViewer::Changes {
            chat: ChatId::from_number(1),
            changes: (changes().into_iter().zip(6..))
                .map(|(change, no)| Numbered {
                    no: ChangeNo(no),
                    change,
                })
                .collect(),
        },
        ToViewer::Ended {
            chat: ChatId::from_number(1),
        },
        ToViewer::Agents(agents()),
        ToViewer::Saved(saved()),
    ];
    to_viewer.extend(
        (outcomes.zip(1..)).map(|(outcome, request)| ToViewer::Done {
            request: RequestNo(request),
            outcome,
        }),
    );
    Sample {
        version: 1,
        from_viewer,
        to_viewer,
    }
}

/// Saved chats, one in each state, failed for every reason.
fn saved() -> Vec<SavedChat> {
    let header = |no| Header {
        chat: ChatId::from_number(no),
        agent: AgentName("claude".into()),
        profile: Some("claude-agent-acp".into()),
        cwd: "/work".into(),
        made: Wall(1_700_000_000_000),
        changed: Wall(1_700_000_060_000),
        label: Some("Fix the tests".into()),
        first: Some("Fix the tests".into()),
        turns: 3,
        closed: no == 2,
    };
    let failures = [
        CannotRestore::NotFound,
        CannotRestore::InUse,
        CannotRestore::Newer { version: 2 },
        CannotRestore::Damaged("a checksum doesn't match".into()),
        CannotRestore::AppData("missing field `title`".into()),
        CannotRestore::Store("No space left on device".into()),
    ];
    [SavedState::Saved, SavedState::Restoring, SavedState::Loaded]
        .into_iter()
        .chain(failures.into_iter().map(SavedState::Failed))
        .zip(1..)
        .map(|(state, no)| SavedChat {
            shelf: Shelf("work".into()),
            header: header(no),
            state,
        })
        .collect()
}

/// What to say when the sample no longer matches.
const CHANGED: &str = "version 1 of the viewer wire is no longer as samples/viewer/v1.json \
    says. What a version sends never changes: a change to version 1 needs a new version \
    (viewer::VERSIONS) with a sample of its own. Only while version 1 is unreleased may \
    `CAF_BLESS_SAMPLE=1 cargo test -p caf-core version_1` rewrite its sample";

/// `samples/viewer/v1.json` reads, and writes back as the same bytes:
/// nothing in version 1 is written differently. It holds one of every
/// message each way, and of every variant of every enum inside them.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test that writes and reads the sample: the core itself still does no I/O"
)]
fn version_1_is_as_its_sample_says() {
    let path = path();
    if std::env::var_os("CAF_BLESS_SAMPLE").is_some() {
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("the samples' folder");
        std::fs::write(&path, text(&version_1())).expect("the sample is written");
    }
    let kept = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("samples/viewer/v1.json: {error}"));
    let sample: Sample =
        serde_json::from_str(&kept).unwrap_or_else(|error| panic!("{CHANGED}: {error}"));
    assert!(text(&sample) == kept, "{CHANGED}");
    assert_eq!(sample.version, 1);
    let mut seen = Seen::default();
    sample
        .from_viewer
        .iter()
        .for_each(|each| seen.message_from(each));
    sample
        .to_viewer
        .iter()
        .for_each(|each| seen.message_to(each));
    let missing = seen.missing_on_the_wire();
    assert!(
        missing.is_empty(),
        "samples/viewer/v1.json lacks some of version 1: {missing:#?}"
    );
}
