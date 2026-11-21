//! [`FileStore`]'s work: files, folders, locks.
//!
//! # A snapshot and its journal
//!
//! A snapshot replaces the old one and empties the journal in one step,
//! though they're two files, by a number the store keeps in front of
//! each: the snapshot's generation. A journal counts only if it has its
//! snapshot's; one with an older one is from before it, and reads as
//! empty.
//!
//! ```text
//! <chat>.caf      generation:u64 flags:u8 snapshot
//! <chat>.journal  generation:u64 record...
//! ```
//!
//! A snapshot is written with the next generation, to a new file renamed
//! into place, and that's synced with its folder before the journal is
//! replaced with an empty one of its generation, so a crash leaves the old
//! snapshot with its journal, or the new one with none. A new chat's
//! journal is written first, so a snapshot never lacks one: one that does
//! is damaged, never read as having nothing to add. Squeezing a snapshot
//! keeps its generation, as it keeps what it holds, and marks it squeezed
//! (flag 1), so it's done once.
//!
//! Loading changes nothing, so a save this CAF can't read is left as it
//! was. A journal's last record that a crash cut off is written over by
//! the next append, which goes after the records that count, as the
//! format reads them.
//!
//! [`FileStore`]: crate::FileStore

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::{DirBuilder, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::thread;
use std::time::{Duration, SystemTime};

use caf_core::save::CannotRestore;
use caf_core::{ChatId, Shelf};
use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags, Stat, Timestamps};
use rustix::io::Errno;
use sha2::{Digest, Sha256};

use crate::{StoreError, Stored, VERSION, format};

/// What a snapshot's file has in front of the snapshot.
const CAF_PREFIX: usize = 9;
/// What a journal's file has in front of its records.
const JOURNAL_PREFIX: usize = 8;
/// The flag of a snapshot squeezed already.
const SQUEEZED: u8 = 1;
/// The longest a shelf's folder name is written in full.
const LONGEST: usize = 200;
/// How much of a longer one is kept, before its hash.
const KEPT: usize = 136;
/// How many times to try a lock another holds before it's in use: long
/// enough for a squeeze to replace a snapshot.
const TRIES: u32 = 20;
/// How long to wait between tries.
const PAUSE: Duration = Duration::from_millis(10);
/// How many times a lock or folder is looked for again when it was
/// replaced as it was taken.
const AGAIN: u32 = 3;
/// How old a squeeze's new file is when it's one a crash left.
const STALE: Duration = Duration::from_secs(3600);
/// The files a chat has.
const EXTENSIONS: [&str; 4] = ["head", "caf", "journal", "lock"];
/// A shelf's lock: shared while a chat's lock is taken, and a new chat's
/// first files made; exclusive while the shelf moves, so no chat comes to
/// it unseen.
const SHELF_LOCK: &str = ".shelf.lock";
/// Moving a chat to a shelf in use: its files linked there, in this order,
/// then removed where they were, in this.
pub(crate) const MOVE_LINKS: [&str; 4] = ["lock", "caf", "journal", "head"];
pub(crate) const MOVE_REMOVES: [&str; 4] = ["caf", "journal", "head", "lock"];
/// Deleting a chat: its files removed in this order, any new files a
/// crash left before the header.
pub(crate) const DELETES: [&str; 4] = ["caf", "journal", "head", "lock"];

/// The locks a store holds, by chat.
pub(crate) type Held = HashMap<(Shelf, ChatId), Hold>;

/// A chat's lock, held.
#[derive(Debug)]
pub(crate) struct Hold {
    /// Its lock file, `flock`ed.
    lock: File,
    /// Where its journal's records that count end, once known: where the
    /// next append goes.
    end: Option<u64>,
}

/// The folder a shelf is kept in, inside the root.
pub(crate) fn folder(shelf: &Shelf) -> io::Result<String> {
    if shelf.0.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a shelf needs a name",
        ));
    }
    let mut name = String::with_capacity(shelf.0.len());
    for byte in shelf.0.bytes() {
        // Not capitals: a file system that ignores case would make two
        // shelves one.
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_' {
            name.push(char::from(byte));
        } else {
            name.push_str(&format!("%{byte:02X}"));
        }
    }
    if name.len() > LONGEST {
        name.truncate(KEPT);
        name.push('~');
        for byte in Sha256::digest(shelf.0.as_bytes()) {
            name.push_str(&format!("{byte:02x}"));
        }
    }
    Ok(name)
}

/// A chat's file.
fn file(chat: ChatId, extension: &str) -> String {
    format!("{chat}.{extension}")
}

/// The new file a chat's file is written to before it's renamed into
/// place, by its holder.
fn new_file(chat: ChatId, extension: &str) -> String {
    format!("{chat}.{extension}.new")
}

/// The chat a file belongs to, and which of its files it is, if it's one
/// of a chat's.
fn chat_of(name: &str) -> Option<(ChatId, &str)> {
    let (stem, extension) = name.split_once('.')?;
    let id = uuid::Uuid::try_parse(stem).ok()?;
    // Only the name CAF writes: another spelling of the same id isn't it.
    (id.hyphenated().to_string() == stem).then_some((ChatId(id), extension))
}

/// The root: `None` if it isn't there.
fn root(path: &Path) -> io::Result<Option<OwnedFd>> {
    match rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(root) => Ok(Some(root)),
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The root, made if it isn't there, with its parent synced.
fn make_root(path: &Path) -> io::Result<OwnedFd> {
    if let Some(root) = root(path)? {
        return Ok(root);
    }
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let parent = rustix::fs::open(
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        sync(parent)?;
    }
    root(path)?.ok_or_else(|| io::ErrorKind::NotFound.into())
}

/// A folder in `parent`, never through a link: `None` if it isn't there,
/// unless `make`, when it's made.
fn subfolder(parent: impl AsFd, name: &str, make: bool) -> io::Result<Option<OwnedFd>> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match rustix::fs::openat(&parent, name, flags, Mode::empty()) {
        Ok(folder) => Ok(Some(folder)),
        Err(Errno::NOENT) if make => {
            match rustix::fs::mkdirat(&parent, name, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            sync(&parent)?;
            Ok(Some(rustix::fs::openat(
                &parent,
                name,
                flags,
                Mode::empty(),
            )?))
        }
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Whether two looks at a file are at the same file.
fn same(one: &Stat, other: &Stat) -> bool {
    (one.st_dev, one.st_ino) == (other.st_dev, other.st_ino)
}

/// Whether `folder` is still the one named `name` in `root`: another
/// program may have moved its shelf since it was opened.
fn still_there(root: impl AsFd, name: &str, folder: impl AsFd) -> io::Result<bool> {
    let opened = rustix::fs::fstat(folder)?;
    match rustix::fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(now) => Ok(same(&now, &opened)),
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// A shelf's folder, with the root it's in, to check it's still there.
struct Place {
    root: OwnedFd,
    name: String,
    folder: OwnedFd,
    /// Its shelf's lock, shared, if it's held for what's being done.
    _shelf: Option<File>,
}

impl Place {
    /// A shelf's folder: `None` if it, or the root, isn't there, unless
    /// `make`, when they're made.
    fn open(path: &Path, shelf: &Shelf, make: bool) -> io::Result<Option<Self>> {
        let name = folder(shelf)?;
        let root = if make {
            make_root(path)?
        } else {
            let Some(root) = root(path)? else {
                return Ok(None);
            };
            root
        };
        Ok(subfolder(&root, &name, make)?.map(|folder| Self {
            root,
            name,
            folder,
            _shelf: None,
        }))
    }

    /// Whether its folder is still where it was.
    fn still_there(&self) -> io::Result<bool> {
        still_there(&self.root, &self.name, &self.folder)
    }
}

/// Opens a regular file in a folder, never through a link, and never
/// waiting on something that isn't a file, such as a pipe.
fn open(folder: impl AsFd, name: &str, flags: OFlags) -> io::Result<File> {
    let flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let file = rustix::fs::openat(folder, name, flags, Mode::from_raw_mode(0o600))?;
    if FileType::from_raw_mode(rustix::fs::fstat(&file)?.st_mode) != FileType::RegularFile {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{name} isn't a file"),
        ));
    }
    Ok(file.into())
}

/// Whether an error is of something the store didn't make: a link, a
/// folder, a pipe, where a file of its should be.
fn not_ours(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported
        || error.raw_os_error() == Some(Errno::LOOP.raw_os_error())
}

/// A file's bytes: `None` if it isn't there.
fn read(folder: impl AsFd, name: &str) -> io::Result<Option<Vec<u8>>> {
    let mut file = match open(folder, name, OFlags::RDONLY) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

/// Up to `length` bytes from a file's start: `None` if it isn't there.
fn read_start(folder: impl AsFd, name: &str, length: u64) -> io::Result<Option<Vec<u8>>> {
    let file = match open(folder, name, OFlags::RDONLY) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

/// The generation a file starts with: `None` if it isn't there, 0 if it's
/// too short to say.
fn generation(folder: impl AsFd, name: &str) -> io::Result<Option<u64>> {
    Ok(read_start(folder, name, 8)?.map(|start| start.try_into().map_or(0, u64::from_le_bytes)))
}

/// Writes `parts` to the new file `new`, synced, with `times` if given.
/// If `exclusive`, it must be made now: one there already is another's.
fn write_new(
    folder: impl AsFd,
    new: &str,
    parts: &[&[u8]],
    times: Option<&Timestamps>,
    exclusive: bool,
) -> io::Result<()> {
    let flags = if exclusive {
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL
    } else {
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC
    };
    let mut file = open(&folder, new, flags)?;
    let written = (|| -> io::Result<()> {
        parts.iter().try_for_each(|part| file.write_all(part))?;
        if let Some(times) = times {
            rustix::fs::futimens(&file, times)?;
        }
        file.sync_all()
    })();
    if written.is_err() {
        let _ = remove(&folder, new);
    }
    written
}

/// Writes `parts` to the new file `new`, synced, and renames it to
/// `name`. The folder isn't synced.
fn replace(folder: impl AsFd, new: &str, name: &str, parts: &[&[u8]]) -> io::Result<()> {
    write_new(&folder, new, parts, None, false)?;
    rustix::fs::renameat(&folder, new, &folder, name).map_err(|error| {
        let _ = remove(&folder, new);
        error.into()
    })
}

/// Syncs a file or folder.
fn sync(file: impl AsFd) -> io::Result<()> {
    Ok(rustix::fs::fsync(file)?)
}

/// What's in a folder.
fn entries(folder: impl AsFd) -> io::Result<rustix::fs::Dir> {
    Ok(rustix::fs::Dir::read_from(folder)?)
}

/// The names in a folder that are text.
fn names(folder: impl AsFd) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in entries(folder)? {
        let entry = entry.map_err(io::Error::from)?;
        if let Ok(name) = entry.file_name().to_str()
            && name != "."
            && name != ".."
        {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// Removes a file, if it's there.
fn remove(folder: impl AsFd, name: &str) -> io::Result<()> {
    match rustix::fs::unlinkat(folder, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// A look at a file, never following a link: `None` if it isn't there.
fn look(folder: impl AsFd, name: &str) -> io::Result<Option<Stat>> {
    match rustix::fs::statat(folder, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(stat)),
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Whether a file is there, never following a link.
fn exists(folder: impl AsFd, name: &str) -> io::Result<bool> {
    Ok(look(folder, name)?.is_some())
}

/// When a file last changed, from a look at it.
fn changed(stat: &Stat) -> SystemTime {
    let seconds = Duration::new(stat.st_mtime.max(0) as u64, stat.st_mtime_nsec as u32);
    SystemTime::UNIX_EPOCH + seconds
}

/// A damaged save, as the store says it.
fn damaged(why: &str) -> StoreError {
    StoreError::Damaged(why.into())
}

/// Takes the lock that's the file `name` in `folder`, exclusive or
/// shared, trying a while if another holds it, or once if not `patient`.
/// Whether `folder` is still where it was is the caller's to check.
fn take(folder: impl AsFd, name: &str, exclusive: bool, patient: bool) -> Result<File, StoreError> {
    let operation = if exclusive {
        FlockOperation::NonBlockingLockExclusive
    } else {
        FlockOperation::NonBlockingLockShared
    };
    let tries = if patient { TRIES } else { 1 };
    let (mut tried, mut again) = (0, 0);
    while tried < tries && again <= AGAIN {
        let lock = open(&folder, name, OFlags::RDWR | OFlags::CREATE)?;
        match rustix::fs::flock(&lock, operation) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => {
                tried += 1;
                if tried < tries {
                    thread::sleep(PAUSE);
                }
                continue;
            }
            Err(error) => return Err(StoreError::Io(error.into())),
        }
        // Held, but maybe of a file a deletion removed meanwhile, and
        // another made since: then it's the other that counts.
        if is_at(&lock, &folder, name)? {
            return Ok(lock);
        }
        again += 1;
    }
    Err(StoreError::InUse)
}

/// Whether an open file is still the one named `name` in `folder`.
fn is_at(file: &File, folder: impl AsFd, name: &str) -> io::Result<bool> {
    let opened = rustix::fs::fstat(file)?;
    Ok(look(folder, name)?.is_some_and(|now| same(&now, &opened)))
}

/// Takes a chat's lock in `folder`, as [`take`] does.
fn lock(folder: impl AsFd, chat: ChatId, patient: bool) -> Result<File, StoreError> {
    take(folder, &file(chat, "lock"), true, patient)
}

/// A chat's shelf's folder, with the chat's lock held for `held`, taken
/// now if it isn't yet, holding the shelf's lock shared as it's taken and
/// until the folder is let go: `None` if the shelf isn't there and not
/// `make`.
fn hold(
    path: &Path,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
    make: bool,
) -> Result<Option<(Place, bool)>, StoreError> {
    let key = (shelf.clone(), chat);
    for _ in 0..=AGAIN {
        let Some(mut place) = Place::open(path, shelf, make)? else {
            return Ok(None);
        };
        if let Some(hold) = held.get(&key) {
            // Still its lock, where the chat is, or it isn't held at all.
            if !is_at(&hold.lock, &place.folder, &file(chat, "lock"))? {
                held.remove(&key);
                return Err(StoreError::InUse);
            }
            return Ok(Some((place, false)));
        }
        let shelf_lock = take(&place.folder, SHELF_LOCK, false, true)?;
        let lock = lock(&place.folder, chat, true)?;
        // Its shelf moved meanwhile: the chat's where it went.
        if place.still_there()? {
            place._shelf = Some(shelf_lock);
            held.insert(key, Hold { lock, end: None });
            return Ok(Some((place, true)));
        }
    }
    Err(StoreError::InUse)
}

/// The chats with files in a folder, each once.
fn chats(folder: impl AsFd) -> io::Result<Vec<ChatId>> {
    let mut chats: Vec<ChatId> = (names(folder)?.iter())
        .filter_map(|name| chat_of(name))
        .filter(|(_, extension)| EXTENSIONS.contains(extension))
        .map(|(chat, _)| chat)
        .collect();
    chats.sort_unstable();
    chats.dedup();
    Ok(chats)
}

/// [`Store::list`](crate::Store::list): every chat's header, and an empty
/// one for a chat with a snapshot and no header.
pub(crate) fn list(path: &Path, shelf: &Shelf) -> Result<Vec<(ChatId, Vec<u8>)>, StoreError> {
    let Some(place) = Place::open(path, shelf, false)? else {
        return Ok(Vec::new());
    };
    let names = names(&place.folder)?;
    let of = |extension| {
        (names.iter()).filter_map(move |name| chat_of(name).filter(|(_, is)| *is == extension))
    };
    let mut heads = Vec::new();
    for (chat, _) in of("head") {
        match read(&place.folder, &file(chat, "head")) {
            Ok(Some(head)) => heads.push((chat, head)),
            // Deleted meanwhile, or not the store's.
            Ok(None) => {}
            Err(error) if not_ours(&error) => {}
            Err(error) => return Err(error.into()),
        }
    }
    // A chat a crash left with no header is listed, so it can be seen: with
    // the header its snapshot has, if it's whole; with none, which reads as
    // damaged, if not.
    for (chat, _) in of("caf") {
        if heads.iter().any(|(listed, _)| *listed == chat)
            || exists(&place.folder, &file(chat, "head"))?
        {
            continue;
        }
        let caf = match read(&place.folder, &file(chat, "caf")) {
            Ok(Some(caf)) => caf,
            Ok(None) => continue,
            Err(error) if not_ours(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        let whole = caf.len() >= CAF_PREFIX
            && caf[8] & !SQUEEZED == 0
            && exists(&place.folder, &file(chat, "journal"))?;
        let head = (whole.then(|| format::snapshot_header(&caf[CAF_PREFIX..]).ok()))
            .flatten()
            .unwrap_or_default();
        heads.push((chat, head));
    }
    Ok(heads)
}

/// [`Store::load`](crate::Store::load). It changes nothing.
pub(crate) fn load(
    path: &Path,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
) -> Result<Stored, StoreError> {
    let place = Place::open(path, shelf, false)?.ok_or(StoreError::NotFound)?;
    // Not locking what isn't there, which would leave a lock behind.
    if !exists(&place.folder, &file(chat, "caf"))? {
        return Err(StoreError::NotFound);
    }
    let (place, taken) = hold(path, held, shelf, chat, false)?.ok_or(StoreError::NotFound)?;
    let loaded = read_chat(&place.folder, chat);
    if taken && loaded.is_err() {
        held.remove(&(shelf.clone(), chat));
    }
    loaded
}

/// Reads a locked chat.
fn read_chat(folder: impl AsFd, chat: ChatId) -> Result<Stored, StoreError> {
    let mut snapshot = read(&folder, &file(chat, "caf"))?.ok_or(StoreError::NotFound)?;
    if snapshot.len() < CAF_PREFIX {
        return Err(damaged("its snapshot's file is cut off"));
    }
    if snapshot[8] & !SQUEEZED != 0 {
        return Err(newer(&snapshot[CAF_PREFIX..]));
    }
    let mut journal =
        read(&folder, &file(chat, "journal"))?.ok_or_else(|| damaged("its journal is missing"))?;
    if journal.len() < JOURNAL_PREFIX {
        return Err(damaged("its journal's file is cut off"));
    }
    match generation_of(&journal).cmp(&generation_of(&snapshot)) {
        Ordering::Equal => {
            journal.drain(..JOURNAL_PREFIX);
        }
        // From before a snapshot a crash cut off as it emptied it.
        Ordering::Less => journal.clear(),
        Ordering::Greater => return Err(damaged("its journal is newer than its snapshot")),
    }
    snapshot.drain(..CAF_PREFIX);
    Ok(Stored::new(snapshot, journal))
}

/// A snapshot a newer CAF's store marked in a way this one doesn't know:
/// of its version, if that's newer, or at least the next.
fn newer(snapshot: &[u8]) -> StoreError {
    let version = format::version(snapshot)
        .ok()
        .filter(|version| *version > VERSION);
    StoreError::Newer {
        version: version.unwrap_or(VERSION + 1),
    }
}

/// The generation a file's bytes start with.
fn generation_of(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().expect("eight bytes"))
}

/// [`Store::snapshot`](crate::Store::snapshot).
pub(crate) fn snapshot(
    path: &Path,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
    header: &[u8],
    snapshot: &[u8],
) -> Result<(), StoreError> {
    let (place, _) = hold(path, held, shelf, chat, true)?.expect("made");
    let key = (shelf.clone(), chat);
    // Not known again until it's done: a snapshot that fails partway may
    // leave the journal from before it, which counts for nothing.
    if let Some(hold) = held.get_mut(&key) {
        hold.end = None;
    }
    let folder = &place.folder;
    let (caf, journal) = (file(chat, "caf"), file(chat, "journal"));
    let was = generation(folder, &caf)?;
    // Past any journal too, as one may be left of a chat deleted halfway.
    let generation = (was
        .unwrap_or(0)
        .max(generation(folder, &journal)?.unwrap_or(0)))
    .checked_add(1)
    .ok_or_else(|| io::Error::other("its generations have run out"))?
    .to_le_bytes();
    let parts: [&[u8]; 3] = [&generation, &[0], snapshot];
    let empty_journal =
        |folder| replace(folder, &new_file(chat, "journal"), &journal, &[&generation]);
    if was.is_none() {
        // A new chat: its journal first, so its snapshot is never without
        // one.
        empty_journal(folder)?;
        sync(folder)?;
        replace(folder, &new_file(chat, "caf"), &caf, &parts)?;
        sync(folder)?;
    } else {
        replace(folder, &new_file(chat, "caf"), &caf, &parts)?;
        sync(folder)?;
        empty_journal(folder)?;
    }
    replace(
        folder,
        &new_file(chat, "head"),
        &file(chat, "head"),
        &[header],
    )?;
    sync(folder)?;
    if let Some(hold) = held.get_mut(&key) {
        hold.end = Some(JOURNAL_PREFIX as u64);
    }
    Ok(())
}

/// [`Store::append`](crate::Store::append), taking the chat's lock if it
/// isn't held, and letting it go again if it then can't add the record.
pub(crate) fn append(
    path: &Path,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
    header: &[u8],
    record: &[u8],
) -> Result<(), StoreError> {
    let place = Place::open(path, shelf, false)?.ok_or(StoreError::NotFound)?;
    if !exists(&place.folder, &file(chat, "caf"))? {
        return Err(StoreError::NotFound);
    }
    let (place, taken) = hold(path, held, shelf, chat, false)?.ok_or(StoreError::NotFound)?;
    let added = add(&place.folder, held, shelf, chat, header, record);
    if taken && added.is_err() {
        held.remove(&(shelf.clone(), chat));
    }
    added
}

/// Adds a record to a locked chat's journal, and replaces its header.
fn add(
    folder: impl AsFd,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
    header: &[u8],
    record: &[u8],
) -> Result<(), StoreError> {
    // The snapshot it follows: a record is in its version, so it's this
    // CAF's, or the record isn't added.
    let caf = read_start(&folder, &file(chat, "caf"), CAF_PREFIX as u64 + 7)?
        .ok_or(StoreError::NotFound)?;
    if caf.len() < CAF_PREFIX + 7 {
        return Err(damaged("its snapshot's file is cut off"));
    }
    if caf[8] & !SQUEEZED != 0 {
        return Err(newer(&caf[CAF_PREFIX..]));
    }
    match format::version(&caf[CAF_PREFIX..]) {
        Ok(VERSION) => {}
        Ok(version) => {
            return Err(damaged(&format!(
                "its snapshot is of version {version}: a snapshot of version {VERSION} is \
                 written before anything's added to its journal"
            )));
        }
        Err(CannotRestore::Newer { version }) => return Err(StoreError::Newer { version }),
        Err(CannotRestore::Damaged(why)) => return Err(StoreError::Damaged(why)),
        Err(other) => return Err(damaged(&format!("{other:?}"))),
    }
    let key = (shelf.clone(), chat);
    let known = held.get(&key).and_then(|hold| hold.end);
    let end = match known {
        Some(end) => end,
        None => end_of_journal(&folder, chat, &caf[..8])?,
    };
    // The header before the record: one a save ahead of the journal is
    // harmless, but a record written and then said to have failed would be
    // written twice when the core tries again.
    replace(
        &folder,
        &new_file(chat, "head"),
        &file(chat, "head"),
        &[header],
    )?;
    let journal = open(&folder, &file(chat, "journal"), OFlags::RDWR)?;
    let length = journal.metadata()?.len();
    if length < end {
        return Err(StoreError::Io(io::Error::other("its journal shrank")));
    }
    let written = (|| -> io::Result<()> {
        if length > end {
            // A record a crash cut off, written over: cut off first, so no
            // part of it is left after what's written.
            journal.set_len(end)?;
            journal.sync_data()?;
        }
        write_at(&journal, record, end)?;
        journal.sync_data()
    })();
    if let Err(error) = written {
        // Nothing half-written stays to be followed by the next.
        let _ = journal.set_len(end);
        return Err(error.into());
    }
    if let Some(hold) = held.get_mut(&key) {
        hold.end = Some(end + record.len() as u64);
    }
    Ok(())
}

/// Where a locked chat's journal's records that count end, starting it
/// again if it's from before the snapshot of `generation`.
fn end_of_journal(folder: impl AsFd, chat: ChatId, generation: &[u8]) -> Result<u64, StoreError> {
    let name = file(chat, "journal");
    let journal = read(&folder, &name)?.ok_or_else(|| damaged("its journal is missing"))?;
    if journal.len() < JOURNAL_PREFIX {
        return Err(damaged("its journal's file is cut off"));
    }
    match generation_of(&journal).cmp(&generation_of(generation)) {
        Ordering::Equal => {
            let kept = format::kept(&journal[JOURNAL_PREFIX..]).map_err(|why| match why {
                CannotRestore::Damaged(why) => StoreError::Damaged(why),
                other => damaged(&format!("{other:?}")),
            })?;
            Ok((JOURNAL_PREFIX + kept) as u64)
        }
        Ordering::Less => {
            replace(&folder, &new_file(chat, "journal"), &name, &[generation])?;
            sync(&folder)?;
            Ok(JOURNAL_PREFIX as u64)
        }
        Ordering::Greater => Err(damaged("its journal is newer than its snapshot")),
    }
}

/// Writes all of `bytes` at `at`.
fn write_at(file: &File, mut bytes: &[u8], mut at: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        match rustix::io::pwrite(file, bytes, at)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            done => {
                bytes = &bytes[done..];
                at += done as u64;
            }
        }
    }
    Ok(())
}

/// [`Store::delete`](crate::Store::delete).
pub(crate) fn delete(
    path: &Path,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
) -> Result<(), StoreError> {
    let key = (shelf.clone(), chat);
    let Some((place, _)) = hold(path, held, shelf, chat, false)? else {
        held.remove(&key);
        return Ok(());
    };
    // Its snapshot first, then its journal, so a crash can't leave a
    // snapshot that reads as having nothing to add; then new files a crash
    // left; its header after them, so it's listed until nothing's left,
    // and the lock last.
    let [first, second, header, lock] = DELETES.map(|extension| file(chat, extension));
    let mut order = vec![first, second];
    let removed = names(&place.folder).and_then(|names| {
        order.extend(names.into_iter().filter(|name| {
            chat_of(name)
                .is_some_and(|(of, extension)| of == chat && !EXTENSIONS.contains(&extension))
        }));
        order.push(header);
        order.push(lock);
        order
            .iter()
            .try_for_each(|name| remove(&place.folder, name))
    });
    held.remove(&key);
    removed?;
    Ok(sync(&place.folder)?)
}

/// [`Store::move_shelf`](crate::Store::move_shelf).
pub(crate) fn move_shelf(
    path: &Path,
    held: &mut Held,
    from: &Shelf,
    to: &Shelf,
) -> Result<(), StoreError> {
    let (from_name, to_name) = (folder(from)?, folder(to)?);
    if from_name == to_name {
        return Ok(());
    }
    let Some(root) = root(path)? else {
        return Ok(());
    };
    let Some(from_folder) = subfolder(&root, &from_name, false)? else {
        return Ok(());
    };
    // No chat comes to it, or starts to, while it moves.
    let from_lock = take(&from_folder, SHELF_LOCK, true, true)?;
    let chats = chats(&from_folder)?;
    // Every chat on it locked, so none moves from under a program that
    // has it open: its next write would be to where it was.
    let mut taken = Vec::new();
    for &chat in &chats {
        if !held.contains_key(&(from.clone(), chat)) {
            taken.push(lock(&from_folder, chat, true)?);
        }
    }
    if !still_there(&root, &from_name, &from_folder)? {
        return Err(StoreError::InUse);
    }
    let rekey = |held: &mut Held, chat| {
        if let Some(hold) = held.remove(&(from.clone(), chat)) {
            held.insert((to.clone(), chat), hold);
        }
    };
    match rustix::fs::renameat_with(&root, &from_name, &root, &to_name, RenameFlags::NOREPLACE) {
        Ok(()) => {
            chats.iter().for_each(|&chat| rekey(held, chat));
            return Ok(sync(&root)?);
        }
        // `to` is there, or the system can't rename without replacing.
        Err(Errno::EXIST | Errno::NOTEMPTY | Errno::INVAL) => {}
        Err(error) => return Err(StoreError::Io(error.into())),
    }
    let to_folder = subfolder(&root, &to_name, true)?.expect("made");
    let to_lock = take(&to_folder, SHELF_LOCK, true, true)?;
    if !still_there(&root, &to_name, &to_folder)? {
        return Err(StoreError::InUse);
    }
    for &chat in &chats {
        move_chat(&from_folder, &to_folder, chat)?;
        rekey(held, chat);
    }
    drop((taken, to_lock));
    // Its lock goes before the folder, held: anyone waiting on it finds
    // it gone, and looks again.
    remove(&from_folder, SHELF_LOCK)?;
    drop(from_lock);
    match rustix::fs::unlinkat(&root, &from_name, AtFlags::REMOVEDIR) {
        Ok(()) => Ok(sync(&root)?),
        // Something not a chat's is in it: it's left.
        Err(Errno::NOTEMPTY | Errno::EXIST) => Ok(()),
        Err(error) => Err(StoreError::Io(error.into())),
    }
}

/// Moves a chat whose lock is held from one folder to another: each of
/// its files is linked where it goes, and once all are, and synced, they
/// go from where they were. A crash leaves it whole in one folder, or in
/// both as the same files, never split; moving again finishes it. A file
/// there already that's another is a clash: [`StoreError::InUse`].
fn move_chat(from: impl AsFd, to: impl AsFd, chat: ChatId) -> Result<(), StoreError> {
    let mut link = Vec::new();
    let mut there_lock = None;
    for extension in MOVE_LINKS {
        let name = file(chat, extension);
        let Some(here) = look(&from, &name)? else {
            continue;
        };
        match look(&to, &name)? {
            None => link.push(name),
            Some(there) if same(&here, &there) => {}
            // A lock left there by a chat gone, held by nobody, is
            // replaced. One that's held is a program writing the chat
            // there.
            Some(_) if extension == "lock" => {
                there_lock = Some(lock(&to, chat, false)?);
                link.push(name);
            }
            Some(_) => return Err(StoreError::InUse),
        }
    }
    for name in &link {
        if there_lock.is_some() && *name == file(chat, "lock") {
            remove(&to, name)?;
        }
        rustix::fs::linkat(&from, name, &to, name, AtFlags::empty()).map_err(io::Error::from)?;
    }
    sync(&to)?;
    drop(there_lock);
    for extension in MOVE_REMOVES {
        remove(&from, &file(chat, extension))?;
    }
    Ok(sync(&from)?)
}

/// [`FileStore::squeeze`](crate::FileStore::squeeze), tidying what crashes
/// left as it goes.
pub(crate) fn squeeze(path: &Path, after: Duration) -> io::Result<u64> {
    let Some(root) = root(path)? else {
        return Ok(0);
    };
    let before = SystemTime::now().checked_sub(after);
    let mut saved = 0;
    for name in names(&root)? {
        // Only folders, never through a link.
        let Ok(Some(folder)) = subfolder(&root, &name, false) else {
            continue;
        };
        tidy(&folder)?;
        for chat in chats(&folder)? {
            // One that can't be squeezed is left as it was.
            if let Ok(less) = squeeze_chat(&root, &name, &folder, chat, before) {
                saved += less;
            }
        }
    }
    Ok(saved)
}

/// Removes what crashes left in a shelf's folder that nobody uses: a
/// squeeze's new files long since made, and chats with nothing but a
/// lock, and maybe a journal.
fn tidy(folder: impl AsFd) -> io::Result<()> {
    let names = names(&folder)?;
    let now = SystemTime::now();
    for name in &names {
        let Some((chat, extension)) = chat_of(name) else {
            continue;
        };
        if extension.starts_with("caf.squeeze-") && extension.ends_with(".new") {
            // Its change time, as its modification time is the snapshot's.
            if let Some(stat) = look(&folder, name)? {
                let made = SystemTime::UNIX_EPOCH
                    + Duration::new(stat.st_ctime.max(0) as u64, stat.st_ctime_nsec as u32);
                if now.duration_since(made).is_ok_and(|age| age > STALE) {
                    remove(&folder, name)?;
                }
            }
        } else if extension == "lock"
            && !(names.iter()).any(|other| {
                other != name
                    && chat_of(other)
                        .is_some_and(|(of, extension)| of == chat && extension != "journal")
            })
            && let Ok(_lock) = lock(&folder, chat, false)
            // Made meanwhile, by whoever had the lock before.
            && !exists(&folder, &file(chat, "caf"))?
            && !exists(&folder, &file(chat, "head"))?
        {
            // Held, so nobody's making the chat: it was given up, maybe
            // after its journal, before its snapshot.
            remove(&folder, &file(chat, "journal"))?;
            remove(&folder, name)?;
        }
    }
    Ok(())
}

/// Squeezes a chat's snapshot, if nobody has it, it's been left alone
/// since `before`, and it isn't squeezed yet. How many bytes it saved.
fn squeeze_chat(
    root: impl AsFd,
    name: &str,
    folder: impl AsFd,
    chat: ChatId,
    before: Option<SystemTime>,
) -> io::Result<u64> {
    let caf = file(chat, "caf");
    let Some(was) = look(&folder, &caf)? else {
        return Ok(0);
    };
    let journal = look(&folder, &file(chat, "journal"))?;
    let last = journal.map_or(changed(&was), |journal| {
        changed(&journal).max(changed(&was))
    });
    if FileType::from_raw_mode(was.st_mode) != FileType::RegularFile
        || before.is_none_or(|before| last > before)
    {
        return Ok(0);
    }
    // Not one a program has: it'd be changing it.
    if lock(&folder, chat, false).is_err() {
        return Ok(0);
    }
    let Some(bytes) = read(&folder, &caf)? else {
        return Ok(0);
    };
    // Squeezed already, or marked in a way this CAF doesn't know.
    if bytes.len() < CAF_PREFIX || bytes[8] != 0 {
        return Ok(0);
    }
    let squeezed = format::squeeze(&bytes[CAF_PREFIX..], crate::Squeeze::Hardest)
        .map_err(|why| io::Error::new(io::ErrorKind::InvalidData, format!("{why:?}")))?;
    // Smaller or not, it's marked squeezed, so it's done once.
    let snapshot = if squeezed.len() < bytes.len() - CAF_PREFIX {
        &squeezed[..]
    } else {
        &bytes[CAF_PREFIX..]
    };
    let flags = [bytes[8] | SQUEEZED];
    let parts: [&[u8]; 3] = [&bytes[..8], &flags, snapshot];
    // Written and synced first, then put in place holding the lock, if
    // it's still the same file, so a program loading it waits only for a
    // rename.
    // Its own, never another squeeze's: made now, by this one.
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let new = format!(
        "{chat}.caf.squeeze-{}-{}.new",
        std::process::id(),
        COUNT.fetch_add(1, AtomicOrdering::Relaxed)
    );
    let times = Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: was.st_atime,
            tv_nsec: was.st_atime_nsec as _,
        },
        last_modification: rustix::fs::Timespec {
            tv_sec: was.st_mtime,
            tv_nsec: was.st_mtime_nsec as _,
        },
    };
    write_new(&folder, &new, &parts, Some(&times), true)?;
    let placed = (|| -> io::Result<()> {
        let _lock = lock(&folder, chat, false).map_err(|_| io::ErrorKind::ResourceBusy)?;
        let unchanged = still_there(&root, name, &folder)?
            && look(&folder, &caf)?.is_some_and(|now| {
                same(&now, &was)
                    && (now.st_size, now.st_mtime, now.st_mtime_nsec)
                        == (was.st_size, was.st_mtime, was.st_mtime_nsec)
            });
        if !unchanged {
            return Err(io::ErrorKind::ResourceBusy.into());
        }
        Ok(rustix::fs::renameat(&folder, &new, &folder, &caf)?)
    })();
    if placed.is_err() {
        let _ = remove(&folder, &new);
    }
    placed?;
    sync(&folder)?;
    Ok((bytes.len() - CAF_PREFIX - snapshot.len()) as u64)
}

#[cfg(test)]
mod tests;
