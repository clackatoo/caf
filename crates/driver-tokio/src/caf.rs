//! The wiring: one task owning the core and its drivers, and handles for
//! the app.

use caf_core::agent::AgentSpec;
use caf_core::chat::UserMessage;
use caf_core::question::Answer;
use caf_core::{AppItem, Chat, ChatId, Host, NoAppItems, Numbered, OpenChat, QuestionId, Refused, TurnId};

use crate::agent::AgentLimits;
use crate::policy::{AlwaysAsk, Policy};

/// What to start CAF with.
#[non_exhaustive]
pub struct Config {
    /// The host's description of itself.
    pub host: Host,
    /// The agents it offers.
    pub agents: Vec<AgentSpec>,
    /// Who answers permission requests. [`AlwaysAsk`] by default.
    pub policy: Box<dyn Policy>,
    /// Limits for the agent driver.
    pub agent_limits: AgentLimits,
}

impl Config {
    /// The defaults for `host`, offering these agents. Set any field to
    /// change it.
    #[must_use]
    pub fn new(host: Host, agents: Vec<AgentSpec>) -> Self {
        Self {
            host,
            agents,
            policy: Box::new(AlwaysAsk),
            agent_limits: AgentLimits::default(),
        }
    }
}

/// CAF, running on tokio. Cheap to clone: every clone talks to the same
/// task.
pub struct Caf<A: AppItem = NoAppItems> {
    _app: std::marker::PhantomData<A>,
}

impl<A: AppItem> Clone for Caf<A> {
    fn clone(&self) -> Self {
        todo!()
    }
}

/// The task CAF runs in. Await it after [`Caf::shutdown`] to know every
/// adapter has stopped.
pub struct Running {}

impl std::future::Future for Running {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        todo!()
    }
}

/// Why something asked of CAF wasn't done.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The core refused it, and changed nothing.
    Refused(Refused),
    /// CAF has shut down, so nothing can be asked of it.
    Gone,
}

impl From<Refused> for Error {
    fn from(refused: Refused) -> Self {
        Self::Refused(refused)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refused) => write!(f, "CAF refused: {refused:?}"),
            Self::Gone => f.write_str("CAF has shut down"),
        }
    }
}

impl std::error::Error for Error {}

impl<A: AppItem> Caf<A> {
    /// Starts CAF in a task of its own, on the current tokio runtime.
    #[must_use]
    pub fn start(config: Config) -> (Self, Running) {
        todo!()
    }

    /// Opens a chat. It's ready to take messages at once: they wait until
    /// its session is open.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn open_chat(&self, open: OpenChat) -> Result<ChatHandle<A>, Error> {
        todo!()
    }

    /// A handle on a chat that's already open.
    #[must_use]
    pub fn chat(&self, chat: ChatId) -> ChatHandle<A> {
        todo!()
    }

    /// Stops everything: every session closed, every adapter stopped.
    pub async fn shutdown(&self) {
        todo!()
    }
}

/// A handle on one chat.
pub struct ChatHandle<A: AppItem = NoAppItems> {
    /// The chat's id.
    pub id: ChatId,
    caf: Caf<A>,
}

impl<A: AppItem> ChatHandle<A> {
    /// Sends a message. See [`caf_core::Core::send`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn send(&self, message: UserMessage) -> Result<TurnId, Error> {
        todo!()
    }

    /// Stops the running turn. See [`caf_core::Core::stop`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn stop(&self) -> Result<(), Error> {
        todo!()
    }

    /// Answers a question. See [`caf_core::Core::answer`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn answer(&self, question: QuestionId, answer: Answer) -> Result<(), Error> {
        todo!()
    }

    /// Closes the chat for good.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] with the core's reason, or [`Error::Gone`].
    pub async fn close(self) -> Result<(), Error> {
        todo!()
    }

    /// Watches the chat: first the whole chat, then its changes as they
    /// happen. A watcher that falls behind gets the whole chat again
    /// rather than holding changes without end.
    #[must_use]
    pub fn watch(&self) -> Watch<A> {
        todo!()
    }
}

/// A watch on a chat.
pub struct Watch<A: AppItem = NoAppItems> {
    _app: std::marker::PhantomData<A>,
}

/// What a watch gets.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Update<A: AppItem = NoAppItems> {
    /// The whole chat, as it is: at the start, and after falling behind.
    Picture(Chat<A>),
    /// What changed since the last update, oldest first.
    Changes(Vec<Numbered<A>>),
}

impl<A: AppItem> Watch<A> {
    /// The next update, or `None` once the chat is closed or CAF has shut
    /// down.
    pub async fn next(&mut self) -> Option<Update<A>> {
        todo!()
    }
}
