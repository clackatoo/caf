//! `caf-debugger`: CAF's chat beside an inspector of its every step, in a
//! browser, live or replaying a recording.
//!
//! ```text
//! caf-debugger [--port 7320] [--agents agents.toml]     live
//! caf-debugger --replay FILE.jsonl [--port 7320]        replay
//! ```
//!
//! A tool for debugging CAF itself, not a library to build on: nothing
//! here is stable. The guide "The Debugger", in CAF's docs, says how to
//! use it.
//!
//! - **Live** ([`live`]), it runs CAF on tokio with the agents of
//!   [`agents`], records the run ([`files`]), and checks every step
//!   against a replay ([`replay`]).
//! - **Replaying**, it reads a recording and replays it ([`replay::load`]):
//!   nothing is started.
//! - **Either way**, it serves the app over HTTP on 127.0.0.1
//!   ([`server`]): the chats on the viewer wire, live, and CAF's steps
//!   and state on the inspector channel ([`inspector`]).

pub mod agents;
pub mod fake;
pub mod files;
pub mod inspector;
pub mod live;
pub mod replay;
pub mod server;
pub mod steps;
