//! The replay core: a [`Core`] of the debugger's own, given CAF's steps.
//!
//! Every step's event is kept as the JSON it was written as, and read
//! back each time a core takes it, as a replay of a recording does. So
//! the replay core is what anyone replaying CAF's events would get:
//!
//! - **live**, it checks CAF's core gave out exactly what a replay does
//!   ([`crate::live`]);
//! - **in replay**, it gives the directives, since a recording holds only
//!   events ([`load`]);
//! - **both**, it shows the core and the chats as they were at any step
//!   ([`Player`]).
//!
//! Moving to a step starts from the nearest copy of the core before it
//! (see [`Book`] for how they're kept), or from a fresh core, and gives
//! it the steps in between; moving forward a little just gives it the
//! next ones. So a move replays at most the copies' spacing:
//! [`CHECKPOINT`] steps, until the run is [`MOST_COPIES`] times that long,
//! and after that a 32nd to a 64th of the run.
//!
//! **The budget** is [`BUDGET`] for any one move. A move replays at most
//! [`CHECKPOINT`] steps in a run of up to 64,000, so that holds while
//! replaying keeps up a rate of [`PER_STEP`] or better. In a longer run,
//! moves replay more, and may take longer. The first time a move, or
//! loading a recording, is slower than its budget, it's logged, once a
//! run.
//!
//! [`CHECKPOINT`]: crate::steps::CHECKPOINT
//! [`MOST_COPIES`]: crate::steps::MOST_COPIES

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use caf_core::agent::AgentSpec;
use caf_core::ids::{ChangeNo, ChatId};
use caf_core::time::Now;
use caf_core::viewer::{ChatSummary, ToViewer};
use caf_core::{Core, Directive, Event, Host, Read};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::inspector::{Mode, Step};
use crate::steps::Book;

/// How long one move to a step may take.
pub const BUDGET: Duration = Duration::from_millis(50);

/// How long replaying one step may take, on average: so the most a move
/// replays, [`CHECKPOINT`](crate::steps::CHECKPOINT) steps, fits in [`BUDGET`].
pub const PER_STEP: Duration = Duration::from_micros(50);

/// The version of recordings the debugger reads.
const VERSION: u32 = 2;

/// What a core is made with: a recording's first line.
#[derive(Clone, Debug, Deserialize)]
pub struct Base {
    /// The host.
    pub host: Host,
    /// The agents.
    pub agents: Vec<AgentSpec>,
}

impl Base {
    /// A fresh core.
    #[must_use]
    pub fn core(&self) -> Core {
        Core::new(self.host.clone(), self.agents.clone())
    }
}

/// Gives a core one step's event, read back from its JSON, and returns
/// what it gave out.
///
/// # Errors
///
/// If the event doesn't read back, when the core is given nothing.
pub fn take(core: &mut Core, now: Now, event: &RawValue) -> serde_json::Result<Vec<Directive>> {
    let event: Event = serde_json::from_str(event.get())?;
    let mut out = Vec::new();
    core.handle(now, event, &mut out);
    Ok(out)
}

/// The directives, as JSON.
///
/// # Panics
///
/// If they don't serialise, which CAF's types always do.
#[must_use]
pub fn to_json(directives: &[Directive]) -> Box<RawValue> {
    serde_json::value::to_raw_value(directives).expect("directives serialise")
}

/// Why a recording couldn't be replayed.
#[derive(Debug)]
pub enum LoadError {
    /// It couldn't be read.
    Io(std::io::Error),
    /// It has no first line.
    Empty,
    /// Its first line isn't a recording's.
    NotARecording(serde_json::Error),
    /// It's a version the debugger doesn't read.
    Version(u32),
    /// A line, numbered from 1, isn't a step.
    Line(usize, serde_json::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "can't read it: {error}"),
            Self::Empty => write!(f, "it's empty"),
            Self::NotARecording(error) => {
                write!(f, "its first line isn't a CAF recording's: {error}")
            }
            Self::Version(version) => write!(
                f,
                "it's a recording of version {version}, and this debugger reads version {VERSION}"
            ),
            Self::Line(line, error) => write!(f, "line {line} isn't a step: {error}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Reads a recording (see the tokio kit's `Config::record`) and replays
/// it into a fresh core, for the directives it gives out: the steps of a
/// replay. Copies of the core are kept along the way, as [`Book`] says.
///
/// It stops where the recording says it stopped, and before a last line
/// cut off partway, as a recording copied while CAF wrote it ends.
///
/// # Errors
///
/// If the file can't be read, isn't a recording of version 2, or has a
/// line that isn't a step, or whose event isn't one this CAF knows: a
/// replay of it would be wrong from there on.
pub fn load(path: &Path) -> Result<Arc<Book>, LoadError> {
    let text = std::fs::read_to_string(path).map_err(LoadError::Io)?;
    let (base, lines) = read(&text)?;
    let book = Book::new(Mode::Replay, Some(path.display().to_string()), None, base);
    let started = Instant::now();
    let mut core = book.base.core();
    let mut steps = Vec::with_capacity(lines.len());
    for ((file_line, line), no) in lines.into_iter().zip(1..) {
        let out = take(&mut core, line.now, &line.event)
            .map_err(|error| LoadError::Line(file_line, error))?;
        steps.push(Step {
            no,
            now: line.now,
            event: line.event,
            directives: to_json(&out),
        });
        book.checkpoint(no, &core);
    }
    let budget = load_budget(steps.len());
    slow(
        &book,
        "loading the recording",
        steps.len(),
        started.elapsed(),
        budget,
    );
    book.add(steps);
    book.complete();
    Ok(Arc::new(book))
}

/// One of a recording's steps.
#[derive(Deserialize)]
struct Line {
    now: Now,
    event: Box<RawValue>,
}

/// The header of a recording, and its steps, each with its line's number
/// in the file, up to where it says it stopped.
fn read(text: &str) -> Result<(Base, Vec<(usize, Line)>), LoadError> {
    #[derive(Deserialize)]
    struct Header {
        caf_recording: u32,
    }
    let mut lines = text.lines();
    let first = lines.next().ok_or(LoadError::Empty)?;
    let header: Header = serde_json::from_str(first).map_err(LoadError::NotARecording)?;
    if header.caf_recording != VERSION {
        return Err(LoadError::Version(header.caf_recording));
    }
    let base: Base = serde_json::from_str(first).map_err(LoadError::NotARecording)?;
    let whole = text.ends_with('\n');
    let mut lines = lines.zip(2..).peekable();
    let mut steps = Vec::new();
    while let Some((line, no)) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        // A recording cut short says so, and a replay stops there.
        if line.starts_with(r#"{"stopped":"#) {
            break;
        }
        match serde_json::from_str(line) {
            Ok(step) => steps.push((no, step)),
            // The last line, with no end: CAF was still writing it.
            Err(_) if !whole && lines.peek().is_none() => {
                log::warn!("line {no} is cut off partway, so the replay ends before it");
            }
            Err(error) => return Err(LoadError::Line(no, error)),
        }
    }
    Ok((base, steps))
}

/// What loading `steps` may take: [`PER_STEP`] each, and never less than
/// a move's [`BUDGET`].
fn load_budget(steps: usize) -> Duration {
    (PER_STEP.saturating_mul(u32::try_from(steps).unwrap_or(u32::MAX))).max(BUDGET)
}

/// Logs, the first time in a run, that replaying `steps` took longer than
/// `budget`.
fn slow(book: &Book, what: &str, steps: usize, took: Duration, budget: Duration) {
    let over = took > budget;
    if over && book.first_slow() {
        let each = took.as_secs_f64() * 1e6 / steps.max(1) as f64;
        log::warn!(
            "{what} took {} ms for {steps} steps, {each:.1} µs a step, over the budget of {} µs \
             a step and {} ms a move: is this a debug build? `cargo run --release` is quicker. \
             Said once a run.",
            took.as_millis(),
            PER_STEP.as_micros(),
            BUDGET.as_millis()
        );
    }
}

/// A core at a position, for looking back: what the chats and the core
/// were like then. Each socket has its own, as each may look at a
/// different step.
pub struct Player {
    book: Arc<Book>,
    core: Core,
    /// The position.
    at: u64,
    /// The last change of each chat the app has been sent, as
    /// [`ToViewer::Picture`] or [`ToViewer::Changes`].
    sent: BTreeMap<ChatId, ChangeNo>,
}

impl Player {
    /// A player at 0, the core fresh, for the steps in `book`.
    #[must_use]
    pub fn new(book: Arc<Book>) -> Self {
        Self {
            core: book.base.core(),
            book,
            at: 0,
            sent: BTreeMap::new(),
        }
    }

    /// The position.
    #[must_use]
    pub fn at(&self) -> u64 {
        self.at
    }

    /// The core's state at the position.
    #[must_use]
    pub fn state(&self) -> serde_json::Value {
        self.core.inspect()
    }

    /// Moves to `to`, or to the end if that's sooner: from the nearest
    /// copy of the core before it, if that's nearer than the position, or
    /// from a fresh core if moving back before the first copy. Returns
    /// the position.
    pub fn go(&mut self, to: u64) -> u64 {
        let started = Instant::now();
        let to = to.min(self.book.end());
        let back = to < self.at;
        match self.book.nearest(to) {
            Some((at, copy)) if back || at > self.at => {
                self.core = Core::clone(&copy);
                self.at = at;
            }
            _ if back => {
                self.core = self.book.base.core();
                self.at = 0;
            }
            _ => {}
        }
        if back {
            // The app's copies are of a later position: each chat is
            // sent whole.
            self.sent.clear();
        }
        let steps = self.book.range(self.at, to);
        for step in &steps {
            debug_assert_eq!(step.no, self.at + 1, "steps in order");
            // An event that doesn't read back is left out, as the live
            // check reports, and a replay refuses.
            let _ = take(&mut self.core, step.now, &step.event);
            self.at = step.no;
        }
        slow(
            &self.book,
            "moving to a step",
            steps.len(),
            started.elapsed(),
            BUDGET,
        );
        self.at
    }

    /// Forgets what the app was sent, so each chat is sent whole next.
    pub fn forget(&mut self) {
        self.sent.clear();
    }

    /// The chats at the position, as the app is sent them: every chat
    /// the core has, closed ones too, then each one's changes since it
    /// was last sent, or the chat whole.
    pub fn past(&mut self) -> Vec<ToViewer> {
        let all = self.core.chats().map(ChatSummary::of).collect();
        let mut out = vec![ToViewer::Chats(all)];
        let ids: Vec<ChatId> = self.core.chats().map(|chat| chat.id).collect();
        for chat in ids {
            let since = self.sent.get(&chat).copied();
            match self.core.read(chat, since) {
                Some(Read::Changes(changes)) => {
                    if let Some(last) = changes.last() {
                        self.sent.insert(chat, last.no);
                        out.push(ToViewer::Changes {
                            chat,
                            changes: changes.to_vec(),
                        });
                    }
                }
                Some(Read::Picture {
                    chat: whole,
                    latest,
                }) => {
                    self.sent.insert(chat, latest);
                    out.push(ToViewer::Picture {
                        chat: whole.clone(),
                        latest,
                    });
                }
                _ => {}
            }
        }
        out
    }
}
