//! Bash: what the model is answered of a command, and of the jobs it
//! leaves running in the background.

use std::fmt::Display;
use std::path::Path;
use std::time::Duration;

use caf_core::event::Exit;

use crate::screen::strip_sgr;
use crate::text::format_size;

/// How long a command runs before it's moved to the background, unless
/// the model says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest the model may ask a command to run before it's moved to
/// the background.
pub const MOST_TIMEOUT: Duration = Duration::from_secs(600);
/// The most characters of output the model is answered: past this, it's
/// saved to a file, and the model given its start.
pub const MOST_ANSWER: usize = 30_000;
/// How much of an output too large to answer whole the model is given, in
/// bytes.
pub const PREVIEW: usize = 2048;
/// The most jobs a chat has running at once.
pub const MOST_JOBS: usize = 8;

/// How long a command may run before it's moved to the background, for
/// the `timeout` the model gave, in milliseconds: [`DEFAULT_TIMEOUT`] if
/// it gave none, or 0, and at most [`MOST_TIMEOUT`].
#[must_use]
pub fn timeout(asked: Option<u64>) -> Duration {
    match asked {
        None | Some(0) => DEFAULT_TIMEOUT,
        Some(ms) => Duration::from_millis(ms).min(MOST_TIMEOUT),
    }
}

/// A command's output lines, as the model reads them: without their
/// colours, the blank lines first dropped, and the blanks at the end.
#[must_use]
pub fn output_text(lines: &[String]) -> String {
    let mut text = String::new();
    let mut started = false;
    for line in lines {
        let line = strip_sgr(line);
        if !started && line.trim().is_empty() {
            continue;
        }
        if started {
            text.push('\n');
        }
        started = true;
        text.push_str(&line);
    }
    text.truncate(text.trim_end().len());
    text
}

/// Whether `text`, a command's [`output_text`], is too large to answer
/// whole: over [`MOST_ANSWER`] characters. It's then saved to a file, and
/// [`answer`] given the file.
#[must_use]
pub fn too_large(text: &str) -> bool {
    text.chars().nth(MOST_ANSWER).is_some()
}

/// What the model is answered when a command ended, as Claude Code
/// words it.
///
/// - `text` is its [`output_text`]. If it's [`too_large`], the model gets
///   where it was `saved` and its first 2 KB; if it couldn't be saved
///   (`None`), its first [`MOST_ANSWER`] characters.
/// - A non-zero `exit` adds `Exit code {n}`, and a signal `Killed by
///   signal {n}`. A command `stopped` adds Claude Code's abort error, not
///   how it ended.
/// - If the user can watch it somewhere, its `place`, the answer starts
///   `Ran in {place}`.
#[must_use]
pub fn answer(
    text: &str,
    exit: Option<Exit>,
    stopped: bool,
    saved: Option<&Path>,
    place: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(place) = place {
        parts.push(format!("Ran in {place}"));
    }
    if too_large(text) {
        let size = format_size(text.len() as u64);
        match saved {
            Some(path) => {
                let preview = &text[..text.floor_char_boundary(PREVIEW)];
                parts.push(format!(
                    "Output too large ({size}). Full output saved to: {}\n\nPreview (first 2KB):\n{preview}\n...",
                    path.display()
                ));
            }
            None => {
                let end = text
                    .char_indices()
                    .nth(MOST_ANSWER)
                    .map_or(text.len(), |(at, _)| at);
                parts.push(format!(
                    "{}\n... (output too large: {size}; only its start is shown)",
                    &text[..end]
                ));
            }
        }
    } else if !text.is_empty() {
        parts.push(text.to_owned());
    }
    if stopped {
        parts.push("<error>Command was aborted before completion</error>".to_owned());
    } else {
        match exit {
            Some(Exit::Code(0)) | None => {}
            Some(Exit::Code(code)) => parts.push(format!("Exit code {code}")),
            Some(Exit::Signal(signal)) => parts.push(format!("Killed by signal {signal}")),
            // A way to end CAF doesn't know yet: as if it said nothing.
            Some(_) => {}
        }
    }
    if parts.len() == usize::from(place.is_some()) {
        parts.push("(Bash completed with no output)".to_owned());
    }
    parts.join("\n")
}

/// What the model is answered when it runs a command in the background,
/// as job `job`, its output written to `path`, as Claude Code words it.
#[must_use]
pub fn background(job: impl Display, path: &Path) -> String {
    format!(
        "Command running in background with ID: {job}. Output is being written to: {}.",
        path.display()
    )
}

/// What the model is answered when a command reached its timeout,
/// `after`, and was moved to the background as job `job`, its output
/// written to `path`.
#[must_use]
pub fn timed_out(after: Duration, job: impl Display, path: &Path) -> String {
    format!(
        "Command still running after {}; it continues in the background as job {job}. Output is being written to: {}.",
        format_duration(after),
        path.display()
    )
}

/// `duration` in a few words: `500ms`, `1.5s`, `45s`, `2m`, `2m 30s`.
#[must_use]
pub fn format_duration(duration: Duration) -> String {
    let ms = duration.as_millis();
    if ms < 1000 {
        return format!("{ms}ms");
    }
    if ms < 60_000 {
        let tenths = (ms + 50) / 100;
        return if tenths.is_multiple_of(10) {
            format!("{}s", tenths / 10)
        } else {
            format!("{}.{}s", tenths / 10, tenths % 10)
        };
    }
    let seconds = (ms + 500) / 1000;
    let (minutes, seconds) = (seconds / 60, seconds % 60);
    if seconds == 0 {
        format!("{minutes}m")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

/// What the model is answered when it asks for another job, with
/// [`MOST_JOBS`] running.
#[must_use]
pub fn too_many_jobs() -> String {
    format!("Too many background tasks running ({MOST_JOBS}); stop one with TaskStop first.")
}

/// What TaskStop answers when it stopped job `job`.
#[must_use]
pub fn job_stopped(job: impl Display) -> String {
    format!("Stopped task {job}")
}

/// What TaskStop answers when `job` isn't one of the chat's own.
#[must_use]
pub fn no_job(job: impl Display) -> String {
    format!("No task {job} in this conversation")
}

/// How a job ended, as the notice of it says.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobEnd {
    /// It exited with this code.
    Exited(i32),
    /// It failed: killed by a signal, or it didn't say.
    Failed,
    /// TaskStop, or the chat's end, stopped it.
    Stopped,
}

impl JobEnd {
    /// A command's end: it `exit`ed as this says, unless it was `stopped`.
    #[must_use]
    pub fn of(exit: Option<Exit>, stopped: bool) -> Self {
        match (stopped, exit) {
            (true, _) => Self::Stopped,
            (false, Some(Exit::Code(code))) => Self::Exited(code),
            (false, _) => Self::Failed,
        }
    }
}

/// The notice the agent is sent when job `job`, called `title`, ended, its
/// output in `path`, as Claude Code words it.
#[must_use]
pub fn notice(job: impl Display, title: &str, end: JobEnd, path: &Path) -> String {
    let how = match end {
        JobEnd::Exited(code) => format!("completed with exit code {code}"),
        JobEnd::Failed => "failed".to_owned(),
        JobEnd::Stopped => "was stopped".to_owned(),
    };
    format!(
        "<task-notification>Task {job} ({title}) {how}. Output: {}</task-notification>",
        path.display()
    )
}

#[cfg(test)]
mod tests;
