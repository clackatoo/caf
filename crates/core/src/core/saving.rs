//! Saving chats, and restoring them: what the core keeps of the store's
//! work, and its rules for when to save.

// A draft: the bodies come with the build.
#![allow(unused_variables, dead_code, clippy::ptr_arg)]

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::chat::{AppData, AppItem};
use crate::directive::Directive;
use crate::event::StorageEvent;
use crate::ids::{ChatId, Shelf, Ticket};
use crate::save::{Entry, RestoreChat, SavedChat};
use crate::time::{Mono, Now};

use super::{Core, Refused};

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
}

impl<A: AppItem, D: AppData> Default for Saving<A, D> {
    fn default() -> Self {
        Self {
            made: 0,
            kept: BTreeMap::new(),
            shelves: BTreeMap::new(),
            restoring: BTreeMap::new(),
            jobs: BTreeMap::new(),
        }
    }
}

/// A loaded chat that's saved.
#[derive(Debug, Serialize)]
#[serde(bound = "A: AppItem, D: AppData")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Kept<A: AppItem, D: AppData> {
    /// Its shelf.
    pub(super) shelf: Shelf,
    /// What's changed since it was last written, squashed as it's added.
    /// Its own list, not the viewers' bounded one, so nothing is lost
    /// however much changes at once; past the size of its last snapshot,
    /// it's written whole instead.
    pub(super) unsaved: Vec<Entry<A, D>>,
    /// About how big its last snapshot was, in bytes, as the store said.
    pub(super) snapshot_size: u64,
    /// When to write what's changed, if anything has.
    pub(super) due: Option<Mono>,
    /// Writes under way.
    pub(super) writing: BTreeSet<Ticket>,
    /// The last write failed: when to try again.
    pub(super) retry: Option<Mono>,
}

/// What a piece of the store's work is for.
#[derive(Debug, Serialize)]
#[serde(tag = "type", content = "value")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) enum Job {
    /// Listing a shelf.
    List(Shelf),
    /// Writing a chat.
    Write(ChatId),
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
        todo!()
    }

    /// Lists a shelf.
    pub(super) fn list_shelf(
        &mut self,
        shelf: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        todo!()
    }

    /// Starts a restore.
    pub(super) fn start_restore(
        &mut self,
        restore: RestoreChat,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        todo!()
    }

    /// Puts a chat away.
    pub(super) fn put_chat_away(
        &mut self,
        now: Now,
        chat: ChatId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        todo!()
    }

    /// Deletes a chat.
    pub(super) fn delete_chat(
        &mut self,
        now: Now,
        chat: ChatId,
        shelf: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        todo!()
    }

    /// Moves a shelf.
    pub(super) fn move_chats(
        &mut self,
        from: Shelf,
        to: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        todo!()
    }

    /// Sets a chat's data slot.
    pub(super) fn set_chat_data(&mut self, now: Now, chat: ChatId, data: D) -> Result<(), Refused> {
        todo!()
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
