//! Tests of [`ChatCopy`] (changes it has, gaps, and ones that don't
//! fit), and the sample that pins version 1 of the wire.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::change::{Change, Numbered};
use crate::chat::{
    ChatState, Failure, FailureReason, Item, ItemKind, Lost, Note, Remedy, Starter, Turn, TurnLife,
};
use crate::core::{Refused, SessionChoice};
use crate::ids::{AgentName, AgentSessionId, ChangeNo, ChatId, ItemId, QuestionId, TurnId};
use crate::question::{Answer, OptionId, SlowChoice};
use crate::shape::{CHANGES, change_kind, chat, item, message, question, tool, turn, usage};
use crate::time::{End, Wall};
use crate::viewer::{
    ChatCopy, ChatSummary, FromViewer, NeedPicture, OpenRequest, Outcome, RequestNo, ToViewer,
};

/// Change `no`: "!" appended to the sample chat's words, item 3.
fn bang(no: u64) -> Numbered {
    Numbered {
        no: ChangeNo(no),
        change: Change::Appended {
            item: ItemId(3),
            text: "!".into(),
        },
    }
}

/// A copy of the sample chat, read up to change 5.
fn copy() -> ChatCopy {
    ChatCopy::new(chat(), ChangeNo(5))
}

/// The words in `copy`'s item 3.
fn words(copy: &ChatCopy) -> &ItemKind {
    &copy.chat().turns[0].items[0].kind
}

#[test]
fn a_copy_starts_from_a_picture() {
    let copy = copy();
    assert_eq!(copy.chat(), &chat());
    assert_eq!(copy.latest(), ChangeNo(5));
}

#[test]
fn changes_are_applied_in_order() {
    let mut copy = copy();
    assert_eq!(copy.take(&[bang(6), bang(7)]), Ok(()));
    assert_eq!(copy.latest(), ChangeNo(7));
    assert_eq!(words(&copy), &ItemKind::Words("On it.!!".into()));
    assert_eq!(copy.take(&[]), Ok(()), "none is nothing to do");
    assert_eq!(copy.latest(), ChangeNo(7));
}

#[test]
fn changes_the_copy_has_are_skipped() {
    let mut copy = copy();
    copy.take(&[bang(6), bang(7)]).expect("they follow");
    assert_eq!(copy.take(&[bang(4), bang(6), bang(7), bang(8)]), Ok(()));
    assert_eq!(copy.latest(), ChangeNo(8));
    assert_eq!(words(&copy), &ItemKind::Words("On it.!!!".into()));
}

#[test]
fn a_gap_before_the_first_new_change_needs_a_picture() {
    let mut copy = copy();
    assert_eq!(copy.take(&[bang(7)]), Err(NeedPicture));
    assert_eq!(copy, self::copy(), "nothing was applied");
}

#[test]
fn a_gap_later_keeps_what_came_before_it() {
    let mut copy = copy();
    assert_eq!(copy.take(&[bang(5), bang(6), bang(8)]), Err(NeedPicture));
    assert_eq!(copy.latest(), ChangeNo(6));
    assert_eq!(words(&copy), &ItemKind::Words("On it.!".into()));
}

#[test]
fn a_change_that_doesnt_fit_keeps_what_came_before_it() {
    let mut copy = copy();
    let misfit = Numbered {
        no: ChangeNo(7),
        change: Change::Appended {
            item: ItemId(99),
            text: "?".into(),
        },
    };
    assert_eq!(copy.take(&[bang(6), misfit, bang(8)]), Err(NeedPicture));
    assert_eq!(copy.latest(), ChangeNo(6));
    assert_eq!(words(&copy), &ItemKind::Words("On it.!".into()));
}

#[test]
fn a_new_picture_replaces_the_copy() {
    let mut copy = copy();
    copy.take(&[bang(6)]).expect("it follows");
    let mut fresh = chat();
    fresh.turns.clear();
    copy = ChatCopy::new(fresh.clone(), ChangeNo(40));
    assert_eq!(copy.chat(), &fresh);
    assert_eq!(copy.take(&[bang(6), bang(40)]), Ok(()), "all had already");
}

/// Where version 1's sample is kept.
fn sample_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/viewer/v1.json")
}

/// Which of [`FromViewer`]'s messages this is, counting from 0. A new
/// one doesn't compile until it's counted here and in [`FROM_VIEWER`].
fn from_viewer_kind(message: &FromViewer) -> usize {
    match message {
        FromViewer::Hello { .. } => 0,
        FromViewer::Watch { .. } => 1,
        FromViewer::Unwatch { .. } => 2,
        FromViewer::Open { .. } => 3,
        FromViewer::Send { .. } => 4,
        FromViewer::Stop { .. } => 5,
        FromViewer::Answer { .. } => 6,
        FromViewer::Close { .. } => 7,
    }
}

/// How many kinds of [`FromViewer`] there are.
const FROM_VIEWER: usize = 8;

/// Which of [`ToViewer`]'s messages this is, counting from 0.
fn to_viewer_kind(message: &ToViewer) -> usize {
    match message {
        ToViewer::Welcome { .. } => 0,
        ToViewer::NoCommonVersion { .. } => 1,
        ToViewer::Chats(_) => 2,
        ToViewer::Picture { .. } => 3,
        ToViewer::Changes { .. } => 4,
        ToViewer::Ended { .. } => 5,
        ToViewer::Done { .. } => 6,
    }
}

/// How many kinds of [`ToViewer`] there are.
const TO_VIEWER: usize = 7;

/// Which [`Outcome`] this is, counting from 0.
fn outcome_kind(outcome: &Outcome) -> usize {
    match outcome {
        Outcome::Opened(_) => 0,
        Outcome::Sent(_) => 1,
        Outcome::Done => 2,
        Outcome::Refused(_) => 3,
        Outcome::NotAllowed => 4,
        Outcome::Gone => 5,
    }
}

/// How many kinds of [`Outcome`] there are.
const OUTCOMES: usize = 6;

/// Version 1 of the viewer wire: one of every message each way, every
/// outcome, and every change.
fn version_1() -> (Vec<FromViewer>, Vec<ToViewer>) {
    let summary = ChatSummary {
        id: ChatId(1),
        agent: AgentName("claude".into()),
        cwd: "/work".into(),
    };
    let from = vec![
        FromViewer::Hello { min: 1, max: 1 },
        FromViewer::Watch {
            chat: ChatId(1),
            since: None,
        },
        FromViewer::Watch {
            chat: ChatId(1),
            since: Some(ChangeNo(5)),
        },
        FromViewer::Unwatch { chat: ChatId(1) },
        FromViewer::Open {
            request: RequestNo(1),
            open: OpenRequest {
                agent: AgentName("claude".into()),
                cwd: "/work".into(),
                session: SessionChoice::Continue(AgentSessionId("s1".into())),
            },
        },
        FromViewer::Send {
            request: RequestNo(2),
            chat: ChatId(1),
            message: message(),
        },
        FromViewer::Stop {
            request: RequestNo(3),
            chat: ChatId(1),
        },
        FromViewer::Answer {
            request: RequestNo(4),
            question: QuestionId(4),
            answer: Answer::Permission(OptionId("allow".into())),
        },
        FromViewer::Answer {
            request: RequestNo(5),
            question: QuestionId(6),
            answer: Answer::SlowToOpen(SlowChoice::KeepWaiting),
        },
        FromViewer::Close {
            request: RequestNo(6),
            chat: ChatId(1),
        },
    ];
    let end = End::At(Wall(1_800_000_001_000));
    let mut later = turn();
    later.id = TurnId(7);
    later.life = TurnLife::Waiting;
    later.items.clear();
    let changes = [
        Change::State(ChatState::Lost {
            reason: Lost::AgentStopped {
                stderr: vec!["out of memory".into()],
            },
        }),
        Change::TurnAdded(later.clone()),
        Change::TurnInserted {
            before: TurnId(7),
            turn: Turn {
                id: TurnId(8),
                starter: Starter::Caf,
                items: vec![Item {
                    id: ItemId(9),
                    kind: ItemKind::Note(Note::SessionGone),
                    ..item()
                }],
                ..turn()
            },
        },
        Change::TurnLife {
            turn: TurnId(2),
            life: TurnLife::Failed(Failure {
                reason: FailureReason::UsageLimit,
                agent_said: Some("Try again at 5pm".into()),
                remedy: Some(Remedy::SendAgain),
            }),
            end,
        },
        Change::ItemAdded {
            turn: TurnId(2),
            item: Item {
                id: ItemId(10),
                kind: ItemKind::Tool(tool()),
                ..item()
            },
        },
        Change::Appended {
            item: ItemId(3),
            text: " Done.".into(),
        },
        Change::ToolChanged {
            item: ItemId(10),
            tool: tool(),
        },
        Change::ItemEnded {
            item: ItemId(3),
            end,
        },
        Change::Usage(usage()),
        Change::QuestionAsked(question()),
        Change::QuestionGone(QuestionId(4)),
    ];
    let to = vec![
        ToViewer::Welcome {
            version: 1,
            agents: vec![AgentName("claude".into())],
            chats: vec![summary.clone()],
        },
        ToViewer::NoCommonVersion { min: 1, max: 1 },
        ToViewer::Chats(vec![summary]),
        ToViewer::Picture {
            chat: chat(),
            latest: ChangeNo(5),
        },
        ToViewer::Changes {
            chat: ChatId(1),
            changes: (changes.into_iter().zip(6..))
                .map(|(change, no)| Numbered {
                    no: ChangeNo(no),
                    change,
                })
                .collect(),
        },
        ToViewer::Ended { chat: ChatId(1) },
        ToViewer::Done {
            request: RequestNo(1),
            outcome: Outcome::Opened(ChatId(1)),
        },
        ToViewer::Done {
            request: RequestNo(2),
            outcome: Outcome::Sent(TurnId(7)),
        },
        ToViewer::Done {
            request: RequestNo(3),
            outcome: Outcome::Done,
        },
        ToViewer::Done {
            request: RequestNo(4),
            outcome: Outcome::Refused(Refused::NoSuchQuestion),
        },
        ToViewer::Done {
            request: RequestNo(5),
            outcome: Outcome::NotAllowed,
        },
        ToViewer::Done {
            request: RequestNo(6),
            outcome: Outcome::Gone,
        },
    ];
    (from, to)
}

/// What the sample of version 1 holds, as JSON.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    version: u32,
    from_viewer: Vec<FromViewer>,
    to_viewer: Vec<ToViewer>,
}

/// `samples/viewer/v1.json` holds one of every message of version 1, as
/// they're written today. It's what pins version 1: what a version sends
/// never changes.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test that writes and reads the sample: the core itself still does no I/O"
)]
fn version_1_is_as_its_sample_says() {
    let (from_viewer, to_viewer) = version_1();
    let mut seen = [false; FROM_VIEWER];
    from_viewer
        .iter()
        .for_each(|each| seen[from_viewer_kind(each)] = true);
    assert_eq!(
        seen, [true; FROM_VIEWER],
        "one of every message from a viewer"
    );
    let mut seen = [false; TO_VIEWER];
    to_viewer
        .iter()
        .for_each(|each| seen[to_viewer_kind(each)] = true);
    assert_eq!(seen, [true; TO_VIEWER], "one of every message to a viewer");
    let mut seen = [false; OUTCOMES];
    let mut changes = [false; CHANGES];
    for each in &to_viewer {
        match each {
            ToViewer::Done { outcome: done, .. } => seen[outcome_kind(done)] = true,
            ToViewer::Changes { changes: all, .. } => {
                all.iter()
                    .for_each(|each| changes[change_kind(&each.change)] = true);
            }
            _ => {}
        }
    }
    assert_eq!(seen, [true; OUTCOMES], "every outcome");
    assert_eq!(changes, [true; CHANGES], "every change");

    let sample = Sample {
        version: 1,
        from_viewer,
        to_viewer,
    };
    let text = serde_json::to_string_pretty(&sample).expect("it writes") + "\n";
    let back: Sample = serde_json::from_str(&text).expect("it reads back");
    assert_eq!(back, sample);
    let path = sample_path();
    if std::env::var_os("CAF_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("the samples' folder");
        std::fs::write(&path, &text).expect("the sample is written");
        return;
    }
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        old == text,
        "version 1 of the viewer wire is no longer as samples/viewer/v1.json says. \
         What a version sends never changes: a change to version 1 needs a new \
         version (viewer::VERSIONS) with a sample of its own. Only while version 1 \
         is unreleased may `CAF_BLESS=1 cargo test -p caf-core` rewrite its sample."
    );
}
