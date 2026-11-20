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
//! Moving back replays from the start into a fresh core, and moving
//! forward gives the core the steps in between. The budget is
//! [`BUDGET`] to replay a whole run: a seek that takes longer is logged.
//! If real recordings ever break it, the core can be cloned at
//! checkpoints along the way; until then, replaying is all there is.

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

/// How long replaying a whole run may take.
pub const BUDGET: Duration = Duration::from_millis(50);

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
/// what it gave out, or `None` if the event didn't read back.
pub fn take(core: &mut Core, now: Now, event: &RawValue) -> Option<Vec<Directive>> {
    let event: Event = serde_json::from_str(event.get()).ok()?;
    let mut out = Vec::new();
    core.handle(now, event, &mut out);
    Some(out)
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
/// replay. Logs if it took longer than [`BUDGET`].
///
/// # Errors
///
/// If the file can't be read, or isn't a recording of version 2.
pub fn load(path: &Path) -> Result<Arc<Book>, LoadError> {
    let text = std::fs::read_to_string(path).map_err(LoadError::Io)?;
    let (base, lines) = read(&text)?;
    let started = Instant::now();
    let mut core = base.core();
    let steps: Vec<Step> = (lines.into_iter().zip(1..))
        .map(|(line, no)| {
            let directives = take(&mut core, line.now, &line.event)
                .map_or_else(|| to_json(&[]), |out| to_json(&out));
            Step {
                no,
                now: line.now,
                event: line.event,
                directives,
            }
        })
        .collect();
    over_budget("replaying the recording", steps.len(), started.elapsed());
    let book = Book::new(Mode::Replay, Some(path.display().to_string()), None, base);
    book.add(steps);
    Ok(Arc::new(book))
}

/// One of a recording's steps.
#[derive(Deserialize)]
struct Line {
    now: Now,
    event: Box<RawValue>,
}

/// The header of a recording, and its steps, up to where it says it
/// stopped.
fn read(text: &str) -> Result<(Base, Vec<Line>), LoadError> {
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
    let mut steps = Vec::new();
    for (line, no) in lines.zip(2..) {
        if line.trim().is_empty() {
            continue;
        }
        // A recording cut short says so, and a replay stops there.
        if line.starts_with(r#"{"stopped":"#) {
            break;
        }
        steps.push(serde_json::from_str(line).map_err(|error| LoadError::Line(no, error))?);
    }
    Ok((base, steps))
}

/// Logs that something took longer than [`BUDGET`].
pub fn over_budget(what: &str, steps: usize, took: Duration) {
    if took > BUDGET {
        log::warn!(
            "{what} took {} ms for {steps} steps, over the budget of {} ms: \
             checkpoints of the core would make it quicker",
            took.as_millis(),
            BUDGET.as_millis()
        );
    }
}

/// A core at a position, for looking back: what the chats and the core
/// were like then. Each socket has its own, as each may look at a
/// different step.
pub struct Player {
    base: Arc<Base>,
    core: Core,
    /// The position.
    at: u64,
    /// The last change of each chat the app has been sent, as
    /// [`ToViewer::Picture`] or [`ToViewer::Changes`].
    sent: BTreeMap<ChatId, ChangeNo>,
}

impl Player {
    /// A player at 0, the core fresh.
    #[must_use]
    pub fn new(base: Arc<Base>) -> Self {
        Self {
            core: base.core(),
            base,
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

    /// Moves to `to`, given every step from the first up to at least
    /// `to`. Moving back starts again from a fresh core.
    pub fn go(&mut self, to: u64, steps: &[Arc<Step>]) {
        let started = Instant::now();
        if to < self.at {
            self.core = self.base.core();
            self.at = 0;
            // The app's copies are of a later position: each chat is
            // sent whole.
            self.sent.clear();
        }
        let from = usize::try_from(self.at).unwrap_or(usize::MAX);
        let count = usize::try_from(to - self.at).unwrap_or(usize::MAX);
        for step in steps.iter().skip(from).take(count) {
            debug_assert_eq!(step.no, self.at + 1, "steps in order, from the first");
            // An event that doesn't read back is left out, as the live
            // check reports.
            let _ = take(&mut self.core, step.now, &step.event);
            self.at = step.no;
        }
        over_budget("moving to a step", count, started.elapsed());
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
