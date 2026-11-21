//! Names for the things the core keeps.
//!
//! Every id is permanent, and never a position.
//!
//! - **A chat's id is a UUID** (version 7, so ids sort by when their chats
//!   were made), unique wherever the app's chats may meet: chats saved by
//!   different runs, or different windows, can be restored into one core.
//! - **Turns and items are numbered within their chat.** Everything that
//!   names one names its chat too, and a restored chat keeps its numbers:
//!   the core counts on from past them.
//! - **Everything else is one run's** (questions, connections, tickets,
//!   change numbers), and never saved.
//!
//! The core gives ids out in order, and a chat's UUID from the seed the
//! host gives it ([`Host::seed`](crate::Host::seed)) and the time, so a
//! replayed session gives out the same ids. Tests start numbers at 1000,
//! so code that mixes an id up with a position fails.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        pub struct $name(pub u64);
    };
}

/// A chat: the user's conversation, over one or more agent sessions.
///
/// Written as a UUID's text in every format, such as
/// `"0192f0c4-5b8e-7c3a-9d41-6e2f8a1b3c5d"`, MessagePack included, so it
/// has one shape everywhere. A number is read too, as the UUID whose last
/// bits it is: what recordings and samples made before chats had UUIDs
/// hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
pub struct ChatId(pub Uuid);

impl Serialize for ChatId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0.hyphenated())
    }
}

impl<'de> Deserialize<'de> for ChatId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Text(String),
            Number(u64),
        }
        match Either::deserialize(deserializer)? {
            Either::Text(text) => Uuid::parse_str(&text)
                .map(ChatId)
                .map_err(serde::de::Error::custom),
            Either::Number(number) => Ok(ChatId::from_number(number)),
        }
    }
}

impl ChatId {
    /// The UUID whose last bits are `number`: how a chat id written as a
    /// number, before chats had UUIDs, is read. For tests, and reading
    /// old recordings.
    #[must_use]
    pub const fn from_number(number: u64) -> Self {
        Self(Uuid::from_u128(number as u128))
    }
}

impl std::fmt::Display for ChatId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

id! {
    /// A turn in a chat, numbered within it.
    TurnId
}
id! {
    /// An item in a turn: words, thinking, a tool call… Numbered within
    /// its chat.
    ItemId
}
id! {
    /// A question waiting for an answer. One run's: questions aren't
    /// restored, as what asked them is gone.
    QuestionId
}
id! {
    /// A connection: one running adapter program, serving one or more
    /// sessions. It keeps its id across restarts; each process it runs
    /// has a [`Ticket`] of its own.
    ConnectionId
}
id! {
    /// A directive whose result the core waits for, and anything that
    /// directive starts.
    ///
    /// Results and later events quote it. Tickets are never reused, so a
    /// result for one the core no longer waits on is simply stale, and
    /// ignored. Each adapter process is known by the ticket of the
    /// directive that started it, so output from a process the core has
    /// replaced can't be mistaken for the new one's.
    Ticket
}
id! {
    /// A change in a chat's numbered list of changes. Viewers say how far
    /// they've read with it.
    ChangeNo
}

/// An agent, by the name the host gave it, such as `claude`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentName(pub String);

/// A session as the agent knows it: its own id for it, kept so the
/// session can be continued later. At most 256 bytes, with no control
/// characters; anything else an agent sends is refused.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentSessionId(pub String);

/// Where a chat is kept: a name the app chooses, such as one per project,
/// or one for everything. Chats are listed a shelf at a time, and a
/// shelf's chats move together ([`Core::move_shelf`](crate::Core::move_shelf)).
/// What a shelf is on disk is the store's: CAF's file store makes each a
/// folder.
///
/// At most 256 bytes, with no control characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Shelf(pub String);
