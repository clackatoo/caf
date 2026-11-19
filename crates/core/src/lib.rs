//! CAF's core: the state and rules of chats with coding agents.
//!
//! **The core does no I/O.** It never reads a clock, starts a process,
//! sets a timer, touches a file or decides a permission. Everything that
//! happens reaches it as an [`Event`], with the time given in, and
//! everything it wants done leaves it as a [`Directive`]:
//!
//! ```text
//! driver ──Event──▶ Core (rules, reading and updating state) ──Directives──▶ drivers
//! ```
//!
//! Drivers are small and do one kind of job each: the agent driver starts
//! adapter programs and carries their bytes, the timer driver wakes the
//! core, the policy driver answers "is this allowed?", the viewer driver
//! shows chats, and the storage driver saves them. Drivers never hear each
//! other; only the core listens to them, and only the core gives them
//! work. `caf-driver-tokio` has ready-made ones; a host with its own event
//! loop, such as clackatoo, writes its own against [`Event`] and
//! [`Directive`].
//!
//! Given the same events, the core always gives the same directives and
//! ends in the same state, so any session can be recorded and replayed as
//! a test.
//!
//! # What's in this feature
//!
//! This is feature 1, *talking to one agent*:
//! - start an adapter;
//! - open a session, or continue one the agent knows;
//! - send messages and see the reply stream in;
//! - stop a turn;
//! - survive the adapter dying or hanging.
//!
//! The types already hold everything the architecture fixed, so later
//! features add behaviour without reshaping them. Items and variants
//! marked *later* are declared but not yet produced.
//!
//! # Guarantees
//!
//! Each of these has a test.
//!
//! 1. **Every request the agent sends is answered, exactly once:** by the
//!    policy, by the user, by the core's own safe refusal, or with an
//!    error. A request is never left waiting because CAF forgot it.
//! 2. **Every request CAF sends ends**, with an answer, an error, or a
//!    terminal outcome when its connection is lost. Nothing is pending
//!    forever without a watchdog.
//! 3. **Nothing the agent sends is silently dropped.** An update CAF can't
//!    read becomes an [`ItemKind::Unknown`] with its original data, and a
//!    message that fits nowhere becomes a [`Report`].
//! 4. **Every event names what it belongs to**: a turn, an item, a session,
//!    a process. A late update lands on its own row, never on whatever is
//!    current.
//! 5. **Results of directives quote their [`Ticket`],** and a result for a
//!    ticket the core no longer waits on is ignored.
//! 6. **A question is answered once, and only while it still stands.**
//!    A late answer to a stopped turn's question does nothing.
//! 7. **Nothing replayed acts.** While an agent replays a session's
//!    history, its requests are refused and nothing it says has effects.
//! 8. **Restarts are visible.** When a connection is restarted or lost,
//!    every session it carried is told, not just the one that asked.
//! 9. **Agent text is never passed off as CAF's.** Failure text from the
//!    agent is cleaned, capped and kept apart from CAF's own words.

// The API draft for review: bodies are `todo!()` until it is approved.
#![allow(unused_variables, dead_code)]

pub mod acp;
pub mod agent;
pub mod change;
pub mod chat;
mod core;
pub mod directive;
pub mod event;
pub mod host;
pub mod ids;
pub mod profiles;
pub mod question;
pub mod time;

pub use crate::core::{Core, OpenChat, Refused, SessionChoice};
pub use change::{Change, Numbered, Read};
pub use chat::{AppItem, Chat, Item, ItemKind, NoAppItems, Turn};
pub use directive::{Directive, Report};
pub use event::{Event, HostCommand};
pub use host::Host;
pub use ids::{ChatId, ItemId, QuestionId, Ticket, TurnId};
pub use time::{Now, Window};
