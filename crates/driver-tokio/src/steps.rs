//! Watching every step CAF takes: for debuggers and tests.
//!
//! A step is one event given to the core and the directives it gave out:
//! exactly one line of a recording ([`Config::record`]) plus what the core
//! did about it. The host's calls, such as [`ChatHandle::send`], are seen
//! as the [`Event::Host`] they're recorded as, and only if the core took
//! them.
//!
//! The event is exactly what the core was given, so a fresh core given
//! the same events, in order, gives out the same directives.
//!
//! **Steps hold secrets.** A recording hides them, but a step can't: the
//! values of MCP servers' variables and headers, in the host's
//! [`Event::Host`] opening a chat and in the directive that starts its
//! session, and the account's email, in Claude's sign-in status. So treat
//! what a watcher sees as private, as the host's own, and never write it
//! out as it is: hide those first, as a recording does. Only a step's
//! [`line`](Step::line), the recording's own, has them hidden.
//!
//! **The cost.** With a watcher, each event is written as JSON and read
//! back on CAF's task, since events can't be copied: that includes the
//! agent's messages, up to
//! [`Limits::biggest_message`](caf_core::host::Limits::biggest_message)
//! each. Without one, watching costs nothing more than a check each step.
//!
//! [`Config::record`]: crate::Config::record
//! [`ChatHandle::send`]: crate::ChatHandle::send
//! [`Event::Host`]: caf_core::Event::Host

use caf_core::chat::AppItem;
use caf_core::{Directive, Event, Now};

/// Sees every step CAF takes. Give one to [`Caf::start_watching`].
///
/// [`Caf::start_watching`]: crate::Caf::start_watching
pub trait Steps<A: AppItem>: Send + 'static {
    /// Called after each step, on CAF's task, in order. Be quick: copy or
    /// encode what you need and return, since every chat waits meanwhile.
    /// A panic here is CAF's panic.
    fn step(&mut self, step: Step<'_, A>);
}

/// One step: an event the core was given, and what it gave out.
#[non_exhaustive]
#[derive(Debug)]
pub struct Step<'a, A: AppItem> {
    /// When, as the core was told.
    pub now: Now,
    /// The event.
    pub event: &'a Event<A>,
    /// The directives it gave out, in order.
    pub directives: &'a [Directive],
    /// The step's line for the recording ([`Config::record`]), if CAF is
    /// recording and the line was handed to the recording:
    /// `{"now": …, "event": …}`, exactly as it's written, with no
    /// newline, the secrets hidden. `None` once the recording has
    /// stopped, or if there's none.
    ///
    /// The recording is written on a thread of its own, after the step:
    /// if a write fails later, the file stops short, and may lack this
    /// line and those after it, though the watcher had them.
    ///
    /// It lets a watcher check what a recording replays: a fresh core
    /// given each line's event, read back, must give out the same
    /// directives as [`Step::directives`], but for what the hidden
    /// secrets are part of, such as the values of MCP servers' variables
    /// in the message that opens a session.
    ///
    /// [`Config::record`]: crate::Config::record
    pub line: Option<&'a str>,
}
