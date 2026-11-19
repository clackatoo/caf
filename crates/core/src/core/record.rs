//! Changing a chat's record, and keeping the numbered list of changes
//! viewers read. Every change to a chat goes through here, so the record
//! and the list never disagree.

use serde::Serialize;

use super::Core;
use crate::change::{Change, Numbered, Read};
use crate::chat::{AppItem, Chat, ChatState, Item, ItemKind, ToolCall, Turn, TurnLife, Usage};
use crate::ids::{ChangeNo, ChatId, ItemId, QuestionId, TurnId};
use crate::question::{Question, QuestionKind};
use crate::time::{End, Now, Window};

/// A chat's numbered changes: the last few, for viewers.
#[derive(Debug, Serialize)]
#[serde(bound = "A: AppItem")]
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

impl<A: AppItem> Core<A> {
    /// Adds `change` to `chat`'s list.
    fn record(&mut self, chat: ChatId, change: Change<A>) {
        let keep = self.host.limits.kept_changes;
        if let Some(log) = self.logs.get_mut(&chat) {
            log.push(change, keep);
            self.dirty.insert(chat);
        }
    }

    /// Sets where `chat` stands with its agent.
    pub(super) fn set_state(&mut self, chat: ChatId, state: ChatState) {
        let Some(whole) = self.chats.get_mut(&chat) else { return };
        if whole.state != state {
            whole.state = state.clone();
            self.record(chat, Change::State(state));
        }
    }

    /// Adds `turn` to the end of `chat`.
    pub(super) fn add_turn(&mut self, chat: ChatId, turn: Turn<A>) {
        let Some(whole) = self.chats.get_mut(&chat) else { return };
        whole.turns.push(turn.clone());
        self.record(chat, Change::TurnAdded(turn));
    }

    /// Adds `turn` to `chat` before its first turn waiting to be sent, or
    /// at the end if none is.
    pub(super) fn add_turn_before_waiting(&mut self, chat: ChatId, turn: Turn<A>) {
        let Some(whole) = self.chats.get_mut(&chat) else { return };
        let Some(at) = whole.turns.iter().position(|each| each.life == TurnLife::Waiting) else {
            return self.add_turn(chat, turn);
        };
        let before = whole.turns[at].id;
        whole.turns.insert(at, turn.clone());
        self.record(chat, Change::TurnInserted { before, turn });
    }

    /// A turn of `chat`'s, by id.
    pub(super) fn turn(&self, chat: ChatId, turn: TurnId) -> Option<&Turn<A>> {
        let whole = self.chats.get(&chat)?;
        whole.turns.iter().rev().find(|each| each.id == turn)
    }

    fn turn_mut(&mut self, chat: ChatId, turn: TurnId) -> Option<&mut Turn<A>> {
        let whole = self.chats.get_mut(&chat)?;
        whole.turns.iter_mut().rev().find(|each| each.id == turn)
    }

    /// Moves a turn on in its life, ending it at `now` if the new life
    /// is an end.
    pub(super) fn set_life(&mut self, now: Now, chat: ChatId, turn: TurnId, life: TurnLife) {
        let Some(each) = self.turn_mut(chat, turn) else { return };
        let ends = !matches!(life, TurnLife::Waiting | TurnLife::Running | TurnLife::Stopping);
        if ends {
            each.window.end = End::At(now.wall);
        }
        each.life = life.clone();
        let end = each.window.end;
        self.record(chat, Change::TurnLife { turn, life, end });
    }

    /// Adds an item to a turn, and returns its id.
    pub(super) fn add_item(&mut self, chat: ChatId, turn: TurnId, kind: ItemKind<A>, window: Window) -> ItemId {
        let id = ItemId(self.ids.take());
        let item = Item {
            id,
            window,
            parent: None,
            kind,
        };
        if let Some(each) = self.turn_mut(chat, turn) {
            each.items.push(item.clone());
            self.record(chat, Change::ItemAdded { turn, item });
        }
        id
    }

    /// An item of `chat`'s, by id: in whichever turn it is.
    pub(super) fn item(&self, chat: ChatId, item: ItemId) -> Option<&Item<A>> {
        let whole = self.chats.get(&chat)?;
        whole.turns.iter().rev().flat_map(|turn| turn.items.iter().rev()).find(|each| each.id == item)
    }

    fn item_mut(&mut self, chat: ChatId, item: ItemId) -> Option<&mut Item<A>> {
        let whole = self.chats.get_mut(&chat)?;
        (whole.turns.iter_mut().rev())
            .flat_map(|turn| turn.items.iter_mut().rev())
            .find(|each| each.id == item)
    }

    /// Adds `text` to the end of words or thinking.
    pub(super) fn append(&mut self, chat: ChatId, item: ItemId, text: &str) {
        let Some(each) = self.item_mut(chat, item) else { return };
        match &mut each.kind {
            ItemKind::Words(words) | ItemKind::Thinking(words) => words.push_str(text),
            _ => return,
        }
        self.record(chat, Change::Appended { item, text: text.to_owned() });
    }

    /// Ends an item at `now`, if it hasn't ended.
    pub(super) fn end_item(&mut self, now: Now, chat: ChatId, item: ItemId) {
        let Some(each) = self.item_mut(chat, item) else { return };
        if each.window.end == End::Open {
            each.window.end = End::At(now.wall);
            self.record(chat, Change::ItemEnded { item, end: End::At(now.wall) });
        }
    }

    /// Sets a tool call as it is now.
    pub(super) fn set_tool(&mut self, chat: ChatId, item: ItemId, tool: ToolCall<A>) {
        let Some(each) = self.item_mut(chat, item) else { return };
        if let ItemKind::Tool(old) = &mut each.kind
            && *old != tool
        {
            *old = tool.clone();
            self.record(chat, Change::ToolChanged { item, tool });
        }
    }

    /// Sets how full the agent's memory is.
    pub(super) fn set_usage(&mut self, chat: ChatId, usage: Usage) {
        let Some(whole) = self.chats.get_mut(&chat) else { return };
        if whole.usage.as_ref() != Some(&usage) {
            whole.usage = Some(usage.clone());
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
        if let Some(whole) = self.chats.get_mut(&chat) {
            whole.questions.push(question.clone());
            self.record(chat, Change::QuestionAsked(question));
        }
        id
    }

    /// Takes a question away, answered or withdrawn.
    pub(super) fn unask(&mut self, chat: ChatId, question: QuestionId) {
        let Some(whole) = self.chats.get_mut(&chat) else { return };
        let before = whole.questions.len();
        whole.questions.retain(|each| each.id != question);
        if whole.questions.len() != before {
            self.record(chat, Change::QuestionGone(question));
        }
    }
}
