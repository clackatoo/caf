//! What the core keeps besides the chats themselves: connections, the
//! processes they run, the requests waiting for answers both ways, and
//! each chat's place in all that.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::Serialize;

use super::clean::Tail;
use super::settings::Choosing;
use super::spending::{Heard, Sent};
use crate::acp::RequestId;
use crate::agent::{InstallState, McpServer, SignInState};
use crate::chat::{AppItem, Cost, Note, Turn};
use crate::ids::{
    AgentName, AgentSessionId, ChatId, ConnectionId, ItemId, QuestionId, Ticket, TurnId,
};
use crate::install::Platform;
use crate::profiles::ProfileReport;
use crate::question::PermissionOption;
use crate::settings::{KnownSettings, SettingChoice};
use crate::time::{Mono, Wall};

/// Gives out ids, in order, from one counter, so no two things of any
/// kind share a number.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Ids {
    /// The next id to give out.
    pub(super) next: u64,
    /// The first id given out: where JSON-RPC ids and change numbers
    /// start too.
    pub(super) first: u64,
}

impl Ids {
    /// The next id.
    pub(super) fn take(&mut self) -> u64 {
        let id = self.next;
        self.next += 1;
        id
    }

    /// Notes that `id` was given out elsewhere, as in a recording being
    /// replayed, so it's never given out again.
    pub(super) fn seen(&mut self, id: u64) {
        self.next = self.next.max(id + 1);
    }
}

/// A connection: an agent's adapter, run as one process at a time.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Conn {
    /// The agent it runs.
    pub(super) agent: AgentName,
    /// The one chat it serves, under per-chat grouping.
    pub(super) only: Option<ChatId>,
    /// The chats it serves.
    pub(super) chats: BTreeSet<ChatId>,
    /// The process running it now, if any. Others it ran may still be
    /// going, retired.
    pub(super) current: Option<Ticket>,
    /// What became of its profile, once an adapter said who it is.
    pub(super) profile: Option<ProfileReport>,
    /// The last lines its processes wrote to their error output.
    pub(super) stderr: Tail,
    /// Its error output said the user must sign in.
    pub(super) sign_in: bool,
    /// Where it is with learning what settings its agent offers
    /// ([`Core::learn_settings`](crate::Core::learn_settings)), if it is:
    /// that keeps it going, as a chat does.
    pub(super) learn: Option<Learn>,
}

/// Where a connection is with learning its agent's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum Learn {
    /// Wanted: waiting for its process to be ready.
    Wanted,
    /// A session to learn them from was asked for, as request `id` of
    /// `proc`.
    Asked {
        /// The process.
        proc: Ticket,
        /// The request.
        id: i64,
    },
}

/// One adapter process.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Proc {
    /// The connection it runs for.
    pub(super) conn: ConnectionId,
    /// The id of the next request CAF sends it.
    pub(super) next_id: i64,
    /// Where it is with `initialize`.
    pub(super) init: Init,
    /// Its process id, for reports only.
    pub(super) pid: Option<u32>,
    /// It exited.
    pub(super) exited: bool,
    /// Its output closed.
    pub(super) output_closed: bool,
    /// It's gone: since when, and until when its last words are waited
    /// for.
    pub(super) gone: Option<Gone>,
    /// It's no longer its connection's: nothing new is asked of it.
    pub(super) retired: bool,
    /// Everything that hung on it has been told it's over.
    pub(super) finalized: bool,
    /// How it's being stopped.
    pub(super) ending: Option<Ending>,
    /// Whether its process group is being watched for emptiness.
    pub(super) group: Group,
    /// The sessions it has had open: until it's let go, no other process
    /// opens them.
    pub(super) sessions: BTreeSet<String>,
    /// The install folder it runs from, if it's CAF's install of its
    /// agent.
    pub(super) folder: Option<String>,
    /// Requests to open a session CAF gave up waiting for, with the
    /// session each asked for, if not a new one. Until one answers, its
    /// session is held, as it may still be replaying; one that answers
    /// late with a session gets it closed.
    pub(super) given_up: BTreeMap<i64, Option<String>>,
}

impl Proc {
    /// Whether CAF may write to it: it isn't gone or being killed.
    pub(super) fn writable(&self) -> bool {
        self.gone.is_none() && !self.ending.as_ref().is_some_and(|ending| ending.firm)
    }

    /// Whether new requests may be sent to it.
    pub(super) fn askable(&self) -> bool {
        self.writable() && !self.retired
    }

    /// What it can do, once it said.
    pub(super) fn caps(&self) -> Option<&Caps> {
        match &self.init {
            Init::Ready(caps) => Some(caps),
            _ => None,
        }
    }
}

/// Where a process is with `initialize`.
#[derive(Debug, Serialize)]
#[serde(tag = "type", content = "value")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) enum Init {
    /// Asked; past `deadline`, the chats waiting on it are told it's
    /// slow, and the deadline is cleared until they choose to wait more.
    Waiting {
        /// When it's slow.
        deadline: Option<Mono>,
    },
    /// Answered.
    Ready(Caps),
    /// Failed: the process is being ended.
    Failed,
}

/// What an adapter said it can do, read leniently: anything missing or
/// malformed is "can't".
#[derive(Clone, Debug, Default, Serialize)]
#[allow(clippy::struct_excessive_bools)] // one plain answer each
pub(super) struct Caps {
    /// `session/load`.
    pub(super) load: bool,
    /// `session/resume`.
    pub(super) resume: bool,
    /// `session/close`.
    pub(super) close: bool,
    /// MCP servers over HTTP.
    pub(super) mcp_http: bool,
}

/// A process that's gone, waited on for its last words.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Gone {
    /// When it went.
    pub(super) since: Mono,
    /// When to stop waiting for its last words.
    pub(super) until: Mono,
}

/// How a process is being stopped.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Ending {
    /// Firmly (signals at once), rather than gently (closing its input
    /// first).
    pub(super) firm: bool,
    /// The next step, and when.
    pub(super) next: Option<(Step, Mono)>,
}

/// A step in stopping a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum Step {
    /// SIGTERM.
    Term,
    /// SIGKILL.
    Kill,
}

/// Whether a process's group is watched for emptiness.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) enum Group {
    /// Not yet.
    Unwatched,
    /// Asked, waiting for the answer.
    Asked,
    /// Not empty last time: ask again then.
    Again(Mono),
}

/// A request CAF sent, waiting for its answer.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Pending {
    /// What it's for, and who asked.
    pub(super) purpose: Purpose,
    /// When it was sent.
    pub(super) sent: Mono,
    /// When it's overdue, if it has a watchdog.
    pub(super) deadline: Option<Mono>,
}

/// What a request is for, and who asked.
#[derive(Debug, Serialize)]
#[serde(tag = "type", content = "value")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) enum Purpose {
    /// `initialize`, for the process's connection.
    Initialize,
    /// Opening a chat's session.
    Open {
        /// The chat.
        chat: ChatId,
        /// How.
        how: How,
        /// The session asked for, if not a new one.
        session: Option<AgentSessionId>,
        /// The chat stopped waiting for it: a session it opens is closed.
        abandoned: bool,
    },
    /// A chat's turn.
    Prompt {
        /// The chat.
        chat: ChatId,
        /// The turn.
        turn: TurnId,
        /// The turn ended without it: its answer changes nothing.
        abandoned: bool,
    },
    /// Closing a session.
    Close {
        /// The session.
        session: AgentSessionId,
    },
    /// Setting one of a chat's settings.
    Set {
        /// The chat.
        chat: ChatId,
        /// What was asked for.
        choice: SettingChoice,
    },
    /// Setting a chat's session's permission mode as it opens.
    Mode {
        /// The chat.
        chat: ChatId,
    },
    /// Opening a session only to learn what settings a connection's agent
    /// offers.
    Learn {
        /// The connection.
        conn: ConnectionId,
    },
}

/// How a session is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum How {
    /// `session/new`.
    New,
    /// `session/load`: the agent replays it first.
    Load,
    /// `session/resume`: no replay.
    Resume,
}

impl How {
    pub(super) fn method(self) -> &'static str {
        match self {
            Self::New => "session/new",
            Self::Load => "session/load",
            Self::Resume => "session/resume",
        }
    }
}

/// A request from the agent waiting for CAF's answer: a permission
/// request, asked of the policy driver or the user. Kept under the
/// ticket of the policy's [`Decide`](crate::directive::PolicyDirective::Decide).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Ask {
    /// The process that asked.
    pub(super) proc: Ticket,
    /// Its id for the request.
    pub(super) id: RequestId,
    /// The chat it's in.
    pub(super) chat: ChatId,
    /// The question the user was asked, once the policy said to.
    pub(super) question: Option<QuestionId>,
    /// The answers the agent offered.
    pub(super) options: Vec<PermissionOption>,
    /// What the agent wants to do, as it puts it.
    pub(super) title: String,
    /// The tool call it's about.
    pub(super) tool: Option<ItemId>,
}

/// A chat's place in it all.
#[derive(Debug, Serialize)]
#[serde(bound = "A: AppItem")]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Run<A: AppItem> {
    /// Its connection.
    pub(super) conn: ConnectionId,
    /// The folder the agent works in, as it's sent.
    pub(super) cwd: String,
    /// The tools given to it.
    pub(super) mcp: Vec<McpServer>,
    /// The tools given to it, as they're sent: CAF's own last, if it has
    /// any, its relay's token never shown.
    pub(super) mcp_sent: SentServers,
    /// What the app tells the agent, after CAF's own instructions, each
    /// time its session is opened.
    pub(super) instructions: Option<String>,
    /// Its session as the agent knows it: open, or to be continued.
    pub(super) session: Option<AgentSessionId>,
    /// Where it is with its session.
    pub(super) open: Open,
    /// The turn the agent is working on.
    pub(super) running: Option<Running>,
    /// A turn the agent started by itself, open while it says things
    /// outside any prompt.
    pub(super) agent_turn: Option<TurnId>,
    /// When that turn has been quiet long enough to end.
    pub(super) quiet_by: Option<Mono>,
    /// A turn the agent started was just stopped: until then, what was in
    /// flight opens no new one.
    pub(super) held_until: Option<Mono>,
    /// The tool calls dropped as in flight then: later updates to them are
    /// dropped too, rather than open a tool with no title.
    pub(super) dropped_tools: BTreeSet<String>,
    /// Its tool calls, by the agent's ids.
    pub(super) tools: BTreeMap<String, ItemId>,
    /// Its tool calls CAF serves, whose record the agent's reports never
    /// change.
    pub(super) served: BTreeMap<ItemId, super::tools::Served>,
    /// The history being replayed by `session/load`, the turn being
    /// built.
    pub(super) import: Option<Turn<A>>,
    /// A `session/load` is replaying its history.
    pub(super) replaying: bool,
    /// The question asking whether to keep waiting for it to open.
    pub(super) slow: Option<QuestionId>,
    /// Waiting for something that holds its session to let it go: when to
    /// ask whether to keep waiting.
    pub(super) held_by: Option<Mono>,
    /// Something to say once the session is open.
    pub(super) note: Option<Note>,
    /// A turn sent again after its session was dropped: it isn't sent a
    /// third time.
    pub(super) resent: Option<TurnId>,
    /// It was restored, so it has its record already: what a
    /// `session/load` replays is thrown away, not imported.
    pub(super) restored: bool,
    /// Where it is with sending what's chosen of its settings.
    pub(super) choosing: Choosing,
    /// What its session has cost so far, as the agent last said: an
    /// update that doesn't say leaves it as it was.
    pub(super) cost: Option<Cost>,
    /// CAF's own tools it was given, served to it as the MCP server `caf`.
    pub(super) caf_tools: Vec<crate::tool::CafTool>,
    /// Its session was told its connection to CAF's tools was lost: it's
    /// told at most once a session.
    pub(super) tools_lost: bool,
    /// The folder its next command runs in: where the last one's shell
    /// ended, from its `cwd` at first. One run's.
    pub(super) shell: String,
}

/// Where a chat is with its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum Open {
    /// Not open, and not wanted until a message comes.
    Idle,
    /// Wanted: waiting for its connection's process to be ready.
    Wanted,
    /// Asked for, as request `id` of `proc`.
    Asked {
        /// The process.
        proc: Ticket,
        /// The request.
        id: i64,
    },
    /// Open on `proc`.
    Live {
        /// The process.
        proc: Ticket,
    },
}

/// The turn the agent is working on.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Running {
    /// The turn.
    pub(super) turn: TurnId,
    /// The process its prompt went to.
    pub(super) proc: Ticket,
    /// The prompt's request id.
    pub(super) id: i64,
    /// Stop was asked for: when to give up waiting for the agent to
    /// stop.
    pub(super) stop_by: Option<Mono>,
    /// What was so as it was sent, that what it spent is worked out from.
    pub(super) sent: Sent,
    /// What the agent said of its usage while it ran.
    pub(super) heard: Heard,
}

/// Where the core is with installing agents and signing in to them.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Setup {
    /// Every agent, in the order the host offered them.
    pub(super) order: Vec<AgentName>,
    /// Where each agent stands.
    pub(super) agents: BTreeMap<AgentName, Standing>,
    /// What the last survey found, once one answered.
    pub(super) machine: Option<Machine>,
    /// The survey waited for: an answer to any other is stale.
    pub(super) survey: Option<Ticket>,
    /// Folders installed while that survey was out, which it may have
    /// looked too early to find.
    pub(super) fresh: BTreeSet<String>,
    /// Work the setup driver is doing, by ticket.
    pub(super) jobs: BTreeMap<Ticket, Job>,
    /// Folders preparing failed for: they're installed as any other, and
    /// never prepared again, so a failure can't repeat itself.
    pub(super) unprepared: BTreeSet<String>,
    /// Agents the host asked to install before any survey answered, whose
    /// recipe patches them: whether they're prepared from an earlier level
    /// or downloaded waits for the survey.
    pub(super) deferred: BTreeSet<AgentName>,
    /// Folders the user stopped installing this run: a survey never
    /// prepares one by itself, until the host asks to install it again.
    pub(super) declined: BTreeSet<String>,
}

/// Where one agent stands.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Standing {
    /// Whether it's installed.
    pub(super) install: InstallState,
    /// Whether it's signed in.
    pub(super) sign_in: SignInState,
    /// A process of the host's own command for it said, in `initialize`,
    /// that it's the adapter its profile is for: commands of its recipe
    /// that run the adapter may run it.
    pub(super) checked: bool,
    /// Whether CAF can turn off the own tools of the adapter that last
    /// answered `initialize` for it in this run, as its profile, checked,
    /// says: `None` until one has.
    pub(super) strips: Option<bool>,
    /// What's known of the settings it offers, this run.
    pub(super) settings: KnownSettings,
}

/// What a survey found: where installs live, what's there, and what the
/// machine has.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Machine {
    /// The folder installs live in.
    pub(super) root: PathBuf,
    /// The complete install folders there, by name.
    pub(super) installed: BTreeSet<String>,
    /// Node's major version, if there's Node.
    pub(super) node: Option<u32>,
    /// Whether there's npm.
    pub(super) npm: bool,
    /// The platform.
    pub(super) platform: Platform,
    /// When each install folder was last used, by name.
    pub(super) used: BTreeMap<String, Wall>,
}

/// A directive the setup driver is carrying out.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Job {
    /// The agent it's for.
    pub(super) agent: AgentName,
    /// What it is.
    pub(super) work: Work,
    /// It was stopped, or is being: what it reports changes nothing,
    /// except an install that finished anyway.
    pub(super) stopped: bool,
    /// When it's stopped if it hasn't ended: a sign-in nobody finishes.
    pub(super) deadline: Option<Mono>,
}

/// What a setup job is.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum Work {
    /// Installing into this folder.
    Install {
        /// The folder.
        folder: String,
        /// The earlier patch level it's prepared from, with nothing
        /// downloaded, if it is.
        from: Option<String>,
        /// The host asked for it: a prepare it asked for that fails is
        /// downloaded at once, as the user agreed to install it.
        asked: bool,
    },
    /// Removing this folder.
    Remove {
        /// The folder.
        folder: String,
    },
    /// Signing in. If it doesn't end signed in, the agent's sign-in state
    /// goes back to `before`.
    SignIn {
        /// The state before it started, kept up to date by reports.
        before: SignInState,
    },
    /// Signing out.
    SignOut,
    /// Running a status command.
    Status,
}

/// A chat's MCP servers as they're sent: what's written or printed of
/// them shows no relay's token, which is `[hidden]`
/// ([`mcp::HIDDEN`](crate::mcp::HIDDEN)), so neither
/// [`Core::inspect`](crate::Core::inspect) nor a log of a run holds one.
#[derive(Clone, Default, PartialEq)]
pub(super) struct SentServers(pub(super) Vec<serde_json::Value>);

/// The variable that holds a relay's token.
pub(super) const TOKEN_VAR: &str = "CAF_RELAY_TOKEN";

impl SentServers {
    /// The servers, with each relay's token `[hidden]`.
    fn shown(&self) -> Vec<serde_json::Value> {
        let mut shown = self.0.clone();
        for server in &mut shown {
            for pair in (server
                .get_mut("env")
                .and_then(serde_json::Value::as_array_mut))
            .into_iter()
            .flatten()
            {
                if pair["name"] == TOKEN_VAR {
                    pair["value"] = crate::mcp::HIDDEN.into();
                }
            }
        }
        shown
    }
}

impl Serialize for SentServers {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.shown().serialize(serializer)
    }
}

impl std::fmt::Debug for SentServers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.shown().fmt(f)
    }
}
