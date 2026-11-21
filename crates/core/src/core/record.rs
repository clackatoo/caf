//! Changing a chat's record, and keeping the numbered list of changes
//! viewers read. Every change to a chat goes through here, so the record
//! and the list never disagree.

use serde::Serialize;

use super::Core;
use crate::change::{Change, Numbered, Read};
use crate::chat::AppData;
use crate::chat::{AppItem, Chat, ChatState, Item, ItemKind, ToolCall, Turn, TurnLife, Usage};
use crate::ids::{ChangeNo, ChatId, ItemId, QuestionId, TurnId};
use crate::question::{Question, QuestionKind};
use crate::time::{End, Now, Window};

/// A chat's numbered changes: the last few, for viewers.
#[derive(Debug, Serialize)]
#[serde(bound = "A: AppItem")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Log<A: AppItem> {
    /// The changes kept, oldest first, numbered without gaps.
    changes: Vec<Numbered<A>>,
    /// The number the next change gets.
    next: u64,
}

impl<A: AppItem> Log<A> {
    /// An empty list whose first change is numbered `first`.
    pub(super) fn new(first: u64) -> Self {
        Self {
            changes: Vec::new(),
            next: first,
        }
    }

    /// The number of the latest change: one before the first if there
    /// are none yet.
    pub(super) fn latest(&self) -> ChangeNo {
        ChangeNo(self.next.saturating_sub(1))
    }

    /// Adds a change, keeping at most `keep`.
    fn push(&mut self, change: Change<A>, keep: usize) {
        self.changes.push(Numbered {
            no: ChangeNo(self.next),
            change,
        });
        self.next += 1;
        // Cut back in steps, so a long chat doesn't move the list on every
        // change.
        let keep = keep.max(1);
        if self.changes.len() > keep + keep / 4 {
            self.changes.drain(..self.changes.len() - keep);
        }
    }

    /// What a viewer that has read up to `since` gets.
    pub(super) fn read<'a>(&'a self, chat: &'a Chat<A>, since: Option<ChangeNo>) -> Read<'a, A> {
        let latest = self.latest();
        let picture = Read::Picture { chat, latest };
        let Some(since) = since else { return picture };
        if since == latest {
            return Read::Changes(&[]);
        }
        let oldest = self.changes.first().map_or(self.next, |first| first.no.0);
        if since > latest || since.0 + 1 < oldest {
            return picture;
        }
        let from = usize::try_from(since.0 + 1 - oldest).unwrap_or(usize::MAX);
        Read::Changes(self.changes.get(from..).unwrap_or(&[]))
    }
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// Applies `change` to `chat`'s record and, if it fits, adds it to
    /// the chat's list: the record is built only from the changes viewers
    /// are sent, so the two can't disagree. A change that doesn't fit is
    /// a bug in the core: it's dropped, and reported when the call ends.
    pub(super) fn record(&mut self, chat: ChatId, change: Change<A>) {
        let (Some(whole), Some(log)) = (self.chats.get_mut(&chat), self.logs.get_mut(&chat)) else {
            return;
        };
        #[cfg(test)]
        let seen = self.applied.entry(chat).or_insert_with(|| Applied {
            picture: whole.clone(),
            latest: log.latest(),
            steps: Vec::new(),
        });
        if let Err(mismatch) = whole.apply(&change) {
            debug_assert!(
                false,
                "chat {}: {change:?} doesn't fit: {mismatch:?}",
                chat.0
            );
            self.misfits.push((chat, change, mismatch));
            return;
        }
        // A saved chat's journal has every change, as the store's copy is
        // built from them too; a turn ending saves it at once.
        if let Some(kept) = self.saving.kept.get_mut(&chat) {
            if let Change::TurnLife { life, .. } = &change
                && !matches!(
                    life,
                    TurnLife::Waiting | TurnLife::Running | TurnLife::Stopping
                )
            {
                kept.urgent = true;
            }
            kept.add(crate::save::Entry::Change(change.clone()));
        }
        log.push(change, self.host.limits.kept_changes);
        #[cfg(test)]
        if let Some(last) = log.changes.last() {
            seen.steps.push((last.clone(), whole.clone()));
        }
        self.dirty.insert(chat);
    }

    /// Sets where `chat` stands with its agent.
    pub(super) fn set_state(&mut self, chat: ChatId, state: ChatState) {
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        if whole.state != state {
            self.record(chat, Change::State(state));
        }
    }

    /// Adds `turn` to the end of `chat`.
    pub(super) fn add_turn(&mut self, chat: ChatId, turn: Turn<A>) {
        self.record(chat, Change::TurnAdded(turn));
    }

    /// Adds `turn` to `chat` before its first turn waiting to be sent, or
    /// at the end if none is.
    pub(super) fn add_turn_before_waiting(&mut self, chat: ChatId, turn: Turn<A>) {
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        let Some(waiting) = (whole.turns.iter()).find(|each| each.life == TurnLife::Waiting) else {
            return self.add_turn(chat, turn);
        };
        let before = waiting.id;
        self.record(chat, Change::TurnInserted { before, turn });
    }

    /// A turn of `chat`'s, by id.
    pub(super) fn turn(&self, chat: ChatId, turn: TurnId) -> Option<&Turn<A>> {
        let whole = self.chats.get(&chat)?;
        whole.turns.iter().rev().find(|each| each.id == turn)
    }

    /// Moves a turn on in its life, ending it at `now` if the new life
    /// is an end.
    pub(super) fn set_life(&mut self, now: Now, chat: ChatId, turn: TurnId, life: TurnLife) {
        let Some(each) = self.turn(chat, turn) else {
            return;
        };
        let ends = !matches!(
            life,
            TurnLife::Waiting | TurnLife::Running | TurnLife::Stopping
        );
        let end = if ends {
            End::At(now.wall)
        } else {
            each.window.end
        };
        self.record(chat, Change::TurnLife { turn, life, end });
    }

    /// Adds an item to a turn, and returns its id.
    pub(super) fn add_item(
        &mut self,
        chat: ChatId,
        turn: TurnId,
        kind: ItemKind<A>,
        window: Window,
    ) -> ItemId {
        let id = ItemId(self.ids.take());
        let item = Item {
            id,
            window,
            parent: None,
            kind,
        };
        if self.turn(chat, turn).is_some() {
            self.record(chat, Change::ItemAdded { turn, item });
        }
        id
    }

    /// An item of `chat`'s, by id: in whichever turn it is.
    pub(super) fn item(&self, chat: ChatId, item: ItemId) -> Option<&Item<A>> {
        let whole = self.chats.get(&chat)?;
        whole
            .turns
            .iter()
            .rev()
            .flat_map(|turn| turn.items.iter().rev())
            .find(|each| each.id == item)
    }

    /// Adds `text` to the end of words or thinking.
    pub(super) fn append(&mut self, chat: ChatId, item: ItemId, text: &str) {
        let Some(each) = self.item(chat, item) else {
            return;
        };
        if matches!(each.kind, ItemKind::Words(_) | ItemKind::Thinking(_)) {
            let text = text.to_owned();
            self.record(chat, Change::Appended { item, text });
        }
    }

    /// Ends an item at `now`, if it hasn't ended.
    pub(super) fn end_item(&mut self, now: Now, chat: ChatId, item: ItemId) {
        let Some(each) = self.item(chat, item) else {
            return;
        };
        if each.window.end == End::Open {
            let end = End::At(now.wall);
            self.record(chat, Change::ItemEnded { item, end });
        }
    }

    /// Sets a tool call as it is now.
    pub(super) fn set_tool(&mut self, chat: ChatId, item: ItemId, tool: ToolCall<A>) {
        let Some(each) = self.item(chat, item) else {
            return;
        };
        if let ItemKind::Tool(old) = &each.kind
            && *old != tool
        {
            self.record(chat, Change::ToolChanged { item, tool });
        }
    }

    /// Sets how full the agent's memory is.
    pub(super) fn set_usage(&mut self, chat: ChatId, usage: Usage) {
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        if whole.usage.as_ref() != Some(&usage) {
            self.record(chat, Change::Usage(usage));
        }
    }

    /// Asks a question in `chat`, and returns its id.
    pub(super) fn ask(&mut self, now: Now, chat: ChatId, kind: QuestionKind) -> QuestionId {
        let id = QuestionId(self.ids.take());
        let question = Question {
            id,
            window: Window::open(now),
            kind,
        };
        self.record(chat, Change::QuestionAsked(question));
        id
    }

    /// Takes a question away, answered or withdrawn.
    pub(super) fn unask(&mut self, chat: ChatId, question: QuestionId) {
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        if whole.questions.iter().any(|each| each.id == question) {
            self.record(chat, Change::QuestionGone(question));
        }
    }
}

/// What a test saw of one chat's record: the chat before its first
/// change, then each change and the chat after it. What the conformance
/// fixtures in `fixtures/apply/` are written from.
#[cfg(test)]
#[derive(Debug)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Applied<A: AppItem> {
    /// The chat before its first change.
    pub(super) picture: Chat<A>,
    /// The number of the last change it includes.
    pub(super) latest: ChangeNo,
    /// Each change, and the chat after it.
    pub(super) steps: Vec<(Numbered<A>, Chat<A>)>,
}
