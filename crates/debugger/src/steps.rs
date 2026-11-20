//! The book of steps: every step so far, shared by every socket.
//!
//! **Memory.** Time travel needs every step, so the book keeps them all,
//! for as long as the debugger runs: each event as the JSON it came as,
//! the agent's messages whole, with its directives. That's about the size
//! of the run's recording.
//!
//! It keeps copies of the core along the way too, so moving to a step
//! needn't replay from the start: one every [`CHECKPOINT`] steps at first,
//! and never more than [`MOST_COPIES`]. When there would be more, every
//! other copy goes and the spacing doubles. Each copy is the whole core as
//! it was then, every chat with all its turns, so the copies take at most
//! [`MOST_COPIES`] times what the core holds at the end, and usually about
//! half that. A move replays at most the spacing: [`CHECKPOINT`] steps
//! until the run is [`MOST_COPIES`] times that long, and a 32nd to a 64th
//! of the run after.
//!
//! So a long live run grows: restart the debugger to start afresh.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use caf_core::Core;
use serde_json::value::RawValue;
use tokio::sync::watch;

use crate::inspector::{Check, Mode, Step, ToInspector};
use crate::replay::Base;

/// How many steps apart the copies of the core are kept at first: so
/// moving to any step replays at most this many from the nearest copy,
/// until there are [`MOST_COPIES`].
pub const CHECKPOINT: u64 = 1000;

/// The most copies of the core kept.
pub const MOST_COPIES: usize = 64;

/// Copies of the core, evenly spaced.
struct Copies {
    /// How many steps apart they are: [`CHECKPOINT`], doubled each time
    /// there would be too many.
    spacing: u64,
    /// The core just after each step a multiple of `spacing`, in order,
    /// with its position.
    list: Vec<(u64, Arc<Core>)>,
}

/// Every step so far, and what's known about them. Live, the checker adds
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
    /// Copies of the core along the way.
    checkpoints: RwLock<Copies>,
    /// The last step's number, for sockets to wait on.
    end: watch::Sender<u64>,
    /// The first step each check failed at.
    diverged: Mutex<Vec<Diverged>>,
    /// How many steps the recording check has checked.
    recording_checked: AtomicU64,
    /// A replay has been slower than its budget, and said so.
    slow: AtomicBool,
    /// No more steps come: CAF has ended, or it's a replay.
    complete: AtomicBool,
}

/// Where a check failed: see [`ToInspector::Diverged`].
#[derive(Clone, Debug)]
pub struct Diverged {
    /// Which check.
    pub check: Check,
    /// The step.
    pub at: u64,
    /// What CAF's core gave out.
    pub expected: Box<RawValue>,
    /// What the check's core gave out, if it could take the event.
    pub got: Option<Box<RawValue>>,
}

impl Diverged {
    /// As the app is sent it.
    #[must_use]
    pub fn message(&self) -> ToInspector {
        ToInspector::Diverged {
            check: self.check,
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
            checkpoints: RwLock::new(Copies {
                spacing: CHECKPOINT,
                list: Vec::new(),
            }),
            end: watch::Sender::new(0),
            diverged: Mutex::default(),
            recording_checked: AtomicU64::new(0),
            slow: AtomicBool::new(false),
            complete: AtomicBool::new(false),
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

    /// Keeps a copy of the core at `at`, the step just taken, if it's
    /// due: at a multiple of the spacing, one after the last copy. The
    /// copy is made only then. With [`MOST_COPIES`] kept already, every
    /// other one goes first, and the spacing doubles.
    pub fn checkpoint(&self, at: u64, core: &Core) {
        let mut copies = (self.checkpoints.write()).unwrap_or_else(PoisonError::into_inner);
        let due = |copies: &Copies| {
            at > 0
                && at.is_multiple_of(copies.spacing)
                && copies.list.last().is_none_or(|(last, _)| at > *last)
        };
        if !due(&copies) {
            return;
        }
        if copies.list.len() >= MOST_COPIES {
            copies.spacing *= 2;
            let spacing = copies.spacing;
            copies
                .list
                .retain(|(position, _)| position.is_multiple_of(spacing));
            if !due(&copies) {
                return;
            }
        }
        copies.list.push((at, Arc::new(core.clone())));
    }

    /// The copy of the core nearest before or at `at`, and its position.
    #[must_use]
    pub fn nearest(&self, at: u64) -> Option<(u64, Arc<Core>)> {
        let copies = (self.checkpoints.read()).unwrap_or_else(PoisonError::into_inner);
        let after = copies.list.partition_point(|(position, _)| *position <= at);
        let (position, copy) = copies.list.get(after.checked_sub(1)?)?;
        Some((*position, Arc::clone(copy)))
    }

    /// How many copies of the core are kept.
    #[must_use]
    pub fn copies(&self) -> usize {
        (self.checkpoints.read())
            .unwrap_or_else(PoisonError::into_inner)
            .list
            .len()
    }

    /// How many steps apart the copies of the core are now.
    #[must_use]
    pub fn spacing(&self) -> u64 {
        (self.checkpoints.read())
            .unwrap_or_else(PoisonError::into_inner)
            .spacing
    }

    /// The steps after the first `after`, up to `to` at most.
    #[must_use]
    pub fn range(&self, after: u64, to: u64) -> Vec<Arc<Step>> {
        let all = self.steps.read().unwrap_or_else(PoisonError::into_inner);
        let start = usize::try_from(after).unwrap_or(usize::MAX).min(all.len());
        let end = usize::try_from(to).unwrap_or(usize::MAX).min(all.len());
        all.get(start..end.max(start)).unwrap_or_default().to_vec()
    }

    /// The steps after the first `after`.
    #[must_use]
    pub fn after(&self, after: u64) -> Vec<Arc<Step>> {
        self.range(after, u64::MAX)
    }

    /// Says no more steps come, and wakes whoever waits for that.
    pub fn complete(&self) {
        self.complete.store(true, Ordering::Release);
        self.end.send_modify(|_| {});
    }

    /// Waits until no more steps come: live, until CAF's task has ended
    /// and its every step is in.
    pub async fn all_in(&self) {
        let mut end = self.end.subscribe();
        while !self.complete.load(Ordering::Acquire) {
            if end.changed().await.is_err() {
                return;
            }
        }
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

    /// Notes where a check failed, if it hasn't before, and wakes the
    /// sockets to say so.
    pub fn diverged_at(&self, diverged: Diverged) {
        let mut all = self.diverged.lock().unwrap_or_else(PoisonError::into_inner);
        if all.iter().any(|each| each.check == diverged.check) {
            return;
        }
        log::error!(
            "a bug in CAF: replaying step {} {} gave out different directives from CAF's own core",
            diverged.at,
            match diverged.check {
                Check::Steps => "as the steps hold it",
                Check::Recording => "as the recording holds it",
            }
        );
        all.push(diverged);
        drop(all);
        self.end.send_modify(|_| {});
    }

    /// Where each check failed, if any has, in the order they did.
    #[must_use]
    pub fn diverged(&self) -> Vec<Diverged> {
        self.diverged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Notes that the recording check has checked one more step.
    pub fn recording_checked_one(&self) {
        self.recording_checked.fetch_add(1, Ordering::Relaxed);
    }

    /// How many steps the recording check has checked.
    #[must_use]
    pub fn recording_checked(&self) -> u64 {
        self.recording_checked.load(Ordering::Relaxed)
    }

    /// Whether this is the first time a replay was too slow: it's said
    /// once a run.
    #[must_use]
    pub fn first_slow(&self) -> bool {
        !self.slow.swap(true, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use caf_core::Host;

    use super::{Book, CHECKPOINT, MOST_COPIES};
    use crate::inspector::Mode;
    use crate::replay::Base;

    #[test]
    fn the_copies_of_the_core_stay_few_however_long_the_run() {
        let book = Book::new(
            Mode::Replay,
            None,
            None,
            Base {
                host: Host::new("test", "1"),
                agents: Vec::new(),
            },
        );
        let core = book.base.core();
        let mut most = 0;
        for at in 1..=1_000_000 {
            book.checkpoint(at, &core);
            most = most.max(book.copies());
        }
        assert!(most <= MOST_COPIES, "{most} copies");
        assert!(book.copies() >= MOST_COPIES / 2, "{} copies", book.copies());
        // Evenly spaced, so a move replays at most the spacing.
        let spacing = book.spacing();
        assert!(spacing >= 1_000_000 / MOST_COPIES as u64 && spacing.is_multiple_of(CHECKPOINT));
        for at in [0, 999, 1000, 500_000, 999_999, 1_000_000] {
            let nearest = book.nearest(at).map_or(0, |(position, _)| position);
            assert!(nearest <= at && at - nearest < spacing, "{at}: {nearest}");
        }
    }
}
