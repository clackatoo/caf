//! CAF's own tools, Read, Edit, Write and Bash: everything about them
//! that isn't I/O.
//!
//! **This crate does no I/O.** It never touches a file, starts a process
//! or reads a clock. What it has is the tools' rules, as pure functions
//! tested on their own: rendering a read, applying an edit, line endings
//! and encodings, whether a file was read before it's changed, the
//! terminal screen a command's bytes are worked into lines on, and the
//! words the model is answered with. The I/O goes through the backends,
//! [`Files`] and [`Commands`], whose defaults, and the runner that uses
//! them all, live in `caf-driver-tokio`.
//!
//! The model is trained on Claude Code's tools, so CAF's take the same
//! inputs, behave the same way and say the same things, except where
//! CAF's design says otherwise, as each function's docs say.

mod backend;
mod bash;
mod edit;
mod read;
mod screen;
mod state;
mod text;

pub use backend::{
    BoxFuture, CommandSpec, Commands, Ended, FileError, FileRead, FileText, Files, Output, Running,
    Version,
};
pub use bash::{
    DEFAULT_TIMEOUT, JobEnd, MOST_ANSWER, MOST_JOBS, MOST_TIMEOUT, PREVIEW, answer, background,
    job_stopped, no_job, notice, output_text, timed_out, timeout, too_large, too_many_jobs,
};
pub use edit::{EditError, Edited, MOST_EDIT, edit, write};
pub use read::{
    DEFAULT_LIMIT, FileTool, ImageRead, MOST_TOKENS, MOST_TOKENS_LARGE, MOST_WHOLE, Reading,
    TextRead, file_error, missing, read,
};
pub use screen::{COLUMNS, ROWS, Redraw, Screen, strip_sgr};
pub use state::{Check, NOT_READ, ReadState, STALE};
pub use text::format_size;
