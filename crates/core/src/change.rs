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
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem")]
pub enum Change<A: AppItem = NoAppItems> {
    /// The chat's state changed.
    State(ChatState),
    /// A turn was added.
    TurnAdded(Turn<A>),
    /// A turn was added before another, rather than at the end. Turns
    /// that belong before a message still waiting to be sent go before it:
    /// history a session's agent replayed while the message waited, and
    /// CAF's own notes about the session it will go to, such as that it's
    /// a new one.
    TurnInserted {
        /// The turn it goes before.
        before: TurnId,
        /// The turn.
        turn: Turn<A>,
    },
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

/// A change that doesn't fit a chat: it names something the chat doesn't
/// have. A viewer that meets one has missed something, and needs the chat
/// whole again; inside the core, it's a bug.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// No turn has this id.
    NoTurn(TurnId),
    /// No item has this id.
    NoItem(ItemId),
    /// Words were appended to an item that isn't words or thinking.
    NotText(ItemId),
    /// A tool changed that isn't a tool call.
    NotTool(ItemId),
    /// No question waiting has this id.
    NoQuestion(QuestionId),
}

impl<A: AppItem> Chat<A> {
    /// Applies one change to the chat.
    ///
    /// The core builds its own record of every chat through this, and every
    /// viewer's copy is built through it or its equal in another language,
    /// so they can't disagree.
    ///
    /// # Errors
    ///
    /// [`Mismatch`] if the change names something the chat doesn't have.
    /// The chat is unchanged.
    pub fn apply(&mut self, change: &Change<A>) -> Result<(), Mismatch> {
        let _ = change;
        todo!()
    }
}
