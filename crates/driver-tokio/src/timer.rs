//! The timer driver: wakes the core at the one time it asked for.

use std::time::{Duration, SystemTime};

use caf_core::time::{Mono, Now, Wall};
use tokio::time::Instant;

/// Keeps the core's single deadline, and gives the time.
///
/// It's also where the kit's [`Now`] comes from: [`Mono`] counts from the
/// moment the driver was made, and the wall clock is the system's.
///
/// [`Mono`] and waking both follow tokio's clock, so a test that pauses
/// it (`tokio::time::pause`) moves them together. The wall clock doesn't
/// pause.
pub struct TimerDriver {
    origin: Instant,
    deadline: Option<Mono>,
}

impl TimerDriver {
    /// A driver whose steady clock starts now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
            deadline: None,
        }
    }

    /// The time now, to hand the core with an event.
    #[must_use]
    pub fn now(&self) -> Now {
        let elapsed = self.origin.elapsed().as_nanos();
        let wall = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
            Err(before) => {
                i64::try_from(before.duration().as_millis()).map_or(i64::MIN, |before| -before)
            }
        };
        Now {
            wall: Wall(wall),
            mono: Mono(u64::try_from(elapsed).unwrap_or(u64::MAX)),
        }
    }

    /// Wakes at this time from now on, or never: the core's
    /// [`TimerDirective::WakeAt`](caf_core::directive::TimerDirective::WakeAt).
    pub fn wake_at(&mut self, at: Option<Mono>) {
        self.deadline = at;
    }

    /// Resolves at the deadline, or never if there is none. Waking means
    /// handing the core [`Event::Woken`](caf_core::Event::Woken).
    ///
    /// It resolves once for each deadline: the deadline is cleared as it
    /// does, so a loop doesn't spin while the core decides its next one.
    /// Cancel-safe: dropped before it resolves, it changes nothing.
    pub async fn woken(&mut self) {
        // A deadline too far off for the clock never comes.
        let at = (self.deadline).and_then(|at| self.origin.checked_add(Duration::from_nanos(at.0)));
        let Some(at) = at else {
            return std::future::pending().await;
        };
        tokio::time::sleep_until(at).await;
        self.deadline = None;
    }
}

impl Default for TimerDriver {
    fn default() -> Self {
        Self::new()
    }
}
