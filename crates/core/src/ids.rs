//! Names for the things the core keeps.
//!
//! Every id is permanent: it's never reused within one core's saved
//! state, and never a position. Ids are given out by the core in order,
//! so a replayed session gives out the same ids. Tests start them at
//! 1000, so code that mixes an id up with a position fails.

use serde::{Deserialize, Serialize};

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub u64);
    };
}

id! {
    /// A chat: the user's conversation, over one or more agent sessions.
    ChatId
}
id! {
    /// A turn in a chat.
    TurnId
}
id! {
    /// An item in a turn: words, thinking, a tool call…
    ItemId
}
id! {
    /// A question waiting for an answer.
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
pub struct AgentName(pub String);

/// A session as the agent knows it: its own id for it, kept so the
/// session can be continued later. At most 256 bytes, with no control
/// characters; anything else an agent sends is refused.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentSessionId(pub String);
