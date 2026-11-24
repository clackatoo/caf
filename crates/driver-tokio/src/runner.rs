//! [`CafRunner`]: the runner of CAF's own tools, Read, Edit, Write and
//! Bash, through the app's backends ([`Files`], [`Commands`]).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::future::Future;
use std::io::{Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use caf_core::event::Exit;
use caf_core::ids::{CallId, ChatId};
use caf_core::mcp::{Answer, AnswerPart};
use caf_core::tool::{Action, Ending, Image, Lines, Output as OutputPiece, Piece, Text};
use caf_tools::{
    CommandSpec, Commands, EditError, Edited, FileError, FileRead, FileText, FileTool, Files,
    JobEnd, MOST_ANSWER, MOST_JOBS, ReadState, Reading, Screen, Version,
};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tokio::time::Instant;

use crate::tools::{Call, Preview, Ran, Runner};

/// How much of a command's output, in bytes of its lines, is kept in
/// memory before it's written to its file.
const MOST_KEPT: usize = 256 * 1024;
/// How often a job's output file is brought up to date, at most.
const FLUSH_EVERY: Duration = Duration::from_millis(250);
/// The most of a command's output kept in its file: past this, the file
/// says the rest wasn't kept.
pub(crate) const MOST_SAVED: u64 = 64 * 1024 * 1024;
/// The line a file of output ends with once it's full.
const NOT_KEPT: &str = "[output beyond 64 MiB not kept]\n";
/// What the model and the record are told of a call whose run panicked.
const PANICKED: &str = "CAF's tool failed unexpectedly";

/// Runs CAF's own tools, as `caf_tools` says they behave, reading and
/// writing files through [`Files`] and running commands through
/// [`Commands`].
///
/// - **Each call runs on a task of its own,** and its heaviest work
///   (reading a file whole, working out an edit) on a blocking thread, so
///   none of it holds up CAF's core.
/// - **Read state is each chat's**, and this run's: a file must be read
///   before it's changed, and not have changed since. Files are known by
///   their real path, symlinks and all.
/// - **An edit is worked out as it's previewed,** against the file's
///   version then, and written expecting that version: a change between
///   the preview and the run makes it stale.
/// - **Output too large to answer whole,** and a job's, goes to a file
///   in `out/<process>/<chat>/` of the tools' folder, only the user's, at
///   most [`MOST_SAVED`], removed when the chat is let go, or the runner
///   is; and those a process that's gone left are removed as the next
///   starts.
pub(crate) struct CafRunner {
    files: Arc<dyn Files>,
    commands: Arc<dyn Commands>,
    shared: Arc<Mutex<Shared>>,
}

/// What the runner keeps, shared with its calls' futures.
struct Shared {
    /// This process's folder in `out/` of the tools' folder.
    out: PathBuf,
    chats: HashMap<ChatId, ChatState>,
    /// The chats' output folders made.
    made: HashSet<ChatId>,
}

#[derive(Default)]
struct ChatState {
    read: ReadState,
    /// The version of the file each change previewed was worked out
    /// against: `None` for a file that wasn't there.
    previewed: HashMap<CallId, Option<Version>>,
    /// How many jobs it has running.
    jobs: usize,
}

impl CafRunner {
    /// A runner through `files` and `commands`, its output files in
    /// `out`, a folder of `outs` that's this process's alone, named for
    /// it: those of processes that are gone are removed.
    pub(crate) fn new(
        files: Arc<dyn Files>,
        commands: Arc<dyn Commands>,
        outs: &Path,
        out: &str,
    ) -> Self {
        sweep(outs);
        Self {
            files,
            commands,
            shared: Arc::new(Mutex::new(Shared {
                out: outs.join(out),
                chats: HashMap::new(),
                made: HashSet::new(),
            })),
        }
    }
}

/// Removes the folders in `outs` of processes that are gone: each named
/// for its process's id, and more after a `-`.
fn sweep(outs: &Path) {
    let Ok(entries) = std::fs::read_dir(outs) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = (name.to_str())
            .and_then(|name| name.split('-').next())
            .and_then(|pid| pid.parse::<libc::pid_t>().ok())
            .filter(|&pid| pid > 0)
        else {
            continue;
        };
        #[allow(unsafe_code)]
        // SAFETY: signal 0 only asks whether the process is there.
        let gone = unsafe { libc::kill(pid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

impl Drop for CafRunner {
    fn drop(&mut self) {
        let shared = lock(&self.shared);
        let _ = std::fs::remove_dir_all(&shared.out);
        // Only if nothing else's is in it.
        if let Some(outs) = shared.out.parent() {
            let _ = std::fs::remove_dir(outs);
        }
    }
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

fn chat_folder(out: &Path, chat: ChatId) -> PathBuf {
    out.join(chat.0.to_string())
}

/// A future of a call's.
type Boxed<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// `future`, run on a task of its own: stopped when what's given back is
/// dropped.
fn on_a_task<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
    panicked: impl FnOnce() -> T + Send + 'static,
) -> Boxed<T> {
    let task = tokio::spawn(future);
    let stop = AbortOnDrop(task.abort_handle());
    Box::pin(async move {
        let _stop = stop;
        match task.await {
            Ok(done) => done,
            Err(_) => {
                log::warn!(target: "caf", "one of CAF's tools panicked");
                panicked()
            }
        }
    })
}

/// Stops a task when it's dropped.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `work`, on a blocking thread.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(done) => done,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}

/// The path `path` is known by in read state: its real one, its folder's
/// symlinks followed, or as it's given if there's no such folder.
fn known_as(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map_or_else(|_| path.to_owned(), |parent| parent.join(name)),
        _ => path.to_owned(),
    }
}

impl Runner for CafRunner {
    fn preview(&mut self, call: &Call, action: Action) -> Preview {
        let files = Arc::clone(&self.files);
        let shared = Arc::clone(&self.shared);
        let (chat, id, cwd) = (call.chat(), call.id(), call.cwd().to_owned());
        let input = call.input().clone();
        on_a_task(
            async move {
                let (path, tool) = match &action {
                    Action::Edit { path } => (PathBuf::from(path), FileTool::Edit),
                    Action::Write { path } => (PathBuf::from(path), FileTool::Write),
                    _ => return Ok(Vec::new()),
                };
                let file = current(files.as_ref(), &path, tool, &cwd).await?;
                let (edited, diff) = blocking({
                    let shared = Arc::clone(&shared);
                    move || {
                        let edited = change(&shared, chat, &path, file.as_ref(), tool, &input)?;
                        let diff = edited.diff(path.to_string_lossy());
                        Ok::<_, EditError>((edited, diff))
                    }
                })
                .await
                .map_err(|error| Ran::failed(error.message(&cwd)))?;
                let mut shared = lock(&shared);
                let state = shared.chats.entry(chat).or_default();
                state.previewed.insert(id, edited.expected.clone());
                Ok(vec![Piece::Diff(diff)])
            },
            || Err(Ran::failed(PANICKED)),
        )
    }

    fn run(&mut self, call: Call, action: Action) -> Boxed<Ran> {
        let files = Arc::clone(&self.files);
        let shared = Arc::clone(&self.shared);
        let panicked = || Ran::failed(PANICKED);
        match action {
            Action::Read { path, lines } => {
                on_a_task(read(files, shared, call, path.into(), lines), panicked)
            }
            Action::Edit { path } => on_a_task(
                change_file(files, shared, call, path.into(), FileTool::Edit),
                panicked,
            ),
            Action::Write { path } => on_a_task(
                change_file(files, shared, call, path.into(), FileTool::Write),
                panicked,
            ),
            Action::Run {
                command,
                folder,
                background,
            } => {
                let commands = Arc::clone(&self.commands);
                on_a_task(
                    bash(commands, shared, call, command, folder, background),
                    panicked,
                )
            }
            // TaskStop is the driver's, and no other tool is CAF's.
            _ => Box::pin(std::future::ready(Ran::failed(
                "CAF doesn't run this tool itself",
            ))),
        }
    }

    fn forget(&mut self, call: CallId) {
        for state in lock(&self.shared).chats.values_mut() {
            state.previewed.remove(&call);
        }
    }

    fn let_go(&mut self, chat: ChatId) {
        let folder = {
            let mut shared = lock(&self.shared);
            shared.chats.remove(&chat);
            shared
                .made
                .remove(&chat)
                .then(|| chat_folder(&shared.out, chat))
        };
        // Not on CAF's own task.
        if let Some(folder) = folder {
            tokio::task::spawn_blocking(move || {
                let _ = std::fs::remove_dir_all(folder);
            });
        }
    }
}

/// The file at `path` as it is now, `None` if there's none: or the call's
/// failure, for `tool`.
async fn current(
    files: &dyn Files,
    path: &Path,
    tool: FileTool,
    cwd: &Path,
) -> Result<Option<FileRead>, Ran> {
    match files.read(path).await {
        Ok(file) => Ok(Some(file)),
        Err(FileError::NotFound) => Ok(None),
        // Edit says so of a file too big to edit, Write of one it would
        // replace without having read.
        Err(error) => Err(Ran::failed(caf_tools::file_error(&error, tool, path, cwd))),
    }
}

/// The change `tool` makes, as `input` asks, to the file at `path`, as
/// it is: `file`. On a blocking thread: it's the heavy part.
fn change(
    shared: &Mutex<Shared>,
    chat: ChatId,
    path: &Path,
    file: Option<&FileRead>,
    tool: FileTool,
    input: &Value,
) -> Result<Edited, EditError> {
    let text = |names: &[&str]| {
        (names.iter())
            .find_map(|name| input.get(*name).and_then(Value::as_str))
            .unwrap_or("")
            .to_owned()
    };
    let known = known_as(path);
    // A copy, so the change is worked out without the runner held.
    let state = lock(shared).chats.entry(chat).or_default().read.clone();
    if tool == FileTool::Write {
        return caf_tools::write(&known, file, &state, &text(&["content"]));
    }
    let replace_all = input
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    caf_tools::edit(
        &known,
        file,
        &state,
        &text(&["old_string", "old_str"]),
        &text(&["new_string", "new_str"]),
        replace_all,
    )
}

/// Read.
async fn read(
    files: Arc<dyn Files>,
    shared: Arc<Mutex<Shared>>,
    call: Call,
    path: PathBuf,
    lines: Option<Lines>,
) -> Ran {
    let cwd = call.cwd().to_owned();
    let file = match files.read(&path).await {
        Ok(file) => file,
        Err(error) => {
            return Ran::failed(caf_tools::file_error(&error, FileTool::Read, &path, &cwd));
        }
    };
    let offset = lines.map(|lines| lines.first);
    let limit = lines.and_then(|lines| lines.last.map(|last| last.saturating_sub(lines.first) + 1));
    let allow_large = (call.input().get("allow_large")).and_then(Value::as_bool) == Some(true);
    let chat = call.chat();
    blocking(
        move || match caf_tools::read(&path, &file.bytes, offset, limit, allow_large) {
            Reading::Text(text) => {
                let known = known_as(&path);
                let mut file = file;
                if file.sha256.is_none() {
                    file = file.clone().with_sha256(Sha256::digest(&file.bytes).into());
                }
                let mut shared = lock(&shared);
                let state = shared.chats.entry(chat).or_default();
                state.read.record_read(known, &file, text.partial);
                Ran::new(
                    true,
                    vec![Piece::Text(Text::new(text.text))],
                    Answer::text(text.answer),
                )
            }
            Reading::Image(image) => {
                let piece = Image::new(image.mime, &file.bytes);
                let answer = match &piece.data {
                    Some(data) => Answer::new(
                        vec![AnswerPart::Image {
                            mime: image.mime.to_owned(),
                            data: data.clone(),
                        }],
                        false,
                    ),
                    None => Answer::error("This image is too large to read."),
                };
                Ran::new(piece.data.is_some(), vec![Piece::Image(piece)], answer)
            }
            Reading::Refused(why) => Ran::failed(why),
            _ => Ran::failed("CAF can't read this file"),
        },
    )
    .await
}

/// Edit and Write: the change previewed, written expecting the version
/// it was worked out against.
async fn change_file(
    files: Arc<dyn Files>,
    shared: Arc<Mutex<Shared>>,
    call: Call,
    path: PathBuf,
    tool: FileTool,
) -> Ran {
    let (chat, id, cwd) = (call.chat(), call.id(), call.cwd().to_owned());
    let previewed = lock(&shared)
        .chats
        .get_mut(&chat)
        .and_then(|state| state.previewed.remove(&id));
    let file = match current(files.as_ref(), &path, tool, &cwd).await {
        Ok(file) => file,
        Err(failed) => return failed,
    };
    // Changed since its preview: what was shown isn't what would be
    // written.
    if let Some(expected) = &previewed
        && file.as_ref().map(|file| &file.version) != expected.as_ref()
    {
        return Ran::failed(caf_tools::STALE);
    }
    let input = call.input().clone();
    let worked_out = blocking({
        let (shared, path) = (Arc::clone(&shared), path.clone());
        move || {
            let edited = change(&shared, chat, &path, file.as_ref(), tool, &input)?;
            let diff = edited.diff(path.to_string_lossy());
            let answer = edited.answer(&path);
            Ok::<_, EditError>((edited, diff, answer))
        }
    })
    .await;
    let (edited, diff, answer) = match worked_out {
        Ok(worked_out) => worked_out,
        Err(error) => return Ran::failed(error.message(&cwd)),
    };
    let expected = edited.expected.clone();
    let bytes = edited.bytes;
    let (sha256, bytes): ([u8; 32], Vec<u8>) =
        blocking(move || (Sha256::digest(&bytes).into(), bytes)).await;
    let written = files.write(&path, FileText::new(bytes), expected, id).await;
    match written {
        Ok(version) => {
            // What it wrote is what it's seen.
            blocking(move || {
                let known = known_as(&path);
                let mut shared = lock(&shared);
                let state = shared.chats.entry(chat).or_default();
                state.read.record_sha256(known, version, sha256, false);
            })
            .await;
            Ran::new(true, vec![Piece::Diff(diff)], Answer::text(answer))
        }
        Err(error) => Ran::failed(caf_tools::file_error(&error, tool, &path, &cwd)),
    }
}

/// A chat's running job, counted until it's dropped.
struct JobCount {
    shared: Arc<Mutex<Shared>>,
    chat: ChatId,
}

impl JobCount {
    /// One more of `chat`'s jobs, unless it has [`MOST_JOBS`] running.
    fn take(shared: &Arc<Mutex<Shared>>, chat: ChatId) -> Option<Self> {
        let mut locked = lock(shared);
        let state = locked.chats.entry(chat).or_default();
        if state.jobs >= MOST_JOBS {
            return None;
        }
        state.jobs += 1;
        Some(Self {
            shared: Arc::clone(shared),
            chat,
        })
    }
}

impl Drop for JobCount {
    fn drop(&mut self) {
        if let Some(state) = lock(&self.shared).chats.get_mut(&self.chat) {
            state.jobs = state.jobs.saturating_sub(1);
        }
    }
}

/// The file a call's output is saved to: made, only the user's, in its
/// chat's folder.
fn output_file(
    shared: &Mutex<Shared>,
    chat: ChatId,
    call: CallId,
) -> Result<(PathBuf, File), String> {
    let mut shared = lock(shared);
    let folder = chat_folder(&shared.out, chat);
    let made = (|| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&folder)?;
        // Made before, it may not have been the user's alone.
        for each in [&shared.out, &folder] {
            std::fs::set_permissions(each, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok::<_, std::io::Error>(())
    })();
    made.map_err(|error| format!("{}: {error}", folder.display()))?;
    shared.made.insert(chat);
    let path = folder.join(format!("{}.txt", call.0));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok((path, file))
}

/// Makes the file a command's output is saved to.
type MakeFile = Box<dyn FnMut() -> Result<(PathBuf, File), String> + Send>;

/// A command's output, as its screen draws it: kept in memory while it's
/// small, then in its file, but for its start, for the model.
struct Log {
    screen: Screen,
    /// The lines from `from` on, as the screen last drew them.
    lines: Vec<String>,
    from: u64,
    make: MakeFile,
    /// Its file, once it has one, and how many bytes of the lines before
    /// `from` are in it.
    file: Option<(PathBuf, File, u64)>,
    /// Whether lines have been moved out of memory, to the file if there
    /// is one: then the model's text is `head`.
    settled: bool,
    /// The start of the lines moved out, for the model: at least
    /// [`MOST_ANSWER`] characters and one more if there's that much; and
    /// how many characters there are in all.
    head: String,
    chars: usize,
    /// Whether a line with text has been moved out: blank lines before it
    /// aren't the model's.
    started: bool,
    /// Whether the lines kept have changed since they were last written.
    dirty: bool,
    flushed: Instant,
    /// The most its file keeps, and whether it's full: then nothing more
    /// is written to it.
    most: u64,
    full: bool,
}

impl Drop for Log {
    /// A job stopped still leaves its file as it was last drawn.
    fn drop(&mut self) {
        if self.dirty {
            self.flush();
        }
    }
}

impl Log {
    fn new(make: MakeFile, most: u64) -> Self {
        Self {
            screen: Screen::new(),
            lines: Vec::new(),
            from: 0,
            make,
            file: None,
            settled: false,
            head: String::new(),
            chars: 0,
            started: false,
            dirty: false,
            flushed: Instant::now(),
            most,
            full: false,
        }
    }

    /// The lines from `from` on are now `lines`: kept, and those that
    /// have scrolled out of the screen's reach moved to the file, if it
    /// has one or they're too many to keep.
    fn redraw(&mut self, from: u64, lines: &[String]) {
        // The screen never redraws what it let go of.
        let at = usize::try_from(from.saturating_sub(self.from)).unwrap_or(usize::MAX);
        self.lines.truncate(at);
        self.lines.extend(lines.iter().cloned());
        self.dirty = true;
        if self.file.is_none() && !self.settled {
            let kept: usize = self.lines.iter().map(String::len).sum();
            if kept <= MOST_KEPT {
                return;
            }
            self.make_file();
        }
        self.settle(self.lines.len().saturating_sub(caf_tools::ROWS));
    }

    /// Makes its file, if it can: if not, what's moved out of memory is
    /// lost but for its start.
    fn make_file(&mut self) {
        if self.file.is_some() || self.settled {
            return;
        }
        match (self.make)() {
            Ok((path, file)) => self.file = Some((path, file, 0)),
            Err(why) => {
                log::warn!(target: "caf", "a command's output couldn't be saved: {why}");
            }
        }
    }

    /// Moves its first `n` lines, which the screen won't redraw, out of
    /// memory, into the file if it has one.
    fn settle(&mut self, n: usize) {
        let n = n.min(self.lines.len());
        if n == 0 {
            return;
        }
        self.settled = true;
        let moved: Vec<String> = self.lines.drain(..n).collect();
        self.from += n as u64;
        let text = self.plain(&moved);
        let (most, full) = (self.most, &mut self.full);
        if let Some((_, file, written)) = &mut self.file
            && !*full
        {
            let _ = file.seek(SeekFrom::Start(*written));
            let fits = *written + text.len() as u64 <= most;
            // As much as fits, in whole lines, then a line saying so.
            let kept = if fits {
                text.as_str()
            } else {
                let room = usize::try_from(most - *written).unwrap_or(usize::MAX);
                let mut end = text.len().min(room);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text[..end].rfind('\n').map_or("", |at| &text[..=at])
            };
            if file.write_all(kept.as_bytes()).is_ok() {
                *written += kept.len() as u64;
            }
            if !fits {
                *full = true;
                if file.write_all(NOT_KEPT.as_bytes()).is_ok() {
                    *written += NOT_KEPT.len() as u64;
                }
            }
            let _ = file.set_len(*written);
        }
    }

    /// `lines` as the file has them, counted into what the model's told.
    fn plain(&mut self, lines: &[String]) -> String {
        let mut text = String::new();
        for line in lines {
            let line = caf_tools::strip_sgr(line);
            if !self.started && line.trim().is_empty() {
                continue;
            }
            self.started = true;
            text.push_str(&line);
            text.push('\n');
        }
        self.chars += text.chars().count();
        let room = (MOST_ANSWER + 1).saturating_sub(self.head.len());
        if room > 0 {
            self.head.extend(text.chars().take(room));
        }
        text
    }

    /// It's a job: its output is all in its file from now on, which is
    /// made now if it isn't yet. Its path, if it has one.
    fn keep_in_file(&mut self) -> Option<PathBuf> {
        self.make_file();
        let path = self.file.as_ref().map(|(path, _, _)| path.clone())?;
        self.settle(self.lines.len().saturating_sub(caf_tools::ROWS));
        self.flush();
        Some(path)
    }

    /// The file has the lines the screen may still redraw too, for now.
    fn flush(&mut self) {
        self.flushed = Instant::now();
        self.dirty = false;
        let Some((_, file, written)) = &mut self.file else {
            return;
        };
        let tail: String = (self.lines.iter())
            .map(|line| caf_tools::strip_sgr(line) + "\n")
            .collect();
        // Full, or would be: what's still drawn waits to be settled.
        if self.full || *written + tail.len() as u64 > self.most {
            return;
        }
        let _ = file.seek(SeekFrom::Start(*written));
        let _ = file.write_all(tail.as_bytes());
        let _ = file.set_len(*written + tail.len() as u64);
    }

    /// The output has ended: its text for the model, how many characters
    /// it has, and its file, if it has one, which is made now if it's too
    /// large to answer whole.
    fn finish(&mut self) -> (String, usize, Option<PathBuf>) {
        if !self.settled {
            let text = caf_tools::output_text(&self.lines);
            let chars = text.chars().count();
            if chars <= MOST_ANSWER {
                return (text, chars, None);
            }
            self.make_file();
        }
        self.settle(self.lines.len());
        let path = self.file.as_ref().map(|(path, _, _)| path.clone());
        (self.head.trim_end().to_owned(), self.chars, path)
    }
}

/// Bash: runs `command` in `folder`, answering the model when it ends, or
/// at once and going on as a job if it's run in the `background`, or when
/// it reaches its timeout.
async fn bash(
    commands: Arc<dyn Commands>,
    shared: Arc<Mutex<Shared>>,
    mut call: Call,
    command: String,
    folder: String,
    background: bool,
) -> Ran {
    let chat = call.chat();
    let input = call.input();
    let timeout = caf_tools::timeout(input.get("timeout").and_then(Value::as_u64));
    let description = (input.get("description").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(&command)
        .to_owned();
    let mut job_count = None;
    if background {
        match JobCount::take(&shared, chat) {
            Some(count) => job_count = Some(count),
            None => return Ran::failed(caf_tools::too_many_jobs()),
        }
    }
    // A folder gone since the last command ended there: it starts again
    // in the chat's, and the model's told.
    let mut folder = PathBuf::from(folder);
    let mut reset = None;
    if !folder.is_dir() {
        reset = Some(format!(
            "The shell's folder, {}, no longer exists, so this ran in {} instead.",
            folder.display(),
            call.cwd().display()
        ));
        folder = call.cwd().to_owned();
    }
    let spec = CommandSpec::new(command.clone(), &folder, call.id());
    let mut running = match commands.start(spec).await {
        Ok(running) => running,
        Err(why) => return Ran::failed(why),
    };
    let place = running.place();
    let id = call.id();
    let made = Arc::clone(&shared);
    let mut log = Log::new(Box::new(move || output_file(&made, chat, id)), MOST_SAVED);
    let mut job = None;
    if background {
        match log.keep_in_file() {
            Some(path) => {
                let answered =
                    call.answer_now(|job| Answer::text(caf_tools::background(job.0, &path)));
                job = Some((answered, path));
            }
            None => {
                running.stop().await;
                return Ran::failed("CAF couldn't make the command's output file");
            }
        }
    }
    let mut stopped = false;
    let timed_out = tokio::time::sleep(timeout);
    tokio::pin!(timed_out);
    let mut output = running.output();
    loop {
        tokio::select! {
            printed = &mut output => match printed {
                Some(printed) => {
                    print(&call, &mut log, printed);
                    output = running.output();
                }
                None => break,
            },
            // A job's file grows as it runs.
            () = tokio::time::sleep_until(log.flushed + FLUSH_EVERY), if job.is_some() && log.dirty => {
                log.flush();
            }
            () = &mut timed_out, if job.is_none() && !stopped => {
                let count = JobCount::take(&shared, chat);
                match count.as_ref().and_then(|_| log.keep_in_file()) {
                    Some(path) => {
                        job_count = count;
                        let answered = call.answer_now(|job| {
                            Answer::text(caf_tools::timed_out(timeout, job.0, &path))
                        });
                        job = Some((answered, path));
                    }
                    // It can't go on in the background: it's stopped.
                    None => {
                        stopped = true;
                        tokio::spawn(running.stop());
                    }
                }
            }
        }
    }
    let ended = running.wait().await;
    if let Some(redraw) = log.screen.finish() {
        log.redraw(redraw.from, &redraw.lines);
        call.print(redraw.from, redraw.lines);
    }
    let (text, chars, saved) = log.finish();
    drop(job_count);
    let record = vec![Piece::Output(OutputPiece::ended(
        ended.exit.map_or(Ending::Unknown, Ending::Exited),
    ))];
    let done = !stopped
        && match ended.exit {
            Some(Exit::Code(code)) => !caf_tools::exit_failed(&command, code),
            _ => false,
        };
    if let Some((job, path)) = job {
        let end = JobEnd::of(ended.exit, stopped);
        let notice = caf_tools::notice(job.0, call.agent_call(), &description, end, &path);
        return Ran::new(done, record, Answer::text("")).with_notice(notice);
    }
    let answered = caf_tools::answer_of(
        &command,
        &text,
        chars,
        ended.exit,
        stopped,
        saved.as_deref(),
        place.as_deref(),
    );
    let mut answer = match &reset {
        Some(reset) => format!("{reset}\n{answered}"),
        None => answered,
    };
    if stopped {
        answer.push_str(&format!(
            "\nCommand reached its {}s timeout and was stopped, as it couldn't be moved to the background: {}",
            timeout.as_secs(),
            caf_tools::too_many_jobs()
        ));
    }
    let mut ran = Ran::new(
        done,
        record,
        Answer::new(vec![AnswerPart::Text(answer)], !done),
    );
    // Where it ended, or where it started if that's not known: so a
    // folder that's gone isn't tried again.
    let ended_in = ended.folder.or_else(|| reset.is_some().then_some(folder));
    if let Some(folder) = ended_in.and_then(|folder| folder.to_str().map(str::to_owned)) {
        ran = ran.with_folder(folder);
    }
    ran
}

/// What a command printed: worked into lines, printed for the record, and
/// kept.
fn print(call: &Call, log: &mut Log, output: caf_tools::Output) {
    let (from, lines) = match output {
        caf_tools::Output::Bytes(bytes) => match log.screen.feed(&bytes) {
            Some(redraw) => (redraw.from, redraw.lines),
            None => return,
        },
        caf_tools::Output::Lines { from, lines } => (from, lines),
        _ => return,
    };
    log.redraw(from, &lines);
    call.print(from, lines);
}

#[cfg(test)]
mod tests;
