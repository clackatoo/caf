//! How viewers hear what changed.
//!
//! Each chat keeps a numbered list of small changes, each naming what it
//! changes by id. A viewer remembers the number it has read up to and asks
//! for what follows. A viewer that's new, or has fallen behind the list's
//! bounded length, gets the whole chat first and then changes from there.
//!
//! [`Chat::apply`] takes a change and updates a chat with it: the core's
//! own record, and every viewer's copy. Every SDK must turn the same
//! changes into the same chat.

use serde::{Deserialize, Serialize};

use crate::chat::{
    AppItem, Chat, ChatState, Item, ItemKind, NoAppItems, ToolCall, Turn, TurnLife, Usage,
};
use crate::ids::{ChangeNo, ItemId, QuestionId, TurnId};
use crate::question::Question;
use crate::settings::{ModelUsed, Settings};
use crate::time::End;
use crate::usage::{Spend, TurnUsage};

/// A change, with its number.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(concrete(A = NoAppItems), bound = "A: ts_rs::TS"))]
#[serde(tag = "type", content = "value")]
#[serde(bound = "A: AppItem")]
pub enum Change<A: AppItem = NoAppItems> {
    /// The chat's state changed.
    State(ChatState),
    /// The chat's settings changed: what the agent offers, or what was
    /// chosen. The whole of them, each time.
    Settings(Settings),
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
    /// A turn was sent, on this model: its
    /// [`model`](crate::chat::Turn::model), as the agent said then.
    TurnModel {
        /// The turn.
        turn: TurnId,
        /// The model that answers it.
        model: ModelUsed,
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
    /// What a turn spent was recorded, as it ended: its
    /// [`spent`](crate::chat::Turn::spent).
    TurnSpent {
        /// The turn.
        turn: TurnId,
        /// What it spent.
        usage: Box<TurnUsage>,
    },
    /// What the chat has spent changed: a turn added to it, or the prices
    /// changed. The whole of it, each time.
    Spend(Box<Spend>),
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
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
        match change {
            Change::State(state) => self.state = state.clone(),
            Change::Settings(settings) => self.settings = settings.clone(),
            Change::TurnAdded(turn) => self.turns.push(turn.clone()),
            Change::TurnInserted { before, turn } => {
                let at = (self.turns.iter().rposition(|each| each.id == *before))
                    .ok_or(Mismatch::NoTurn(*before))?;
                self.turns.insert(at, turn.clone());
            }
            Change::TurnLife { turn, life, end } => {
                let each = self.turn_mut(*turn)?;
                each.life = life.clone();
                each.window.end = *end;
            }
            Change::TurnModel { turn, model } => self.turn_mut(*turn)?.model = Some(model.clone()),
            Change::ItemAdded { turn, item } => self.turn_mut(*turn)?.items.push(item.clone()),
            Change::Appended { item, text } => match &mut self.item_mut(*item)?.kind {
                ItemKind::Words(words) | ItemKind::Thinking(words) => words.push_str(text),
                _ => return Err(Mismatch::NotText(*item)),
            },
            Change::ToolChanged { item, tool } => match &mut self.item_mut(*item)?.kind {
                ItemKind::Tool(old) => old.clone_from(tool),
                _ => return Err(Mismatch::NotTool(*item)),
            },
            Change::ItemEnded { item, end } => self.item_mut(*item)?.window.end = *end,
            Change::Usage(usage) => self.usage = Some(usage.clone()),
            Change::TurnSpent { turn, usage } => self.turn_mut(*turn)?.spent = Some(usage.clone()),
            Change::Spend(spend) => self.spend = spend.clone(),
            Change::QuestionAsked(question) => self.questions.push(question.clone()),
            Change::QuestionGone(question) => {
                // Every question with the id goes, as the core's record
                // always took them.
                let before = self.questions.len();
                self.questions.retain(|each| each.id != *question);
                if self.questions.len() == before {
                    return Err(Mismatch::NoQuestion(*question));
                }
            }
        }
        Ok(())
    }

    /// A turn, by id. Newest first, as changes are mostly to the last
    /// turn.
    fn turn_mut(&mut self, turn: TurnId) -> Result<&mut Turn<A>, Mismatch> {
        (self.turns.iter_mut().rev())
            .find(|each| each.id == turn)
            .ok_or(Mismatch::NoTurn(turn))
    }

    /// An item, by id, in whichever turn it is. Newest first, so the words
    /// streaming in are found at once.
    fn item_mut(&mut self, item: ItemId) -> Result<&mut Item<A>, Mismatch> {
        (self.turns.iter_mut().rev())
            .flat_map(|turn| turn.items.iter_mut().rev())
            .find(|each| each.id == item)
            .ok_or(Mismatch::NoItem(item))
    }
}

#[cfg(test)]
mod tests;
