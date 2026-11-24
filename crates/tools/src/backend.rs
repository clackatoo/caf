//! Where CAF's tools read and write files and run commands: the disk and
//! CAF's own pty by default, or what the app gives, such as its editor's
//! buffers and its visible terminals.
//!
//! The defaults live in `caf-driver-tokio` (`DiskFiles`, `PtyCommands`).
//! An app gives its own in its `ToolsConfig`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use caf_core::event::Exit;
use caf_core::ids::CallId;

/// A future a backend gives back: boxed, sendable, owning what it needs.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Where files are read and written: the disk by default, or an app's
/// editor buffers.
pub trait Files: Send + Sync + 'static {
    /// The file's text and its version, as it is now (an editor's buffer,
    /// unsaved edits included).
    fn read(&self, path: &Path) -> BoxFuture<Result<FileText, FileError>>;

    /// Replace the file's text, if its version is still `expected` (`None`
    /// for a file that mustn't exist yet). An editor makes this one undoable
    /// change. Returns the new version.
    ///
    /// A file whose version isn't `expected` any more is
    /// [`FileError::Changed`], and isn't written.
    fn write(
        &self,
        path: &Path,
        text: &FileText,
        expected: Option<&Version>,
        call: CallId,
    ) -> BoxFuture<Result<Version, FileError>>;
}

/// A file's raw bytes, and its version. CAF decodes them itself, and keeps
/// a byte order mark and the line endings as they are.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileText {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// Its version, as it was read: none for text about to be written.
    pub version: Option<Version>,
}

impl FileText {
    /// Text of these bytes, of no version yet: what's written.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            version: None,
        }
    }

    /// Text of these bytes as read, at `version`.
    #[must_use]
    pub fn read(bytes: Vec<u8>, version: Version) -> Self {
        Self {
            bytes,
            version: Some(version),
        }
    }
}

/// A file's version, opaque: whatever tells its backend whether the file
/// changed. The disk's is its mtime, size, inode and device, and its
/// content's sha256; an editor's could be its buffer's change count.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version(pub String);

impl Version {
    /// A version of this text.
    #[must_use]
    pub fn new(version: impl Into<String>) -> Self {
        Self(version.into())
    }

    /// Its text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a file couldn't be read or written.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileError {
    /// There's no file there.
    NotFound,
    /// It's a directory.
    IsDirectory,
    /// It's not a regular file: a device, FIFO or socket.
    NotRegular,
    /// It's bigger than the backend reads, this many bytes.
    TooBig {
        /// Its size, in bytes.
        size: u64,
    },
    /// Its version isn't the one expected: it changed.
    Changed,
    /// The backend may not touch it, for this reason.
    Denied(String),
    /// Anything else, said in a line.
    Other(String),
}

/// Where commands run: CAF's own pty by default, or an app's visible
/// terminal.
pub trait Commands: Send + Sync + 'static {
    /// Starts `spec`'s command, giving its output to `out` as it comes,
    /// in order. An error is why it couldn't start, said in a line.
    fn start(
        &self,
        spec: CommandSpec,
        out: Box<dyn FnMut(Output) + Send>,
    ) -> BoxFuture<Result<Box<dyn Running>, String>>;
}

/// A command to run.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// The command, as the model wrote it: a shell's line.
    pub command: String,
    /// The folder it starts in.
    pub folder: PathBuf,
    /// Variables to set for it, beyond the app's own environment.
    pub env: Vec<(String, String)>,
    /// The call it's for.
    pub call: CallId,
}

impl CommandSpec {
    /// `command`, run in `folder` for `call`, with no variables of its own.
    #[must_use]
    pub fn new(command: impl Into<String>, folder: impl Into<PathBuf>, call: CallId) -> Self {
        Self {
            command: command.into(),
            folder: folder.into(),
            env: Vec::new(),
            call,
        }
    }

    /// With the variable `name` set to `value`.
    #[must_use]
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }
}

/// What a running command printed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// Raw terminal bytes: CAF's `Screen` works them into
    /// lines.
    Bytes(Vec<u8>),
    /// Lines already worked out, for apps with their own terminal emulator:
    /// the lines from `from` on are now `lines`, as a
    /// `Redraw` says.
    Lines {
        /// The first line replaced, numbered from 0.
        from: u64,
        /// The lines from there, each with its colours as SGR codes that
        /// start from the default style and reset at its end.
        lines: Vec<String>,
    },
}

/// A command that's running.
///
/// Dropping it must stop the command, as [`stop`](Self::stop) does,
/// without waiting.
pub trait Running: Send {
    /// How it ended, and the folder its shell ended in if known.
    fn wait(&mut self) -> BoxFuture<Ended>;
    /// Ctrl-C to its foreground (process group).
    fn interrupt(&mut self);
    /// Stop it: SIGTERM to the group, a grace period, then SIGKILL.
    fn stop(&mut self) -> BoxFuture<()>;
    /// Where the user can watch it, for the model and the record, such as
    /// "terminal 7".
    fn place(&self) -> Option<String>;
}

/// How a command ended.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    /// How it exited, if that's known.
    pub exit: Option<Exit>,
    /// The folder its shell ended in, if that's known: where the chat's
    /// next command starts.
    pub folder: Option<PathBuf>,
}

impl Ended {
    /// It ended as `exit` says, in `folder`.
    #[must_use]
    pub fn new(exit: Option<Exit>, folder: Option<PathBuf>) -> Self {
        Self { exit, folder }
    }
}
