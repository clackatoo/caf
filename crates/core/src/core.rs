//! The core itself.
//!
//! The public methods are here; the rules live in private modules, one
//! for each part of the work.

mod asks;
mod clean;
mod frames;
mod procs;
mod record;
mod sessions;
mod state;
mod timers;
mod turns;
mod updates;
mod wire;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use self::record::Log;
use self::state::{Ask, Conn, Ids, Pending, Proc, Run};
use crate::agent::{AgentSpec, McpServer};
use crate::change::Read;
use crate::chat::{AppItem, Chat, NoAppItems, UserMessage};
use crate::directive::{Directive, Level, Report};
use crate::event::{Event, HostCommand};
use crate::host::Host;
use crate::ids::{AgentName, AgentSessionId, ChangeNo, ChatId, ConnectionId, QuestionId, Ticket, TurnId};
use crate::profiles::ProfileReport;
use crate::question::Answer;
use crate::time::{Mono, Now};

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
    /// Gives out every id.
    ids: Ids,
    /// Each chat's place with its connection and session.
    runs: BTreeMap<ChatId, Run<A>>,
    /// Each chat's changes, for viewers.
    logs: BTreeMap<ChatId, Log<A>>,
    /// Every connection.
    conns: BTreeMap<ConnectionId, Conn>,
    /// Every process not yet let go, by the ticket that started it.
    procs: BTreeMap<Ticket, Proc>,
    /// CAF's requests waiting for answers, by process and request id.
    pending: BTreeMap<(Ticket, i64), Pending>,
    /// The agent's requests waiting for CAF's answer, by the ticket of
    /// the policy's decision.
    asks: BTreeMap<Ticket, Ask>,
    /// The time the timer driver was last asked to wake the core at.
    wake: Option<Mono>,
    /// Chats changed since viewers were last told.
    dirty: BTreeSet<ChatId>,
    /// The host asked for everything to stop: nothing is started again
    /// until it asks for something new.
    shutting_down: bool,
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
    /// Something can't be sent to the agent exactly as it is: a folder,
    /// program, argument or variable that isn't valid UTF-8, or what ACP
    /// can't say for an MCP server (a variable to remove, or a folder to
    /// start in). CAF never sends something else in its place.
    Unsendable,
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
        Self::starting_ids_at(host, agents, 1)
    }

    /// A core whose ids start at `first`: tests start them at 1000, so
    /// an id mixed up with a position shows.
    pub(crate) fn starting_ids_at(host: Host, agents: Vec<AgentSpec>, first: u64) -> Self {
        Self {
            host,
            agents: agents.into_iter().map(|agent| (agent.name.clone(), agent)).collect(),
            chats: BTreeMap::new(),
            ids: Ids { next: first, first },
            runs: BTreeMap::new(),
            logs: BTreeMap::new(),
            conns: BTreeMap::new(),
            procs: BTreeMap::new(),
            pending: BTreeMap::new(),
            asks: BTreeMap::new(),
            wake: None,
            dirty: BTreeSet::new(),
            shutting_down: false,
        }
    }

    /// Takes in something that happened, at `now`, and adds the work it
    /// leads to onto `out`.
    pub fn handle(&mut self, now: Now, event: Event<A>, out: &mut Vec<Directive>) {
        match event {
            Event::Host(command) => {
                if let Err(refused) = self.command(now, command, out) {
                    report(out, Level::Debug, format!("refused a host command: {refused:?}"));
                }
            }
            Event::Agent { process, event } => self.agent_event(now, process, event, out),
            Event::Woken => self.woken(now, out),
            Event::Policy { ticket, answer } => self.policy_answered(now, ticket, answer, out),
            Event::Storage(storage) => match storage {},
        }
        self.finish(now, out);
    }

    /// Opens a chat, and returns its id. Its agent is started, or its
    /// running adapter shared, as the host's grouping says; messages sent
    /// before the session is open wait for it.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchAgent`], [`Refused::RelativeFolder`] or
    /// [`Refused::Unsendable`].
    pub fn open_chat(&mut self, now: Now, open: OpenChat, out: &mut Vec<Directive>) -> Result<ChatId, Refused> {
        let chat = ChatId(self.ids.next);
        self.command(now, HostCommand::OpenChat { chat, open }, out)?;
        self.finish(now, out);
        Ok(chat)
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
        let turn = TurnId(self.ids.next);
        self.command(now, HostCommand::Send { chat, turn, message }, out)?;
        self.finish(now, out);
        Ok(turn)
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
        self.command(now, HostCommand::Stop { chat }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Answers a question.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchQuestion`] if it isn't waiting (answered,
    /// withdrawn or never asked), or [`Refused::WrongAnswer`].
    pub fn answer(&mut self, now: Now, question: QuestionId, answer: Answer, out: &mut Vec<Directive>) -> Result<(), Refused> {
        self.command(now, HostCommand::Answer { question, answer }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Closes a chat for good. Its session is closed, if the agent can
    /// close sessions, and its adapter stopped if nothing else uses it.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchChat`].
    pub fn close_chat(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive>) -> Result<(), Refused> {
        self.command(now, HostCommand::CloseChat { chat }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Stops everything: closes every session, stops every adapter
    /// gently, then firmly. Done once [`Core::is_idle`] is true.
    pub fn shutdown(&mut self, now: Now, out: &mut Vec<Directive>) {
        // Shutting down is never refused.
        let _ = self.command(now, HostCommand::Shutdown, out);
        self.finish(now, out);
    }

    /// Whether nothing is running: no adapters, nothing waiting.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.procs.is_empty() && self.pending.is_empty() && self.asks.is_empty() && self.runs.values().all(|run| run.running.is_none())
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
        let whole = self.chats.get(&chat)?;
        let log = self.logs.get(&chat)?;
        Some(log.read(whole, since))
    }

    /// Every connection: what each adapter process is, and serves.
    #[must_use]
    pub fn connections(&self) -> Vec<ConnectionInfo> {
        (self.conns.iter())
            .map(|(&id, conn)| ConnectionInfo {
                id,
                agent: conn.agent.clone(),
                profile: conn.profile.clone(),
                chats: conn.chats.iter().copied().collect(),
                stderr: conn.stderr.lines.iter().cloned().collect(),
            })
            .collect()
    }

    /// Carries out a host's command, as a method or a recorded event.
    fn command(&mut self, now: Now, command: HostCommand<A>, out: &mut Vec<Directive>) -> Result<(), Refused> {
        match command {
            HostCommand::OpenChat { chat, open } => self.open_chat_as(now, chat, open, out),
            HostCommand::Send { chat, turn, message } => self.send_as(now, chat, turn, message, out),
            HostCommand::Stop { chat } => self.stop_chat(now, chat, out),
            HostCommand::Answer { question, answer } => self.answer_question(now, question, answer, out),
            HostCommand::CloseChat { chat } => self.close(now, chat, out),
            HostCommand::Shutdown => {
                self.shut_down(now, out);
                Ok(())
            }
            HostCommand::AddAppItem(_) => {
                report(out, Level::Debug, "app items come in a later feature".to_owned());
                Ok(())
            }
        }
    }

    /// Ends a call into the core: every connection makes what progress it
    /// now can, viewers hear which chats changed, and the timer driver
    /// when to wake the core next.
    fn finish(&mut self, now: Now, out: &mut Vec<Directive>) {
        self.advance_all(now, out);
        for chat in std::mem::take(&mut self.dirty) {
            if let Some(log) = self.logs.get(&chat) {
                out.push(Directive::Viewer(crate::directive::ViewerDirective::Changed { chat, latest: log.latest() }));
            }
        }
        let next = self.next_deadline();
        if next != self.wake {
            self.wake = next;
            out.push(Directive::Timer(crate::directive::TimerDirective::WakeAt(next)));
        }
    }

    /// A snapshot of everything the core keeps, as JSON: what tests and
    /// recorded traces check.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> serde_json::Value {
        let pending: Vec<_> = (self.pending.iter())
            .map(|((proc, id), pending)| serde_json::json!({ "proc": proc, "id": id, "pending": pending }))
            .collect();
        serde_json::json!({
            "chats": self.chats.values().collect::<Vec<_>>(),
            "runs": self.runs,
            "conns": self.conns,
            "procs": self.procs,
            "pending": pending,
            "asks": self.asks,
            "wake": self.wake,
            "next_id": self.ids.next,
        })
    }
}

/// Adds a report for the host's log to `out`.
fn report(out: &mut Vec<Directive>, level: Level, message: String) {
    out.push(Directive::Report(Report { level, message }));
}
