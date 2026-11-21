//! Saving chats, and restoring them: what the core keeps of the store's
//! work, and its rules for when to save.
//!
//! Each saved chat that's loaded keeps its own list of what changed since
//! it was last written ([`Kept::unsaved`]), fed by [`Core::record`], the
//! one way a chat's record changes. It's written [`Timers::save`] after it
//! first changes, at once when a turn ends, and whole when it's put away
//! or CAF shuts down. One write of a chat is under way at a time, so a
//! failed one is never followed by one that counts on it: the next is a
//! snapshot, tried again later.
//!
//! [`Timers::save`]: crate::host::Timers::save

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::Serialize;

use super::record::Log;
use super::sessions::mcp_server;
use super::state::{Open, Run};
use super::{Core, Refused, report};
use crate::change::Change;
use crate::chat::{
    AppData, AppItem, Chat, ChatState, ItemKind, Part, Starter, ToolStatus, TurnLife,
};
use crate::directive::{Directive, Level, StorageDirective};
use crate::event::StorageEvent;
use crate::ids::{AgentSessionId, ChatId, Shelf, Ticket};
use crate::save::{CannotRestore, Entry, Header, RestoreChat, Saved, SavedChat, SavedState};
use crate::time::{End, Mono, Now, Wall};

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
    /// Chats deleted while being restored, with their shelves: the lock
    /// their load takes, if it comes, is let go.
    pub(super) deleted: BTreeMap<ChatId, Shelf>,
}

impl<A: AppItem, D: AppData> Default for Saving<A, D> {
    fn default() -> Self {
        Self {
            made: 0,
            kept: BTreeMap::new(),
            shelves: BTreeMap::new(),
            restoring: BTreeMap::new(),
            jobs: BTreeMap::new(),
            away: BTreeMap::new(),
            deleted: BTreeMap::new(),
        }
    }
}

impl<A: AppItem, D: AppData> Saving<A, D> {
    /// Whether every chat that's to be written whole and let go has been:
    /// nothing put away or shutting down waits on the store.
    pub(super) fn settled(&self) -> bool {
        self.away.is_empty() && self.kept.values().all(|kept| !kept.releasing)
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
    /// About how big its last snapshot was, in bytes, as the store said:
    /// 0 until this run writes one.
    pub(super) snapshot_size: u64,
    /// When to write what's changed, if anything has.
    pub(super) due: Option<Mono>,
    /// Writes under way: one at most.
    pub(super) writing: BTreeSet<Ticket>,
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
            writing: BTreeSet::new(),
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
    /// entry grew are added to that entry.
    pub(super) fn add(&mut self, entry: Entry<A, D>) {
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
        } else {
            self.unsaved.push(entry);
        }
        self.touched = true;
    }
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
    Write(ChatId),
    /// Writing a loaded chat whole.
    Snapshot(ChatId),
    /// Writing a chat put away whole, to let it go once it's written.
    Away(ChatId),
    /// Deleting a chat.
    Delete(ChatId),
    /// Moving a shelf.
    Move {
        /// From.
        from: Shelf,
        /// To.
        to: Shelf,
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
        let first = mix(self.host.seed ^ mix(self.saving.made));
        let second = mix(first);
        let mut random = [0; 10];
        random[..8].copy_from_slice(&first.to_le_bytes());
        random[8..].copy_from_slice(&second.to_le_bytes()[..2]);
        // Before 1970 can't be: such a clock is wrong, and 0 still sorts.
        let millis = u64::try_from(now.wall.0).unwrap_or(0);
        ChatId(uuid::Builder::from_unix_timestamp_millis(millis, &random).into_uuid())
    }

    /// Takes in the storage driver's report.
    pub(super) fn storage_event(
        &mut self,
        now: Now,
        ticket: Ticket,
        event: StorageEvent<A, D>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if let Some(restore) = self.saving.restoring.remove(&ticket) {
            return match event {
                StorageEvent::Loaded(saved) => self.restored(restore, *saved, out),
                StorageEvent::NotLoaded(why) => {
                    report(
                        out,
                        Level::Info,
                        format!("chat {} couldn't be restored: {why:?}", restore.chat),
                    );
                    self.relist_state(&restore.shelf, restore.chat, SavedState::Failed(why));
                }
                other => report(
                    out,
                    Level::Warn,
                    format!("the store answered a load with {}", kind(&other)),
                ),
            };
        }
        let Some(job) = self.saving.jobs.remove(&ticket) else {
            // A load nobody waits for any more, as its chat was deleted
            // meanwhile: the lock it took is let go.
            if let StorageEvent::Loaded(saved) = event {
                let chat = saved.chat.id;
                if let Some(shelf) = self.saving.deleted.remove(&chat) {
                    out.push(Directive::Storage(StorageDirective::Release {
                        shelf,
                        chat,
                    }));
                }
                return;
            }
            return report(
                out,
                Level::Debug,
                format!("ignored the store's answer to ticket {}", ticket.0),
            );
        };
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
            (Job::Write(chat), StorageEvent::Done { written }) => {
                if let Some(kept) = self.saving.kept.get_mut(&chat) {
                    kept.writing.remove(&ticket);
                    kept.journal = kept.journal.saturating_add(written);
                    kept.failures = 0;
                }
                self.release_if_done(now, chat, out);
            }
            (Job::Snapshot(chat), StorageEvent::Done { written }) => {
                if let Some(kept) = self.saving.kept.get_mut(&chat) {
                    kept.writing.remove(&ticket);
                    kept.snapshot_size = written;
                    kept.journal = 0;
                    kept.failures = 0;
                }
                self.release_if_done(now, chat, out);
            }
            (Job::Write(chat) | Job::Snapshot(chat), StorageEvent::Failed(why)) => {
                let Some(kept) = self.saving.kept.get_mut(&chat) else {
                    return;
                };
                kept.writing.remove(&ticket);
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
            (Job::Away(chat), StorageEvent::Done { .. }) => {
                if let Some(away) = self.saving.away.remove(&chat) {
                    let shelf = away.shelf;
                    out.push(Directive::Storage(StorageDirective::Release {
                        shelf,
                        chat,
                    }));
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
            (Job::Delete(_), StorageEvent::Done { .. }) => {}
            (Job::Move { to, .. }, StorageEvent::Done { .. }) => {
                // What the shelf they went to holds now is the store's:
                // it's read again, if it's listed.
                if self.saving.shelves.contains_key(&to) {
                    self.list_again(to, out);
                }
            }
            (Job::Delete(chat), StorageEvent::Failed(why)) => report(
                out,
                Level::Warn,
                format!("chat {chat} couldn't be deleted: {why}"),
            ),
            (Job::Move { from, to }, StorageEvent::Failed(why)) => {
                report(
                    out,
                    Level::Warn,
                    format!("shelf {:?} couldn't be moved to {:?}: {why}", from.0, to.0),
                );
                for shelf in [from, to] {
                    if self.saving.shelves.contains_key(&shelf) {
                        self.list_again(shelf, out);
                    }
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
        shelf: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        check_shelf(&shelf)?;
        self.list_again(shelf, out);
        Ok(())
    }

    /// Asks the store for a shelf's chats.
    fn list_again(&mut self, shelf: Shelf, out: &mut Vec<Directive<A, D>>) {
        let ticket = Ticket(self.ids.take());
        self.saving.jobs.insert(ticket, Job::List(shelf.clone()));
        out.push(Directive::Storage(StorageDirective::List { ticket, shelf }));
    }

    /// The store listed `shelf`: its chats, each loaded one as it is now,
    /// with those loaded that it doesn't have yet.
    fn listed(&mut self, shelf: Shelf, headers: Vec<Header>) {
        let mut list: Vec<SavedChat> = (headers.into_iter())
            .map(|header| {
                let state = self.saved_state(&shelf, header.chat);
                let header = match state {
                    SavedState::Loaded => self.header(header.chat).unwrap_or(header),
                    _ => header,
                };
                SavedChat {
                    shelf: shelf.clone(),
                    header,
                    state,
                }
            })
            .collect();
        let unlisted: Vec<ChatId> = (self.saving.kept.iter())
            .filter(|(chat, kept)| {
                kept.shelf == shelf && !list.iter().any(|each| each.header.chat == **chat)
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
        restore: RestoreChat,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        check_shelf(&restore.shelf)?;
        (restore.mcp_servers.iter())
            .map(mcp_server)
            .collect::<Option<Vec<_>>>()
            .ok_or(Refused::Unsendable)?;
        if self.is_loaded(restore.chat) {
            return Err(Refused::Loaded);
        }
        self.saving.deleted.remove(&restore.chat);
        let ticket = Ticket(self.ids.take());
        out.push(Directive::Storage(StorageDirective::Load {
            ticket,
            shelf: restore.shelf.clone(),
            chat: restore.chat,
        }));
        self.relist_state(&restore.shelf, restore.chat, SavedState::Restoring);
        self.saving.restoring.insert(ticket, restore);
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
    /// with no agent started until a message is sent.
    fn restored(
        &mut self,
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
        if whole.id != id || header.chat != id {
            let why = CannotRestore::Damaged(format!("it holds chat {} instead", whole.id));
            report(
                out,
                Level::Warn,
                format!("chat {id} couldn't be restored: {why:?}"),
            );
            self.relist_state(&restore.shelf, id, SavedState::Failed(why));
            let shelf = restore.shelf;
            out.push(Directive::Storage(StorageDirective::Release {
                shelf,
                chat: id,
            }));
            return;
        }
        // Renamed by the host: found again by its built-in profile, if
        // exactly one agent has it.
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
        let changes = tidy(&whole, header.changed);
        let mcp_sent = (restore.mcp_servers.iter())
            .map(mcp_server)
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        let conn = self.conn_for(id, &whole.agent);
        let cwd = whole.cwd.to_string_lossy().into_owned();
        let mut kept = Kept::new(restore.shelf, header.made, true);
        kept.changed = header.changed;
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
                session,
                open: Open::Idle,
                running: None,
                agent_turn: None,
                quiet_by: None,
                held_until: None,
                dropped_tools: BTreeSet::new(),
                tools: BTreeMap::new(),
                import: None,
                replaying: false,
                slow: None,
                held_by: None,
                note: None,
                resent: None,
                restored: true,
            },
        );
        // Tidied as changes, so the store gets them as journal entries,
        // and viewers as changes like any other.
        for change in changes {
            self.record(id, change);
        }
        self.relist_loaded(id);
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
        self.set_state(chat, ChatState::Idle);
        self.put_away_now(now, chat, out);
        if let Some(conn) = conn {
            self.advance(now, conn, out);
        }
        Ok(())
    }

    /// Writes saved `chat`, wound down already, whole, and lets it go from
    /// memory. It's let go in the store once it's written.
    pub(super) fn put_away_now(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        self.note_session(chat);
        self.note_changed(now, chat);
        let saved = self.saved_whole(chat);
        let Some(kept) = self.forget(chat) else {
            return;
        };
        let Some(saved) = saved else {
            return;
        };
        let shelf = kept.shelf;
        let ticket = Ticket(self.ids.take());
        self.saving.jobs.insert(ticket, Job::Away(chat));
        out.push(Directive::Storage(StorageDirective::Snapshot {
            ticket,
            shelf: shelf.clone(),
            saved: Box::new(saved.clone()),
        }));
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
                writing: Some(ticket),
                retry: None,
                failures: 0,
            },
        );
    }

    /// Lets `chat` go from memory, with everything kept for it, and
    /// returns what was kept of its saving.
    fn forget(&mut self, chat: ChatId) -> Option<Kept<A, D>> {
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

    /// Writes a chat put away again, after a write of it failed.
    pub(super) fn write_away(&mut self, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let ticket = Ticket(self.ids.take());
        let Some(away) = self.saving.away.get_mut(&chat) else {
            return;
        };
        away.retry = None;
        away.writing = Some(ticket);
        self.saving.jobs.insert(ticket, Job::Away(chat));
        out.push(Directive::Storage(StorageDirective::Snapshot {
            ticket,
            shelf: away.shelf.clone(),
            saved: away.saved.clone(),
        }));
    }

    /// Deletes a chat.
    pub(super) fn delete_chat(
        &mut self,
        now: Now,
        chat: ChatId,
        shelf: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        check_shelf(&shelf)?;
        if self.chats.contains_key(&chat) {
            if (self.saving.kept.get(&chat)).is_none_or(|kept| kept.shelf != shelf) {
                return Err(Refused::NoSuchChat);
            }
            self.close_session(now, chat, out);
            self.forget(chat);
        }
        if (self.saving.away.get(&chat)).is_some_and(|away| away.shelf == shelf) {
            self.saving.away.remove(&chat);
        }
        let restoring = self.saving.restoring.len();
        (self.saving.restoring).retain(|_, each| !(each.chat == chat && each.shelf == shelf));
        if self.saving.restoring.len() != restoring {
            self.saving.deleted.insert(chat, shelf.clone());
        }
        self.relist(&shelf, chat, None);
        let ticket = Ticket(self.ids.take());
        self.saving.jobs.insert(ticket, Job::Delete(chat));
        out.push(Directive::Storage(StorageDirective::Delete {
            ticket,
            shelf,
            chat,
        }));
        Ok(())
    }

    /// Moves a shelf.
    pub(super) fn move_chats(
        &mut self,
        from: Shelf,
        to: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        check_shelf(&from)?;
        check_shelf(&to)?;
        if from == to {
            return Ok(());
        }
        for kept in self.saving.kept.values_mut() {
            if kept.shelf == from {
                kept.shelf = to.clone();
            }
        }
        for away in self.saving.away.values_mut() {
            if away.shelf == from {
                away.shelf = to.clone();
            }
        }
        for restore in self.saving.restoring.values_mut() {
            if restore.shelf == from {
                restore.shelf = to.clone();
            }
        }
        for shelf in self.saving.deleted.values_mut() {
            if *shelf == from {
                *shelf = to.clone();
            }
        }
        // What's listed of the one goes to the other, if that's listed
        // too: it's read again once the move is done.
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
        let loaded: Vec<ChatId> = (self.saving.kept.iter())
            .filter(|(_, kept)| kept.shelf == to)
            .map(|(&chat, _)| chat)
            .collect();
        for chat in loaded {
            self.relist_loaded(chat);
        }
        let ticket = Ticket(self.ids.take());
        let job = Job::Move {
            from: from.clone(),
            to: to.clone(),
        };
        self.saving.jobs.insert(ticket, job);
        out.push(Directive::Storage(StorageDirective::MoveShelf {
            ticket,
            from,
            to,
        }));
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

    /// Saves every saved chat whole and lets it go in the store, as CAF
    /// shuts down. It stays loaded: if it changes again, it's written
    /// whole, which takes it again.
    pub(super) fn save_all(&mut self, now: Now) {
        for kept in self.saving.kept.values_mut() {
            kept.releasing = true;
            kept.due = Some(now.mono);
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
        let chats: Vec<ChatId> = self.saving.kept.keys().copied().collect();
        for chat in chats {
            self.note_session(chat);
            self.note_changed(now, chat);
            if self.save_at(chat).is_some_and(|at| at <= now.mono) {
                self.write(chat, out);
            }
        }
        let away: Vec<ChatId> = (self.saving.away.iter())
            .filter(|(_, away)| {
                away.writing.is_none() && away.retry.is_none_or(|at| at <= now.mono)
            })
            .map(|(&chat, _)| chat)
            .collect();
        for chat in away {
            self.write_away(chat, out);
        }
    }

    /// When `chat` is next written, if it's waiting to be: never while a
    /// write of it is under way.
    pub(super) fn save_at(&self, chat: ChatId) -> Option<Mono> {
        let kept = self.saving.kept.get(&chat)?;
        if !kept.writing.is_empty() {
            return None;
        }
        let due = kept.due?;
        Some(kept.retry.map_or(due, |retry| retry.max(due)))
    }

    /// When a chat put away is next written, after a write of it failed.
    pub(super) fn away_at(&self, chat: ChatId) -> Option<Mono> {
        let away = self.saving.away.get(&chat)?;
        away.writing.is_none().then_some(away.retry?)
    }

    /// Writes what's changed of `chat`: whole if it must be or its journal
    /// has outgrown its snapshot, and otherwise added to its journal.
    /// Nothing, if nothing changed.
    pub(super) fn write(&mut self, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let Some(kept) = self.saving.kept.get_mut(&chat) else {
            return;
        };
        kept.due = None;
        kept.retry = None;
        if kept.unsaved.is_empty() && !kept.behind && !kept.releasing {
            return;
        }
        let whole = kept.whole || kept.releasing || kept.journal > kept.snapshot_size;
        let ticket = Ticket(self.ids.take());
        let (saved, header) = if whole {
            (self.saved_whole(chat), None)
        } else {
            (None, self.header(chat))
        };
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
                (append, Job::Write(chat))
            }
            (None, None) => return,
        };
        kept.behind = false;
        kept.writing.insert(ticket);
        self.saving.jobs.insert(ticket, job);
        out.push(Directive::Storage(directive));
        self.relist_loaded(chat);
    }

    /// A write of `chat` is done: if CAF is shutting down and it's written
    /// whole, it's let go in the store; if not yet, it's written again.
    fn release_if_done(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let Some(kept) = self.saving.kept.get_mut(&chat) else {
            return;
        };
        if !kept.releasing || !kept.writing.is_empty() {
            return;
        }
        if !kept.unsaved.is_empty() || kept.behind || kept.whole {
            kept.due = Some(now.mono);
            return;
        }
        kept.releasing = false;
        kept.whole = true;
        out.push(Directive::Storage(StorageDirective::Release {
            shelf: kept.shelf.clone(),
            chat,
        }));
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
        })
    }

    /// Starts keeping new chat `chat`, made at `now`, on `shelf`.
    pub(super) fn keep_new(&mut self, now: Now, chat: ChatId, shelf: Shelf) {
        (self.saving.kept).insert(chat, Kept::new(shelf, now.wall, false));
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
        session,
        open: _,
        running: _,
        agent_turn: _,
        quiet_by: _,
        held_until: _,
        dropped_tools: _,
        tools: _,
        import: _,
        replaying: _,
        slow: _,
        held_by: _,
        note: _,
        resent: _,
        restored: _,
    } = run;
    session.clone()
}

/// The changes that tidy `chat`, last saved at `at`, as a restore finds
/// it: turns running or stopping are cut off, their unfinished tool calls
/// failed and their items ended, at `at`; messages waiting are stopped;
/// questions go; and it's idle.
pub(super) fn tidy<A: AppItem>(chat: &Chat<A>, at: Wall) -> Vec<Change<A>> {
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
                && matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)
            {
                let mut tool = tool.clone();
                tool.status = ToolStatus::Failed;
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
    if chat.state != ChatState::Idle {
        changes.push(Change::State(ChatState::Idle));
    }
    changes
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

/// SplitMix64's step: spreads `x`'s bits over all 64, so nearby counts
/// give unrelated ids.
fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
