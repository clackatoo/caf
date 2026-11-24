//! [`DiskFiles`]: CAF's tools' files, on the disk.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use caf_core::ids::CallId;
use caf_tools::{BoxFuture, FileError, FileRead, FileText, Files, Version};
use sha2::{Digest as _, Sha256};

/// The biggest file read, in bytes.
pub const MOST_READ: u64 = 256 * 1024 * 1024;

/// How many symlinks are followed to the file they lead to, as the
/// system's own limit.
const MOST_LINKS: usize = 40;

/// The files CAF's tools read and write, on the disk: the default
/// [`ToolsConfig::files`](crate::ToolsConfig::files).
///
/// - **Only regular files are read,** never through a FIFO or a device,
///   so a read never waits on one. At most [`MOST_READ`] bytes.
/// - **A file's version** is its modification time, size, inode and
///   device, and its content's sha256.
/// - **A write is atomic:** the new text goes to a file of a random name
///   beside it, made with the file's mode, synced, then renamed over it,
///   after its version is checked once more. A symlink is followed and
///   the file it leads to written; a file with more than one hard link is
///   written in place, so it keeps them. A new file's folders are made.
/// - The work is done on tokio's blocking threads.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiskFiles;

impl Files for DiskFiles {
    fn read(&self, path: &Path) -> BoxFuture<Result<FileRead, FileError>> {
        let path = path.to_owned();
        Box::pin(blocking(move || read(&path)))
    }

    fn write(
        &self,
        path: &Path,
        text: FileText,
        expected: Option<Version>,
        _call: CallId,
    ) -> BoxFuture<Result<Version, FileError>> {
        let path = path.to_owned();
        Box::pin(blocking(move || {
            write(&path, &text.bytes, expected.as_ref())
        }))
    }
}

/// Runs `work` on a blocking thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, FileError> + Send + 'static,
) -> Result<T, FileError> {
    match tokio::task::spawn_blocking(work).await {
        Ok(done) => done,
        Err(_) => Err(FileError::Other(
            "CAF's file work stopped unexpectedly".to_owned(),
        )),
    }
}

/// The file at `path`, as it is now.
fn read(path: &Path) -> Result<FileRead, FileError> {
    let (bytes, meta) = read_regular(path)?;
    let version = version(&meta, &bytes);
    Ok(FileRead::new(bytes, version))
}

/// The bytes of the regular file at `path`, and its metadata as they were
/// read.
fn read_regular(path: &Path) -> Result<(Vec<u8>, Metadata), FileError> {
    // Looked at first, so a FIFO or a device is never opened...
    let meta = std::fs::metadata(path).map_err(|error| io_error(path, &error))?;
    kind(&meta)?;
    // ...and opened so that one swapped in since can't hold it up.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(|error| io_error(path, &error))?;
    let meta = file.metadata().map_err(|error| io_error(path, &error))?;
    kind(&meta)?;
    if meta.len() > MOST_READ {
        return Err(FileError::TooBig { size: meta.len() });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    (&file)
        .take(MOST_READ + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, &error))?;
    if bytes.len() as u64 > MOST_READ {
        return Err(FileError::TooBig {
            size: bytes.len() as u64,
        });
    }
    Ok((bytes, meta))
}

/// Whether `meta` is a regular file's, or why not.
fn kind(meta: &Metadata) -> Result<(), FileError> {
    if meta.is_dir() {
        Err(FileError::IsDirectory)
    } else if !meta.is_file() {
        Err(FileError::NotRegular)
    } else {
        Ok(())
    }
}

/// The version of a file of `bytes`, whose metadata is `meta`.
fn version(meta: &Metadata, bytes: &[u8]) -> Version {
    let mtime = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
    let mut hash = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        hash.push_str(&format!("{byte:02x}"));
    }
    Version::new(format!(
        "{mtime}:{}:{}:{}:{hash}",
        meta.len(),
        meta.ino(),
        meta.dev()
    ))
}

/// Whether `a` and `b` are the same file, unchanged: its version, but for
/// the content's hash.
fn same(a: &Metadata, b: &Metadata) -> bool {
    (a.mtime(), a.mtime_nsec(), a.len(), a.ino(), a.dev())
        == (b.mtime(), b.mtime_nsec(), b.len(), b.ino(), b.dev())
}

/// What `error`, from the file at `path`, means for a tool.
fn io_error(path: &Path, error: &io::Error) -> FileError {
    match error.raw_os_error() {
        Some(libc::ENOENT | libc::ENOTDIR) => FileError::NotFound,
        Some(libc::EISDIR) => FileError::IsDirectory,
        Some(libc::ENXIO) => FileError::NotRegular,
        Some(libc::EACCES | libc::EPERM | libc::EROFS) => {
            FileError::Denied(format!("{}: {error}", path.display()))
        }
        _ => FileError::Other(format!("{}: {error}", path.display())),
    }
}

/// The file `path` leads to: itself, or what its symlinks point to, which
/// may not be there yet.
fn resolve(path: &Path) -> Result<PathBuf, FileError> {
    let mut at = path.to_owned();
    for _ in 0..MOST_LINKS {
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let to = std::fs::read_link(&at).map_err(|error| io_error(&at, &error))?;
                at = match at.parent() {
                    Some(parent) if to.is_relative() => parent.join(to),
                    _ => to,
                };
            }
            _ => return Ok(at),
        }
    }
    Err(FileError::Other(format!(
        "{}: too many levels of symbolic links",
        path.display()
    )))
}

/// Writes `bytes` to the file at `path`, if its version is still
/// `expected` (`None`: there's none).
fn write(path: &Path, bytes: &[u8], expected: Option<&Version>) -> Result<Version, FileError> {
    let target = resolve(path)?;
    let current = match read_regular(&target) {
        Ok((before, meta)) => Some((version(&meta, &before), meta)),
        Err(FileError::NotFound) => None,
        Err(error) => return Err(error),
    };
    if current.as_ref().map(|(version, _)| version) != expected {
        return Err(FileError::Changed);
    }
    match current {
        Some((_, meta)) if meta.nlink() > 1 => in_place(&target, bytes, &meta),
        Some((_, meta)) => replace(&target, bytes, Some(&meta)),
        None => replace(&target, bytes, None),
    }
}

/// Writes `bytes` over the file at `path` as it is, keeping its hard
/// links, if it's still as `before`.
fn in_place(path: &Path, bytes: &[u8], before: &Metadata) -> Result<Version, FileError> {
    let error = |error: io::Error| io_error(path, &error);
    let mut file = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(error)?;
    let now = file.metadata().map_err(error)?;
    if !same(before, &now) {
        return Err(FileError::Changed);
    }
    file.set_len(0).map_err(error)?;
    file.write_all(bytes).map_err(error)?;
    file.sync_all().map_err(error)?;
    let meta = file.metadata().map_err(error)?;
    Ok(version(&meta, bytes))
}

/// Writes `bytes` to a new file beside `path`, and renames it over the
/// file there, `before`, if it's still as it was, or to make it, if
/// there's none (`None`).
fn replace(path: &Path, bytes: &[u8], before: Option<&Metadata>) -> Result<Version, FileError> {
    let error = |error: io::Error| io_error(path, &error);
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(FileError::IsDirectory);
    };
    if before.is_none() {
        std::fs::create_dir_all(folder).map_err(|e| io_error(folder, &e))?;
    }
    let (temp, mut file) = temp_beside(folder, &name.to_string_lossy())?;
    let temp = TempFile(Some(temp));
    let written = (|| {
        if let Some(before) = before {
            // Its mode exactly, not as the umask leaves it, and its owner
            // if that's allowed.
            let mode = before.mode() & 0o7777;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            let _ = std::os::unix::fs::fchown(&file, Some(before.uid()), Some(before.gid()));
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        file.metadata()
    })()
    .map_err(error)?;
    let temp_path = temp.0.as_deref().unwrap_or(path);
    match before {
        Some(before) => {
            let now = std::fs::metadata(path).map_err(error)?;
            if !same(before, &now) {
                return Err(FileError::Changed);
            }
            std::fs::rename(temp_path, path).map_err(error)?;
        }
        // Never over a file made since.
        None => rustix::fs::renameat_with(
            rustix::fs::CWD,
            temp_path,
            rustix::fs::CWD,
            path,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .or_else(|errno| match errno {
            // A filesystem without it, such as NFS or many FUSE ones: a
            // hard link is made only where there's nothing, too.
            rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS | rustix::io::Errno::OPNOTSUPP => {
                std::fs::hard_link(temp_path, path)
                    .map_err(|e| rustix::io::Errno::from_io_error(&e).unwrap_or(errno))?;
                let _ = std::fs::remove_file(temp_path);
                Ok(())
            }
            errno => Err(errno),
        })
        .map_err(|errno| match errno {
            rustix::io::Errno::EXIST => FileError::Changed,
            errno => error(errno.into()),
        })?,
    }
    temp.kept();
    if let Ok(folder) = File::open(folder) {
        let _ = folder.sync_all();
    }
    Ok(version(&written, bytes))
}

/// A new file of a random name in `folder`, for `name`'s new text, made
/// so that nothing there already, a symlink included, is opened.
fn temp_beside(folder: &Path, name: &str) -> Result<(PathBuf, File), FileError> {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let mut last = None;
    for _ in 0..8 {
        let mut random = [0u8; 8];
        if File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut random))
            .is_err()
        {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos() as u64);
            random = (nanos ^ MADE.fetch_add(1, Ordering::Relaxed).rotate_left(40)).to_le_bytes();
        }
        let tag: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let temp = folder.join(format!(".{name}.caf-{tag}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o666)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp)
        {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(io_error(folder, &error)),
        }
    }
    let error = last.unwrap_or_else(|| io::Error::other("no name was free"));
    Err(io_error(folder, &error))
}

/// A temporary file, removed when dropped unless it was kept.
struct TempFile(Option<PathBuf>);

impl TempFile {
    fn kept(mut self) {
        self.0 = None;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests;
