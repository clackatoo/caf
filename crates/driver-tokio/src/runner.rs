//! [`CafRunner`]: the runner of CAF's own tools, Read, Edit, Write and
//! Bash, through the app's backends ([`Files`], [`Commands`]).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::future::Future;
use std::io::{Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use caf_core::event::Exit;
use caf_core::ids::{CallId, ChatId};
use caf_core::mcp::{Answer, AnswerPart};
use caf_core::tool::{Action, Ending, Image, Lines, Output as OutputPiece, Piece, Text};
use caf_tools::{
    CommandSpec, Commands, EditError, Edited, FileError, FileRead, FileTool, Files, JobEnd,
    MOST_ANSWER, MOST_JOBS, ReadState, Reading, Screen, Version,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::tools::{Call, Preview, Ran, Runner};

/// How much of a command's output, in bytes of its lines, is kept in
/// memory before it's written to its file.
const MOST_KEPT: usize = 256 * 1024;
/// How often a job's output file is brought up to date, at most.
const FLUSH_EVERY: Duration = Duration::from_millis(250);

/// Runs CAF's own tools, as `caf_tools` says they behave, reading and
/// writing files through [`Files`] and running commands through
/// [`Commands`].
///
/// - **Read state is each chat's**, and this run's: a file must be read
///   before it's changed, and not have changed since.
/// - **An edit is worked out as it's previewed,** against the file's
///   version then, and written expecting that version: a change between
///   the preview and the run makes it stale.
/// - **Output too large to answer whole,** and a job's, goes to a file
///   in `out/<chat>/` of the tools' folder, only the user's, removed when
///   the chat is let go, or the runner is.
pub(crate) struct CafRunner {
    files: Arc<dyn Files>,
    commands: Arc<dyn Commands>,
    shared: Arc<Mutex<Shared>>,
}

/// What the runner keeps, shared with its calls' futures.
struct Shared {
    /// `out/` in the tools' folder.
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
    /// `out`.
    pub(crate) fn new(files: Arc<dyn Files>, commands: Arc<dyn Commands>, out: PathBuf) -> Self {
        Self {
            files,
            commands,
            shared: Arc::new(Mutex::new(Shared {
                out,
                chats: HashMap::new(),
                made: HashSet::new(),
            })),
        }
    }
}

impl Drop for CafRunner {
    fn drop(&mut self) {
        let shared = lock(&self.shared);
        for chat in &shared.made {
            let _ = std::fs::remove_dir_all(chat_folder(&shared.out, *chat));
        }
        // Only if nothing else's is in it.
        let _ = std::fs::remove_dir(&shared.out);
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

impl Runner for CafRunner {
    fn preview(&mut self, call: &Call, action: Action) -> Preview {
        let files = Arc::clone(&self.files);
        let shared = Arc::clone(&self.shared);
        let (chat, id, cwd) = (call.chat(), call.id(), call.cwd().to_owned());
        let input = call.input().clone();
        Box::pin(async move {
            let (path, tool) = match &action {
                Action::Edit { path } => (PathBuf::from(path), FileTool::Edit),
                Action::Write { path } => (PathBuf::from(path), FileTool::Write),
                _ => return Ok(Vec::new()),
            };
            let file = current(files.as_ref(), &path, tool, &cwd).await?;
            let edited = change(&shared, chat, &path, file.as_ref(), tool, &input)
                .map_err(|error| Ran::failed(error.message(&cwd)))?;
            let mut shared = lock(&shared);
            let state = shared.chats.entry(chat).or_default();
            state.previewed.insert(id, edited.expected.clone());
            Ok(vec![Piece::Diff(edited.diff(path.to_string_lossy()))])
        })
    }

    fn run(&mut self, call: Call, action: Action) -> Boxed<Ran> {
        let files = Arc::clone(&self.files);
        let shared = Arc::clone(&self.shared);
        match action {
            Action::Read { path, lines } => Box::pin(read(files, shared, call, path.into(), lines)),
            Action::Edit { path } => Box::pin(change_file(
                files,
                shared,
                call,
                path.into(),
                FileTool::Edit,
            )),
            Action::Write { path } => Box::pin(change_file(
                files,
                shared,
                call,
                path.into(),
                FileTool::Write,
            )),
            Action::Run {
                command,
                folder,
                background,
            } => {
                let commands = Arc::clone(&self.commands);
                Box::pin(bash(commands, shared, call, command, folder, background))
            }
            // TaskStop is the driver's, and no other tool is CAF's.
            _ => Box::pin(std::future::ready(Ran::failed(
                "CAF doesn't run this tool itself",
            ))),
        }
    }

    fn let_go(&mut self, chat: ChatId) {
        let mut shared = lock(&self.shared);
        shared.chats.remove(&chat);
        if shared.made.remove(&chat) {
            let _ = std::fs::remove_dir_all(chat_folder(&shared.out, chat));
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
/// it is: `file`.
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
    let mut shared = lock(shared);
    let state = &shared.chats.entry(chat).or_default().read;
    if tool == FileTool::Write {
        return caf_tools::write(path, file, state, &text(&["content"]));
    }
    let replace_all = input
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    caf_tools::edit(
        path,
        file,
        state,
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
    let cwd = call.cwd();
    let file = match files.read(&path).await {
        Ok(file) => file,
        Err(error) => {
            return Ran::failed(caf_tools::file_error(&error, FileTool::Read, &path, cwd));
        }
    };
    let offset = lines.map(|lines| lines.first);
    let limit = lines.and_then(|lines| lines.last.map(|last| last.saturating_sub(lines.first) + 1));
    let allow_large = (call.input().get("allow_large")).and_then(Value::as_bool) == Some(true);
    match caf_tools::read(&path, &file.bytes, offset, limit, allow_large) {
        Reading::Text(text) => {
            let mut shared = lock(&shared);
            let state = shared.chats.entry(call.chat()).or_default();
            state
                .read
                .record(path, file.version, &file.bytes, text.partial);
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
    }
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
    let edited = match change(&shared, chat, &path, file.as_ref(), tool, call.input()) {
        Ok(edited) => edited,
        Err(error) => return Ran::failed(error.message(&cwd)),
    };
    let written = files
        .write(&path, edited.text(), edited.expected.clone(), id)
        .await;
    match written {
        Ok(version) => {
            let mut shared = lock(&shared);
            let state = shared.chats.entry(chat).or_default();
            state
                .read
                .record(path.clone(), version, &edited.bytes, false);
            Ran::new(
                true,
                vec![Piece::Diff(edited.diff(path.to_string_lossy()))],
                Answer::text(edited.answer(&path)),
            )
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
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
        .map_err(|error| format!("{}: {error}", folder.display()))?;
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
    fn new(make: MakeFile) -> Self {
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
        if let Some((_, file, written)) = &mut self.file {
            let _ = file.seek(SeekFrom::Start(*written));
            if file.write_all(text.as_bytes()).is_ok() {
                *written += text.len() as u64;
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
    let (out, mut printed) = mpsc::unbounded_channel();
    let spec = CommandSpec::new(command.clone(), folder, call.id());
    let started = commands
        .start(
            spec,
            Box::new(move |output| {
                let _ = out.send(output);
            }),
        )
        .await;
    let mut running = match started {
        Ok(running) => running,
        Err(why) => return Ran::failed(why),
    };
    let place = running.place();
    let id = call.id();
    let made = Arc::clone(&shared);
    let mut log = Log::new(Box::new(move || output_file(&made, chat, id)));
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
    let mut ended = running.wait();
    let timed_out = tokio::time::sleep(timeout);
    tokio::pin!(timed_out);
    let ended = loop {
        tokio::select! {
            output = printed.recv() => match output {
                Some(output) => print(&call, &mut log, output),
                // Its backend let go of its output.
                None => break (&mut ended).await,
            },
            ended = &mut ended => break ended,
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
    };
    // What it printed as it ended.
    while let Ok(output) = printed.try_recv() {
        print(&call, &mut log, output);
    }
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
    let mut answer = caf_tools::answer_of(
        &command,
        &text,
        chars,
        ended.exit,
        stopped,
        saved.as_deref(),
        place.as_deref(),
    );
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
    if let Some(folder) = ended
        .folder
        .and_then(|folder| folder.to_str().map(str::to_owned))
    {
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
