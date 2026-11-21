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
//! shows chats, the storage driver saves them, and the setup driver
//! installs agents and signs in to them. Drivers never hear each
//! other; only the core listens to them, and only the core gives them
//! work. `caf-driver-tokio` has ready-made ones; a host with its own event
//! loop, such as clackatoo, writes its own against [`Event`] and
//! [`Directive`].
//!
//! Given the same events, the core always gives the same directives and
//! ends in the same state, so any session can be recorded and replayed as
//! a test.
//!
//! # What's in it so far
//!
//! Feature 1, *talking to one agent*:
//! - start an adapter;
//! - open a session, or continue one the agent knows;
//! - send messages and see the reply stream in;
//! - stop a turn;
//! - survive the adapter dying or hanging.
//!
//! Feature 2 added the [`viewer`] wire. Feature 3 added installing the
//! agents CAF has profiles for, and signing in to them, each from its
//! profile's recipe ([`install`]): [`Core::install`], [`Core::sign_in`]
//! and the rest, with where each agent stands in [`Core::agents`].
//! Feature 4 adds saving chats and restoring them ([`save`]):
//! [`Core::restore`], [`Core::put_away`] and the rest, with the app's own
//! data on each chat ([`AppData`]).
//!
//! The types already hold everything the architecture fixed, so later
//! features add behaviour without reshaping them. Items and variants
//! marked *later* are declared but not yet produced.
//!
//! # Closed, but growing
//!
//! Every enum CAF owns, and every struct it hands out or takes as
//! settings, is `#[non_exhaustive]`. That doesn't make them open: apps
//! can't add variants, and their own kind of item goes only through
//! [`ItemKind::App`]. It lets CAF add variants and fields later without
//! breaking anyone's code. So a `match` on one needs a `_` arm, and
//! settings are made with their constructor and changed by setting
//! fields.
//!
//! # One shape, in every format
//!
//! Every CAF type is written the same way in JSON (recordings, the viewer
//! wire to TypeScript) and in MessagePack in named mode (saved state, the
//! viewer wire to Rust), so the same rules describe them all:
//!
//! - **Every enum CAF owns is its variant's name as `type`, with what it
//!   holds, if anything, as `value`**: `{"type": "Open"}`,
//!   `{"type": "Words", "value": "hi"}`,
//!   `{"type": "Lost", "value": {"reason": {"type": "ShutDown"}}}`.
//!   TypeScript reads each as a plain union, by `switch (x.type)`. `type`
//!   is always written first.
//! - **Names are as in Rust**: fields in snake_case, variants in
//!   PascalCase, so these docs describe the wire exactly.
//! - **Ids are plain numbers or strings**, such as a [`TurnId`] as `1000`,
//!   or a [`ChatId`] as its UUID's text.
//! - **What isn't CAF's keeps its own shape:** what an agent sent, such as
//!   a message's params or an [`Unknown`](chat::Unknown) item's data, stays
//!   exactly as it came; Rust's own types keep serde's shapes, such as
//!   `Option` as the value or `null`, `Result` as `{"Ok": …}` or
//!   `{"Err": …}`, `Duration` as `{"secs", "nanos"}`, and an `OsString` as
//!   `{"Unix": [bytes]}`.
//!
//! A JSON-RPC message's params, result and error data
//! ([`acp::Message`]) are kept as the agent wrote them, so they're only
//! ever written as JSON, never as MessagePack, and read back only with
//! each `type` before its `value`, as CAF writes it.
//!
//! # Numbers
//!
//! Every number CAF itself writes in a chat or on the viewer wire
//! ([`viewer`]) is below 2^53, so JavaScript reads it exactly: CAF's ids
//! count up from 1 (a chat's is a UUID, as text), times are milliseconds
//! ([`Wall`](time::Wall)), and a number the agent gives that CAF keeps,
//! such as how full its memory is, is read only if it's below 2^53. An update with a bigger one is
//! unreadable, and kept as an [`Unknown`](chat::Unknown) item. A viewer's
//! own request numbers are quoted back as it sent them.
//!
//! The agent's own JSON that a chat carries ([`Unknown`](chat::Unknown)'s
//! `raw`, a tool call's `input`) is the agent's, kept as it came:
//! JavaScript reads it as it reads any JSON.
//!
//! What only debugging tools read may hold bigger numbers: events,
//! directives and recordings carry readings of the steady clock
//! ([`Mono`](time::Mono)), which are nanoseconds and pass 2^53 after about
//! 104 days of the host running, and what an agent sent, such as its
//! request ids, is any number it chose.
//!
//! # Guarantees
//!
//! Each of these has a test.
//!
//! 1. **Every request the agent sends is answered, exactly once:** by the
//!    policy, by the user, by the core's own safe refusal, or with an
//!    error. A request is never left waiting because CAF forgot it. The
//!    one exception is a request from a process that has gone, which
//!    nothing can be written to: it's dropped, and reported.
//! 2. **Every request CAF sends ends**, with an answer, an error, or a
//!    terminal outcome when its connection is lost. Every request has a
//!    watchdog ([`host::Timers`]) except a prompt, which may run as long as
//!    the agent works; once the user stops it, it has one too. Where the
//!    user may well choose to wait (starting an adapter, opening a
//!    session), the watchdog asks them whether to, and waits as they
//!    choose.
//! 3. **Nothing the agent sends is silently dropped.** An update CAF can't
//!    read becomes an [`ItemKind::Unknown`] with its original data in the
//!    turn that's open, and a message that fits nowhere becomes a
//!    [`Report`]. Only the agent's own output (words, thinking, tools)
//!    opens a turn: anything else said outside one, such as a notice, is
//!    reported, with what it says, as are updates about the whole chat
//!    (its plan, commands, mode, settings and title) until the features
//!    that keep them. CAF drops two things on purpose: all but the kind of
//!    an adapter's own sign-in status (`_auth/status_update`), as the rest
//!    holds the account's email, which CAF never keeps; and what an agent
//!    replays when a restored chat's session is loaded, as CAF has the
//!    chat's record already: reported once each time the session is asked
//!    to load.
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

// `acp`'s framer and parser are being written apart from the rest.
pub mod acp;
pub mod agent;
pub mod change;
pub mod chat;
mod core;
pub mod directive;
pub mod event;
#[cfg(test)]
mod fixtures;
pub mod host;
pub mod ids;
pub mod install;
#[cfg(test)]
mod kinds;
pub mod profiles;
pub mod question;
pub mod save;
#[cfg(test)]
mod shape;
pub mod time;
#[cfg(feature = "ts")]
mod ts;
pub mod viewer;

pub use crate::core::{Core, OpenChat, Refused, SessionChoice};
pub use change::{Change, Mismatch, Numbered, Read};
pub use chat::{AppData, AppItem, Chat, Item, ItemKind, NoAppData, NoAppItems, Turn};
pub use directive::{Directive, Report};
pub use event::{Event, HostCommand};
pub use host::Host;
pub use ids::{ChatId, ItemId, QuestionId, Shelf, Ticket, TurnId};
pub use save::{RestoreChat, SavedChat};
pub use time::{Now, Window};
