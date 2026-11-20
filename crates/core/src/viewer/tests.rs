//! Tests of [`ChatCopy`]: changes it has, gaps, and ones that don't fit.

use crate::change::{Change, Numbered};
use crate::chat::ItemKind;
use crate::ids::{ChangeNo, ItemId};
use crate::shape::chat;
use crate::viewer::{ChatCopy, NeedPicture};

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

/// A picture numbered lower than the copy, as after CAF restarts and
/// numbers its changes again, replaces it too: the copy goes on from the
/// picture's number, not its old one.
#[test]
fn a_lower_picture_starts_the_numbering_again() {
    let mut copy = copy();
    copy.take(&[bang(6), bang(7)]).expect("they follow");
    let mut old = copy.clone();
    assert_eq!(old.take(&[bang(4)]), Ok(()));
    assert_eq!(old, copy, "change 4 is one the copy had");
    copy = ChatCopy::new(chat(), ChangeNo(3));
    assert_eq!(copy.latest(), ChangeNo(3));
    assert_eq!(copy.take(&[bang(4)]), Ok(()), "change 4 is new now");
    assert_eq!(copy.latest(), ChangeNo(4));
    assert_eq!(copy.take(&[bang(3), bang(4)]), Ok(()), "both had already");
    assert_eq!(words(&copy), &ItemKind::Words("On it.!".into()));
}
