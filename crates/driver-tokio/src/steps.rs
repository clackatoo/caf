//! Watching every step CAF takes: for debuggers and tests.
//!
//! A step is one event given to the core and the directives it gave out:
//! exactly one line of a recording ([`Config::record`]) plus what the core
//! did about it. The host's calls, such as [`ChatHandle::send`], are seen
//! as the [`Event::Host`] they're recorded as, and only if the core took
//! them.
//!
//! The event is the recording's line read back, so it's what a replay
//! gives the core, with what a recording hides hidden here too, such as
//! the account's email in Claude's sign-in status and the values of MCP
//! servers' variables. None of it changes what the core does: a fresh
//! core given the same events gives out the same directives. Without a
//! watcher, watching costs nothing more than a check each step.
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
}
