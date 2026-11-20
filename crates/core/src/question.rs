//! Questions waiting for an answer.
//!
//! A question is asked by the agent (may I run this?) or by CAF itself
//! (the agent is slow: keep waiting, retry or stop?). Each is answered
//! once, and only while it still stands: answering a question that's gone,
//! or answering it twice, is refused and does nothing.

use serde::{Deserialize, Serialize};

use crate::ids::{ItemId, QuestionId};
use crate::time::Window;

/// A question waiting for an answer.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Question {
    /// Its id, to answer it by.
    pub id: QuestionId,
    /// When it was asked.
    pub window: Window,
    /// What it asks.
    pub kind: QuestionKind,
}

/// What a question asks.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum QuestionKind {
    /// The agent asks permission to do something.
    Permission(Permission),
    /// CAF asks: the session is taking long to open. This is CAF's own
    /// question, never framed as the agent's.
    SlowToOpen(SlowToOpen),
    // Forms and links the agent asks the user to fill in or open: later.
}

/// The agent asks permission.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Permission {
    /// The tool call it's about, once CAF has seen it. It may arrive
    /// after the question.
    pub tool: Option<ItemId>,
    /// What the agent wants to do, as it puts it.
    pub title: String,
    /// The answers the agent offers.
    pub options: Vec<PermissionOption>,
}

/// One answer an agent offers to a permission question.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PermissionOption {
    /// Its id, to answer with.
    pub id: OptionId,
    /// What the agent calls it.
    pub label: String,
    /// What it means.
    pub kind: OptionKind,
}

/// The agent's id for one of its answers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OptionId(pub String);

/// What a permission answer means.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum OptionKind {
    /// Allow this once.
    AllowOnce,
    /// Allow this from now on.
    AllowAlways,
    /// Refuse this once.
    RejectOnce,
    /// Refuse this from now on.
    RejectAlways,
    /// A kind CAF doesn't know. Never the default, never picked by
    /// itself.
    Other,
}

/// The session is taking long to open.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlowToOpen {
    /// Whether the adapter has answered at all. If it hasn't, the trouble
    /// is starting it, not the session.
    pub adapter_answered: bool,
}

/// An answer to a question.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Answer {
    /// Choose one of a permission question's options.
    Permission(OptionId),
    /// Answer the slow-to-open question.
    SlowToOpen(SlowChoice),
}

/// What to do about a session that's slow to open.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum SlowChoice {
    /// Keep waiting. What a host should pick when the user dismisses the
    /// question.
    KeepWaiting,
    /// Restart the adapter and try again. If another chat sharing the
    /// adapter is mid-turn, the adapter isn't restarted: the session is
    /// just asked for again.
    Retry,
    /// Give up: the waiting message ends as stopped.
    Stop,
}
