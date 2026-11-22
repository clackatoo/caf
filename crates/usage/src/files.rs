//! Files and folders, as the ledger and prices keep them: locks, and files
//! replaced in one step.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use rustix::fs::FlockOperation;
use rustix::io::Errno;

/// The lock in every folder CAF writes in.
pub(crate) const LOCK: &str = ".lock";

/// Makes a folder and those it's in, if they aren't there, private to the
/// user.
pub(crate) fn make_folder(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

/// Takes the folder's lock, waiting while another holds it: held until
/// the file is dropped.
pub(crate) fn lock(folder: &Path) -> io::Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(folder.join(LOCK))?;
    loop {
        match rustix::fs::flock(&lock, FlockOperation::LockExclusive) {
            Ok(()) => return Ok(lock),
            Err(Errno::INTR) => {}
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
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&new)?;
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
    File::open(folder)?.sync_all()
}

/// A file's bytes, or `None` if it isn't there.
pub(crate) fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
