//! The core itself.
//!
//! The public methods are here; the rules live in private modules, one
//! for each part of the work.

mod asks;
mod clean;
mod frames;
mod procs;
mod record;
mod saving;
mod sessions;
mod setup;
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
use self::saving::Saving;
use self::state::{Ask, Conn, Ids, Pending, Proc, Run, Setup};
use crate::agent::{AgentSpec, AgentStatus, McpServer};
use crate::change::{Change, Mismatch, Read};
use crate::chat::AppData;
use crate::chat::{AppItem, Chat, NoAppData, NoAppItems, UserMessage};
use crate::directive::{Directive, Level, Report};
use crate::event::{Event, HostCommand};
use crate::host::Host;
use crate::ids::{
    AgentName, AgentSessionId, ChangeNo, ChatId, ConnectionId, QuestionId, Shelf, Ticket, TurnId,
};
use crate::install::Recipe;
use crate::profiles::ProfileReport;
use crate::question::Answer;
use crate::save::{RestoreChat, SavedChat};
use crate::time::{Mono, Now};

/// The most of a change that didn't fit that's reported, in bytes.
const MOST_MISFIT: usize = 1024;

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
///
/// `A` is the app's own kind of item ([`AppItem`]) and `D` its data on
/// each chat ([`AppData`]); an app with neither leaves them out.
///
/// With the feature `inspect`, it's `Clone`, so a debugging tool can keep
/// copies of a replay along the way and start again from the nearest. A
/// copy is a core of its own: given the same events, it does what the
/// original would.
#[cfg_attr(feature = "inspect", derive(Clone))]
pub struct Core<A: AppItem = NoAppItems, D: AppData = NoAppData> {
    host: Host,
    agents: BTreeMap<AgentName, AgentSpec>,
    /// Each agent's recipe, from its profile, if it has one.
    recipes: BTreeMap<AgentName, Recipe>,
    /// The name of each agent's built-in profile that never changes, which
    /// its installs are named after.
    slugs: BTreeMap<AgentName, &'static str>,
    /// Where each agent stands, and the setup driver's work.
    setup: Setup,
    chats: BTreeMap<ChatId, Chat<A>>,
    /// Each chat's data slot.
    data: BTreeMap<ChatId, D>,
    /// Where the core is with saving: each saved chat's shelf and what's
    /// still to write, the shelves listed, and the store's work.
    saving: Saving<A, D>,
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
    /// Changes that didn't fit their chat's record since the call began,
    /// and why: bugs, reported when it ends with what they said.
    misfits: Vec<(ChatId, Change<A>, Mismatch)>,
    /// What tests saw of each chat's record, change by change.
    #[cfg(test)]
    applied: BTreeMap<ChatId, record::Applied<A>>,
}

/// What to open a chat with.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OpenChat {
    /// The agent, by the name the host gave it.
    pub agent: AgentName,
    /// The folder it works in. Must be absolute.
    pub cwd: PathBuf,
    /// A new session, or one the agent already knows.
    pub session: SessionChoice,
    /// Tools to give it, as MCP servers.
    pub mcp_servers: Vec<McpServer>,
    /// Where to keep it, if it's to be saved. `None` (the default) keeps
    /// it in memory only, gone when CAF stops.
    pub shelf: Option<Shelf>,
}

impl OpenChat {
    /// A new session with `agent` in `cwd`, with no tools of the host's,
    /// not saved.
    #[must_use]
    pub fn new(agent: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            agent: AgentName(agent.into()),
            cwd: cwd.into(),
            session: SessionChoice::New,
            mcp_servers: Vec::new(),
            shelf: None,
        }
    }
}

/// Which session a chat opens.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
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
    /// The message is empty, or has a part this agent can't take.
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
    /// CAF can't install this agent: the host starts it itself, or it
    /// can't be installed on this machine.
    NotInstallable,
    /// It's installed already.
    AlreadyInstalled,
    /// CAF can't sign in to this agent: its profile has no sign-in, the
    /// sign-in needs a file from CAF's install and it isn't installed yet,
    /// or it runs the adapter, which is the host's own command, and no
    /// process of that has yet said, in `initialize`, that it's the
    /// adapter the profile is for.
    CannotSignIn,
    /// No sign-in is under way for this agent.
    NotSigningIn,
    /// The chat isn't saved: it was opened with no shelf.
    NotSaved,
    /// A chat with that id is loaded, or being restored, already.
    Loaded,
    /// The shelf's name is empty, too long, or has control characters.
    BadShelf,
}

/// What the host can see of a connection: one adapter process.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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

impl<A: AppItem, D: AppData> Core<A, D> {
    /// A core for a host, offering these agents.
    #[must_use]
    pub fn new(host: Host, agents: Vec<AgentSpec>) -> Self {
        Self::starting_ids_at(host, agents, 1)
    }

    /// A core whose ids start at `first`: tests start them at 1000, so
    /// an id mixed up with a position shows.
    pub(crate) fn starting_ids_at(host: Host, agents: Vec<AgentSpec>, first: u64) -> Self {
        let mut setup = Setup::default();
        let mut recipes = BTreeMap::new();
        let mut slugs = BTreeMap::new();
        for agent in &agents {
            match &agent.profile {
                crate::profiles::ProfileChoice::Builtin(builtin) => {
                    slugs.insert(agent.name.clone(), builtin.slug())
                }
                _ => slugs.remove(&agent.name),
            };
            if !setup.agents.contains_key(&agent.name) {
                setup.order.push(agent.name.clone());
            }
            setup
                .agents
                .insert(agent.name.clone(), setup::standing(&agent.launch));
            match agent.profile.recipe() {
                Some(recipe) => recipes.insert(agent.name.clone(), recipe),
                None => recipes.remove(&agent.name),
            };
        }
        Self {
            host,
            agents: agents
                .into_iter()
                .map(|agent| (agent.name.clone(), agent))
                .collect(),
            recipes,
            slugs,
            setup,
            chats: BTreeMap::new(),
            data: BTreeMap::new(),
            saving: Saving::default(),
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
            misfits: Vec::new(),
            #[cfg(test)]
            applied: BTreeMap::new(),
        }
    }

    /// Takes in something that happened, at `now`, and adds the work it
    /// leads to onto `out`.
    pub fn handle(&mut self, now: Now, event: Event<A, D>, out: &mut Vec<Directive<A, D>>) {
        match event {
            Event::Host(command) => {
                if let Err(refused) = self.command(now, command, out) {
                    report(
                        out,
                        Level::Debug,
                        format!("refused a host command: {refused:?}"),
                    );
                }
            }
            Event::Agent { process, event } => self.agent_event(now, process, event, out),
            Event::Woken => self.woken(now, out),
            Event::Policy { ticket, answer } => self.policy_answered(now, ticket, answer, out),
            Event::Storage { ticket, event } => self.storage_event(now, ticket, event, out),
            Event::Setup { ticket, event } => self.setup_event(now, ticket, event, out),
        }
        self.finish(now, out);
    }

    /// Looks at what's installed and what the machine has (Node, npm, its
    /// platform). Call it when CAF starts, and again whenever the user may
    /// have changed something, such as installing Node. Until the first
    /// answer comes, every installed-by-CAF agent's state is
    /// [`InstallState::Unknown`](crate::agent::InstallState::Unknown);
    /// a later look keeps what's known until it answers. Agents whose
    /// recipe has a status command are then asked whether they're signed
    /// in, where the command can run.
    pub fn check_agents(&mut self, now: Now, out: &mut Vec<Directive<A, D>>) {
        // Looking is never refused.
        let _ = self.command(now, HostCommand::CheckAgents, out);
        self.finish(now, out);
    }

    /// Where every agent stands: installed or not, signed in or not, in
    /// the order the host offered them.
    #[must_use]
    pub fn agents(&self) -> Vec<AgentStatus> {
        self.statuses()
    }

    /// Installs an agent CAF installs itself, at the version its profile
    /// was checked at. Chats waiting for it start once it's done, or are
    /// told why it failed. Installing one already being installed does
    /// nothing more.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchAgent`]; [`Refused::NotInstallable`] if the host
    /// starts it itself, or it can't be installed on this machine;
    /// [`Refused::AlreadyInstalled`].
    pub fn install(
        &mut self,
        now: Now,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::Install(agent.clone()), out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Stops installing an agent. Chats waiting for it have their waiting
    /// messages stopped, with a note saying it wasn't installed, and go on
    /// waiting for it.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchAgent`], or [`Refused::NothingToStop`].
    pub fn stop_install(
        &mut self,
        now: Now,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::StopInstall(agent.clone()), out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Starts signing in to an agent, with its profile's own sign-in,
    /// never one the agent suggests. Its state becomes
    /// [`SignInState::SigningIn`](crate::agent::SignInState::SigningIn),
    /// with the link once the command gives it. What a failed turn's
    /// [`Remedy::SignIn`](crate::chat::Remedy::SignIn) does.
    ///
    /// For an agent the host starts with its own command, the command is
    /// that, with its arguments and variables, and the recipe's arguments
    /// after them. If the agent was signed out, new chats with it wait
    /// until the sign-in ends. One nobody finishes is stopped after
    /// [`Timers::sign_in`](crate::host::Timers::sign_in).
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchAgent`]; [`Refused::CannotSignIn`] if its profile
    /// has no sign-in, or its sign-in can't run yet (see
    /// [`Refused::CannotSignIn`]).
    pub fn sign_in(
        &mut self,
        now: Now,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::SignIn(agent.clone()), out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Gives a sign-in under way what the user pasted, such as the code a
    /// sign-in page on another machine showed them. It's passed on trimmed,
    /// as one line.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchAgent`]; [`Refused::NotSigningIn`]; or
    /// [`Refused::Unsupported`] if the sign-in takes no paste, or the text
    /// is empty, more than one line, or longer than 4 KiB.
    pub fn paste(
        &mut self,
        now: Now,
        agent: &AgentName,
        text: String,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        // Refused here, not as a command, so a recording's paste, written
        // without its text, still replays.
        if text.trim().is_empty() {
            return Err(Refused::Unsupported);
        }
        let agent = agent.clone();
        self.command(now, HostCommand::Paste { agent, text }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Stops a sign-in under way.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchAgent`], or [`Refused::NotSigningIn`].
    pub fn stop_sign_in(
        &mut self,
        now: Now,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::StopSignIn(agent.clone()), out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Signs out of an agent, with its profile's own command, stopping any
    /// sign-in to it first. It's signed out once the command succeeds.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchAgent`], or [`Refused::CannotSignIn`] if its
    /// profile has no way to sign out, or it can't run yet, as for
    /// signing in.
    pub fn sign_out(
        &mut self,
        now: Now,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::SignOut(agent.clone()), out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Opens a chat, and returns its id. Its agent is started, or its
    /// running adapter shared, as the host's grouping says; messages sent
    /// before the session is open wait for it.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchAgent`], [`Refused::RelativeFolder`] or
    /// [`Refused::Unsendable`].
    pub fn open_chat(
        &mut self,
        now: Now,
        open: OpenChat,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<ChatId, Refused> {
        let chat = self.next_chat_id(now);
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
    pub fn send(
        &mut self,
        now: Now,
        chat: ChatId,
        message: UserMessage,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<TurnId, Refused> {
        let turn = TurnId(self.ids.next);
        self.command(
            now,
            HostCommand::Send {
                chat,
                turn,
                message,
            },
            out,
        )?;
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
    /// A turn the agent started by itself is stopped too. ACP doesn't say
    /// when such a turn ends, so it ends as stopped at once, and for the
    /// next 2 s what the agent says opens no new turn: it was in flight,
    /// and is dropped with a report, as are later updates to a tool call
    /// dropped so.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchChat`] or [`Refused::NothingToStop`].
    pub fn stop(
        &mut self,
        now: Now,
        chat: ChatId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
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
    pub fn answer(
        &mut self,
        now: Now,
        question: QuestionId,
        answer: Answer,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::Answer { question, answer }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Closes a chat: the user is done with it. Its session is closed, if
    /// the agent can close sessions, and its adapter stopped if nothing
    /// else uses it. A saved chat is then put away ([`Core::put_away`]),
    /// marked closed, and can still be restored; one that isn't saved
    /// stays, closed, until CAF stops.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchChat`].
    pub fn close_chat(
        &mut self,
        now: Now,
        chat: ChatId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::CloseChat { chat }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Lists a shelf's saved chats, from the store. They're in
    /// [`Core::saved`] once it answers, and kept up to date from then on
    /// as chats on it are saved, put away and deleted. Listing it again
    /// reads it again, such as to see chats another program saved.
    ///
    /// # Errors
    ///
    /// [`Refused::BadShelf`].
    pub fn list_saved(
        &mut self,
        now: Now,
        shelf: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::ListSaved(shelf), out)?;
        self.finish(now, out);
        Ok(())
    }

    /// The saved chats on a shelf that has been listed, in the order they
    /// were made, with whether each is loaded. `None` for a shelf never
    /// listed.
    #[must_use]
    pub fn saved(&self, shelf: &Shelf) -> Option<Vec<SavedChat>> {
        self.saving.shelves.get(shelf).cloned()
    }

    /// Restores a saved chat. Once the store has read it, it's loaded,
    /// as [`Core::saved`] says, and in [`Core::chat`], tidied as the
    /// [`save`](crate::save) module says: [idle](crate::chat::ChatState::Idle),
    /// with no agent started until a message is sent. Or it's
    /// [failed](crate::save::SavedState::Failed), saying why, and its save
    /// is left as it was. Its shelf needn't have been listed.
    ///
    /// # Errors
    ///
    /// [`Refused::Loaded`], [`Refused::BadShelf`], or
    /// [`Refused::Unsendable`] for its MCP servers. A chat whose agent the
    /// host no longer offers is still restored, and sending to it is
    /// refused ([`Refused::NoSuchAgent`]): see [`save`](crate::save).
    pub fn restore(
        &mut self,
        now: Now,
        restore: RestoreChat,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::Restore(restore), out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Saves a chat whole and lets it go from memory: its session is
    /// closed, its adapter stopped if nothing else uses it, and its lock
    /// in the store released. A turn running is
    /// [cut off](crate::chat::TurnLife::CutOff) first, and messages waiting
    /// are stopped. It stays on its shelf, and restoring it carries on.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchChat`], or [`Refused::NotSaved`].
    pub fn put_away(
        &mut self,
        now: Now,
        chat: ChatId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::PutAway { chat }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Deletes a chat for good, loaded or not: a loaded one is closed
    /// first, and let go from memory.
    ///
    /// # Errors
    ///
    /// [`Refused::BadShelf`], or [`Refused::NoSuchChat`] if it's loaded on
    /// another shelf.
    pub fn delete(
        &mut self,
        now: Now,
        chat: ChatId,
        shelf: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::Delete { chat, shelf }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Moves every chat on one shelf to another, loaded or not, such as
    /// when framewright saves a project under a new name.
    ///
    /// # Errors
    ///
    /// [`Refused::BadShelf`].
    pub fn move_shelf(
        &mut self,
        now: Now,
        from: Shelf,
        to: Shelf,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::MoveShelf { from, to }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// A chat's data slot: the default until the app sets it.
    #[must_use]
    pub fn data(&self, chat: ChatId) -> Option<&D> {
        self.data.get(&chat)
    }

    /// Sets a chat's data slot, whole. It's saved, if the chat is, and its
    /// [label](AppData::label) shown to viewers.
    ///
    /// # Errors
    ///
    /// [`Refused::NoSuchChat`].
    pub fn set_data(
        &mut self,
        now: Now,
        chat: ChatId,
        data: D,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        self.command(now, HostCommand::SetData { chat, data }, out)?;
        self.finish(now, out);
        Ok(())
    }

    /// Stops everything: closes every session, stops every adapter
    /// gently, then firmly, and saves every saved chat whole. Done once
    /// [`Core::is_idle`] is true.
    pub fn shutdown(&mut self, now: Now, out: &mut Vec<Directive<A, D>>) {
        // Shutting down is never refused.
        let _ = self.command(now, HostCommand::Shutdown, out);
        self.finish(now, out);
    }

    /// Whether nothing is running: no adapters, nothing waiting.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.procs.is_empty()
            && self.pending.is_empty()
            && self.asks.is_empty()
            && self.runs.values().all(|run| run.running.is_none())
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

    /// Every chat, oldest first, as lists of chats show them: with the
    /// app's label for each, and its shelf if it's saved.
    #[must_use]
    pub fn summaries(&self) -> Vec<crate::viewer::ChatSummary> {
        todo!()
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
    fn command(
        &mut self,
        now: Now,
        command: HostCommand<A, D>,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        match command {
            HostCommand::OpenChat { chat, open } => self.open_chat_as(now, chat, open, out),
            HostCommand::Send {
                chat,
                turn,
                message,
            } => self.send_as(now, chat, turn, message, out),
            HostCommand::Stop { chat } => self.stop_chat(now, chat, out),
            HostCommand::Answer { question, answer } => {
                self.answer_question(now, question, answer, out)
            }
            HostCommand::CloseChat { chat } => self.close(now, chat, out),
            HostCommand::Shutdown => {
                self.shut_down(now, out);
                Ok(())
            }
            HostCommand::CheckAgents => {
                self.survey(out);
                Ok(())
            }
            HostCommand::Install(agent) => self.install_agent(&agent, out),
            HostCommand::StopInstall(agent) => self.stop_installing(now, &agent, out),
            HostCommand::SignIn(agent) => self.start_sign_in(now, &agent, out),
            HostCommand::Paste { agent, text } => self.paste_to(&agent, &text, out),
            HostCommand::StopSignIn(agent) => self.stop_signing_in(&agent, out),
            HostCommand::SignOut(agent) => self.start_sign_out(&agent, out),
            HostCommand::ListSaved(shelf) => self.list_shelf(shelf, out),
            HostCommand::Restore(restore) => self.start_restore(restore, out),
            HostCommand::PutAway { chat } => self.put_chat_away(now, chat, out),
            HostCommand::Delete { chat, shelf } => self.delete_chat(now, chat, shelf, out),
            HostCommand::MoveShelf { from, to } => self.move_chats(from, to, out),
            HostCommand::SetData { chat, data } => self.set_chat_data(now, chat, data),
            HostCommand::AddAppItem(_) => {
                report(
                    out,
                    Level::Debug,
                    "app items come in a later feature".to_owned(),
                );
                Ok(())
            }
        }
    }

    /// Ends a call into the core: every connection makes what progress it
    /// now can, viewers hear which chats changed, and the timer driver
    /// when to wake the core next.
    fn finish(&mut self, now: Now, out: &mut Vec<Directive<A, D>>) {
        self.advance_all(now, out);
        for (chat, change, mismatch) in std::mem::take(&mut self.misfits) {
            // What it said may be the agent's, and long.
            let change = clean::agent_text(&format!("{change:?}"), MOST_MISFIT);
            report(
                out,
                Level::Warn,
                format!(
                    "a bug in CAF: a change to chat {} didn't fit its record, and was dropped: {mismatch:?}: {change}",
                    chat.0
                ),
            );
        }
        for chat in std::mem::take(&mut self.dirty) {
            if let Some(log) = self.logs.get(&chat) {
                out.push(Directive::Viewer(
                    crate::directive::ViewerDirective::Changed {
                        chat,
                        latest: log.latest(),
                    },
                ));
            }
        }
        let next = self.next_deadline();
        if next != self.wake {
            self.wake = next;
            out.push(Directive::Timer(crate::directive::TimerDirective::WakeAt(
                next,
            )));
        }
    }

    /// Everything the core keeps, as JSON: every chat and its numbered
    /// changes, connections, processes, requests waiting for answers,
    /// questions, timers, whether it's shutting down, and the host and
    /// agents it was made with. For debugging tools, such as
    /// `caf-debugger`.
    ///
    /// **No stability promise:** its shape changes without notice, in any
    /// release. Apps never use it; what they need is in [`Core::chat`],
    /// [`Core::read`] and [`Core::connections`].
    ///
    /// It holds secrets, such as the values of MCP servers' variables and
    /// headers, and of agents' environment variables: treat it as private,
    /// and never write it out as it is.
    ///
    /// Readings of the steady clock ([`Mono`](crate::time::Mono)) are
    /// nanoseconds, and pass 2^53 after about 104 days of the host running,
    /// so a JavaScript reader may round them.
    ///
    /// Needs the feature `inspect`.
    #[cfg(any(test, feature = "inspect"))]
    #[must_use]
    pub fn inspect(&self) -> serde_json::Value {
        let pending: Vec<_> = (self.pending.iter())
            .map(|((proc, id), pending)| serde_json::json!({ "proc": proc, "id": id, "pending": pending }))
            .collect();
        serde_json::json!({
            "chats": self.chats.values().collect::<Vec<_>>(),
            "logs": self.logs,
            "runs": self.runs,
            "conns": self.conns,
            "procs": self.procs,
            "pending": pending,
            "asks": self.asks,
            "wake": self.wake,
            "next_id": self.ids.next,
            "first_id": self.ids.first,
            "shutting_down": self.shutting_down,
            "host": self.host,
            "agents": self.agents,
            "setup": self.setup,
            "data": self.data,
            "saving": self.saving,
        })
    }
}

/// Adds a report for the host's log to `out`.
fn report<A: AppItem, D: AppData>(out: &mut Vec<Directive<A, D>>, level: Level, message: String) {
    out.push(Directive::Report(Report { level, message }));
}
