//! How viewers hear what changed.
//!
//! Each chat keeps a numbered list of small changes, each naming what it
//! changes by id. A viewer remembers the number it has read up to and asks
//! for what follows. A viewer that's new, or has fallen behind the list's
//! bounded length, gets the whole chat first and then changes from there.
//!
//! `Chat::apply` (in caf-sdk) takes a change and updates a viewer's own
//! copy; every SDK must turn the same changes into the same chat.

use serde::{Deserialize, Serialize};

use crate::chat::{AppItem, Chat, ChatState, Item, NoAppItems, ToolCall, Turn, TurnLife, Usage};
use crate::ids::{ChangeNo, ItemId, QuestionId, TurnId};
use crate::question::Question;
use crate::time::End;

/// A change, with its number.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "A: AppItem")]
pub struct Numbered<A: AppItem = NoAppItems> {
    /// Its number in the chat's list.
    pub no: ChangeNo,
    /// What changed.
    pub change: Change<A>,
}

/// One change to a chat.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "A: AppItem")]
pub enum Change<A: AppItem = NoAppItems> {
    /// The chat's state changed.
    State(ChatState),
    /// A turn was added.
    TurnAdded(Turn<A>),
    /// A turn moved on in its life, and ended if `end` says so.
    TurnLife {
        /// The turn.
        turn: TurnId,
        /// Where it is now.
        life: TurnLife,
        /// Whether it has ended.
        end: End,
    },
    /// An item was added to a turn.
    ItemAdded {
        /// The turn.
        turn: TurnId,
        /// The item.
        item: Item<A>,
    },
    /// Words or thinking grew.
    Appended {
        /// The item.
        item: ItemId,
        /// What was added to its end.
        text: String,
    },
    /// A tool call changed.
    ToolChanged {
        /// The item.
        item: ItemId,
        /// The tool call as it is now.
        tool: ToolCall<A>,
    },
    /// An item ended.
    ItemEnded {
        /// The item.
        item: ItemId,
        /// When.
        end: End,
    },
    /// The agent said how full its memory is.
    Usage(Usage),
    /// A question was asked.
    QuestionAsked(Question),
    /// A question was answered or withdrawn.
    QuestionGone(QuestionId),
}

/// What a viewer gets when it asks for a chat's changes.
#[non_exhaustive]
#[derive(Debug)]
pub enum Read<'a, A: AppItem = NoAppItems> {
    /// The changes after the one it named, oldest first: none if it's up
    /// to date.
    Changes(&'a [Numbered<A>]),
    /// The whole chat, because the viewer is new or too far behind. Read
    /// changes after `latest` next.
    Picture {
        /// The chat as it is.
        chat: &'a Chat<A>,
        /// The number of the last change it includes.
        latest: ChangeNo,
    },
}
