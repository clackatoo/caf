//! Time, as the host gives it.
//!
//! The core never reads a clock. Every call into it carries a [`Now`],
//! and everything it stamps (turns, items, questions) takes its time from
//! there. That keeps recorded sessions replayable exactly.

use serde::{Deserialize, Serialize};

/// The time an event happened, as the host's clocks say.
///
/// It carries two readings because they answer different questions:
/// [`Wall`] is what to show and save, and [`Mono`] is what deadlines and
/// durations are measured on, since a wall clock can jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Now {
    /// The calendar time, for showing and saving.
    pub wall: Wall,
    /// The steady clock, for deadlines.
    pub mono: Mono,
}

/// Calendar time: milliseconds since the Unix epoch, UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Wall(pub i64);

/// The host's steady clock: nanoseconds since an origin the host chooses
/// when it starts.
///
/// Only comparable within one run of the host, so it's never saved. After
/// a restart, the core measures again from the new origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Mono(pub u64);

impl Mono {
    /// This reading plus `by`, saturating at the end of time.
    #[must_use]
    pub fn after(self, by: std::time::Duration) -> Self {
        let by = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        Self(self.0.saturating_add(by))
    }
}

/// When something started and ended, as CAF heard it.
///
/// The times are when CAF heard of things, not when the agent did them:
/// ACP carries no timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// When CAF first heard of it. `None` only for history imported from
    /// an agent's replay, where the times aren't known.
    pub start: Option<Wall>,
    /// Whether, and when, it ended.
    pub end: End,
}

/// The end of a [`Window`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum End {
    /// It's still going.
    Open,
    /// It ended then. An instant thing ends when it starts.
    At(Wall),
    /// It ended, at a time nobody knows: imported history.
    Unknown,
}

impl Window {
    /// A window opening now.
    #[must_use]
    pub fn open(now: Now) -> Self {
        Self {
            start: Some(now.wall),
            end: End::Open,
        }
    }

    /// A window for something instant, starting and ending now.
    #[must_use]
    pub fn instant(now: Now) -> Self {
        Self {
            start: Some(now.wall),
            end: End::At(now.wall),
        }
    }

    /// A window for imported history, whose times aren't known.
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            start: None,
            end: End::Unknown,
        }
    }
}
