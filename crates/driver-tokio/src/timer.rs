//! The timer driver: wakes the core at the one time it asked for.

use std::time::Instant;

use caf_core::time::{Mono, Now};

/// Keeps the core's single deadline, and gives the time.
///
/// It's also where the kit's [`Now`] comes from: [`Mono`] counts from the
/// moment the driver was made, and the wall clock is the system's.
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
        todo!()
    }

    /// Wakes at this time from now on, or never: the core's
    /// [`Directive::WakeAt`](caf_core::Directive::WakeAt).
    pub fn wake_at(&mut self, at: Option<Mono>) {
        self.deadline = at;
    }

    /// Resolves at the deadline, or never if there is none. Waking means
    /// handing the core [`Event::Woken`](caf_core::Event::Woken).
    pub async fn woken(&mut self) {
        todo!()
    }
}

impl Default for TimerDriver {
    fn default() -> Self {
        Self::new()
    }
}
