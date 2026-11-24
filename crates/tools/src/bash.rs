//! Bash: what the model is answered of a command, and of the jobs it
//! leaves running in the background, as Claude Code 2.1.310 words it.

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
/// The most of an output too large to answer whole the model is given, in
/// characters.
pub const PREVIEW: usize = 2000;
/// The most jobs a chat has running at once.
pub const MOST_JOBS: usize = 8;

/// Claude Code's words after a command moved to the background: CAF
/// sends a notice when it ends, and the model's tool for files is Read.
const NOTIFIED: &str =
    "You will be notified when it completes. To check interim output, use Read on that file path.";

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

/// What the model is answered when `command` ended, as Claude Code words
/// it.
///
/// - `text` is its [`output_text`]. If it's [`too_large`], the model gets
///   where it was `saved` and its first 2,000 characters, cut at a line's
///   end in their second half if there is one; if it couldn't be saved
///   (`None`), as much of its start as fits.
/// - **A command that failed** starts with how it ended: `Exit code {n}`,
///   or `Killed by signal {n}`. Exit 1 from `grep`, `rg`, `egrep`,
///   `fgrep`, `find`, `diff`, `test`, `[`, `git grep` or `git diff`, the
///   last command in the line (quotes kept whole), isn't a failure: it
///   says "no", as Claude Code reads it.
/// - **A command `stopped`** ends with Claude Code's abort error instead,
///   whatever its exit.
/// - **If the user can watch it somewhere,** its `place`, the answer
///   starts `Ran in {place}`.
#[must_use]
pub fn answer(
    command: &str,
    text: &str,
    exit: Option<Exit>,
    stopped: bool,
    saved: Option<&Path>,
    place: Option<&str>,
) -> String {
    answer_of(
        command,
        text,
        text.chars().count(),
        exit,
        stopped,
        saved,
        place,
    )
}

/// [`answer`], for an output of `size` characters of which `text` is the
/// start: for a runner that keeps only so much of a long output, at least
/// its first [`MOST_ANSWER`] and one more, with the rest in the file
/// `saved`.
#[must_use]
pub fn answer_of(
    command: &str,
    text: &str,
    size: usize,
    exit: Option<Exit>,
    stopped: bool,
    saved: Option<&Path>,
    place: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(place) = place {
        parts.push(format!("Ran in {place}"));
    }
    let start = parts.len();
    if !stopped {
        match exit {
            Some(Exit::Code(code)) if exit_failed(command, code) => {
                parts.push(format!("Exit code {code}"));
            }
            Some(Exit::Signal(signal)) => parts.push(format!("Killed by signal {signal}")),
            _ => {}
        }
    }
    if size > MOST_ANSWER || too_large(text) {
        let size = format_size(size.max(text.chars().count()) as u64);
        parts.push(match saved {
            Some(path) => {
                let (preview, more) = preview(text, PREVIEW);
                let more = if more { "\n...\n" } else { "\n" };
                format!(
                    "<persisted-output>\nOutput too large ({size}). Full output saved to: {}\n\nPreview (first 2KB):\n{preview}{more}</persisted-output>",
                    path.display()
                )
            }
            None => {
                let room = MOST_ANSWER.saturating_sub(unsaved(&size, "1023 bytes", "").chars().count());
                let (kept, _) = preview(text, room);
                unsaved(&size, &format_size(kept.chars().count() as u64), kept)
            }
        });
    } else if !text.is_empty() {
        parts.push(text.to_owned());
    }
    if stopped {
        parts.push("<error>Command was aborted before completion</error>".to_owned());
    }
    if parts.len() == start {
        parts.push("(Bash completed with no output)".to_owned());
    }
    parts.join("\n")
}

/// Whether `command` exiting with `code` failed, as Claude Code reads it:
/// by the last command in the line, its redirects aside, and, for git, its
/// subcommand. Exit 1 from `grep` and the like says "no", not that it
/// failed (see [`answer`]).
#[must_use]
pub fn exit_failed(command: &str, code: i32) -> bool {
    let words = words(last_command(command));
    let says_no = match words.first().copied().unwrap_or("") {
        "grep" | "rg" | "egrep" | "fgrep" | "find" | "diff" | "test" | "[" => true,
        "git" => matches!(git_subcommand(&words), Some("diff" | "grep")),
        _ => false,
    };
    if says_no { code >= 2 } else { code != 0 }
}

/// The last command in `command`'s line: after its last `|`, `&`, `;` or
/// line end outside quotes, not escaped, and not part of a redirect, such
/// as `2>&1`, `&>` or `>|`. A `#` starting a word outside quotes starts a
/// comment, to the line's end.
fn last_command(command: &str) -> &str {
    let (mut single, mut double, mut escaped, mut comment) = (false, false, false, false);
    let mut start = 0;
    // Where the command before a comment ends.
    let mut end = None;
    let mut last = "";
    let mut before = ' ';
    let mut chars = command.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let after = chars.peek().map_or(' ', |&(_, c)| c);
        let previous = std::mem::replace(&mut before, c);
        if comment && c != '\n' {
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        let redirect = match c {
            '&' => matches!(previous, '>' | '<') || after == '>',
            '|' => previous == '>',
            _ => false,
        };
        match c {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single
                && !double
                && (previous.is_whitespace() || matches!(previous, ';' | '|' | '&')) =>
            {
                comment = true;
                end = Some(at);
            }
            '|' | '&' | ';' | '\n' if !single && !double && !redirect => {
                let part = &command[start..end.unwrap_or(at)];
                if !part.trim().is_empty() {
                    last = part;
                }
                (start, end, comment) = (at + 1, None, false);
            }
            _ => {}
        }
    }
    let part = &command[start..end.unwrap_or(command.len())];
    if part.trim().is_empty() { last } else { part }
}

/// `command`'s words, its redirects, such as `2>&1`, `>/dev/null` or
/// `> out`, left out.
fn words(command: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut all = command.split_whitespace();
    while let Some(word) = all.next() {
        let rest = word.trim_start_matches(|c: char| c.is_ascii_digit());
        let redirect = rest.starts_with(['<', '>']) || rest.starts_with("&>");
        if !redirect {
            words.push(word);
        } else if rest.trim_end_matches(['<', '>', '|', '&']).is_empty() {
            // Its target is the next word.
            all.next();
        }
    }
    words
}

/// git's subcommand in `words`, a git command's: its first word that isn't
/// an option, past `-C` and `-c`'s values.
fn git_subcommand<'a>(words: &[&'a str]) -> Option<&'a str> {
    let mut words = words.iter().skip(1);
    while let Some(word) = words.next() {
        if word.starts_with('-') {
            if matches!(*word, "-C" | "-c") {
                words.next();
            }
            continue;
        }
        return Some(word);
    }
    None
}

/// The start of `text`, at most `most` characters, cut at the last line
/// end if that's past their half, and whether there's more.
fn preview(text: &str, most: usize) -> (&str, bool) {
    let Some((end, _)) = text.char_indices().nth(most) else {
        return (text, false);
    };
    let start = &text[..end];
    let cut = match start.rfind('\n') {
        Some(at) if start[..at].chars().count() > most / 2 => at,
        _ => end,
    };
    (&text[..cut], true)
}

/// Claude Code's words for an output too large that couldn't be saved,
/// `size` in all, of which `kept`, `shown`, is shown.
fn unsaved(size: &str, shown: &str, kept: &str) -> String {
    format!(
        "<truncated-output>\nOutput too large ({size}). It could not be saved, so only the first {shown} are shown; the rest was dropped. If the tool can page or filter its results, call it again for the part you need.\n\n{kept}\n</truncated-output>"
    )
}

/// What the model is answered when it runs a command in the background,
/// as job `job`, its output written to `path`.
#[must_use]
pub fn background(job: impl Display, path: &Path) -> String {
    format!(
        "Command running in background with ID: {job}. Output is being written to: {}. {NOTIFIED}",
        path.display()
    )
}

/// What the model is answered when a command reached its timeout,
/// `after`, and was moved to the background as job `job`, its output
/// written to `path`.
#[must_use]
pub fn timed_out(after: Duration, job: impl Display, path: &Path) -> String {
    let seconds = ((after.as_millis() + 500) / 1000).max(1);
    format!(
        "Command did not complete within its {seconds}s timeout and was moved to the background (ID: {job}). Output is being written to: {}. {NOTIFIED}",
        path.display()
    )
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

/// The notice the agent is sent when job `job` ended, as Claude Code's:
/// the model's call that started it (`tool_use`, the agent's id for it, if
/// known), what it is (`description`, the call's, or its command), and
/// where its output is (`path`). Each is escaped as XML, so none can end
/// its tag, or forge another part: the agent's id is the agent's to choose.
///
/// A job that exited is "completed", with its code, whatever the code, as
/// Claude Code says it.
#[must_use]
pub fn notice(
    job: impl Display,
    tool_use: Option<&str>,
    description: &str,
    end: JobEnd,
    path: &Path,
) -> String {
    let (status, how) = match end {
        JobEnd::Exited(code) => ("completed", format!("completed (exit code {code})")),
        JobEnd::Failed => ("failed", "failed".to_owned()),
        JobEnd::Stopped => ("killed", "was stopped".to_owned()),
    };
    // Nothing in it can end the tags it's in: the agent's id and the
    // path are anyone's.
    let mut out = format!(
        "<task-notification>\n<task-id>{}</task-id>",
        escape(&job.to_string())
    );
    if let Some(tool_use) = tool_use {
        out.push_str(&format!(
            "\n<tool-use-id>{}</tool-use-id>",
            escape(tool_use)
        ));
    }
    out.push_str(&format!(
        "\n<output-file>{}</output-file>\n<status>{status}</status>\n<summary>Background command \"{}\" {how}</summary>\n</task-notification>",
        escape(&path.display().to_string()),
        escape(description)
    ));
    out
}

/// `text` with `&`, `<` and `>` escaped, so it can't end the tags it's in:
/// for any part of a notice.
#[must_use]
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests;
