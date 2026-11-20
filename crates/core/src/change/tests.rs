//! Tests of [`Chat::apply`]: each change, and each one that doesn't fit.

use crate::change::{Change, Mismatch};
use crate::chat::{Chat, ChatState, Item, ItemKind, Lost, ToolStatus, TurnLife, Usage};
use crate::ids::{ItemId, QuestionId, TurnId};
use crate::shape::{chat, item, question, tool, turn, usage};
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

/// `change` applied to [`two_turns`].
#[track_caller]
fn applied(change: Change) -> Chat {
    let mut chat = two_turns();
    chat.apply(&change).expect("it fits");
    chat
}

/// Applying `change` to [`two_turns`] fails with `mismatch`, and leaves
/// the chat as it was.
#[track_caller]
fn misfits(change: Change, mismatch: Mismatch) {
    let mut chat = two_turns();
    assert_eq!(chat.apply(&change), Err(mismatch));
    assert_eq!(chat, two_turns(), "the chat is unchanged");
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
    let mut chat = asked;
    chat.apply(&Change::QuestionGone(QuestionId(4)))
        .expect("it fits");
    assert_eq!(chat.questions, [second]);
}

#[test]
fn a_change_naming_what_the_chat_lacks_changes_nothing() {
    let turn = turn();
    let end = End::At(Wall(1_800_000_009_000));
    misfits(
        Change::TurnInserted {
            before: TurnId(99),
            turn: turn.clone(),
        },
        Mismatch::NoTurn(TurnId(99)),
    );
    misfits(
        Change::TurnLife {
            turn: TurnId(99),
            life: TurnLife::Finished,
            end,
        },
        Mismatch::NoTurn(TurnId(99)),
    );
    misfits(
        Change::ItemAdded {
            turn: TurnId(99),
            item: item(),
        },
        Mismatch::NoTurn(TurnId(99)),
    );
    misfits(
        Change::Appended {
            item: ItemId(99),
            text: "x".into(),
        },
        Mismatch::NoItem(ItemId(99)),
    );
    misfits(
        Change::Appended {
            item: ItemId(7),
            text: "x".into(),
        },
        Mismatch::NotText(ItemId(7)),
    );
    misfits(
        Change::ToolChanged {
            item: ItemId(99),
            tool: tool(),
        },
        Mismatch::NoItem(ItemId(99)),
    );
    misfits(
        Change::ToolChanged {
            item: ItemId(3),
            tool: tool(),
        },
        Mismatch::NotTool(ItemId(3)),
    );
    misfits(
        Change::ItemEnded {
            item: ItemId(99),
            end,
        },
        Mismatch::NoItem(ItemId(99)),
    );
    misfits(
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
