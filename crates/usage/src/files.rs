//! Files and folders, as the ledger and prices keep them: locks, and files
//! replaced in one step.
//!
//! Nothing here waits for ever. Every file is opened without waiting
//! (`O_NONBLOCK`), and is only used if it's a plain file, so a pipe where
//! one should be is an error, never a read that waits for a writer. A lock
//! another holds is tried again for [`LOCK_WAIT`], then given up on.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

use rustix::fs::{FileType, FlockOperation, OFlags};
use rustix::io::Errno;

/// The lock in every folder CAF writes in.
pub(crate) const LOCK: &str = ".lock";
/// How long a lock another holds is waited for before giving up.
pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(5);
/// The longest pause between tries of a lock.
const LONGEST_PAUSE: Duration = Duration::from_millis(50);

/// Opens a file as `options` say, without waiting, as long as it's a plain
/// file: anything else, such as a pipe or a folder, is
/// [`Unsupported`](io::ErrorKind::Unsupported).
pub(crate) fn open(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    let file = options
        .custom_flags(OFlags::NONBLOCK.bits() as i32)
        .open(path)?;
    if FileType::from_raw_mode(rustix::fs::fstat(&file)?.st_mode) != FileType::RegularFile {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{} isn't a plain file", name(path)),
        ));
    }
    Ok(file)
}

/// A file's name in the ledger's folder, such as `prices/own.json`: every
/// file CAF keeps is in a folder of the ledger's. What an error says, as
/// errors can reach a viewer, and the rest of the path, with the user's
/// home folder in it, is no viewer's business.
fn name(path: &Path) -> String {
    let mut parts: Vec<_> = path.iter().rev().take(2).collect();
    parts.reverse();
    parts
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// A file, opened to read: `None` if it isn't there.
pub(crate) fn open_to_read(path: &Path) -> io::Result<Option<File>> {
    match open(path, OpenOptions::new().read(true)) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Makes a folder and those it's in, if they aren't there, private to the
/// user, each synced into the folder it's in.
pub(crate) fn make_folder(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    make_folder(parent)?;
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => sync_folder(parent),
        // Made meanwhile, by another that syncs it.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Takes the folder's lock, held until the file is dropped. While another
/// holds it, it's tried again, more slowly each time, for [`LOCK_WAIT`];
/// then it's [`TimedOut`](io::ErrorKind::TimedOut), so a program stopped
/// while it held the lock never stops this one.
pub(crate) fn lock(folder: &Path) -> io::Result<File> {
    let lock = open(
        &folder.join(LOCK),
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600),
    )?;
    let started = Instant::now();
    let mut pause = Duration::from_millis(1);
    loop {
        match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(lock),
            Err(Errno::INTR) => {}
            Err(Errno::WOULDBLOCK) => {
                let waited = started.elapsed();
                if waited >= LOCK_WAIT {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "another program has held {} for {waited:?}",
                            name(&folder.join(LOCK))
                        ),
                    ));
                }
                std::thread::sleep(pause.min(LOCK_WAIT - waited));
                pause = (pause * 2).min(LONGEST_PAUSE);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Writes `bytes` to the file `name` in `folder`, replacing what's there
/// in one step: to a new file, synced, renamed into place, and the folder
/// synced. Only under the folder's lock, as the new file's name is fixed.
pub(crate) fn replace(folder: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let new = folder.join(format!("{name}.new"));
    let written = (|| {
        let mut file = open(
            &new,
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600),
        )?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&new, folder.join(name))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    written?;
    sync_folder(folder)
}

/// Syncs a folder, so what was renamed or removed in it stays so.
pub(crate) fn sync_folder(folder: &Path) -> io::Result<()> {
    // Only a folder: a pipe in its place is an error, never waited on.
    OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::DIRECTORY | OFlags::NONBLOCK).bits() as i32)
        .open(folder)?
        .sync_all()
}

/// A file's bytes, or `None` if it isn't there.
pub(crate) fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let Some(mut file) = open_to_read(path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}
