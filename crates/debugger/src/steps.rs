//! The book of steps: every step so far, shared by every socket.

use std::sync::{Arc, Mutex, PoisonError, RwLock};

use serde_json::value::RawValue;
use tokio::sync::watch;

use crate::inspector::{Mode, Step, ToInspector};
use crate::replay::Base;

/// Every step so far, and what's known about them. Live, the hub adds
/// each step as CAF takes it; in replay, they're all there from the
/// start.
pub struct Book {
    /// Live or replaying.
    pub mode: Mode,
    /// The recording: written to live, replayed otherwise.
    pub file: Option<String>,
    /// This run of CAF, live.
    pub run: Option<String>,
    /// What the core was made with.
    pub base: Arc<Base>,
    steps: RwLock<Vec<Arc<Step>>>,
    /// The last step's number, for sockets to wait on.
    end: watch::Sender<u64>,
    /// The first step the replay check failed at.
    diverged: Mutex<Option<Diverged>>,
}

/// Where the replay check failed: see [`ToInspector::Diverged`].
#[derive(Clone, Debug)]
pub struct Diverged {
    /// The step.
    pub at: u64,
    /// What CAF's core gave out.
    pub expected: Box<RawValue>,
    /// What the replay core gave out, if it could take the event.
    pub got: Option<Box<RawValue>>,
}

impl Diverged {
    /// As the app is sent it.
    #[must_use]
    pub fn message(&self) -> ToInspector {
        ToInspector::Diverged {
            at: self.at,
            expected: self.expected.clone(),
            got: self.got.clone(),
        }
    }
}

impl Book {
    /// An empty book.
    #[must_use]
    pub fn new(mode: Mode, file: Option<String>, run: Option<String>, base: Base) -> Self {
        Self {
            mode,
            file,
            run,
            base: Arc::new(base),
            steps: RwLock::default(),
            end: watch::Sender::new(0),
            diverged: Mutex::default(),
        }
    }

    /// Adds steps, numbered on from the last.
    pub fn add(&self, steps: impl IntoIterator<Item = Step>) {
        let end = {
            let mut all = self.steps.write().unwrap_or_else(PoisonError::into_inner);
            for step in steps {
                debug_assert_eq!(step.no, all.len() as u64 + 1, "steps numbered in order");
                all.push(Arc::new(step));
            }
            all.len() as u64
        };
        self.end.send_replace(end);
    }

    /// The steps after the first `after`.
    #[must_use]
    pub fn after(&self, after: u64) -> Vec<Arc<Step>> {
        let all = self.steps.read().unwrap_or_else(PoisonError::into_inner);
        let after = usize::try_from(after).unwrap_or(usize::MAX);
        all.get(after..).unwrap_or_default().to_vec()
    }

    /// The last step's number.
    #[must_use]
    pub fn end(&self) -> u64 {
        *self.end.borrow()
    }

    /// Follows the last step's number.
    #[must_use]
    pub fn watch_end(&self) -> watch::Receiver<u64> {
        self.end.subscribe()
    }

    /// Notes where the replay check failed, if it hasn't before, and
    /// wakes the sockets to say so.
    pub fn diverged_at(&self, diverged: Diverged) {
        let mut first = self.diverged.lock().unwrap_or_else(PoisonError::into_inner);
        if first.is_none() {
            log::error!(
                "a bug in CAF: replaying step {} gave out different directives from CAF's own core",
                diverged.at
            );
            *first = Some(diverged);
            drop(first);
            self.end.send_modify(|_| {});
        }
    }

    /// Where the replay check failed, if it has.
    #[must_use]
    pub fn diverged(&self) -> Option<Diverged> {
        self.diverged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}
