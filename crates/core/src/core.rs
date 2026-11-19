//! The core itself.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::{AgentSpec, McpServer};
use crate::change::Read;
use crate::chat::{AppItem, Chat, NoAppItems, UserMessage};
use crate::directive::Directive;
use crate::event::Event;
use crate::host::Host;
use crate::ids::{AgentName, AgentSessionId, ChangeNo, ChatId, ConnectionId, QuestionId, TurnId};
use crate::profiles::ProfileReport;
use crate::question::Answer;
use crate::time::Now;

/// CAF's core: every chat's state, and the rules that change it.
///
/// Feed it what happens with [`Core::handle`] and the host methods, and
/// carry out the [`Directive`]s it adds to `out`. It never blocks, never
/// does I/O, and gives the same directives for the same events every
/// time.
///
/// ```ignore
/// let mut core = Core::<NoAppItems>::new(Host::new("clackatoo", "2026.12.1"), vec![claude]);
/// let mut out = Vec::new();
/// let chat = core.open_chat(now, OpenChat::new("claude", "/home/me/project"), &mut out)?;
/// let turn = core.send(now, chat, UserMessage::text("Fix the tests"), &mut out)?;
/// // …carry out `out`, feed back what the drivers report…
/// ```
pub struct Core<A: AppItem = NoAppItems> {
    host: Host,
    agents: BTreeMap<AgentName, AgentSpec>,
    chats: BTreeMap<ChatId, Chat<A>>,
    _app: PhantomData<A>,
}

/// What to open a chat with.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenChat {
    /// The agent, by the name the host gave it.
    pub agent: AgentName,
    /// The folder it works in. Must be absolute.
    pub cwd: PathBuf,
    /// A new session, or one the agent already knows.
    pub session: SessionChoice,
    /// Tools to give it, as MCP servers.
    pub mcp_servers: Vec<McpServer>,
}

impl OpenChat {
    /// A new session with `agent` in `cwd`, with no tools of the host's.
    #[must_use]
    pub fn new(agent: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            agent: AgentName(agent.into()),
            cwd: cwd.into(),
            session: SessionChoice::New,
            mcp_servers: Vec::new(),
        }
    }
}

/// Which session a chat opens.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionChoice {
    /// A new one.
    New,
    /// One the agent already knows, by its id: continued without replaying
    /// its history if the agent can, and otherwise loaded, with the history
    /// it replays imported into the chat and marked as imported. If the
    /// agent no longer has it, a new session is opened and the chat says
    /// so.
    Continue(AgentSessionId),
}

/// Why the core refused a host's command. A refused command changes
/// nothing.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refused {
    /// No chat has that id.
    NoSuchChat,
    /// The chat is closed.
    Closed,
    /// No agent has that name.
    NoSuchAgent,
    /// The folder isn't absolute.
    RelativeFolder,
    /// The chat already has as many messages waiting as the host allows.
    QueueFull,
    /// The message has a part this agent can't take.
    Unsupported,
    /// A slash command must be a message's only part.
    CommandNotAlone,
    /// No question with that id is waiting: it was answered, withdrawn,
    /// or never asked.
    NoSuchQuestion,
    /// That answer doesn't fit the question, such as an option the agent
    /// didn't offer.
    WrongAnswer,
    /// There's nothing to stop.
    NothingToStop,
}

/// What the host can see of a connection: one adapter process.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionInfo {
    /// Its id.
    pub id: ConnectionId,
    /// The agent it runs.
    pub agent: AgentName,
    /// What became of its profile, once the adapter said who it is.
    pub profile: Option<ProfileReport>,
    /// The chats it serves.
    pub chats: Vec<ChatId>,
    /// The last lines of its error output, cleaned.
    pub stderr: Vec<String>,
}

impl<A: AppItem> Core<A> {
    /// A core for a host, offering these agents.
    #[must_use]
    pub fn new(host: Host, agents: Vec<AgentSpec>) -> Self {
        todo!()
    }

    /// Takes in something that happened, at `now`, and adds the work it
    /// leads to onto `out`.
    pub fn handle(&mut self, now: Now, event: Event<A>, out: &mut Vec<Directive>) {
        todo!()
    }

    /// Opens a chat, and returns its id. Its agent is started, or its
    /// running adapter shared, as the host's grouping says; messages sent
    /// before the session is open wait for it.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchAgent`] or [`Refused::RelativeFolder`].
    pub fn open_chat(&mut self, now: Now, open: OpenChat, out: &mut Vec<Directive>) -> Result<ChatId, Refused> {
        todo!()
    }

    /// Sends a message as a new turn, and returns the turn's id.
    ///
    /// If another turn is running, or the session isn't open yet, the new
    /// turn waits, in order: messages are never dropped. If the chat's
    /// session was lost, sending opens it again, continuing it where the
    /// agent can.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchChat`], [`Refused::Closed`], [`Refused::QueueFull`],
    /// [`Refused::Unsupported`] or [`Refused::CommandNotAlone`].
    pub fn send(&mut self, now: Now, chat: ChatId, message: UserMessage, out: &mut Vec<Directive>) -> Result<TurnId, Refused> {
        todo!()
    }

    /// Stops the chat's running turn, and drops its waiting ones, which end
    /// as stopped.
    ///
    /// The agent's waiting permission questions are answered "cancelled",
    /// as ACP requires, and the agent is told to stop. The turn is
    /// stopping until the agent confirms; if it doesn't within the host's
    /// stop timer, the turn fails and the session is restarted. If the
    /// session is still opening, opening it is abandoned.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchChat`] or [`Refused::NothingToStop`].
    pub fn stop(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive>) -> Result<(), Refused> {
        todo!()
    }

    /// Answers a question.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchQuestion`] if it isn't waiting (answered,
    /// withdrawn or never asked), or [`Refused::WrongAnswer`].
    pub fn answer(&mut self, now: Now, question: QuestionId, answer: Answer, out: &mut Vec<Directive>) -> Result<(), Refused> {
        todo!()
    }

    /// Closes a chat for good. Its session is closed, if the agent can
    /// close sessions, and its adapter stopped if nothing else uses it.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchChat`].
    pub fn close_chat(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive>) -> Result<(), Refused> {
        todo!()
    }

    /// Stops everything: closes every session, stops every adapter
    /// gently, then firmly. Done once [`Core::is_idle`] is true.
    pub fn shutdown(&mut self, now: Now, out: &mut Vec<Directive>) {
        todo!()
    }

    /// Whether nothing is running: no adapters, nothing waiting.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        todo!()
    }

    /// A chat, by id.
    #[must_use]
    pub fn chat(&self, chat: ChatId) -> Option<&Chat<A>> {
        self.chats.get(&chat)
    }

    /// Every chat, oldest first.
    pub fn chats(&self) -> impl Iterator<Item = &Chat<A>> {
        self.chats.values()
    }

    /// A chat's changes after `since`, or the whole chat if `since` is
    /// `None` or too old.
    #[must_use]
    pub fn read(&self, chat: ChatId, since: Option<ChangeNo>) -> Option<Read<'_, A>> {
        todo!()
    }

    /// Every connection: what each adapter process is, and serves.
    #[must_use]
    pub fn connections(&self) -> Vec<ConnectionInfo> {
        todo!()
    }
}
