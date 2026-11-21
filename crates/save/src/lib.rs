//! How CAF's chats are saved: the format, a store that keeps them in
//! files, and the storage driver's work, with no async runtime, so any
//! runtime's driver uses it: `caf-driver-tokio` runs it on a blocking
//! thread, and a host on plain threads, such as framewright, on one of its
//! own.
//!
//! The core decides what to save and when
//! ([`StorageDirective`]);
//! [`carry_out`] does it, turning chats into bytes and handing them to a
//! [`Store`], which only keeps bytes. An app that keeps its chats
//! somewhere else, such as eyespy in its SQLite database, writes a
//! [`Store`] and nothing more: the format stays CAF's.
//!
//! # The format
//!
//! A saved chat is three things, each kept by the store as it likes:
//!
//! - **its header** ([`Header`]): what lists show, small, rewritten with
//!   every save;
//! - **its snapshot**: the chat whole ([`Saved`]);
//! - **its journal**: what changed since the snapshot ([`Entry`]), added
//!   to the end, a save at a time.
//!
//! Each is MessagePack in named mode, compressed with zstd (RFC 8878),
//! behind the format's version:
//!
//! ```text
//! header, snapshot:  "CAF" version:u32 zstd-frame
//! journal record:    length:u32 zstd-frame      (repeated)
//! ```
//!
//! Every zstd frame carries the checksum of what it holds, so a record cut
//! off by a crash, or a byte gone wrong, is found. Each record is written
//! whole and synced before the next, so only a journal's last record can
//! be cut off: short of its length, or, on a file system that writes a
//! file's size before its data, as long as it should be but zeros or
//! garbage. So a last record that's short, fails its checksum or doesn't
//! decompress, or a run of zeros at the end, is dropped, as it was never
//! wholly written, and the next append is written over it. Anything else
//! that's wrong is [`CannotRestore::Damaged`].
//!
//! **Saves stay readable forever, and a CAF never changes one it can't
//! read.**
//! - [`VERSION`] goes up with *every* change to what's saved: a field or
//!   an enum's variant added anywhere in a saved type, as well as a change
//!   of shape. So an older CAF meeting a newer save always knows it
//!   ([`CannotRestore::Newer`]) and leaves it untouched, rather than read
//!   it, losing what it doesn't know, and write it back without it.
//! - An addition needs no migration: an older version's save reads as it
//!   is, with defaults for what it didn't have. A change of shape needs
//!   one, from every older version.
//! - Every version has a sample saved by it (`samples/save/`), which must
//!   always read as it did; a test fails if a saved type's shape changes
//!   while [`VERSION`] doesn't.
//! - A journal's records are in the version of the snapshot before them,
//!   so a journal never mixes versions: a CAF that loads a save of an
//!   older version writes a snapshot in its own before it adds anything
//!   to its journal. That's the core's, on
//!   [`StorageEvent::Loaded`]; [`FileStore`] refuses an append to a
//!   snapshot of another version.
//!
//! **App items are kept even when the app can't read them.** One is saved
//! in an [`Unknown`](caf_core::chat::Unknown) item's shape, its envelope:
//! `what` is `App`, `raw` is the item as JSON, and `summary` is its
//! [summary](AppItem::summary). Read back, each one the app's type reads
//! is its item again; one it can't, as when the app changed the type, is
//! an [`Unknown`](caf_core::chat::Unknown) item, which is saved back
//! byte for byte, so a version of the app that reads it gets it back. A
//! program that doesn't know the app's type at all, such as a debugger,
//! reads them all as unknown items, with their summaries. Through JSON,
//! an app item keeps what serde writes with text keys and no raw bytes,
//! as [`AppItem`] asks.
//!
//! **What the app reads, it must keep.** An app item is the app's again
//! only if what its type writes back holds all of what was saved: every
//! key of every map, with what it held; every list, as long, with what it
//! held; and everything else equal. Keys the type adds, as a newer
//! version does, are fine; nothing else may differ. One that fails, as
//! when an older version of the app reads what a newer one added, stays
//! an [`Unknown`](caf_core::chat::Unknown) item, so it's saved back
//! unchanged. The app's data, and its items in tool calls
//! ([`ToolCall::app`](caf_core::chat::ToolCall::app)), which have no
//! envelope, are checked the same way, and one that fails, or can't be
//! read at all, stops the chat restoring ([`CannotRestore::AppData`]),
//! its save left as it was.
//!
//! **Small, losslessly.** Live chats are compressed fast (zstd's level 3:
//! hundreds of MB/s, so never a wait). A chat left alone a while is
//! compressed again once at zstd's strongest, with a long window that
//! finds repeats across the whole chat, such as a file read many times
//! ([`FileStore::squeeze`]).

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use caf_core::directive::StorageDirective;
use caf_core::event::StorageEvent;
use caf_core::save::{CannotRestore, Entry, Header, Saved};
use caf_core::{AppData, AppItem, ChatId, Shelf, Ticket};

mod files;
mod format;

/// What the integration tests share, for the unit tests too.
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

/// The version of the format this CAF writes.
pub const VERSION: u32 = 1;

/// How hard to compress.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Squeeze {
    /// Fast, for live chats: zstd's level 3.
    Fast,
    /// As small as zstd can, slowly: its strongest level, with a long
    /// window. For chats left alone.
    Hardest,
}

/// Writes a chat's header.
///
/// # Panics
///
/// Never: a header always writes.
#[must_use]
pub fn encode_header(header: &Header) -> Vec<u8> {
    format::header(header).expect("a header writes")
}

/// Reads a chat's header.
///
/// # Errors
///
/// [`CannotRestore::Newer`] or [`CannotRestore::Damaged`].
pub fn decode_header(bytes: &[u8]) -> Result<Header, CannotRestore> {
    format::read_header(bytes)
}

/// Writes a chat whole, as its snapshot.
///
/// # Panics
///
/// If an app item can't be written as JSON, or the app's data as
/// MessagePack: an [`AppItem`] with keys that aren't text, say.
/// [`carry_out`] says so as a failed write instead.
#[must_use]
pub fn encode_snapshot<A: AppItem, D: AppData>(saved: &Saved<A, D>, squeeze: Squeeze) -> Vec<u8> {
    format::snapshot(Cow::Borrowed(saved), squeeze).unwrap_or_else(|why| panic!("{why}"))
}

/// Writes one record of a chat's journal: what changed since its last
/// save, in order.
///
/// # Panics
///
/// As [`encode_snapshot`] does.
#[must_use]
pub fn encode_record<A: AppItem, D: AppData>(entries: &[Entry<A, D>]) -> Vec<u8> {
    format::record(Cow::Borrowed(entries)).unwrap_or_else(|why| panic!("{why}"))
}

/// Reads a chat: its snapshot, with its journal applied, as it was saved.
/// A journal whose last record was cut off reads as if it weren't there.
/// An app item the app's type can't read is kept as an
/// [`Unknown`](caf_core::chat::Unknown) item.
///
/// # Errors
///
/// [`CannotRestore::Newer`], [`CannotRestore::Damaged`] (including a
/// change in the journal that doesn't fit the chat), or
/// [`CannotRestore::AppData`].
pub fn decode<A: AppItem, D: AppData>(
    snapshot: &[u8],
    journal: &[u8],
) -> Result<Saved<A, D>, CannotRestore> {
    format::read(snapshot, journal)
}

/// Compresses a snapshot again, as `squeeze` says, keeping what it holds
/// byte for byte.
///
/// # Errors
///
/// [`CannotRestore::Newer`] or [`CannotRestore::Damaged`]: it's left as
/// it is.
pub fn recompress(snapshot: &[u8], squeeze: Squeeze) -> Result<Vec<u8>, CannotRestore> {
    format::squeeze(snapshot, squeeze)
}

/// A chat as a store keeps it, read back.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stored {
    /// Its snapshot.
    pub snapshot: Vec<u8>,
    /// Its journal: every record, in order, as they were added.
    pub journal: Vec<u8>,
}

impl Stored {
    /// A chat kept as this snapshot and journal.
    #[must_use]
    pub fn new(snapshot: Vec<u8>, journal: Vec<u8>) -> Self {
        Self { snapshot, journal }
    }
}

/// Why a store couldn't do something.
#[non_exhaustive]
#[derive(Debug)]
pub enum StoreError {
    /// It has no such chat on that shelf.
    NotFound,
    /// Another program has the chat locked; or, moving a shelf, the shelf
    /// it goes to has one of its chats already.
    InUse,
    /// The system failed.
    Io(io::Error),
    /// What it keeps of the chat is damaged, in its own way of keeping
    /// it, such as a file missing: what's wrong, in its words. A load
    /// that meets it is [`CannotRestore::Damaged`].
    Damaged(String),
    /// A newer CAF kept it, in a way this one can't read, and it's left
    /// as it is: [`CannotRestore::Newer`].
    Newer {
        /// The format's version, as far as the store can tell: at least
        /// the one after this CAF's.
        version: u32,
    },
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl StoreError {
    /// What went wrong, in a line.
    fn describe(&self) -> String {
        match self {
            Self::NotFound => "the store has no such chat".into(),
            Self::InUse => "another program has the chat open".into(),
            Self::Io(error) => error.to_string(),
            Self::Damaged(why) => format!("it's damaged: {why}"),
            Self::Newer { version } => format!("a newer CAF saved it, in version {version}"),
        }
    }
}

/// Somewhere chats are kept, a shelf at a time, as bytes: a chat's header,
/// its snapshot, and its journal. It never reads them: [`carry_out`] does.
///
/// Its side of the contract is the storage driver's
/// ([`StorageDirective`]'s):
/// - **one chat is locked by one program at a time**, from
///   [`load`](Store::load) or its first [`snapshot`](Store::snapshot)
///   until [`release`](Store::release), so a second gets
///   [`StoreError::InUse`];
/// - **a snapshot replaces the old one and empties the journal in one
///   step**, so a crash leaves one or the other, whole;
/// - **an append is added after everything before it**; one cut off by a
///   crash may be left, as the format finds it;
/// - **a header replaces the old one whole**;
/// - **it never removes or changes a chat it wasn't told to.**
///
/// Calls block. It's used from one thread at a time.
pub trait Store: Send + 'static {
    /// The headers of every chat on `shelf`, by chat, in any order. A
    /// shelf with nothing on it, or never used, has none.
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`]. A chat it can't read the header of is listed
    /// with what it has, which [`carry_out`] finds unreadable.
    fn list(&mut self, shelf: &Shelf) -> Result<Vec<(ChatId, Vec<u8>)>, StoreError>;

    /// Locks a chat and reads it.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`], [`StoreError::InUse`],
    /// [`StoreError::Damaged`] or [`StoreError::Newer`] for what it keeps
    /// that it can't read, or [`StoreError::Io`]. A chat it can't read is
    /// left as it was, and not locked.
    fn load(&mut self, shelf: &Shelf, chat: ChatId) -> Result<Stored, StoreError>;

    /// Writes a chat's header and snapshot, emptying its journal, and
    /// locks it if it isn't yet: a new chat's first write is one of these.
    ///
    /// # Errors
    ///
    /// [`StoreError::InUse`] or [`StoreError::Io`].
    fn snapshot(
        &mut self,
        shelf: &Shelf,
        chat: ChatId,
        header: &[u8],
        snapshot: &[u8],
    ) -> Result<(), StoreError>;

    /// Adds a record to a locked chat's journal, and replaces its header.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if it has no snapshot, [`StoreError::InUse`]
    /// if it isn't locked and another has it, [`StoreError::Damaged`] or
    /// [`StoreError::Newer`] if what it keeps can't take the record, or
    /// [`StoreError::Io`]. A failed append adds nothing.
    fn append(
        &mut self,
        shelf: &Shelf,
        chat: ChatId,
        header: &[u8],
        record: &[u8],
    ) -> Result<(), StoreError>;

    /// Unlocks a chat.
    fn release(&mut self, shelf: &Shelf, chat: ChatId);

    /// Removes a chat for good, unlocking it if it's locked.
    ///
    /// # Errors
    ///
    /// [`StoreError::InUse`] if another program has it locked, or
    /// [`StoreError::Io`]. One that isn't there is already removed.
    fn delete(&mut self, shelf: &Shelf, chat: ChatId) -> Result<(), StoreError>;

    /// Moves every chat on `from` to `to`, keeping the locks it holds.
    ///
    /// # Errors
    ///
    /// [`StoreError::InUse`] if `to` has a chat already that `from` has
    /// too, or another program has one of them locked, or
    /// [`StoreError::Io`]. What moved before a failure stays moved.
    fn move_shelf(&mut self, from: &Shelf, to: &Shelf) -> Result<(), StoreError>;
}

/// Does one of the core's directives to a store, and says how it went, if
/// the directive asks for an answer: the storage driver's work, for any
/// runtime. Encoding, compressing and decoding happen here, on the
/// caller's thread, never the core's.
///
/// A chat loaded that can't be read is released at once, and left as it
/// was. A failure to list a shelf is [`StorageEvent::Failed`]. For a
/// write, [`StorageEvent::Done`] says how big the snapshot or record was:
/// the header, rewritten each time, isn't counted.
pub fn carry_out<A: AppItem, D: AppData>(
    store: &mut dyn Store,
    directive: StorageDirective<A, D>,
) -> Option<(Ticket, StorageEvent<A, D>)> {
    let done = |written| StorageEvent::Done { written };
    Some(match directive {
        StorageDirective::List { ticket, shelf } => (ticket, list(store, &shelf)),
        StorageDirective::Load {
            ticket,
            shelf,
            chat,
        } => {
            let event = match store.load(&shelf, chat) {
                Ok(stored) => match decode(&stored.snapshot, &stored.journal) {
                    Ok(saved) => StorageEvent::Loaded(Box::new(saved)),
                    Err(why) => {
                        store.release(&shelf, chat);
                        StorageEvent::NotLoaded(why)
                    }
                },
                Err(StoreError::NotFound) => StorageEvent::NotLoaded(CannotRestore::NotFound),
                Err(StoreError::InUse) => StorageEvent::NotLoaded(CannotRestore::InUse),
                Err(StoreError::Io(error)) => {
                    StorageEvent::NotLoaded(CannotRestore::Store(error.to_string()))
                }
                Err(StoreError::Damaged(why)) => {
                    StorageEvent::NotLoaded(CannotRestore::Damaged(why))
                }
                Err(StoreError::Newer { version }) => {
                    StorageEvent::NotLoaded(CannotRestore::Newer { version })
                }
            };
            (ticket, event)
        }
        StorageDirective::Snapshot {
            ticket,
            shelf,
            saved,
        } => {
            let chat = saved.header.chat;
            let written = format::header(&saved.header).and_then(|header| {
                let snapshot = format::snapshot(Cow::Owned(*saved), Squeeze::Fast)?;
                (store.snapshot(&shelf, chat, &header, &snapshot))
                    .map_err(|error| error.describe())?;
                Ok(snapshot.len() as u64)
            });
            (ticket, written.map_or_else(StorageEvent::Failed, done))
        }
        StorageDirective::Append {
            ticket,
            shelf,
            header,
            entries,
        } => {
            let written = format::header(&header).and_then(|head| {
                let record = format::record(Cow::Owned(entries))?;
                (store.append(&shelf, header.chat, &head, &record))
                    .map_err(|error| error.describe())?;
                Ok(record.len() as u64)
            });
            (ticket, written.map_or_else(StorageEvent::Failed, done))
        }
        StorageDirective::Release { shelf, chat } => {
            store.release(&shelf, chat);
            return None;
        }
        StorageDirective::Delete {
            ticket,
            shelf,
            chat,
        } => (ticket, answer(store.delete(&shelf, chat))),
        StorageDirective::MoveShelf { ticket, from, to } => {
            let event = match store.move_shelf(&from, &to) {
                Err(StoreError::InUse) => StorageEvent::Failed(
                    "a chat on it is open in another program, or on the shelf it goes to already"
                        .into(),
                ),
                done => answer(done),
            };
            (ticket, event)
        }
        // None the core gives yet.
        _ => return None,
    })
}

/// A shelf's chats, read: in the order they were made, and those that
/// can't be, in the order of their ids.
fn list<A: AppItem, D: AppData>(store: &mut dyn Store, shelf: &Shelf) -> StorageEvent<A, D> {
    let heads = match store.list(shelf) {
        Ok(heads) => heads,
        Err(error) => return StorageEvent::Failed(error.describe()),
    };
    let mut chats = Vec::new();
    let mut unreadable = Vec::new();
    for (chat, head) in heads {
        if head.is_empty() {
            let why = CannotRestore::Damaged("its header is missing".into());
            unreadable.push((chat, why));
            continue;
        }
        match decode_header(&head) {
            Ok(header) if header.chat == chat => chats.push(header),
            Ok(_) => unreadable.push((
                chat,
                CannotRestore::Damaged("its header is another chat's".into()),
            )),
            Err(why) => unreadable.push((chat, why)),
        }
    }
    chats.sort_by_key(|header| (header.made, header.chat));
    unreadable.sort_by_key(|(chat, _)| *chat);
    StorageEvent::Listed { chats, unreadable }
}

/// What a deletion or move says.
fn answer<A: AppItem, D: AppData>(done: Result<(), StoreError>) -> StorageEvent<A, D> {
    match done {
        Ok(()) => StorageEvent::Done { written: 0 },
        Err(error) => StorageEvent::Failed(error.describe()),
    }
}

/// CAF's own store: each shelf a folder, and each chat three files in it.
///
/// ```text
/// <root>/<shelf>/<chat>.head      the header
/// <root>/<shelf>/<chat>.caf       the snapshot
/// <root>/<shelf>/<chat>.journal   the journal
/// <root>/<shelf>/<chat>.lock      held while a program has it loaded
/// ```
///
/// A shelf's folder is its name with anything but small letters, digits,
/// `-` and `_` written as `%` and two hex digits, capitals included, so
/// any name is one folder, and different names are different folders,
/// even where the file system ignores case. A name that would make a
/// folder name longer than 200 bytes, such as a long path, is cut to its
/// first 136 bytes so written, then `~` and the SHA-256 of the whole name
/// in hex: still one folder per name. Folders and files are readable only
/// by the user.
///
/// Snapshots and headers are written to a new file and renamed into
/// place, then synced with their folder; appends are synced before they
/// count as done. Locks are `flock`s, so a program that dies lets go.
///
/// A snapshot and its journal each start with the snapshot's generation,
/// a number the store counts up: a journal counts only with the snapshot
/// of its generation, so a new snapshot empties the journal the moment
/// it's in place, and a crash between the two files leaves one or the
/// other whole. A new chat's journal is written before its snapshot, so
/// a snapshot without one is damaged ([`StoreError::Damaged`]), never
/// read as having nothing to add.
///
/// Loading changes nothing, so a save this CAF can't read, such as a
/// newer version's, is left as it was. An append goes after the records
/// that count, as the format reads them, writing over a last one a crash
/// cut off; it takes the chat's lock if it isn't held, and refuses a
/// snapshot of another version than this CAF's.
///
/// A chat a crash left with a snapshot and no header is listed with the
/// header its snapshot holds, which may be behind its journal; one whose
/// snapshot can't be read is listed with an empty header, which reads as
/// damaged, so it can be seen and deleted.
/// Deleting removes the snapshot first and the header last, so a chat
/// half deleted is still listed.
///
/// Each shelf's folder has a lock of its own, `.shelf.lock`: shared while
/// a chat's lock is taken, and a new chat's first files made, exclusive
/// while the shelf moves, so no chat comes to it, or starts to, unseen.
/// The store checks a chat's lock it holds is still the chat's before it
/// writes. A snapshot it marked in a way this CAF doesn't know is
/// [`StoreError::Newer`].
///
/// Moving a shelf to a new one renames its folder. Moving it to one in
/// use moves a chat at a time, linking each file where it goes before
/// removing it where it was, so a crash leaves each chat whole in one
/// place or both, and moving again finishes. Moving a shelf with a chat
/// another program has open, or onto a shelf that has one of its chats
/// already, is [`StoreError::InUse`].
///
/// It never follows a link inside its root, nor opens anything but a file
/// as one: a shelf's folder or a chat's file that's a link is an error,
/// and isn't listed. [`squeeze`](FileStore::squeeze) also removes what
/// crashes left that nobody uses.
#[derive(Debug)]
pub struct FileStore {
    root: PathBuf,
    /// The locks it holds, by chat.
    held: files::Held,
}

impl FileStore {
    /// A store keeping its shelves in `root`, made if it isn't there.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            held: files::Held::new(),
        }
    }

    /// Where an app keeps its chats by default:
    /// `$XDG_DATA_HOME/<app>/chats`, so by default
    /// `~/.local/share/<app>/chats`. `None` if there's no home.
    #[must_use]
    pub fn default_root(app: &str) -> Option<PathBuf> {
        // XDG says a relative one is ignored.
        let data = (std::env::var_os("XDG_DATA_HOME").map(PathBuf::from))
            .filter(|data| data.is_absolute())
            .or_else(|| {
                let home = PathBuf::from(std::env::var_os("HOME")?);
                home.is_absolute().then(|| home.join(".local/share"))
            })?;
        Some(data.join(app).join("chats"))
    }

    /// The folder it keeps its shelves in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Compresses again, at zstd's strongest, every snapshot no program
    /// has changed for `after`, such as a week, and that isn't locked. Each
    /// is replaced in one step, holding its lock, so a program loading it
    /// meanwhile waits or finds it whole. Slow: run it on a thread of its
    /// own, now and then. Returns how many bytes it saved.
    ///
    /// # Errors
    ///
    /// The first failure that stops it: one chat it can't squeeze is
    /// skipped and left as it was.
    pub fn squeeze(&mut self, after: Duration) -> io::Result<u64> {
        files::squeeze(&self.root, after)
    }
}

impl Store for FileStore {
    fn list(&mut self, shelf: &Shelf) -> Result<Vec<(ChatId, Vec<u8>)>, StoreError> {
        files::list(&self.root, shelf)
    }

    fn load(&mut self, shelf: &Shelf, chat: ChatId) -> Result<Stored, StoreError> {
        files::load(&self.root, &mut self.held, shelf, chat)
    }

    fn snapshot(
        &mut self,
        shelf: &Shelf,
        chat: ChatId,
        header: &[u8],
        snapshot: &[u8],
    ) -> Result<(), StoreError> {
        files::snapshot(&self.root, &mut self.held, shelf, chat, header, snapshot)
    }

    fn append(
        &mut self,
        shelf: &Shelf,
        chat: ChatId,
        header: &[u8],
        record: &[u8],
    ) -> Result<(), StoreError> {
        files::append(&self.root, &mut self.held, shelf, chat, header, record)
    }

    fn release(&mut self, shelf: &Shelf, chat: ChatId) {
        self.held.remove(&(shelf.clone(), chat));
    }

    fn delete(&mut self, shelf: &Shelf, chat: ChatId) -> Result<(), StoreError> {
        files::delete(&self.root, &mut self.held, shelf, chat)
    }

    fn move_shelf(&mut self, from: &Shelf, to: &Shelf) -> Result<(), StoreError> {
        files::move_shelf(&self.root, &mut self.held, from, to)
    }
}
