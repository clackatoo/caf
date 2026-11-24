//! Saving chats, and restoring them: what the core keeps of the store's
//! work, and its rules for when to save.
//!
//! Each saved chat that's loaded keeps its own list of what changed since
//! it was last written ([`Kept::unsaved`]), fed by [`Core::record`], the
//! one way a chat's record changes. It's written [`Timers::save`] after it
//! first changes, at once when a turn ends, and whole when it's put away
//! or CAF shuts down. A loaded chat has one write under way at a time, so
//! a failed one is never followed by one that counts on it: the next is a
//! snapshot, tried again later. Putting a chat away writes it whole at
//! once, after any write of it under way, as the store does a chat's
//! work in the order it's given.
//!
//! A move of a shelf is a barrier: it's sent once nothing for a chat on
//! either shelf is under way, and until it's answered, work for them
//! (writes, loads, deletions, lists, letting chats go) waits, then goes
//! in order, on the shelf the answer says.
//!
//! [`Timers::save`]: crate::host::Timers::save

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::Serialize;

use super::record::Log;
use super::sessions::{check_instructions, mcp_server};
use super::settings::Choosing;
use super::spending::{counted_within_bounds, spend_of};
use super::state::{Open, Run};
use super::{Core, Refused, report};
use crate::change::Change;
use crate::chat::{
    AppData, AppItem, Chat, ChatState, ItemKind, Part, Starter, ToolStatus, TurnLife,
};
use crate::directive::{Directive, Level, StorageDirective};
use crate::event::StorageEvent;
use crate::ids::{AgentSessionId, ChatId, ItemId, Shelf, Ticket};
use crate::save::{CannotRestore, Entry, Header, RestoreChat, Saved, SavedChat, SavedState};
use crate::time::{End, Mono, Now, Wall};

/// The latest time a chat's UUID holds, in milliseconds: 48 bits.
const MOST_MILLIS: u64 = (1 << 48) - 1;

/// The longest shelf name, in bytes.
const MOST_SHELF: usize = 256;

/// The longest first line a header keeps, in characters.
const MOST_FIRST: usize = 200;

/// The longest wait before trying a failed write again.
const MOST_RETRY: Duration = Duration::from_mins(1);

/// Where the core is with saving.
#[derive(Debug, Serialize)]
#[serde(bound = "A: AppItem, D: AppData")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Saving<A: AppItem, D: AppData> {
    /// How many chats the core has made: with the host's seed, what the
    /// next chat's id is made from.
    pub(super) made: u64,
    /// The last chat made with a UUID: the next sorts after it.
    pub(super) last: Option<ChatId>,
    /// Each saved chat that's loaded: where it's kept, and what's still
    /// to write.
    pub(super) kept: BTreeMap<ChatId, Kept<A, D>>,
    /// The shelves listed, with their chats.
    pub(super) shelves: BTreeMap<Shelf, Vec<SavedChat>>,
    /// Restores under way, by the ticket of their load.
    pub(super) restoring: BTreeMap<Ticket, RestoreChat>,
    /// The store's other work under way, by ticket: what each was for.
    pub(super) jobs: BTreeMap<Ticket, Job>,
    /// Chats put away whose last snapshot isn't written yet: each is let
    /// go in the store once it is, and kept until then to try again.
    pub(super) away: BTreeMap<ChatId, Away<A, D>>,
    /// Chats deleted while being restored, by the ticket of their load:
    /// the lock it takes, if it comes, goes with them, or is let go if the
    /// deletion fails.
    pub(super) deleted: BTreeMap<Ticket, Deleted>,
    /// The shelf of each piece of work sent to the store that it hasn't
    /// answered yet, by ticket: what a move of the shelf waits for.
    pub(super) flights: BTreeMap<Ticket, Shelf>,
    /// Moves of shelves, waiting or sent, in the order they were asked
    /// for. While one is, nothing more is sent for either shelf: it waits
    /// with it.
    pub(super) moves: Vec<Moving>,
    /// Loads, deletions and lists waiting for a move of their shelf, by
    /// ticket, in order.
    pub(super) waiting: Vec<Ticket>,
    /// Chats to let go in the store once no move of their shelf waits.
    pub(super) releases: Vec<(Shelf, ChatId)>,
    /// The chats the store holds locked for this run, on their shelves, as
    /// its answers say: each loaded, or written whole, and not deleted or
    /// let go since. One nothing has a use for any more is let go.
    pub(super) locks: BTreeSet<(Shelf, ChatId)>,
}

/// A move of a shelf, waiting or sent.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Moving {
    /// Its ticket.
    pub(super) ticket: Ticket,
    /// From.
    pub(super) from: Shelf,
    /// To.
    pub(super) to: Shelf,
    /// It's been sent: it waits for nothing more, and the store does it.
    pub(super) sent: bool,
}

/// A chat deleted while it was being restored.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Deleted {
    /// The chat.
    pub(super) chat: ChatId,
    /// Its shelf.
    pub(super) shelf: Shelf,
}

impl<A: AppItem, D: AppData> Default for Saving<A, D> {
    fn default() -> Self {
        Self {
            made: 0,
            last: None,
            kept: BTreeMap::new(),
            shelves: BTreeMap::new(),
            restoring: BTreeMap::new(),
            jobs: BTreeMap::new(),
            away: BTreeMap::new(),
            deleted: BTreeMap::new(),
            flights: BTreeMap::new(),
            moves: Vec::new(),
            waiting: Vec::new(),
            releases: Vec::new(),
            locks: BTreeSet::new(),
        }
    }
}

impl<A: AppItem, D: AppData> Saving<A, D> {
    /// Whether every chat that's to be written whole and let go has been:
    /// nothing put away, being restored, deleted, moved or shutting down
    /// waits on the store, nor any chat waits to be let go.
    pub(super) fn settled(&self) -> bool {
        self.away.is_empty()
            && self.restoring.is_empty()
            && self.deleted.is_empty()
            && self.moves.is_empty()
            && self.waiting.is_empty()
            && self.releases.is_empty()
            && self.kept.values().all(|kept| !kept.releasing)
            && (self.jobs.values()).all(|job| !matches!(job, Job::Delete { .. }))
    }

    /// Whether every saved chat that's loaded is written as it is, with
    /// no write of it under way: as CAF shuts down, it isn't idle until
    /// they are.
    pub(super) fn all_written(&self) -> bool {
        (self.kept.values()).all(|kept| {
            !kept.behind && kept.unsaved.is_empty() && kept.due.is_none() && kept.writing.is_none()
        })
    }

    /// Whether a move of `shelf` waits or is under way: work for chats on
    /// it waits until it's answered.
    pub(super) fn blocked(&self, shelf: &Shelf) -> bool {
        (self.moves.iter()).any(|each| each.from == *shelf || each.to == *shelf)
    }

    /// Whether work for chats on `shelf` is under way: a move of it waits
    /// until it's answered.
    fn busy(&self, shelf: &Shelf) -> bool {
        self.flights.values().any(|on| on == shelf)
    }
}

/// A loaded chat that's saved.
#[derive(Debug, Serialize)]
#[serde(bound = "A: AppItem, D: AppData")]
#[cfg_attr(feature = "inspect", derive(Clone))]
#[allow(clippy::struct_excessive_bools)] // one plain answer each
pub(super) struct Kept<A: AppItem, D: AppData> {
    /// Its shelf.
    pub(super) shelf: Shelf,
    /// What's changed since it was last written, squashed as it's added.
    /// Its own list, not the viewers' bounded one, so nothing is lost
    /// however much changes at once; past the size of its last snapshot,
    /// it's written whole instead.
    pub(super) unsaved: Vec<Entry<A, D>>,
    /// About how many bytes its last snapshot took in the store, as the
    /// store said: 0 until this run writes one, so a restored chat's
    /// second write is whole.
    pub(super) snapshot_size: u64,
    /// When to write what's changed, if anything has.
    pub(super) due: Option<Mono>,
    /// The write under way, if any: one at a time.
    pub(super) writing: Option<Ticket>,
    /// The last write failed: when to try again.
    pub(super) retry: Option<Mono>,
    /// When it was made.
    pub(super) made: Wall,
    /// When it last changed.
    pub(super) changed: Wall,
    /// Its agent's built-in profile, as it was saved: what its header
    /// says while the host offers no agent by its name.
    pub(super) profile: Option<String>,
    /// The session to continue, as its journal last said.
    pub(super) session: Option<AgentSessionId>,
    /// How many bytes its journal has taken in the store since its last
    /// snapshot.
    pub(super) journal: u64,
    /// Its next write is whole: the store doesn't have it, or no longer
    /// holds it locked.
    pub(super) whole: bool,
    /// The store is behind it even with nothing unsaved: it was never
    /// written, or its last write failed.
    pub(super) behind: bool,
    /// It changed in this call into the core.
    pub(super) touched: bool,
    /// A turn of it ended in this call: it's written at once.
    pub(super) urgent: bool,
    /// It's to be let go in the store once it's written whole, as CAF
    /// shuts down.
    pub(super) releasing: bool,
    /// Writes that failed in a row.
    pub(super) failures: u32,
}

impl<A: AppItem, D: AppData> Kept<A, D> {
    /// A chat kept on `shelf`, made at `made`, that the store has as it is
    /// now if `stored`.
    fn new(shelf: Shelf, made: Wall, stored: bool) -> Self {
        Self {
            shelf,
            unsaved: Vec::new(),
            snapshot_size: 0,
            due: None,
            writing: None,
            retry: None,
            made,
            changed: made,
            profile: None,
            session: None,
            journal: 0,
            whole: !stored,
            behind: !stored,
            touched: !stored,
            urgent: false,
            releasing: false,
            failures: 0,
        }
    }

    /// Adds `entry` to what's unsaved: words streamed to the item the last
    /// entry grew are added to that entry, and a tool call that changed
    /// again replaces the last entry, if that was its change.
    pub(super) fn add(&mut self, entry: Entry<A, D>) {
        self.add_with(|| entry);
    }

    /// Keeps the last entry unsaved, if it's a run of changes to `item`'s
    /// output merged into one, about as big as the output `chat` keeps:
    /// once it's bigger than [`MOST_OUTPUT`](crate::tool::MOST_OUTPUT),
    /// it's the call as it is instead, which the record keeps capped. A
    /// merged change keeps every line printed since, even those dropped.
    pub(super) fn bound_output(&mut self, item: ItemId, chat: &Chat<A>) {
        let Some(Entry::Change(Change::OutputChanged {
            item: last, lines, ..
        })) = self.unsaved.last()
        else {
            return;
        };
        if *last != item || crate::tool::output_size(lines) <= crate::tool::MOST_OUTPUT {
            return;
        }
        let tool = (chat.turns.iter().rev())
            .flat_map(|turn| turn.items.iter())
            .find(|each| each.id == item)
            .and_then(|each| match &each.kind {
                ItemKind::Tool(tool) => Some(tool.clone()),
                _ => None,
            });
        let Some(tool) = tool else {
            return;
        };
        self.unsaved.pop();
        self.add_with(|| Entry::Change(Change::ToolChanged { item, tool }));
    }

    /// Adds the entry `entry` makes to what's unsaved, as
    /// [`Kept::add`]: or, if it's written whole next anyway, notes only
    /// that it changed, as the chat itself holds the change.
    pub(super) fn add_with(&mut self, entry: impl FnOnce() -> Entry<A, D>) {
        self.touched = true;
        if self.whole {
            self.behind = true;
            return;
        }
        let entry = entry();
        if let (
            Some(Entry::Change(Change::Appended { item, text })),
            Entry::Change(Change::Appended {
                item: more,
                text: added,
            }),
        ) = (self.unsaved.last_mut(), &entry)
            && item == more
        {
            text.push_str(added);
            return;
        }
        if let (Some(Entry::Change(last)), Entry::Change(next)) = (self.unsaved.last_mut(), &entry)
            && squash_output(last, next)
        {
            return;
        }
        if let (
            Some(Entry::Change(Change::ToolChanged { item, .. })),
            Entry::Change(Change::ToolChanged { item: again, .. }),
        ) = (self.unsaved.last(), &entry)
            && item == again
            && let Some(last) = self.unsaved.last_mut()
        {
            *last = entry;
            return;
        }
        self.unsaved.push(entry);
    }
}

/// Makes `last` a change to a command's output do what it does and then
/// `next` does, if both are, of the same call. Returns whether it did.
///
/// `last` replaces the lines from `from` on with `lines`, then drops
/// `dropped` from the top; `next` then counts from what's left. Where
/// `next` starts within what `last` left of the lines before it, it
/// replaces from there; else it keeps the start of `last`'s lines.
pub(super) fn squash_output<A: AppItem>(last: &mut Change<A>, next: &Change<A>) -> bool {
    let (
        Change::OutputChanged {
            item,
            from,
            lines,
            dropped,
        },
        Change::OutputChanged {
            item: again,
            from: next_from,
            lines: next_lines,
            dropped: next_dropped,
        },
    ) = (last, next)
    else {
        return false;
    };
    if item != again {
        return false;
    }
    // Where `next` starts, counted before `last` dropped any.
    let start = *next_from + *dropped;
    if start <= *from {
        *from = start;
        lines.clear();
    } else {
        lines.truncate(usize::try_from(start - *from).unwrap_or(usize::MAX));
    }
    lines.extend(next_lines.iter().cloned());
    *dropped += *next_dropped;
    true
}

/// A chat put away, whose last snapshot isn't written yet.
#[derive(Debug, Serialize)]
#[serde(bound = "A: AppItem, D: AppData")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Away<A: AppItem, D: AppData> {
    /// Its shelf.
    pub(super) shelf: Shelf,
    /// It, whole.
    pub(super) saved: Box<Saved<A, D>>,
    /// The write under way.
    pub(super) writing: Option<Ticket>,
    /// The last write failed: when to try again.
    pub(super) retry: Option<Mono>,
    /// Writes that failed in a row.
    pub(super) failures: u32,
}

/// What a piece of the store's work is for.
#[derive(Debug, Serialize)]
#[serde(tag = "type", content = "value")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) enum Job {
    /// Listing a shelf.
    List(Shelf),
    /// Adding to a chat's journal.
    Append(ChatId),
    /// Writing a loaded chat whole.
    Snapshot(ChatId),
    /// Writing a chat put away whole, to let it go once it's written.
    Away(ChatId),
    /// Deleting a chat.
    Delete {
        /// The chat.
        chat: ChatId,
        /// Its shelf.
        shelf: Shelf,
    },
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// The id the next chat made gets: a UUID of version 7, from `now`'s
    /// time and, through the host's seed, how many chats came before.
    pub(super) fn next_chat_id(&self, now: Now) -> ChatId {
        #[cfg(test)]
        if self.numbered {
            return ChatId::from_number(self.ids.next);
        }
        // Before 1970 can't be: such a clock is wrong, and 0 still sorts.
        let mut millis = u64::try_from(now.wall.0).unwrap_or(0) & MOST_MILLIS;
        // Within one millisecond, or if the clock went back, the next of
        // a count after the last, so ids sort as their chats were made.
        let mut count = 0;
        if let Some(last) = self.saving.last {
            let (at, counted) = (last.0.as_u128() >> 80, (last.0.as_u128() >> 64) & 0xfff);
            let at = u64::try_from(at).unwrap_or(0);
            if millis <= at {
                millis = at;
                count = counted + 1;
                if count > 0xfff {
                    millis += 1;
                    count = 0;
                }
            }
        }
        // 62 bits from the seed and how many chats came before, so runs
        // with different seeds don't meet.
        let random = mix(self.host.seed ^ mix(self.saving.made)) >> 2;
        let id = (u128::from(millis) << 80)
            | (0x7 << 76)
            | (count << 64)
            | (0b10 << 62)
            | u128::from(random);
        ChatId(uuid::Uuid::from_u128(id))
    }

    /// Notes chat `chat` was made: the next made sorts after it, if it's a
    /// UUID of version 7.
    pub(super) fn made_chat(&mut self, chat: ChatId) {
        self.saving.made += 1;
        if chat.0.get_version_num() == 7 && self.saving.last.is_none_or(|last| last < chat) {
            self.saving.last = Some(chat);
        }
    }

    /// Takes in the storage driver's report.
    pub(super) fn storage_event(
        &mut self,
        now: Now,
        ticket: Ticket,
        event: StorageEvent<A, D>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if let Some(at) =
            (self.saving.moves.iter()).position(|each| each.sent && each.ticket == ticket)
        {
            let moving = self.saving.moves.remove(at);
            return self.moved(now, moving, event, out);
        }
        // An answer to work not under way, sent or not, changes nothing.
        let Some(shelf) = self.saving.flights.remove(&ticket) else {
            return report(
                out,
                Level::Debug,
                format!("ignored the store's answer to ticket {}", ticket.0),
            );
        };
        if let Some(restore) = self.saving.restoring.remove(&ticket) {
            return match event {
                StorageEvent::Loaded(saved) => self.restored(now, restore, *saved, out),
                StorageEvent::NotLoaded(why) => self.restore_failed(restore, why, false, out),
                StorageEvent::Failed(why) => {
                    self.restore_failed(restore, CannotRestore::Store(why), false, out);
                }
                other => {
                    let what = kind(&other);
                    let why = CannotRestore::Damaged(format!("the store answered with {what}"));
                    self.restore_failed(restore, why, false, out);
                }
            };
        }
        // The load of a chat deleted meanwhile: it took the chat's lock,
        // for the deletion to take with it, or to let go if it fails.
        if let Some(deleted) = self.saving.deleted.remove(&ticket) {
            if matches!(event, StorageEvent::Loaded(_)) {
                self.saving.locks.insert((deleted.shelf, deleted.chat));
            }
            return;
        }
        let Some(job) = self.saving.jobs.remove(&ticket) else {
            return report(
                out,
                Level::Debug,
                format!("ignored the store's answer to ticket {}", ticket.0),
            );
        };
        // Anything but done or failed, from a write or a deletion, is a
        // failure: the chat is written again, or let go, rather than wait
        // for an answer that won't come.
        let event = match (&job, event) {
            (
                Job::Append(_) | Job::Snapshot(_) | Job::Away(_) | Job::Delete { .. },
                event @ (StorageEvent::Listed { .. }
                | StorageEvent::Loaded(_)
                | StorageEvent::NotLoaded(_)),
            ) => StorageEvent::Failed(format!("the store answered with {}", kind(&event))),
            (_, event) => event,
        };
        // A chat written whole is locked, whatever became of it meanwhile.
        if let (Job::Snapshot(chat) | Job::Away(chat), StorageEvent::Done { .. }) = (&job, &event) {
            self.saving.locks.insert((shelf, *chat));
        }
        match (job, event) {
            (Job::List(shelf), StorageEvent::Listed { chats, unreadable }) => {
                for (chat, why) in &unreadable {
                    report(
                        out,
                        Level::Warn,
                        format!("chat {chat} on shelf {:?} can't be read: {why:?}", shelf.0),
                    );
                }
                self.listed(shelf, chats);
            }
            // A write of a chat that's gone, or isn't the one under way,
            // changes nothing.
            (Job::Append(chat) | Job::Snapshot(chat), _)
                if (self.saving.kept.get(&chat))
                    .is_none_or(|kept| kept.writing != Some(ticket)) => {}
            (Job::Append(chat), StorageEvent::Done { written }) => {
                if let Some(kept) = self.saving.kept.get_mut(&chat) {
                    kept.writing = None;
                    kept.journal = kept.journal.saturating_add(written);
                    kept.failures = 0;
                }
                self.release_if_done(now, chat, out);
            }
            (Job::Snapshot(chat), StorageEvent::Done { written }) => {
                if let Some(kept) = self.saving.kept.get_mut(&chat) {
                    kept.writing = None;
                    kept.snapshot_size = written;
                    kept.journal = 0;
                    kept.failures = 0;
                }
                self.release_if_done(now, chat, out);
            }
            (Job::Append(chat) | Job::Snapshot(chat), StorageEvent::Failed(why)) => {
                let Some(kept) = self.saving.kept.get_mut(&chat) else {
                    return;
                };
                kept.writing = None;
                kept.whole = true;
                kept.behind = true;
                let wait = retry_after(self.host.timers.save, kept.failures);
                kept.failures = kept.failures.saturating_add(1);
                kept.retry = Some(now.mono.after(wait));
                kept.due = Some(kept.due.map_or(now.mono, |due| due.min(now.mono)));
                report(
                    out,
                    Level::Warn,
                    format!(
                        "chat {chat} couldn't be saved, and is tried again whole in {} s: {why}",
                        wait.as_secs()
                    ),
                );
            }
            (Job::Away(chat), _)
                if (self.saving.away.get(&chat))
                    .is_none_or(|away| away.writing != Some(ticket)) => {}
            (Job::Away(chat), StorageEvent::Done { .. }) => {
                if let Some(away) = self.saving.away.remove(&chat) {
                    let Away { shelf, saved, .. } = away;
                    // Listed as written, even if it wasn't when the shelf
                    // was read.
                    self.relist(&shelf, chat, Some((saved.header, SavedState::Saved)));
                    self.release(shelf, chat, out);
                }
            }
            (Job::Away(chat), StorageEvent::Failed(why)) => {
                let Some(away) = self.saving.away.get_mut(&chat) else {
                    return;
                };
                away.writing = None;
                let wait = retry_after(self.host.timers.save, away.failures);
                away.failures = away.failures.saturating_add(1);
                away.retry = Some(now.mono.after(wait));
                report(
                    out,
                    Level::Warn,
                    format!(
                        "chat {chat}, put away, couldn't be saved, and is tried again in {} s: {why}",
                        wait.as_secs()
                    ),
                );
            }
            (Job::Delete { chat, shelf }, StorageEvent::Done { .. }) => {
                // Gone, with its lock.
                self.saving.locks.remove(&(shelf, chat));
            }
            (Job::Delete { chat, shelf }, StorageEvent::Failed(why)) => {
                // It's still there: let go, if this run held it and has no
                // more use for it, and listed again.
                report(
                    out,
                    Level::Warn,
                    format!(
                        "chat {chat} couldn't be deleted, and an older copy of it stays on shelf {:?}: what it hadn't written yet is gone: {why}",
                        shelf.0
                    ),
                );
                if self.saving.shelves.contains_key(&shelf) {
                    self.list_again(now, shelf, out);
                }
            }
            (Job::List(shelf), StorageEvent::Failed(why)) => report(
                out,
                Level::Warn,
                format!("shelf {:?} couldn't be listed: {why}", shelf.0),
            ),
            (job, event) => report(
                out,
                Level::Warn,
                format!("the store answered {job:?} with {}", kind(&event)),
            ),
        }
    }

    /// Lists a shelf.
    pub(super) fn list_shelf(
        &mut self,
        now: Now,
        shelf: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        check_shelf(&shelf)?;
        self.list_again(now, shelf, out);
        Ok(())
    }

    /// Asks the store for a shelf's chats.
    fn list_again(&mut self, now: Now, shelf: Shelf, out: &mut Vec<Directive<A, D>>) {
        let ticket = Ticket(self.ids.take());
        self.saving.jobs.insert(ticket, Job::List(shelf));
        self.dispatch(now, ticket, out);
    }

    /// Sends the load, deletion or list of `ticket`, or has it wait for a
    /// move of its shelf.
    fn dispatch(&mut self, now: Now, ticket: Ticket, out: &mut Vec<Directive<A, D>>) {
        let directive = match (
            self.saving.restoring.get(&ticket),
            self.saving.jobs.get(&ticket),
        ) {
            (Some(restore), _) => StorageDirective::Load {
                ticket,
                shelf: restore.shelf.clone(),
                chat: restore.chat,
            },
            (None, Some(Job::List(shelf))) => StorageDirective::List {
                ticket,
                shelf: shelf.clone(),
            },
            (None, Some(Job::Delete { chat, shelf })) => StorageDirective::Delete {
                ticket,
                shelf: shelf.clone(),
                chat: *chat,
            },
            _ => return,
        };
        match shelf_of(&directive) {
            Some(shelf) if self.saving.blocked(&shelf) => self.saving.waiting.push(ticket),
            _ => {
                if let StorageDirective::Delete { chat, shelf, .. } = &directive {
                    self.deleting(now, ticket, (*chat, shelf), out);
                }
                self.hand_over(directive, out);
            }
        }
    }

    /// Sends `directive`, noting its shelf until it's answered.
    fn hand_over(&mut self, directive: StorageDirective<A, D>, out: &mut Vec<Directive<A, D>>) {
        if let (Some(ticket), Some(shelf)) = (ticket_of(&directive), shelf_of(&directive)) {
            self.saving.flights.insert(ticket, shelf);
        }
        out.push(Directive::Storage(directive));
    }

    /// Lets `chat` go in the store, on `shelf`: once no move of the shelf
    /// waits.
    fn release(&mut self, shelf: Shelf, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        self.saving.locks.remove(&(shelf.clone(), chat));
        if self.saving.blocked(&shelf) {
            self.saving.releases.push((shelf, chat));
        } else {
            out.push(Directive::Storage(StorageDirective::Release {
                shelf,
                chat,
            }));
        }
    }

    /// A move was answered: if it was done, every chat on the shelf it
    /// was from is on the shelf it went to, and work waiting for one there
    /// goes there. Both are listed again, if listed, as a list read before
    /// it doesn't hold what moved.
    fn moved(
        &mut self,
        now: Now,
        moving: Moving,
        event: StorageEvent<A, D>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Moving { from, to, .. } = moving;
        match event {
            StorageEvent::Done { .. } => {
                let on = |shelf: &mut Shelf| {
                    if *shelf == from {
                        shelf.clone_from(&to);
                    }
                };
                self.saving
                    .kept
                    .values_mut()
                    .for_each(|kept| on(&mut kept.shelf));
                self.saving
                    .away
                    .values_mut()
                    .for_each(|away| on(&mut away.shelf));
                (self.saving.releases.iter_mut()).for_each(|(shelf, _)| on(shelf));
                // A chat deleted while restored has its load under way,
                // which a move waits for: none is here.
                let locks = std::mem::take(&mut self.saving.locks);
                self.saving.locks = (locks.into_iter())
                    .map(|(mut shelf, chat)| {
                        on(&mut shelf);
                        (shelf, chat)
                    })
                    .collect();
                // What's listed of the one is the other's now, if that's
                // listed; both are read again below.
                if let Some(list) = self.saving.shelves.get_mut(&from) {
                    let moved = std::mem::take(list);
                    if let Some(into) = self.saving.shelves.get_mut(&to) {
                        into.extend(moved.into_iter().map(|each| SavedChat {
                            shelf: to.clone(),
                            ..each
                        }));
                        into.sort_by_key(|each| (each.header.made, each.header.chat));
                    }
                }
                for ticket in &self.saving.waiting {
                    if let Some(restore) = self.saving.restoring.get_mut(ticket) {
                        on(&mut restore.shelf);
                    }
                    if let Some(Job::Delete { chat: _, shelf }) = self.saving.jobs.get_mut(ticket) {
                        on(shelf);
                    }
                }
            }
            StorageEvent::Failed(why) => report(
                out,
                Level::Warn,
                format!("shelf {:?} couldn't be moved to {:?}: {why}", from.0, to.0),
            ),
            other => report(
                out,
                Level::Warn,
                format!("the store answered a move with {}", kind(&other)),
            ),
        }
        // What waited for it goes first, in order.
        self.unblock(now, out);
        for shelf in [from, to] {
            if self.saving.shelves.contains_key(&shelf) {
                self.list_again(now, shelf, out);
            }
        }
    }

    /// Once no move of their shelves waits: chats waiting to be let go
    /// are, and loads, deletions and lists waiting are sent, in order.
    /// Those whose shelves still wait go on waiting.
    fn unblock(&mut self, now: Now, out: &mut Vec<Directive<A, D>>) {
        let releases = std::mem::take(&mut self.saving.releases);
        for (shelf, chat) in releases {
            self.release(shelf, chat, out);
        }
        for ticket in std::mem::take(&mut self.saving.waiting) {
            self.dispatch(now, ticket, out);
        }
    }

    /// Lets go of each chat the store holds locked for this run that
    /// nothing has a use for: it isn't loaded, put away or being restored,
    /// and no write or deletion of it waits or is under way.
    fn let_go_orphans(&mut self, out: &mut Vec<Directive<A, D>>) {
        let saving = &self.saving;
        let used = |shelf: &Shelf, chat: ChatId| {
            (saving.kept.get(&chat)).is_some_and(|kept| kept.shelf == *shelf)
                || (saving.away.get(&chat)).is_some_and(|away| away.shelf == *shelf)
                || (saving.restoring.values()).any(|each| each.chat == chat && each.shelf == *shelf)
                || (saving.deleted.values()).any(|each| each.chat == chat && each.shelf == *shelf)
                || (saving.jobs.iter()).any(|(ticket, job)| match job {
                    // A write is always under way: its shelf is noted.
                    Job::Append(each) | Job::Snapshot(each) | Job::Away(each) => {
                        *each == chat && saving.flights.get(ticket) == Some(shelf)
                    }
                    Job::Delete {
                        chat: each,
                        shelf: on,
                    } => *each == chat && on == shelf,
                    Job::List(_) => false,
                })
        };
        let orphans: Vec<(ChatId, Shelf)> = (saving.locks.iter())
            .filter(|&(shelf, chat)| !used(shelf, *chat))
            .map(|(shelf, chat)| (*chat, shelf.clone()))
            .collect();
        for (chat, shelf) in orphans {
            self.release(shelf, chat, out);
        }
    }

    /// Sends each move waiting whose shelves have no work under way, and
    /// no earlier move of either waiting.
    fn send_moves(&mut self, out: &mut Vec<Directive<A, D>>) {
        for at in 0..self.saving.moves.len() {
            let moving = &self.saving.moves[at];
            let shares = |each: &Moving| {
                [&each.from, &each.to].contains(&&moving.from)
                    || [&each.from, &each.to].contains(&&moving.to)
            };
            let ready = !moving.sent
                && !self.saving.busy(&moving.from)
                && !self.saving.busy(&moving.to)
                && !self.saving.moves[..at].iter().any(shares);
            if ready {
                let moving = &mut self.saving.moves[at];
                moving.sent = true;
                out.push(Directive::Storage(StorageDirective::MoveShelf {
                    ticket: moving.ticket,
                    from: moving.from.clone(),
                    to: moving.to.clone(),
                }));
            }
        }
    }

    /// The store listed `shelf`: its chats, each loaded one as it is now,
    /// with those loaded that it doesn't have yet.
    fn listed(&mut self, shelf: Shelf, headers: Vec<Header>) {
        // A chat being deleted is gone, if a list read before the deletion
        // still has it.
        let deleting: BTreeSet<ChatId> = (self.saving.jobs.values())
            .filter_map(|job| match job {
                Job::Delete { chat, shelf: on } if *on == shelf => Some(*chat),
                _ => None,
            })
            .collect();
        let mut list: Vec<SavedChat> = (headers.into_iter())
            .filter(|header| !deleting.contains(&header.chat))
            .map(|header| {
                let state = self.saved_state(&shelf, header.chat);
                // Loaded or put away, it's as it is now, not as it was
                // last written.
                let away = (self.saving.away.get(&header.chat))
                    .filter(|away| away.shelf == shelf)
                    .map(|away| away.saved.header.clone());
                let header = match state {
                    SavedState::Loaded => self.header(header.chat).unwrap_or(header),
                    _ => away.unwrap_or(header),
                };
                SavedChat {
                    shelf: shelf.clone(),
                    header,
                    state,
                }
            })
            .collect();
        // Put away, and not written yet: as it was put away.
        let away: Vec<SavedChat> = (self.saving.away.iter())
            .filter(|(chat, away)| {
                away.shelf == shelf
                    && !deleting.contains(*chat)
                    && !list.iter().any(|each| each.header.chat == **chat)
            })
            .map(|(_, away)| SavedChat {
                shelf: shelf.clone(),
                header: away.saved.header.clone(),
                state: SavedState::Saved,
            })
            .collect();
        list.extend(away);
        let unlisted: Vec<ChatId> = (self.saving.kept.iter())
            .filter(|(chat, kept)| {
                kept.shelf == shelf
                    && !deleting.contains(*chat)
                    && !list.iter().any(|each| each.header.chat == **chat)
            })
            .map(|(&chat, _)| chat)
            .collect();
        for chat in unlisted {
            if let Some(header) = self.header(chat) {
                list.push(SavedChat {
                    shelf: shelf.clone(),
                    header,
                    state: SavedState::Loaded,
                });
            }
        }
        list.sort_by_key(|each| (each.header.made, each.header.chat));
        self.saving.shelves.insert(shelf, list);
    }

    /// Where the core is with a saved chat on `shelf`, as lists show it.
    fn saved_state(&self, shelf: &Shelf, chat: ChatId) -> SavedState {
        let restoring = || {
            (self.saving.restoring.values()).any(|each| each.chat == chat && each.shelf == *shelf)
        };
        if (self.saving.kept.get(&chat)).is_some_and(|kept| kept.shelf == *shelf) {
            return SavedState::Loaded;
        }
        if restoring() {
            return SavedState::Restoring;
        }
        // A restore that failed is still said to have, until another.
        let listed = (self.saving.shelves.get(shelf).into_iter())
            .flatten()
            .find(|each| each.header.chat == chat);
        match listed.map(|each| &each.state) {
            Some(SavedState::Failed(why)) => SavedState::Failed(why.clone()),
            _ => SavedState::Saved,
        }
    }

    /// Puts `chat` in `shelf`'s list, with `header` and `state`, if the
    /// shelf is listed; `None` takes it out.
    fn relist(&mut self, shelf: &Shelf, chat: ChatId, entry: Option<(Header, SavedState)>) {
        // One being deleted there is gone, as the host sees it.
        let deleting = (self.saving.jobs.values()).any(
            |job| matches!(job, Job::Delete { chat: each, shelf: on } if *each == chat && on == shelf),
        );
        let entry = entry.filter(|_| !deleting);
        let Some(list) = self.saving.shelves.get_mut(shelf) else {
            return;
        };
        list.retain(|each| each.header.chat != chat);
        if let Some((header, state)) = entry {
            let key = (header.made, header.chat);
            let at = list.partition_point(|each| (each.header.made, each.header.chat) < key);
            list.insert(
                at,
                SavedChat {
                    shelf: shelf.clone(),
                    header,
                    state,
                },
            );
        }
    }

    /// Sets the state `chat` is listed with on `shelf`, if it's listed.
    fn relist_state(&mut self, shelf: &Shelf, chat: ChatId, state: SavedState) {
        let listed = (self.saving.shelves.get_mut(shelf).into_iter())
            .flatten()
            .find(|each| each.header.chat == chat);
        if let Some(each) = listed {
            each.state = state;
        }
    }

    /// Lists loaded `chat` as it is now, if its shelf is listed.
    fn relist_loaded(&mut self, chat: ChatId) {
        let (Some(header), Some(shelf)) = (
            self.header(chat),
            self.saving.kept.get(&chat).map(|kept| kept.shelf.clone()),
        ) else {
            return;
        };
        self.relist(&shelf, chat, Some((header, SavedState::Loaded)));
    }

    /// Starts a restore.
    pub(super) fn start_restore(
        &mut self,
        now: Now,
        restore: RestoreChat,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        check_shelf(&restore.shelf)?;
        super::sessions::check_servers(&restore.mcp_servers)?;
        (restore.mcp_servers.iter())
            .map(mcp_server)
            .collect::<Option<Vec<_>>>()
            .ok_or(Refused::Unsendable)?;
        check_instructions(restore.instructions.as_deref())?;
        self.caf_server(restore.chat, &restore.tools.caf)?;
        if self.is_loaded(restore.chat) {
            return Err(Refused::Loaded);
        }
        let ticket = Ticket(self.ids.take());
        self.relist_state(&restore.shelf, restore.chat, SavedState::Restoring);
        self.saving.restoring.insert(ticket, restore);
        self.dispatch(now, ticket, out);
        Ok(())
    }

    /// Whether a chat with this id is loaded, being restored, or put away
    /// and not yet written.
    fn is_loaded(&self, chat: ChatId) -> bool {
        self.chats.contains_key(&chat)
            || self.saving.away.contains_key(&chat)
            || (self.saving.restoring.values()).any(|each| each.chat == chat)
    }

    /// The store read a chat for `restore`: it's tidied and loaded, idle,
    /// with no agent started until a message is sent; or archived, if it
    /// was, on no connection until it's unarchived.
    fn restored(
        &mut self,
        now: Now,
        restore: RestoreChat,
        saved: Saved<A, D>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Saved {
            header,
            chat: mut whole,
            session,
            data,
        } = saved;
        let id = restore.chat;
        let checked = if whole.id != id || header.chat != id {
            Err(format!("it holds chat {} instead", whole.id))
        } else if !numbered_within_bounds(&whole) {
            Err("a turn or item in it is numbered too near 2^53".to_owned())
        } else if !(whole.turns.iter())
            .filter_map(|turn| turn.spent.as_deref())
            .all(|usage| counted_within_bounds(usage.models.iter().map(|row| &row.tokens)))
        {
            Err("what a turn in it spent counts past 2^53".to_owned())
        } else {
            match whole.cwd.to_str() {
                None => Err("its folder isn't UTF-8".to_owned()),
                Some(_) if !whole.cwd.is_absolute() => Err("its folder isn't absolute".to_owned()),
                Some(cwd) => Ok(cwd.to_owned()),
            }
        };
        let cwd = match checked {
            Ok(cwd) => cwd,
            Err(damage) => {
                let why = CannotRestore::Damaged(damage);
                return self.restore_failed(restore, why, true, out);
            }
        };
        // Renamed by the host: found again by its built-in profile, if
        // exactly one agent has it.
        let mut renamed = false;
        if !self.agents.contains_key(&whole.agent)
            && let Some(profile) = &header.profile
        {
            let mut same = (self.slugs.iter()).filter(|(_, slug)| **slug == profile.as_str());
            if let (Some((name, _)), None) = (same.next(), same.next()) {
                report(
                    out,
                    Level::Info,
                    format!(
                        "chat {id}: its agent {:?} is now called {:?}",
                        whole.agent.0, name.0
                    ),
                );
                whole.agent = name.clone();
                renamed = true;
            }
        }
        // Its turns and items keep their numbers: new ones count on from
        // past them.
        for turn in &whole.turns {
            self.ids.seen(turn.id.0);
            for item in &turn.items {
                self.ids.seen(item.id.0);
            }
        }
        // The header read may be the snapshot's, from before its journal:
        // only when the chat was made, and its agent's profile, which no
        // journal entry changes, are taken from it. Its label, first line,
        // turns and whether it's closed are the chat's, as each header is
        // made; and when it last changed is the later of the header's word
        // and the latest time the chat holds.
        let changed = last_change(&whole).map_or(header.changed, |at| at.max(header.changed));
        let mut changes = tidy(&whole, changed, now.wall);
        // What it has spent is worked out again from its turns, with the
        // prices the core has now: the save's is never trusted.
        let spend = spend_of(&whole, &self.prices);
        if *whole.spend != spend {
            changes.push(Change::Spend(Box::new(spend)));
        }
        let mut mcp_sent = (restore.mcp_servers.iter())
            .map(mcp_server)
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        let caf_tools = restore.tools.caf.clone();
        // Checked as the restore was asked for: nothing serves tools less.
        if let Ok(Some(caf)) = self.caf_server(id, &caf_tools) {
            mcp_sent.push(caf);
        }
        let conn = self.conn_for(id, &whole.agent);
        self.saving.locks.insert((restore.shelf.clone(), id));
        let mut kept = Kept::new(restore.shelf, header.made, true);
        kept.changed = changed;
        // Tidied, it's written at once.
        kept.urgent = !changes.is_empty();
        // Its agent's new name is in no journal entry: it's written whole,
        // at once, so the store has it. And as CAF shuts down, it's
        // written whole and let go, as every saved chat is.
        if renamed || self.shutting_down {
            kept.whole = true;
            kept.behind = true;
            kept.touched = true;
            kept.urgent = true;
            kept.releasing = self.shutting_down;
        }
        kept.profile = header.profile;
        kept.session.clone_from(&session);
        self.saving.kept.insert(id, kept);
        self.chats.insert(id, whole);
        self.data.insert(id, data);
        self.logs.insert(id, Log::new(self.ids.first));
        // Its changes are numbered again from the first, as each loaded
        // chat's are: a fixture is of one load's changes.
        #[cfg(test)]
        self.applied.remove(&id);
        self.dirty.insert(id);
        self.runs.insert(
            id,
            Run {
                conn,
                cwd,
                mcp: restore.mcp_servers,
                mcp_sent,
                instructions: restore.instructions,
                session,
                open: Open::Idle,
                running: None,
                agent_turn: None,
                quiet_by: None,
                held_until: None,
                dropped_tools: BTreeSet::new(),
                tools: BTreeMap::new(),
                served: BTreeMap::new(),
                import: None,
                replaying: false,
                slow: None,
                held_by: None,
                note: None,
                resent: None,
                restored: true,
                choosing: Choosing::default(),
                cost: None,
                caf_tools,
                tools_lost: false,
            },
        );
        self.serve(id, out);
        // Tidied as changes, so the store gets them as journal entries,
        // and viewers as changes like any other.
        for change in changes {
            self.record(id, change);
        }
        self.restored_choices(id);
        // An archived chat is on no connection.
        if (self.chats.get(&id)).is_some_and(|whole| whole.state == ChatState::Closed) {
            if let Some(each) = self.conns.get_mut(&conn) {
                each.chats.remove(&id);
            }
            self.tidy(conn);
        }
        self.relist_loaded(id);
    }

    /// `restore` failed, as `why` says, and its save is left as it was: let
    /// go in the store, if `locked`, as its load locked it.
    fn restore_failed(
        &mut self,
        restore: RestoreChat,
        why: CannotRestore,
        locked: bool,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let RestoreChat { chat, shelf, .. } = restore;
        let level = if locked { Level::Warn } else { Level::Info };
        report(
            out,
            level,
            format!("chat {chat} couldn't be restored: {why:?}"),
        );
        self.relist_state(&shelf, chat, SavedState::Failed(why));
        // Its load locked it: with no use for it, it's let go.
        if locked {
            self.saving.locks.insert((shelf, chat));
        }
    }

    /// Puts a chat away.
    pub(super) fn put_chat_away(
        &mut self,
        now: Now,
        chat: ChatId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        if !self.chats.contains_key(&chat) {
            return Err(Refused::NoSuchChat);
        }
        if !self.saving.kept.contains_key(&chat) {
            return Err(Refused::NotSaved);
        }
        self.wind_down(now, chat, TurnLife::CutOff, out);
        let conn = self.let_go(now, chat, out);
        // An archived chat stays so.
        if (self.chats.get(&chat)).is_some_and(|whole| whole.state != ChatState::Closed) {
            self.set_state(chat, ChatState::Idle);
        }
        self.put_away_now(now, chat, out);
        if let Some(conn) = conn {
            self.advance(now, conn, out);
        }
        Ok(())
    }

    /// Writes saved `chat`, wound down already, whole, and lets it go from
    /// memory. It's let go in the store once it's written.
    pub(super) fn put_away_now(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        if !self.saving.kept.contains_key(&chat) {
            return;
        }
        self.note_session(chat);
        self.note_changed(now, chat);
        let Some(saved) = self.saved_whole(chat) else {
            return;
        };
        let Some(kept) = self.forget(chat) else {
            return;
        };
        let Kept { shelf, .. } = kept;
        let blocked = self.saving.blocked(&shelf);
        self.relist(
            &shelf,
            chat,
            Some((saved.header.clone(), SavedState::Saved)),
        );
        self.saving.away.insert(
            chat,
            Away {
                shelf,
                saved: Box::new(saved),
                writing: None,
                retry: None,
                failures: 0,
            },
        );
        // At once, unless a move of its shelf waits: then once it's
        // answered.
        if !blocked {
            self.write_away(chat, out);
        }
    }

    /// Lets `chat` go from memory, with everything kept for it, and
    /// returns what was kept of its saving.
    fn forget(&mut self, chat: ChatId) -> Option<Kept<A, D>> {
        self.forget_calls(chat);
        self.unserve(chat);
        self.chats.remove(&chat);
        self.data.remove(&chat);
        self.logs.remove(&chat);
        self.dirty.remove(&chat);
        if let Some(run) = self.runs.remove(&chat)
            && let Some(each) = self.conns.get_mut(&run.conn)
        {
            each.chats.remove(&chat);
        }
        self.saving.kept.remove(&chat)
    }

    /// Writes a chat put away whole: as it's put away, and again after a
    /// write of it failed.
    pub(super) fn write_away(&mut self, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let ticket = Ticket(self.ids.take());
        let Some(away) = self.saving.away.get_mut(&chat) else {
            return;
        };
        away.retry = None;
        away.writing = Some(ticket);
        let snapshot = StorageDirective::Snapshot {
            ticket,
            shelf: away.shelf.clone(),
            saved: away.saved.clone(),
        };
        self.saving.jobs.insert(ticket, Job::Away(chat));
        self.hand_over(snapshot, out);
    }

    /// Deletes a chat: as it's sent to the store, which may wait for a
    /// move of its shelf, the chat is let go from memory if it's loaded or
    /// put away there, as `deleting` says.
    pub(super) fn delete_chat(
        &mut self,
        now: Now,
        chat: ChatId,
        shelf: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        check_shelf(&shelf)?;
        // Loaded or put away, its shelf is known: it's on no other.
        let elsewhere = (self.saving.away.get(&chat)).is_some_and(|away| away.shelf != shelf)
            || (self.chats.contains_key(&chat)
                && (self.saving.kept.get(&chat)).is_none_or(|kept| kept.shelf != shelf));
        if elsewhere {
            return Err(Refused::NoSuchChat);
        }
        self.relist(&shelf, chat, None);
        let ticket = Ticket(self.ids.take());
        let job = Job::Delete { chat, shelf };
        self.saving.jobs.insert(ticket, job);
        self.dispatch(now, ticket, out);
        Ok(())
    }

    /// Deletion `ticket` of `chat` from `shelf` is being sent: if the chat
    /// is loaded there, it's closed and let go from memory, and if it's
    /// put away there, it's let go too; a restore of it there asked for
    /// before isn't, if its load isn't sent, and its load's lock goes with
    /// the chat if it is.
    fn deleting(
        &mut self,
        now: Now,
        ticket: Ticket,
        (chat, shelf): (ChatId, &Shelf),
        out: &mut Vec<Directive<A, D>>,
    ) {
        if (self.saving.kept.get(&chat)).is_some_and(|kept| kept.shelf == *shelf) {
            self.close_session(now, chat, out);
            self.forget(chat);
        }
        if (self.saving.away.get(&chat)).is_some_and(|away| away.shelf == *shelf) {
            self.saving.away.remove(&chat);
        }
        let loads: Vec<Ticket> = (self.saving.restoring.iter())
            .filter(|&(&load, each)| load < ticket && each.chat == chat && each.shelf == *shelf)
            .map(|(&load, _)| load)
            .collect();
        for load in loads {
            self.saving.restoring.remove(&load);
            // A load not sent yet just isn't.
            if let Some(at) = self.saving.waiting.iter().position(|each| *each == load) {
                self.saving.waiting.remove(at);
                continue;
            }
            let deleted = Deleted {
                chat,
                shelf: shelf.clone(),
            };
            self.saving.deleted.insert(load, deleted);
        }
        self.relist(shelf, chat, None);
    }

    /// Moves a shelf: once nothing is under way for a chat on either, and
    /// meanwhile nothing more is sent for them.
    pub(super) fn move_chats(&mut self, from: Shelf, to: Shelf) -> Result<(), Refused> {
        check_shelf(&from)?;
        check_shelf(&to)?;
        if from == to {
            return Ok(());
        }
        let ticket = Ticket(self.ids.take());
        self.saving.moves.push(Moving {
            ticket,
            from,
            to,
            sent: false,
        });
        Ok(())
    }

    /// Sets a chat's data slot.
    pub(super) fn set_chat_data(&mut self, chat: ChatId, data: D) -> Result<(), Refused> {
        if !self.chats.contains_key(&chat) {
            return Err(Refused::NoSuchChat);
        }
        if let Some(kept) = self.saving.kept.get_mut(&chat) {
            kept.add(Entry::Data(data.clone()));
        }
        self.data.insert(chat, data);
        Ok(())
    }

    /// The host asked for something new, calling shutting down off: saved
    /// chats aren't let go after all, as they're in use.
    /// One let go already is written whole at once, which locks it again.
    pub(super) fn call_off_shutdown(&mut self, now: Now) {
        if !self.shutting_down {
            return;
        }
        self.shutting_down = false;
        for (&chat, kept) in &mut self.saving.kept {
            kept.releasing = false;
            let held = self.saving.locks.contains(&(kept.shelf.clone(), chat));
            if !held && kept.writing.is_none() {
                kept.whole = true;
                kept.behind = true;
                kept.due = Some(now.mono);
            }
        }
    }

    /// Saves every saved chat whole and lets it go in the store, as CAF
    /// shuts down. It stays loaded: if it changes again, it's written
    /// whole at once and let go again; if the shutdown is called off, it's
    /// written whole, which locks it again.
    pub(super) fn save_all(&mut self, now: Now) {
        for kept in self.saving.kept.values_mut() {
            kept.releasing = true;
            kept.whole = true;
            kept.due = Some(now.mono);
            // At once, not after a failure's wait.
            kept.retry = None;
        }
        for away in self.saving.away.values_mut() {
            away.retry = away.retry.map(|_| now.mono);
        }
    }

    /// The journal hears if the session `chat` continues changed.
    fn note_session(&mut self, chat: ChatId) {
        let session = self.runs.get(&chat).and_then(saved_session);
        if let Some(kept) = self.saving.kept.get_mut(&chat)
            && kept.session != session
        {
            kept.session.clone_from(&session);
            kept.add(Entry::Session(session));
        }
    }

    /// Notes when `chat` changed and when to write it, if it changed in
    /// this call.
    fn note_changed(&mut self, now: Now, chat: ChatId) {
        let save = self.host.timers.save;
        let Some(kept) = self.saving.kept.get_mut(&chat) else {
            return;
        };
        if kept.touched {
            kept.touched = false;
            kept.changed = now.wall;
            // Changed as CAF shuts down: written whole at once, and let go
            // again.
            if self.shutting_down {
                kept.releasing = true;
                kept.whole = true;
                kept.behind = true;
                kept.urgent = true;
            }
            let soon = if kept.urgent {
                now.mono
            } else {
                now.mono.after(save)
            };
            kept.due = Some(kept.due.map_or(soon, |due| due.min(soon)));
        }
        kept.urgent = false;
    }

    /// As a call ends: what changed of each saved chat is noted, and each
    /// due to be written is.
    pub(super) fn save_due(&mut self, now: Now, out: &mut Vec<Directive<A, D>>) {
        self.unblock(now, out);
        self.let_go_orphans(out);
        let chats: Vec<ChatId> = self.saving.kept.keys().copied().collect();
        for chat in chats {
            self.note_session(chat);
            self.note_changed(now, chat);
            if self.save_at(chat).is_some_and(|at| at <= now.mono) {
                self.write(now, chat, out);
            }
        }
        let away: Vec<ChatId> = (self.saving.away.iter())
            .filter(|(_, away)| {
                away.writing.is_none()
                    && away.retry.is_none_or(|at| at <= now.mono)
                    && !self.saving.blocked(&away.shelf)
            })
            .map(|(&chat, _)| chat)
            .collect();
        for chat in away {
            self.write_away(chat, out);
        }
        self.send_moves(out);
    }

    /// When `chat` is next written, if it's waiting to be: never while a
    /// write of it is under way.
    pub(super) fn save_at(&self, chat: ChatId) -> Option<Mono> {
        let kept = self.saving.kept.get(&chat)?;
        if kept.writing.is_some() || self.saving.blocked(&kept.shelf) {
            return None;
        }
        let due = kept.due?;
        Some(kept.retry.map_or(due, |retry| retry.max(due)))
    }

    /// When a chat put away is next written, after a write of it failed.
    pub(super) fn away_at(&self, chat: ChatId) -> Option<Mono> {
        let away = self.saving.away.get(&chat)?;
        (away.writing.is_none() && !self.saving.blocked(&away.shelf)).then_some(away.retry?)
    }

    /// Writes what's changed of `chat`: whole if it must be or its journal
    /// has outgrown its snapshot, and otherwise added to its journal.
    /// Nothing, if nothing changed.
    pub(super) fn write(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        // What changed this call, and its session, go with it.
        self.note_session(chat);
        self.note_changed(now, chat);
        let Some(kept) = self.saving.kept.get_mut(&chat) else {
            return;
        };
        kept.due = None;
        kept.retry = None;
        if kept.unsaved.is_empty() && !kept.behind && !kept.releasing {
            return;
        }
        let whole = kept.whole || kept.releasing || kept.journal > kept.snapshot_size;
        let (saved, header) = if whole {
            (self.saved_whole(chat), None)
        } else {
            (None, self.header(chat))
        };
        if saved.is_none() && header.is_none() {
            debug_assert!(false, "chat {chat} is kept, so it has a header");
            return;
        }
        let ticket = Ticket(self.ids.take());
        let Some(kept) = self.saving.kept.get_mut(&chat) else {
            return;
        };
        let shelf = kept.shelf.clone();
        let (directive, job) = match (saved, header) {
            (Some(saved), _) => {
                kept.unsaved.clear();
                kept.whole = false;
                let saved = Box::new(saved);
                let snapshot = StorageDirective::Snapshot {
                    ticket,
                    shelf,
                    saved,
                };
                (snapshot, Job::Snapshot(chat))
            }
            (None, Some(header)) => {
                let entries = std::mem::take(&mut kept.unsaved);
                let append = StorageDirective::Append {
                    ticket,
                    shelf,
                    header,
                    entries,
                };
                (append, Job::Append(chat))
            }
            (None, None) => return,
        };
        kept.behind = false;
        kept.writing = Some(ticket);
        self.saving.jobs.insert(ticket, job);
        self.hand_over(directive, out);
        self.relist_loaded(chat);
    }

    /// A write of `chat` is done: if CAF is shutting down and it's written
    /// whole, it's let go in the store; if not yet, it's written again.
    fn release_if_done(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let Some(kept) = self.saving.kept.get_mut(&chat) else {
            return;
        };
        if !kept.releasing || kept.writing.is_some() {
            return;
        }
        if !kept.unsaved.is_empty() || kept.behind || kept.whole {
            kept.due = Some(now.mono);
            return;
        }
        kept.releasing = false;
        kept.whole = true;
        let shelf = kept.shelf.clone();
        self.release(shelf, chat, out);
    }

    /// `chat`, whole, as it's saved.
    fn saved_whole(&self, chat: ChatId) -> Option<Saved<A, D>> {
        Some(Saved {
            header: self.header(chat)?,
            chat: self.chats.get(&chat)?.clone(),
            session: self.runs.get(&chat).and_then(saved_session),
            data: self.data.get(&chat).cloned().unwrap_or_default(),
        })
    }

    /// What lists of saved chats show of loaded `chat`.
    fn header(&self, chat: ChatId) -> Option<Header> {
        let whole = self.chats.get(&chat)?;
        let kept = self.saving.kept.get(&chat)?;
        let profile = if self.agents.contains_key(&whole.agent) {
            self.slugs.get(&whole.agent).map(|slug| (*slug).to_owned())
        } else {
            kept.profile.clone()
        };
        Some(Header {
            chat,
            agent: whole.agent.clone(),
            profile,
            cwd: whole.cwd.clone(),
            made: kept.made,
            changed: kept.changed,
            label: self.data.get(&chat).and_then(AppData::label),
            first: first_line(whole),
            turns: u64::try_from(whole.turns.len()).unwrap_or(u64::MAX),
            closed: whole.state == ChatState::Closed,
            archived: whole.archived,
        })
    }

    /// Starts keeping new chat `chat`, made at `now`, on `shelf`.
    pub(super) fn keep_new(&mut self, now: Now, chat: ChatId, shelf: Shelf) {
        self.saving
            .kept
            .insert(chat, Kept::new(shelf, now.wall, false));
        self.relist_loaded(chat);
    }
}

/// The session a chat's run continues, as it's saved. Everything else a
/// run holds is that run's: a field added to it fails to build here until
/// it's said which it is.
fn saved_session<A: AppItem>(run: &Run<A>) -> Option<AgentSessionId> {
    let Run {
        // A connection, its processes and requests are one run's.
        conn: _,
        // The chat's folder is saved in its record.
        cwd: _,
        // The host gives a restore its MCP servers again: they hold
        // tokens and sockets that last one run.
        mcp: _,
        mcp_sent: _,
        // So is what the app tells the agent: the host gives it again.
        instructions: _,
        session,
        open: _,
        running: _,
        agent_turn: _,
        quiet_by: _,
        held_until: _,
        dropped_tools: _,
        tools: _,
        served: _,
        import: _,
        replaying: _,
        slow: _,
        held_by: _,
        note: _,
        resent: _,
        restored: _,
        // Where it is with sending its settings is this run's: what's
        // chosen is in its record.
        choosing: _,
        // What the session has cost is the agent's word in this run: a
        // session opened again counts afresh, and a turn's cost reads it
        // that way.
        cost: _,
        // The host gives a restore its tools again, as its MCP servers.
        caf_tools: _,
        // Each session is told once.
        tools_lost: _,
    } = run;
    session.clone()
}

/// The changes that tidy `chat`, last saved at `at`, as a restore at
/// `now` finds it: turns running or stopping are cut off, their
/// unfinished tool calls failed and their items ended, at `at`; messages
/// waiting are stopped; questions go; and it's idle, unless it was
/// archived, which it stays: as of `now` if it was saved closed before
/// CAF kept when.
pub(super) fn tidy<A: AppItem>(chat: &Chat<A>, at: Wall, now: Wall) -> Vec<Change<A>> {
    let mut changes = Vec::new();
    // Never before it started, as a clock set back could have it.
    let end = |start: Option<Wall>| End::At(start.map_or(at, |start| start.max(at)));
    for turn in &chat.turns {
        let life = match turn.life {
            TurnLife::Running | TurnLife::Stopping => TurnLife::CutOff,
            TurnLife::Waiting => TurnLife::Stopped,
            _ => continue,
        };
        for item in turn
            .items
            .iter()
            .filter(|item| item.window.end == End::Open)
        {
            if let ItemKind::Tool(tool) = &item.kind
                && !tool.status.ended()
            {
                let mut tool = tool.clone();
                tool.status = ToolStatus::Failed;
                // A command cut off ended, not saying how.
                tool.settle_output();
                changes.push(Change::ToolChanged {
                    item: item.id,
                    tool,
                });
            }
            changes.push(Change::ItemEnded {
                item: item.id,
                end: end(item.window.start),
            });
        }
        changes.push(Change::TurnLife {
            turn: turn.id,
            life,
            end: end(turn.window.start),
        });
    }
    let mut gone = BTreeSet::new();
    for question in &chat.questions {
        if gone.insert(question.id) {
            changes.push(Change::QuestionGone(question.id));
        }
    }
    if chat.state == ChatState::Closed || chat.archived.is_some() {
        if chat.state != ChatState::Closed {
            changes.push(Change::State(ChatState::Closed));
        }
        if chat.archived.is_none() {
            changes.push(Change::Archived(Some(now)));
        }
    } else if chat.state != ChatState::Idle {
        changes.push(Change::State(ChatState::Idle));
    }
    changes
}

/// Whether every turn and item of `chat` is numbered far enough below
/// 2^53 that what's numbered after it, counting on from past them, is
/// below 2^53 too, as every number CAF gives out is: 2^32 to spare.
fn numbered_within_bounds<A: AppItem>(chat: &Chat<A>) -> bool {
    const MOST: u64 = (1 << 53) - (1 << 32);
    (chat.turns.iter())
        .all(|turn| turn.id.0 < MOST && turn.items.iter().all(|item| item.id.0 < MOST))
}

/// The latest time `chat` holds: when anything in it started or ended.
fn last_change<A: AppItem>(chat: &Chat<A>) -> Option<Wall> {
    let times = |window: &crate::time::Window| {
        let end = match window.end {
            End::At(at) => Some(at),
            _ => None,
        };
        window.start.into_iter().chain(end)
    };
    let turns = (chat.turns.iter()).flat_map(|turn| {
        let items = turn.items.iter().flat_map(|item| times(&item.window));
        times(&turn.window).chain(items)
    });
    let questions = (chat.questions.iter()).flat_map(|question| times(&question.window));
    turns.chain(questions).max()
}

/// The first line of `chat`'s first message from the user, cut to 200
/// characters, if it has one.
// Text is a message's only kind of part, until images come.
#[allow(clippy::unnecessary_find_map)]
fn first_line<A: AppItem>(chat: &Chat<A>) -> Option<String> {
    let text = (chat.turns.iter()).find_map(|turn| match &turn.starter {
        Starter::User(message) => message.parts.iter().find_map(|part| match part {
            Part::Text(text) => Some(text),
        }),
        _ => None,
    })?;
    let line = text.trim_start().lines().next()?.trim_end();
    (!line.is_empty()).then(|| line.chars().take(MOST_FIRST).collect())
}

/// Checks a shelf's name: not empty, at most 256 bytes, and with no
/// control characters.
pub(super) fn check_shelf(shelf: &Shelf) -> Result<(), Refused> {
    let name = &shelf.0;
    if name.is_empty() || name.len() > MOST_SHELF || name.chars().any(char::is_control) {
        return Err(Refused::BadShelf);
    }
    Ok(())
}

/// How long to wait before trying a write again, after `failures` before
/// it in a row: the save timer (at least 1 s), doubling each time, to a
/// minute.
fn retry_after(save: Duration, failures: u32) -> Duration {
    let first = save.max(Duration::from_secs(1));
    (first.saturating_mul(1 << failures.min(6))).min(MOST_RETRY.max(first))
}

/// What a storage report is, in a word, for reporting one that doesn't
/// fit its work.
fn kind<A: AppItem, D: AppData>(event: &StorageEvent<A, D>) -> &'static str {
    match event {
        StorageEvent::Listed { .. } => "a list",
        StorageEvent::Loaded(_) => "a chat",
        StorageEvent::NotLoaded(_) => "a failed load",
        StorageEvent::Done { .. } => "done",
        StorageEvent::Failed(_) => "a failure",
    }
}

/// The ticket a storage directive is answered with, if it's answered.
fn ticket_of<A: AppItem, D: AppData>(directive: &StorageDirective<A, D>) -> Option<Ticket> {
    match directive {
        StorageDirective::List { ticket, .. }
        | StorageDirective::Load { ticket, .. }
        | StorageDirective::Snapshot { ticket, .. }
        | StorageDirective::Append { ticket, .. }
        | StorageDirective::Delete { ticket, .. }
        | StorageDirective::MoveShelf { ticket, .. } => Some(*ticket),
        StorageDirective::Release { .. } => None,
    }
}

/// The shelf a storage directive's work is on: none for a move, which
/// is on two.
fn shelf_of<A: AppItem, D: AppData>(directive: &StorageDirective<A, D>) -> Option<Shelf> {
    match directive {
        StorageDirective::List { shelf, .. }
        | StorageDirective::Load { shelf, .. }
        | StorageDirective::Snapshot { shelf, .. }
        | StorageDirective::Append { shelf, .. }
        | StorageDirective::Delete { shelf, .. }
        | StorageDirective::Release { shelf, .. } => Some(shelf.clone()),
        StorageDirective::MoveShelf { .. } => None,
    }
}

/// SplitMix64's step: spreads `x`'s bits over all 64, so nearby counts
/// give unrelated ids.
fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
