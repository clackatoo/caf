//! The timer driver: wakes the core at the one time it asked for.

use std::time::{Duration, SystemTime};

use std::pin::Pin;

use caf_core::time::{Mono, Now, Wall};
use tokio::time::{Instant, Sleep};

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
    /// When to wake, or never.
    deadline: Option<Instant>,
    /// One sleep, made on the first wait and moved to each new deadline,
    /// rather than a new one for every wait.
    sleep: Option<Pin<Box<Sleep>>>,
}

impl TimerDriver {
    /// A driver whose steady clock starts now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
            deadline: None,
            sleep: None,
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
        // A deadline too far off for the clock never comes.
        self.deadline = at.and_then(|at| self.origin.checked_add(Duration::from_nanos(at.0)));
    }

    /// Resolves at the deadline, or never if there is none. Waking means
    /// handing the core [`Event::Woken`](caf_core::Event::Woken).
    ///
    /// It resolves once for each deadline: the deadline is cleared as it
    /// does, so a loop doesn't spin while the core decides its next one.
    /// Cancel-safe: dropped before it resolves, it changes nothing.
    pub async fn woken(&mut self) {
        let Some(at) = self.deadline else {
            return std::future::pending().await;
        };
        let sleep = match &mut self.sleep {
            Some(sleep) => {
                if sleep.deadline() != at {
                    sleep.as_mut().reset(at);
                }
                sleep
            }
            None => self.sleep.insert(Box::pin(tokio::time::sleep_until(at))),
        };
        sleep.as_mut().await;
        self.deadline = None;
    }
}

impl Default for TimerDriver {
    fn default() -> Self {
        Self::new()
    }
}
