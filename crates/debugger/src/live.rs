//! Running CAF live: its steps go into the book, each checked against
//! the replay core.

use std::path::PathBuf;
use std::sync::Arc;

use caf_core::agent::AgentSpec;
use caf_core::host::Grouping;
use caf_core::time::Now;
use caf_core::{Core, Host, NoAppItems, OpenChat};
use caf_driver_tokio::viewer::ViewerRules;
use caf_driver_tokio::{Caf, Config, Running, Step as CafStep, Steps};
use serde_json::value::RawValue;
use tokio::sync::mpsc;

use crate::inspector::{Mode, Step};
use crate::replay::{Base, take, to_json};
use crate::steps::{Book, Diverged};

/// CAF, running, with its steps going into a book.
pub struct Live {
    /// CAF.
    pub caf: Caf,
    /// CAF's task: done once it has shut down.
    pub running: Running,
    /// Its steps.
    pub book: Arc<Book>,
    /// What viewers may do.
    pub rules: Arc<ViewerRules>,
}

/// The host the debugger is to CAF: it shows permission questions and
/// thinking, but no forms or links, and starts an adapter for each chat,
/// so chats with the fake agent, whose script serves one session, don't
/// share one.
#[must_use]
pub fn host() -> Host {
    let mut host = Host::new("caf-debugger", env!("CARGO_PKG_VERSION"));
    host.shows.permission_questions = true;
    host.shows.thinking = true;
    host.shows.forms = false;
    host.shows.links = false;
    host.grouping = Grouping::PerChat;
    host
}

/// What viewers may do: open a chat with any of `agents` in any absolute
/// folder, with no MCP servers, and act on every chat.
#[must_use]
pub fn rules(agents: &[AgentSpec]) -> ViewerRules {
    let names: Vec<_> = agents.iter().map(|agent| agent.name.clone()).collect();
    let mut rules = ViewerRules::default();
    rules.open = Box::new(move |request| {
        if !names.contains(&request.agent) || !request.cwd.is_absolute() {
            return None;
        }
        let mut open = OpenChat::new(request.agent.0.clone(), request.cwd.clone());
        open.session = request.session.clone();
        Some(open)
    });
    rules
}

/// Starts CAF with `agents`, recording to `record` if given, and checking
/// every step against the replay core. Must be called on a tokio
/// runtime.
#[must_use]
pub fn start(agents: Vec<AgentSpec>, record: Option<PathBuf>) -> Live {
    let base = Base {
        host: host(),
        agents: agents.clone(),
    };
    let rules = Arc::new(rules(&agents));
    let mut config = Config::new(base.host.clone(), agents);
    config.record.clone_from(&record);
    let (tap, steps) = mpsc::unbounded_channel();
    let (caf, running) = Caf::start_watching(config, Tap(tap));
    let file = record.map(|path| path.display().to_string());
    let book = Arc::new(Book::new(Mode::Live, file, None, base));
    tokio::spawn(check(Arc::clone(&book), steps));
    Live {
        caf,
        running,
        book,
        rules,
    }
}

/// A step, as CAF's task hands it on.
struct Raw {
    now: Now,
    event: Box<RawValue>,
    directives: Box<RawValue>,
}

/// Takes each step on CAF's task: it writes the event and directives as
/// JSON, which is all the time it takes there, and hands them on.
struct Tap(mpsc::UnboundedSender<Raw>);

impl Steps<NoAppItems> for Tap {
    fn step(&mut self, step: CafStep<'_, NoAppItems>) {
        // An event that won't write is left as `null`, which won't read
        // back, so the check reports it.
        let event = serde_json::value::to_raw_value(step.event).unwrap_or_else(|error| {
            log::error!("a step's event didn't write as JSON: {error}");
            RawValue::NULL.to_owned()
        });
        let raw = Raw {
            now: step.now,
            event,
            directives: to_json(step.directives),
        };
        // Once the checker has gone, so has the debugger.
        let _ = self.0.send(raw);
    }
}

/// Gives every step to the replay core, which must give out the same
/// directives, and adds it to the book.
async fn check(book: Arc<Book>, mut steps: mpsc::UnboundedReceiver<Raw>) {
    let mut core: Option<Core> = Some(book.base.core());
    let mut no = 0;
    while let Some(raw) = steps.recv().await {
        no += 1;
        let diverged = core.as_mut().and_then(|replay| compare(replay, no, &raw));
        book.add([Step {
            no,
            now: raw.now,
            event: raw.event,
            directives: raw.directives,
        }]);
        // After a divergence, the replay core is no copy of CAF's: it
        // stops.
        if let Some(diverged) = diverged {
            book.diverged_at(diverged);
            core = None;
        }
    }
}

/// Gives the replay core a step, and says how it diverged, if it did.
fn compare(core: &mut Core, no: u64, raw: &Raw) -> Option<Diverged> {
    let diverged = |got| Diverged {
        at: no,
        expected: raw.directives.clone(),
        got,
    };
    let Some(got) = take(core, raw.now, &raw.event) else {
        return Some(diverged(None));
    };
    let got = to_json(&got);
    let same = matches!(
        (
            serde_json::from_str::<serde_json::Value>(raw.directives.get()),
            serde_json::from_str::<serde_json::Value>(got.get()),
        ),
        (Ok(expected), Ok(got)) if expected == got
    );
    (!same).then(|| diverged(Some(got)))
}
