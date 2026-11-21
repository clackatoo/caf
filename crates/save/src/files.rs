//! [`FileStore`]'s work: files, folders, locks.
//!
//! # A snapshot and its journal
//!
//! A snapshot replaces the old one and empties the journal in one step,
//! though they're two files, by a number the store keeps in front of
//! each: the snapshot's generation. A journal counts only if it has its
//! snapshot's; one with another is from before it, and reads as empty.
//!
//! ```text
//! <chat>.caf      generation:u64 flags:u8 snapshot
//! <chat>.journal  generation:u64 record...
//! ```
//!
//! A snapshot is written with the next generation, to a new file renamed
//! into place, and that's synced with its folder before the journal is
//! replaced with an empty one of its generation, so a crash leaves the old
//! snapshot with its journal, or the new one with none. Squeezing a
//! snapshot keeps its generation, as it keeps what it holds, and marks it
//! squeezed (flag 1), so it's done once.
//!
//! A journal's last record cut off by a crash is cut off the file when
//! it's next loaded, so what's appended after it follows a whole record.
//! That's only the journal's framing: the store reads no more of it.

use std::collections::HashMap;
use std::fs::{DirBuilder, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime};

use caf_core::{ChatId, Shelf};
use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags, Timestamps};
use rustix::io::Errno;
use sha2::{Digest, Sha256};

use crate::{StoreError, Stored, format};

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
/// The files a chat has.
const EXTENSIONS: [&str; 4] = ["head", "caf", "journal", "lock"];

/// The locks a store holds, by chat.
pub(crate) type Held = HashMap<(Shelf, ChatId), File>;

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
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
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

/// The root, made if it isn't there.
pub(crate) fn root(path: &Path) -> io::Result<OwnedFd> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    Ok(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
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

/// A shelf's folder.
pub(crate) fn shelf(root: &Path, shelf: &Shelf, make: bool) -> io::Result<Option<OwnedFd>> {
    subfolder(self::root(root)?, &folder(shelf)?, make)
}

/// Opens a file in a folder, never through a link.
fn open(folder: impl AsFd, name: &str, flags: OFlags) -> io::Result<File> {
    let flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(rustix::fs::openat(folder, name, flags, Mode::from_raw_mode(0o600))?.into())
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

/// The generation a file starts with: 0 if it isn't there, or is too short
/// to say.
fn generation(folder: impl AsFd, name: &str) -> io::Result<u64> {
    let mut file = match open(folder, name, OFlags::RDONLY) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut bytes = [0; 8];
    match file.read_exact(&mut bytes) {
        Ok(()) => Ok(u64::from_le_bytes(bytes)),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
        Err(error) => Err(error),
    }
}

/// Writes `parts` to the new file `new`, synced, with `times` if given.
fn write_new(
    folder: impl AsFd,
    new: &str,
    parts: &[&[u8]],
    times: Option<&Timestamps>,
) -> io::Result<()> {
    let written = (|| -> io::Result<()> {
        let mut file = open(
            &folder,
            new,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
        )?;
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
    write_new(&folder, new, parts, None)?;
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

/// Removes a file, if it's there.
fn remove(folder: impl AsFd, name: &str) -> io::Result<()> {
    match rustix::fs::unlinkat(folder, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Whether a file is there, never following a link.
fn exists(folder: impl AsFd, name: &str) -> io::Result<bool> {
    match rustix::fs::statat(folder, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Takes a chat's lock, trying a while if another holds it, or once if
/// not `patient`.
fn lock(folder: impl AsFd, chat: ChatId, patient: bool) -> Result<File, StoreError> {
    let name = file(chat, "lock");
    let tries = if patient { TRIES } else { 1 };
    for tried in 1..=tries {
        let lock = open(&folder, &name, OFlags::RDWR | OFlags::CREATE)?;
        match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => {
                if tried < tries {
                    thread::sleep(PAUSE);
                }
                continue;
            }
            Err(error) => return Err(StoreError::Io(error.into())),
        }
        // Held, but maybe of a file a deletion removed meanwhile, and
        // another made since: then it's the other that counts.
        let held = rustix::fs::fstat(&lock).map_err(io::Error::from)?;
        match rustix::fs::statat(&folder, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(now) if (now.st_dev, now.st_ino) == (held.st_dev, held.st_ino) => return Ok(lock),
            Ok(_) | Err(Errno::NOENT) => {}
            Err(error) => return Err(StoreError::Io(error.into())),
        }
    }
    Err(StoreError::InUse)
}

/// Takes a chat's lock for `held`, if it isn't there already. Whether
/// it was taken now.
fn hold(
    held: &mut Held,
    folder: impl AsFd,
    shelf: &Shelf,
    chat: ChatId,
) -> Result<bool, StoreError> {
    let key = (shelf.clone(), chat);
    if held.contains_key(&key) {
        return Ok(false);
    }
    let lock = lock(folder, chat, true)?;
    held.insert(key, lock);
    Ok(true)
}

/// The chats with files in a folder, each once.
fn chats(folder: impl AsFd) -> io::Result<Vec<ChatId>> {
    let mut chats = Vec::new();
    for entry in entries(folder)? {
        let entry = entry.map_err(io::Error::from)?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if let Some((chat, extension)) = chat_of(name)
            && EXTENSIONS.contains(&extension)
        {
            chats.push(chat);
        }
    }
    chats.sort_unstable();
    chats.dedup();
    Ok(chats)
}

pub(crate) fn list(root: &Path, shelf: &Shelf) -> Result<Vec<(ChatId, Vec<u8>)>, StoreError> {
    let Some(folder) = self::shelf(root, shelf, false)? else {
        return Ok(Vec::new());
    };
    let mut heads = Vec::new();
    for entry in entries(&folder)? {
        let entry = entry.map_err(io::Error::from)?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        let Some((chat, "head")) = chat_of(name) else {
            continue;
        };
        // Not the store's.
        if matches!(entry.file_type(), FileType::Directory | FileType::Symlink) {
            continue;
        }
        match read(&folder, name) {
            Ok(Some(head)) => heads.push((chat, head)),
            // Deleted meanwhile.
            Ok(None) => {}
            // A link, which isn't the store's.
            Err(error) if error.raw_os_error() == Some(Errno::LOOP.raw_os_error()) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(heads)
}

pub(crate) fn load(
    root: &Path,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
) -> Result<Stored, StoreError> {
    let folder = self::shelf(root, shelf, false)?.ok_or(StoreError::NotFound)?;
    // Not locking what isn't there, which would leave a lock behind.
    if !exists(&folder, &file(chat, "caf"))? {
        return Err(StoreError::NotFound);
    }
    let locked = hold(held, &folder, shelf, chat)?;
    let loaded = read_chat(&folder, chat);
    if locked && loaded.is_err() {
        held.remove(&(shelf.clone(), chat));
    }
    loaded
}

/// Reads a locked chat, cutting off its journal's last record if that's
/// cut off.
fn read_chat(folder: impl AsFd, chat: ChatId) -> Result<Stored, StoreError> {
    let caf = read(&folder, &file(chat, "caf"))?.ok_or(StoreError::NotFound)?;
    if caf.len() < CAF_PREFIX {
        return Err(StoreError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "the snapshot's file is cut off",
        )));
    }
    let generation = &caf[..8];
    let mut journal = Vec::new();
    let name = file(chat, "journal");
    if let Some(mut bytes) = read(&folder, &name)?
        && bytes.get(..8) == Some(generation)
    {
        let whole = JOURNAL_PREFIX + format::whole_records(&bytes[JOURNAL_PREFIX..]);
        if whole < bytes.len() {
            let file = open(&folder, &name, OFlags::WRONLY)?;
            file.set_len(whole as u64)?;
            file.sync_all()?;
            bytes.truncate(whole);
        }
        bytes.drain(..JOURNAL_PREFIX);
        journal = bytes;
    }
    let mut snapshot = caf;
    snapshot.drain(..CAF_PREFIX);
    Ok(Stored::new(snapshot, journal))
}

pub(crate) fn snapshot(
    root: &Path,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
    header: &[u8],
    snapshot: &[u8],
) -> Result<(), StoreError> {
    let folder = self::shelf(root, shelf, true)?.expect("made");
    hold(held, &folder, shelf, chat)?;
    let caf = file(chat, "caf");
    let journal = file(chat, "journal");
    // Past any journal too, as one may be left of a chat deleted halfway.
    let generation = generation(&folder, &caf)?.max(generation(&folder, &journal)?) + 1;
    let generation = generation.to_le_bytes();
    let parts: [&[u8]; 3] = [&generation, &[0], snapshot];
    replace(&folder, &new_file(chat, "caf"), &caf, &parts)?;
    sync(&folder)?;
    replace(
        &folder,
        &new_file(chat, "journal"),
        &journal,
        &[&generation],
    )?;
    let head = file(chat, "head");
    replace(&folder, &new_file(chat, "head"), &head, &[header])?;
    sync(&folder)?;
    Ok(())
}

pub(crate) fn append(
    root: &Path,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
    header: &[u8],
    record: &[u8],
) -> Result<(), StoreError> {
    let folder = self::shelf(root, shelf, false)?.ok_or(StoreError::NotFound)?;
    if !exists(&folder, &file(chat, "caf"))? {
        return Err(StoreError::NotFound);
    }
    hold(held, &folder, shelf, chat)?;
    // The header first: one a save ahead of the journal is harmless, but a
    // record written and then said to have failed would be written twice
    // when the core tries again.
    let head = file(chat, "head");
    replace(&folder, &new_file(chat, "head"), &head, &[header])?;
    let generation = generation(&folder, &file(chat, "caf"))?.to_le_bytes();
    let name = file(chat, "journal");
    let mut journal = open(&folder, &name, OFlags::RDWR | OFlags::CREATE)?;
    let mut start = [0; 8];
    let length = journal.metadata()?.len();
    let matches = length >= 8 && {
        journal.read_exact(&mut start)?;
        start == generation
    };
    if !matches {
        // None yet, or one from before the snapshot, which counts for
        // nothing.
        journal.set_len(0)?;
        write_at(&journal, &generation, 0)?;
    }
    let end = if matches { length } else { 8 };
    let written = rustix::io::pwrite(&journal, record, end)
        .map_err(io::Error::from)
        .and_then(|done| {
            if done == record.len() {
                Ok(())
            } else {
                // Short only on a full disk, or the like: once more says why.
                write_at(&journal, &record[done..], end + done as u64)
            }
        })
        .and_then(|()| journal.sync_data());
    if let Err(error) = written {
        // Nothing half-written stays to be followed by the next.
        let _ = journal.set_len(end);
        return Err(error.into());
    }
    Ok(())
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

pub(crate) fn delete(
    root: &Path,
    held: &mut Held,
    shelf: &Shelf,
    chat: ChatId,
) -> Result<(), StoreError> {
    let Some(folder) = self::shelf(root, shelf, false)? else {
        held.remove(&(shelf.clone(), chat));
        return Ok(());
    };
    hold(held, &folder, shelf, chat)?;
    // Its header first, so it's gone from lists, then the rest, new files
    // a crash left included, and the lock last.
    let mut names = vec![file(chat, "head"), file(chat, "caf"), file(chat, "journal")];
    let others = entries(&folder).and_then(|entries| {
        for entry in entries {
            let entry = entry.map_err(io::Error::from)?;
            if let Ok(name) = entry.file_name().to_str()
                && chat_of(name).is_some_and(|(of, _)| of == chat)
                && !names.iter().any(|named| named == name)
                && name != file(chat, "lock")
            {
                names.push(name.to_owned());
            }
        }
        Ok(())
    });
    let removed = others
        .and_then(|()| names.iter().try_for_each(|name| remove(&folder, name)))
        .and_then(|()| remove(&folder, &file(chat, "lock")))
        .and_then(|()| sync(&folder));
    held.remove(&(shelf.clone(), chat));
    Ok(removed?)
}

pub(crate) fn move_shelf(
    root: &Path,
    held: &mut Held,
    from: &Shelf,
    to: &Shelf,
) -> Result<(), StoreError> {
    let (from_name, to_name) = (folder(from)?, folder(to)?);
    if from_name == to_name {
        return Ok(());
    }
    let root = self::root(root)?;
    let Some(from_folder) = subfolder(&root, &from_name, false)? else {
        return Ok(());
    };
    let chats = chats(&from_folder)?;
    // Every chat on it locked, so none moves from under a program that
    // has it open: its next write would be to where it was.
    let mut taken = Vec::new();
    for &chat in &chats {
        if !held.contains_key(&(from.clone(), chat)) {
            taken.push(lock(&from_folder, chat, true)?);
        }
    }
    let rekey = |held: &mut Held, chat| {
        if let Some(lock) = held.remove(&(from.clone(), chat)) {
            held.insert((to.clone(), chat), lock);
        }
    };
    match rustix::fs::renameat_with(&root, &from_name, &root, &to_name, RenameFlags::NOREPLACE) {
        Ok(()) => {
            sync(&root)?;
            chats.iter().for_each(|&chat| rekey(held, chat));
            return Ok(());
        }
        // `to` is there, or the system can't rename without replacing.
        Err(Errno::EXIST | Errno::NOTEMPTY | Errno::INVAL) => {}
        Err(error) => return Err(StoreError::Io(error.into())),
    }
    let to_folder = subfolder(&root, &to_name, true)?.expect("made");
    for &chat in &chats {
        move_chat(&from_folder, &to_folder, chat)?;
        rekey(held, chat);
    }
    drop(taken);
    match rustix::fs::unlinkat(&root, &from_name, AtFlags::REMOVEDIR) {
        Ok(()) => Ok(sync(&root)?),
        // Something not a chat's is in it: it's left.
        Err(Errno::NOTEMPTY | Errno::EXIST) => Ok(()),
        Err(error) => Err(StoreError::Io(error.into())),
    }
}

/// Moves a chat whose lock is held from one folder to another, unless
/// the other has any of the same files. A move cut off halfway moves the
/// rest when it's done again.
fn move_chat(from: impl AsFd, to: impl AsFd, chat: ChatId) -> Result<(), StoreError> {
    for extension in ["caf", "journal", "head"] {
        let name = file(chat, extension);
        if exists(&from, &name)? && exists(&to, &name)? {
            return Err(StoreError::InUse);
        }
    }
    // A lock left there, held by nobody, is replaced. One that's held is a
    // program writing the chat there.
    let _there = if exists(&to, &file(chat, "lock"))? {
        Some(lock(&to, chat, false)?)
    } else {
        None
    };
    // Its snapshot before its header: a chat half moved isn't listed
    // where it went, and where it was it can't be loaded.
    for extension in ["lock", "caf", "journal", "head"] {
        let name = file(chat, extension);
        match rustix::fs::renameat(&from, &name, &to, &name) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(error) => return Err(StoreError::Io(error.into())),
        }
    }
    sync(&to)?;
    sync(&from)?;
    Ok(())
}

pub(crate) fn squeeze(root: &Path, after: Duration) -> io::Result<u64> {
    let root = self::root(root)?;
    let before = SystemTime::now().checked_sub(after);
    let mut saved = 0;
    for entry in entries(&root)? {
        let entry = entry.map_err(io::Error::from)?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if entry.file_type() != FileType::Directory || name.starts_with('.') {
            continue;
        }
        let Some(folder) = subfolder(&root, name, false)? else {
            continue;
        };
        for chat in chats(&folder)? {
            // One that can't be squeezed is left as it was.
            if let Ok(less) = squeeze_chat(&folder, chat, before) {
                saved += less;
            }
        }
    }
    Ok(saved)
}

/// Squeezes a chat's snapshot, if nobody has it, it's been left alone
/// since `before`, and it isn't squeezed yet. How many bytes it saved.
fn squeeze_chat(folder: impl AsFd, chat: ChatId, before: Option<SystemTime>) -> io::Result<u64> {
    let name = file(chat, "caf");
    let Ok(was) = rustix::fs::statat(&folder, &name, AtFlags::SYMLINK_NOFOLLOW) else {
        return Ok(0);
    };
    let changed =
        SystemTime::UNIX_EPOCH + Duration::new(was.st_mtime as u64, was.st_mtime_nsec as u32);
    if FileType::from_raw_mode(was.st_mode) != FileType::RegularFile
        || before.is_none_or(|before| changed > before)
    {
        return Ok(0);
    }
    // Not one a program has: it'd be changing it.
    if lock(&folder, chat, false).is_err() {
        return Ok(0);
    }
    let Some(caf) = read(&folder, &name)? else {
        return Ok(0);
    };
    if caf.len() < CAF_PREFIX || caf[8] & SQUEEZED != 0 {
        return Ok(0);
    }
    let squeezed = format::squeeze(&caf[CAF_PREFIX..], crate::Squeeze::Hardest)
        .map_err(|why| io::Error::new(io::ErrorKind::InvalidData, format!("{why:?}")))?;
    // Smaller or not, it's marked squeezed, so it's done once.
    let snapshot = if squeezed.len() < caf.len() - CAF_PREFIX {
        &squeezed[..]
    } else {
        &caf[CAF_PREFIX..]
    };
    let flags = [caf[8] | SQUEEZED];
    let parts: [&[u8]; 3] = [&caf[..8], &flags, snapshot];
    // Written and synced first, then put in place holding the lock, if
    // it's still the same file, so a program loading it waits only for a
    // rename.
    let new = format!("{chat}.caf.squeeze-{}.new", std::process::id());
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
    write_new(&folder, &new, &parts, Some(&times))?;
    let placed = (|| -> io::Result<()> {
        let _lock = lock(&folder, chat, false).map_err(|_| io::ErrorKind::ResourceBusy)?;
        let now = rustix::fs::statat(&folder, &name, AtFlags::SYMLINK_NOFOLLOW)?;
        if (now.st_ino, now.st_size, now.st_mtime, now.st_mtime_nsec)
            != (was.st_ino, was.st_size, was.st_mtime, was.st_mtime_nsec)
        {
            return Err(io::ErrorKind::ResourceBusy.into());
        }
        rustix::fs::renameat(&folder, &new, &folder, &name)?;
        sync(&folder)
    })();
    if placed.is_err() {
        let _ = remove(&folder, &new);
    }
    placed?;
    Ok((caf.len() - CAF_PREFIX - snapshot.len()) as u64)
}
