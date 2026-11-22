//! Tests of [`Chat::apply`]: each change, and each one that doesn't fit.
//! Each writes a conformance fixture (see [`fixtures`]).

use crate::change::{Change, Mismatch, Numbered};
use crate::chat::{Chat, ChatState, Item, ItemKind, Lost, ToolStatus, TurnLife, Usage};
use crate::fixtures::{self, After};
use crate::ids::{ChangeNo, ItemId, QuestionId, TurnId};
use crate::settings::{ModelUsed, SettingChoice};
use crate::shape::{chat, item, question, settings, spend, tool, turn, turn_usage, usage};
use crate::time::{End, Wall};

/// The sample chat (turn 2, holding words 3; question 4), with a second
/// turn, 5, holding thinking 6 and tool 7.
fn two_turns() -> Chat {
    let mut chat = chat();
    let mut second = turn();
    second.id = TurnId(5);
    second.life = TurnLife::Running;
    second.items = vec![
        Item {
            id: ItemId(6),
            kind: ItemKind::Thinking("Hmm".into()),
            ..item()
        },
        Item {
            id: ItemId(7),
            kind: ItemKind::Tool(tool()),
            ..item()
        },
    ];
    chat.turns.push(second);
    chat
}

/// The change after [`two_turns`]'s last, which is 5.
fn numbered(no: u64, change: Change) -> Numbered {
    Numbered {
        no: ChangeNo(5 + no),
        change,
    }
}

/// `changes` applied to [`two_turns`] in turn, each written to the
/// test's conformance fixture with the chat after it.
#[track_caller]
fn applied_all(changes: Vec<Change>) -> Chat {
    let mut chat = two_turns();
    let mut steps = Vec::new();
    for (change, no) in changes.into_iter().zip(1..) {
        chat.apply(&change).expect("it fits");
        steps.push((numbered(no, change), chat.clone()));
    }
    let text = fixtures::text(
        &two_turns(),
        ChangeNo(5),
        (steps.iter()).map(|(change, chat)| (change, After::Chat(chat))),
    );
    fixtures::check(&format!("{}.json", fixtures::next_script()), &text);
    chat
}

/// `change` applied to [`two_turns`].
#[track_caller]
fn applied(change: Change) -> Chat {
    applied_all(vec![change])
}

/// Applying `change` to [`two_turns`] fails with `mismatch`, and leaves
/// the chat as it was. Its conformance fixture is named after the test
/// and `case`.
#[track_caller]
fn misfits(case: &str, change: Change, mismatch: Mismatch) {
    let mut chat = two_turns();
    assert_eq!(chat.apply(&change), Err(mismatch.clone()));
    assert_eq!(chat, two_turns(), "the chat is unchanged");
    let change = numbered(1, change);
    let text = fixtures::text(
        &two_turns(),
        ChangeNo(5),
        [(&change, After::Mismatch(&mismatch))],
    );
    fixtures::check(&format!("{}--{case}.json", fixtures::test_name()), &text);
}

fn ids(chat: &Chat) -> Vec<u64> {
    chat.turns.iter().map(|turn| turn.id.0).collect()
}

#[test]
fn the_state_and_usage_are_replaced() {
    let lost = ChatState::Lost {
        reason: Lost::Restarted,
    };
    assert_eq!(applied(Change::State(lost.clone())).state, lost);
    let less = Usage {
        used: 10,
        cost: None,
        ..usage()
    };
    assert_eq!(applied(Change::Usage(less.clone())).usage, Some(less));
}

#[test]
fn a_turn_is_added_at_the_end() {
    let mut third = turn();
    third.id = TurnId(8);
    let chat = applied(Change::TurnAdded(third.clone()));
    assert_eq!(ids(&chat), [2, 5, 8]);
    assert_eq!(chat.turns[2], third);
}

#[test]
fn an_inserted_turn_goes_before_the_turn_it_names() {
    let mut third = turn();
    third.id = TurnId(8);
    let chat = applied(Change::TurnInserted {
        before: TurnId(5),
        turn: third.clone(),
    });
    assert_eq!(ids(&chat), [2, 8, 5]);
    assert_eq!(chat.turns[1], third);
    let first = applied(Change::TurnInserted {
        before: TurnId(2),
        turn: third,
    });
    assert_eq!(ids(&first), [8, 2, 5]);
}

#[test]
fn a_turn_moves_on_and_ends() {
    let end = End::At(Wall(1_800_000_009_000));
    let chat = applied(Change::TurnLife {
        turn: TurnId(5),
        life: TurnLife::Stopped,
        end,
    });
    assert_eq!(chat.turns[1].life, TurnLife::Stopped);
    assert_eq!(chat.turns[1].window.end, end);
    assert_eq!(
        chat.turns[0],
        two_turns().turns[0],
        "other turns are as they were"
    );
}

#[test]
fn the_settings_are_replaced_whole() {
    let mut settings = settings();
    settings.chosen = vec![SettingChoice::effort("low")];
    let chat = applied(Change::Settings(settings.clone()));
    assert_eq!(chat.settings, settings);
}

#[test]
fn a_turn_sent_says_its_model() {
    let model = ModelUsed {
        id: "haiku".into(),
        name: Some("Haiku".into()),
    };
    let chat = applied(Change::TurnModel {
        turn: TurnId(5),
        model: model.clone(),
    });
    assert_eq!(chat.turns[1].model, Some(model));
    assert_eq!(chat.turns[0], two_turns().turns[0]);
}

#[test]
fn what_a_turn_spent_is_set_on_it() {
    let chat = applied(Change::TurnSpent {
        turn: TurnId(5),
        usage: Box::new(turn_usage()),
    });
    assert_eq!(chat.turns[1].spent, Some(Box::new(turn_usage())));
    assert_eq!(chat.turns[0], two_turns().turns[0]);
}

#[test]
fn the_spend_is_replaced_whole() {
    let chat = applied(Change::Spend(Box::new(spend())));
    assert_eq!(*chat.spend, spend());
}

#[test]
fn an_item_is_added_to_its_turn() {
    let new = Item {
        id: ItemId(9),
        ..item()
    };
    let chat = applied(Change::ItemAdded {
        turn: TurnId(2),
        item: new.clone(),
    });
    assert_eq!(chat.turns[0].items.last(), Some(&new));
    assert_eq!(chat.turns[1], two_turns().turns[1]);
}

#[test]
fn words_and_thinking_grow() {
    let words = applied(Change::Appended {
        item: ItemId(3),
        text: " Done.".into(),
    });
    assert_eq!(
        words.turns[0].items[0].kind,
        ItemKind::Words("On it. Done.".into())
    );
    let thinking = applied(Change::Appended {
        item: ItemId(6),
        text: "m.".into(),
    });
    assert_eq!(
        thinking.turns[1].items[0].kind,
        ItemKind::Thinking("Hmmm.".into())
    );
}

#[test]
fn a_tool_call_is_replaced() {
    let mut now = tool();
    now.status = ToolStatus::Failed;
    now.output = "no such file".into();
    let chat = applied(Change::ToolChanged {
        item: ItemId(7),
        tool: now.clone(),
    });
    assert_eq!(chat.turns[1].items[1].kind, ItemKind::Tool(now));
}

#[test]
fn an_item_ends() {
    let end = End::At(Wall(1_800_000_009_000));
    let chat = applied(Change::ItemEnded {
        item: ItemId(6),
        end,
    });
    assert_eq!(chat.turns[1].items[0].window.end, end);
}

#[test]
fn questions_come_and_go() {
    let mut second = question();
    second.id = QuestionId(10);
    let asked = applied(Change::QuestionAsked(second.clone()));
    assert_eq!(asked.questions, [question(), second.clone()]);
    let gone = applied_all(vec![
        Change::QuestionAsked(second.clone()),
        Change::QuestionGone(QuestionId(4)),
    ]);
    assert_eq!(gone.questions, [second], "only the one answered goes");
}

#[test]
fn a_change_naming_what_the_chat_lacks_changes_nothing() {
    let turn = turn();
    let end = End::At(Wall(1_800_000_009_000));
    misfits(
        "insert-before-no-turn",
        Change::TurnInserted {
            before: TurnId(99),
            turn: turn.clone(),
        },
        Mismatch::NoTurn(TurnId(99)),
    );
    misfits(
        "life-of-no-turn",
        Change::TurnLife {
            turn: TurnId(99),
            life: TurnLife::Finished,
            end,
        },
        Mismatch::NoTurn(TurnId(99)),
    );
    misfits(
        "model-of-no-turn",
        Change::TurnModel {
            turn: TurnId(99),
            model: ModelUsed {
                id: "sonnet".into(),
                name: None,
            },
        },
        Mismatch::NoTurn(TurnId(99)),
    );
    misfits(
        "spent-of-no-turn",
        Change::TurnSpent {
            turn: TurnId(99),
            usage: Box::new(turn_usage()),
        },
        Mismatch::NoTurn(TurnId(99)),
    );
    misfits(
        "item-for-no-turn",
        Change::ItemAdded {
            turn: TurnId(99),
            item: item(),
        },
        Mismatch::NoTurn(TurnId(99)),
    );
    misfits(
        "words-for-no-item",
        Change::Appended {
            item: ItemId(99),
            text: "x".into(),
        },
        Mismatch::NoItem(ItemId(99)),
    );
    misfits(
        "words-for-a-tool",
        Change::Appended {
            item: ItemId(7),
            text: "x".into(),
        },
        Mismatch::NotText(ItemId(7)),
    );
    misfits(
        "tool-for-no-item",
        Change::ToolChanged {
            item: ItemId(99),
            tool: tool(),
        },
        Mismatch::NoItem(ItemId(99)),
    );
    misfits(
        "tool-for-words",
        Change::ToolChanged {
            item: ItemId(3),
            tool: tool(),
        },
        Mismatch::NotTool(ItemId(3)),
    );
    misfits(
        "end-of-no-item",
        Change::ItemEnded {
            item: ItemId(99),
            end,
        },
        Mismatch::NoItem(ItemId(99)),
    );
    misfits(
        "no-question",
        Change::QuestionGone(QuestionId(99)),
        Mismatch::NoQuestion(QuestionId(99)),
    );
}

/// An item is found by its id in whichever turn it is, not only the
/// last.
#[test]
fn an_item_in_an_earlier_turn_is_found() {
    let end = End::At(Wall(1_800_000_009_000));
    let chat = applied(Change::ItemEnded {
        item: ItemId(3),
        end,
    });
    assert_eq!(chat.turns[0].items[0].window.end, end);
    assert_eq!(chat.turns[1], two_turns().turns[1]);
}

/// A question gone takes every question with its id, as the core's
/// record always did, not only the first.
#[test]
fn a_question_gone_takes_every_question_with_its_id() {
    let mut chat = two_turns();
    let mut again = question();
    again.window.end = End::At(Wall(1_800_000_009_000));
    chat.questions.push(again);
    let mut other = question();
    other.id = QuestionId(10);
    chat.questions.push(other.clone());
    let picture = chat.clone();
    let gone = numbered(1, Change::QuestionGone(QuestionId(4)));
    chat.apply(&gone.change).expect("it fits");
    assert_eq!(chat.questions, [other]);
    let text = fixtures::text(&picture, ChangeNo(5), [(&gone, After::Chat(&chat))]);
    fixtures::check(&format!("{}.json", fixtures::next_script()), &text);
}
