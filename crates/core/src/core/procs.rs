//! Adapter processes: starting one, `initialize`, hearing it go, and
//! stopping it.
//!
//! A connection runs one process at a time. A process that goes, or is
//! replaced, is retired: nothing new is asked of it, and every chat and
//! request that hung on it is told. CAF keeps listening to a gone
//! process for its last words, then watches its process group until it's
//! empty before letting it go, and a connection never starts a new
//! process while an old one of its own is still around: the program
//! under an adapter (Claude Code, for Claude) could otherwise go on
//! writing a session the new process opens again.

use serde_json::{Value, json};

use super::clean::agent_text;
use super::frames::Reply;
use super::state::{Caps, Conn, Ending, Gone, Group, Init, Learn, Open, Proc, Purpose, Step};
use super::{Core, report};
use crate::agent::{InstallState, Launch};
use crate::chat::AppData;
use crate::chat::{AppItem, ChatState, Incompatible, Lost};
use crate::directive::{AgentDirective, Directive, Level, Signal};
use crate::event::AgentEvent;
use crate::ids::{ChatId, ConnectionId, Ticket};
use crate::profiles::{AUTH_REQUIRED, AdapterInfo, ProfileChoice};
use crate::time::Now;

/// The version of ACP CAF speaks.
const PROTOCOL_VERSION: u64 = 1;

/// The longest adapter name or version kept, in bytes.
const MOST_NAME: usize = 200;

/// How many times `gone` CAF waits at most for a gone process's last
/// words, while it keeps saying things.
const MOST_GONE: u32 = 5;

/// Why a process's chats lost it.
#[derive(Clone, Debug)]
pub(super) enum Cause {
    /// CAF restarted it.
    Restarted,
    /// It went by itself.
    Died,
    /// It couldn't be started, or failed `initialize`.
    Failed(Lost),
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// Takes in what happened to an adapter process.
    pub(super) fn agent_event(
        &mut self,
        now: Now,
        proc: Ticket,
        event: AgentEvent,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(each) = self.procs.get_mut(&proc) else {
            report(
                out,
                Level::Debug,
                format!(
                    "ignored an event from process {}, which CAF has let go",
                    proc.0
                ),
            );
            return;
        };
        match event {
            AgentEvent::Started { pid } => each.pid = Some(pid),
            AgentEvent::StartFailed { error } => self.start_failed(now, proc, &error, out),
            AgentEvent::Frame(frame) => self.frame(now, proc, frame, out),
            AgentEvent::Stderr(bytes) => self.stderr(proc, &bytes),
            AgentEvent::OutputClosed => {
                each.output_closed = true;
                self.went(now, proc, out);
            }
            AgentEvent::WriteFailed { .. } => self.went(now, proc, out),
            AgentEvent::Exited(_) => {
                each.exited = true;
                self.went(now, proc, out);
            }
            AgentEvent::Group { empty } => self.group_said(now, proc, empty, out),
        }
    }

    /// Makes progress for a connection: asks for the sessions its chats
    /// want, and one to learn its agent's settings from if that's wanted,
    /// starting a process for them if it must, or stops its process if
    /// nothing needs it.
    pub(super) fn advance(&mut self, now: Now, conn: ConnectionId, out: &mut Vec<Directive<A, D>>) {
        let Some(each) = self.conns.get(&conn) else {
            return;
        };
        let wanted: Vec<ChatId> = (each.chats.iter().copied())
            .filter(|chat| {
                self.runs
                    .get(chat)
                    .is_some_and(|run| run.open == Open::Wanted)
            })
            .collect();
        let current = each.current;
        let learning = each.learn == Some(Learn::Wanted);
        if each.chats.is_empty() && each.learn.is_none() {
            if let Some(proc) = current {
                self.retire(proc);
                self.end_proc(now, proc, false, out);
            }
            self.tidy(conn);
            return;
        }
        if let Some(proc) = current
            && self
                .procs
                .get(&proc)
                .is_some_and(|each| matches!(each.init, Init::Waiting { deadline: None }))
        {
            return self.still_slow(now, proc, &wanted, out);
        }
        if wanted.is_empty() && !learning {
            return;
        }
        match current {
            Some(proc) => {
                if self
                    .procs
                    .get(&proc)
                    .is_some_and(|each| each.caps().is_some() && each.askable())
                {
                    for chat in wanted {
                        self.ask_session(now, chat, proc, out);
                    }
                    if learning {
                        self.ask_learn(now, conn, proc, out);
                    }
                }
            }
            // A process of its own that's still around goes first.
            None if !self.shutting_down && !self.procs.values().any(|each| each.conn == conn) => {
                // An agent not installed yet, or being signed in to, is
                // waited for.
                let agent = each.agent.clone();
                let ready = self.agent_ready(&agent);
                let state = if ready {
                    ChatState::Opening
                } else {
                    ChatState::WaitingForAgent
                };
                for &chat in &wanted {
                    if self.chats.get(&chat).is_some_and(|whole| {
                        matches!(whole.state, ChatState::Opening | ChatState::WaitingForAgent)
                    }) {
                        self.set_state(chat, state.clone());
                    }
                }
                if ready {
                    self.start_process(now, conn, out);
                } else if learning {
                    // Unlike a chat, learning doesn't wait for it.
                    let why = if self.signing_in(&agent).is_some() {
                        "the agent is being signed in to"
                    } else if matches!(self.install_state(&agent), Some(InstallState::Unknown)) {
                        "CAF hasn't looked whether the agent is installed yet"
                    } else {
                        "the agent isn't installed"
                    };
                    self.learn_failed(conn, why);
                    self.tidy(conn);
                }
            }
            None => {}
        }
    }

    /// `proc` is already slow to answer `initialize`: every chat waiting
    /// on it that isn't being asked whether to keep waiting is asked now.
    /// If none waits any more, it's stopped: nobody needs it, and it
    /// seems stuck.
    fn still_slow(
        &mut self,
        now: Now,
        proc: Ticket,
        wanted: &[ChatId],
        out: &mut Vec<Directive<A, D>>,
    ) {
        if wanted.is_empty() {
            report(
                out,
                Level::Info,
                format!(
                    "stopping process {}: it never answered, and nobody waits for it",
                    proc.0
                ),
            );
            // Learning its agent's settings doesn't wait either.
            if let Some(conn) = self.procs.get(&proc).map(|each| each.conn) {
                self.learn_failed(conn, "the agent didn't start in time");
            }
            return self.lose_process(now, proc, Cause::Restarted, out);
        }
        for &chat in wanted {
            self.slow(now, chat, false);
        }
    }

    /// Makes progress for every connection: what one chat or process does
    /// can free a session another chat waits for.
    pub(super) fn advance_all(&mut self, now: Now, out: &mut Vec<Directive<A, D>>) {
        let conns: Vec<ConnectionId> = self.conns.keys().copied().collect();
        for conn in conns {
            self.advance(now, conn, out);
        }
    }

    /// Starts a process for `conn`, and introduces CAF to it.
    fn start_process(&mut self, now: Now, conn: ConnectionId, out: &mut Vec<Directive<A, D>>) {
        let Some(each) = self.conns.get(&conn) else {
            return;
        };
        let Some(spec) = self.agents.get(&each.agent) else {
            return;
        };
        let (command, hold) = match &spec.launch {
            Launch::Command(command) => (command.clone(), None),
            Launch::Installed => match self.installed_launch(&each.agent) {
                Some((command, folder)) => (command, Some(folder)),
                // Never: it's started only once it's installed.
                None => return,
            },
        };
        let folder = (hold.as_ref())
            .and_then(|hold| hold.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": spec.profile.declarations(),
            "clientInfo": { "name": self.host.name, "version": self.host.version },
        });
        let proc = Ticket(self.ids.take());
        if let Some(each) = self.conns.get_mut(&conn) {
            each.current = Some(proc);
        }
        out.push(Directive::Agent(AgentDirective::Start {
            process: proc,
            command,
            hold,
        }));
        self.procs.insert(
            proc,
            Proc {
                conn,
                next_id: i64::try_from(self.ids.first).unwrap_or(0),
                init: Init::Waiting {
                    deadline: Some(now.mono.after(self.host.timers.initialize)),
                },
                pid: None,
                exited: false,
                output_closed: false,
                gone: None,
                retired: false,
                finalized: false,
                ending: None,
                group: Group::Unwatched,
                sessions: std::collections::BTreeSet::new(),
                folder,
                given_up: std::collections::BTreeMap::new(),
            },
        );
        self.request(
            now,
            proc,
            ("initialize", params),
            Purpose::Initialize,
            None,
            out,
        );
    }

    /// The process couldn't be started: its chats are told, and it's let
    /// go, as there's nothing to stop.
    fn start_failed(
        &mut self,
        now: Now,
        proc: Ticket,
        error: &str,
        out: &mut Vec<Directive<A, D>>,
    ) {
        report(
            out,
            Level::Warn,
            format!("couldn't start an adapter: {}", agent_text(error, 300)),
        );
        let error = agent_text(error, self.host.limits.failure_text);
        if let Some(each) = self.procs.get_mut(&proc) {
            each.finalized = true;
            each.init = Init::Failed;
            // Nothing ran, so there's nothing to signal.
            each.ending = Some(Ending {
                firm: true,
                next: None,
            });
        }
        self.lose_process(now, proc, Cause::Failed(Lost::CouldNotStart { error }), out);
        // Let go at once, so the driver frees what it kept for it.
        if let Some(each) = self.procs.remove(&proc) {
            out.push(Directive::Agent(AgentDirective::Forget { process: proc }));
            self.advance(now, each.conn, out);
            self.tidy(each.conn);
        }
    }

    /// `initialize` was answered.
    pub(super) fn initialized(
        &mut self,
        now: Now,
        proc: Ticket,
        reply: Reply,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(conn) = self.procs.get(&proc).map(|each| each.conn) else {
            return;
        };
        let result = match reply {
            Reply::Result(result) => {
                // One JavaScript can't read exactly isn't kept: every number
                // in a chat is below 2^53.
                let version = (result.get("protocolVersion").and_then(Value::as_u64))
                    .filter(|&version| version <= super::updates::MOST_EXACT);
                if version == Some(PROTOCOL_VERSION) {
                    Ok(result)
                } else {
                    Err(Lost::Incompatible(Incompatible::Protocol { version }))
                }
            }
            Reply::Error(error) if error.code == AUTH_REQUIRED => Err(Lost::SignInNeeded),
            Reply::Error(error) => Err(Lost::Refused {
                message: agent_text(&error.message, self.host.limits.failure_text),
            }),
            Reply::Unreadable => Err(Lost::Incompatible(Incompatible::BadAnswer)),
        };
        // A process already retired, answering late, says nothing of the
        // adapter its connection now runs: its answer changes nothing.
        if self.procs.get(&proc).is_some_and(|each| each.retired) {
            return report(
                out,
                Level::Debug,
                format!(
                    "process {} answered initialize after it was retired",
                    proc.0
                ),
            );
        }
        let result = match result {
            Ok(result) => result,
            Err(lost) => {
                report(
                    out,
                    Level::Warn,
                    format!("an adapter failed initialize: {lost:?}"),
                );
                if let Some(each) = self.procs.get_mut(&proc) {
                    each.init = Init::Failed;
                }
                return self.lose_process(now, proc, Cause::Failed(lost), out);
            }
        };
        let adapter = (result.get("agentInfo")).and_then(|info| {
            let text = |key: &str| {
                info.get(key)
                    .and_then(Value::as_str)
                    .map(|text| agent_text(text, MOST_NAME))
            };
            Some(AdapterInfo {
                name: text("name")?,
                version: text("version")?,
            })
        });
        let Some(each) = self.conns.get_mut(&conn) else {
            return;
        };
        let Some(spec) = self.agents.get(&each.agent) else {
            return;
        };
        let checked = spec.profile.check(adapter);
        report(
            out,
            Level::Info,
            format!(
                "agent {}: {:?}, checked {:?}",
                each.agent.0, checked.adapter, checked.checked
            ),
        );
        let (agent, how) = (each.agent.clone(), checked.checked);
        let strips = checked.profile.strips();
        each.profile = Some(checked);
        if let Some(standing) = self.setup.agents.get_mut(&agent) {
            standing.strips = Some(strips);
        }
        self.agent_answered(now, proc, &agent, how, out);
        if let Some(each) = self.procs.get_mut(&proc) {
            each.init = Init::Ready(caps(&result));
        }
        // Chats told it's slow to start hear again about their session.
        let slow: Vec<_> = (self.runs.iter())
            .filter(|(_, run)| run.conn == conn && run.open == Open::Wanted)
            .filter_map(|(&chat, run)| Some((chat, run.slow?)))
            .collect();
        for (chat, question) in slow {
            self.unask_slow(chat, question);
        }
        self.advance(now, conn, out);
    }

    /// The process went, or is going: it exited, its output closed, or
    /// writing to it failed. What it said last is still read, until its
    /// output closes or `gone` passes; its group is ended firmly at once,
    /// so nothing it started goes on with a session.
    fn went(&mut self, now: Now, proc: Ticket, out: &mut Vec<Directive<A, D>>) {
        let gone = self.host.timers.gone;
        let Some(each) = self.procs.get_mut(&proc) else {
            return;
        };
        if each.gone.is_none() {
            each.gone = Some(Gone {
                since: now.mono,
                until: now.mono.after(gone),
            });
        }
        let closed = each.output_closed;
        self.end_proc(now, proc, true, out);
        if closed {
            self.finalize(now, proc, out);
        }
    }

    /// A gone process said something: its last words are waited for a
    /// little longer, up to a limit.
    pub(super) fn heard(&mut self, now: Now, proc: Ticket) {
        let wait = self.host.timers.gone;
        let Some(gone) = self
            .procs
            .get_mut(&proc)
            .and_then(|each| each.gone.as_mut())
        else {
            return;
        };
        let most = gone.since.after(wait * MOST_GONE);
        gone.until = gone.until.max(now.mono.after(wait).min(most));
    }

    /// The gone process's last words are in: everything that hung on it
    /// is told, and its group is watched until it's empty.
    pub(super) fn finalize(&mut self, now: Now, proc: Ticket, out: &mut Vec<Directive<A, D>>) {
        let Some(each) = self.procs.get_mut(&proc) else {
            return;
        };
        if each.finalized {
            return;
        }
        each.finalized = true;
        self.lose_process(now, proc, Cause::Died, out);
        self.check_group(proc, out);
    }

    /// Asks whether anything is left in `proc`'s group.
    pub(super) fn check_group(&mut self, proc: Ticket, out: &mut Vec<Directive<A, D>>) {
        if let Some(each) = self.procs.get_mut(&proc) {
            each.group = Group::Asked;
            out.push(Directive::Agent(AgentDirective::CheckGroup {
                process: proc,
            }));
        }
    }

    /// The agent driver said whether `proc`'s group is empty. Once it is,
    /// the process is let go, and its connection may start another.
    fn group_said(&mut self, now: Now, proc: Ticket, empty: bool, out: &mut Vec<Directive<A, D>>) {
        let poll = self.host.timers.group_poll;
        let Some(each) = self.procs.get_mut(&proc) else {
            return;
        };
        if each.group != Group::Asked {
            return;
        }
        if !empty {
            each.group = Group::Again(now.mono.after(poll));
            return;
        }
        let conn = each.conn;
        self.procs.remove(&proc);
        out.push(Directive::Agent(AgentDirective::Forget { process: proc }));
        self.advance(now, conn, out);
        self.tidy(conn);
    }

    /// When a gone process is let go at the latest, whatever holds its
    /// output open or its group: its last words are waited for at most
    /// five times `gone`, then its group has `kill_after` more to empty.
    /// Something it started that keeps writing could otherwise keep the
    /// driver reading it forever.
    pub(super) fn forget_by(&self, proc: Ticket) -> Option<crate::time::Mono> {
        let timers = &self.host.timers;
        let gone = self.procs.get(&proc)?.gone.as_ref()?;
        Some(
            gone.since
                .after(timers.gone * MOST_GONE + timers.kill_after),
        )
    }

    /// A gone process's time is up: it's let go, killed again first in
    /// case its group never emptied.
    pub(super) fn forget_now(&mut self, now: Now, proc: Ticket, out: &mut Vec<Directive<A, D>>) {
        self.finalize(now, proc, out);
        let Some(each) = self.procs.remove(&proc) else {
            return;
        };
        report(
            out,
            Level::Warn,
            format!(
                "letting process {} go: its group or output never ended in time",
                proc.0
            ),
        );
        out.push(signal(proc, Signal::Kill));
        out.push(Directive::Agent(AgentDirective::Forget { process: proc }));
        self.advance(now, each.conn, out);
        self.tidy(each.conn);
    }

    /// Takes `proc` from its connection: nothing new is asked of it.
    fn retire(&mut self, proc: Ticket) {
        let Some(each) = self.procs.get_mut(&proc) else {
            return;
        };
        each.retired = true;
        if let Some(conn) = self.conns.get_mut(&each.conn)
            && conn.current == Some(proc)
        {
            conn.current = None;
        }
    }

    /// Stops `proc`: firmly (SIGTERM now, SIGKILL after `kill_after`), or
    /// gently (its input closed first, then the same). A gentle stop
    /// turns firm if asked.
    pub(super) fn end_proc(
        &mut self,
        now: Now,
        proc: Ticket,
        firm: bool,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let wait = self.host.timers.kill_after;
        let Some(each) = self.procs.get_mut(&proc) else {
            return;
        };
        match &mut each.ending {
            None if firm => {
                out.push(signal(proc, Signal::Term));
                each.ending = Some(Ending {
                    firm,
                    next: Some((Step::Kill, now.mono.after(wait))),
                });
            }
            None => {
                out.push(Directive::Agent(AgentDirective::CloseInput {
                    process: proc,
                }));
                each.ending = Some(Ending {
                    firm,
                    next: Some((Step::Term, now.mono.after(wait))),
                });
            }
            Some(ending) if firm && !ending.firm => {
                ending.firm = true;
                if matches!(ending.next, Some((Step::Term, _))) {
                    out.push(signal(proc, Signal::Term));
                    ending.next = Some((Step::Kill, now.mono.after(wait)));
                }
            }
            Some(_) => {}
        }
    }

    /// The next step in stopping `proc` is due.
    pub(super) fn end_step(&mut self, now: Now, proc: Ticket, out: &mut Vec<Directive<A, D>>) {
        let wait = self.host.timers.kill_after;
        let Some(ending) = self
            .procs
            .get_mut(&proc)
            .and_then(|each| each.ending.as_mut())
        else {
            return;
        };
        match ending.next.take() {
            Some((Step::Term, _)) => {
                out.push(signal(proc, Signal::Term));
                ending.next = Some((Step::Kill, now.mono.after(wait)));
            }
            Some((Step::Kill, _)) => out.push(signal(proc, Signal::Kill)),
            None => {}
        }
    }

    /// Restarts `conn`'s process `proc`, or tells everything that hung on
    /// it that it's gone, for `cause`: every chat on it hears, not just
    /// the one that asked, and every request to it or from it ends.
    pub(super) fn lose_process(
        &mut self,
        now: Now,
        proc: Ticket,
        cause: Cause,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(each) = self.procs.get(&proc) else {
            return;
        };
        let conn = each.conn;
        let initialized = each.caps().is_some();
        let was_current = self
            .conns
            .get(&conn)
            .is_some_and(|each| each.current == Some(proc));
        self.retire(proc);
        self.end_proc(now, proc, true, out);
        let chats: Vec<ChatId> = (self.runs.iter())
            .filter(|(_, run)| run.conn == conn)
            .filter(|(_, run)| match run.open {
                Open::Asked { proc: on, .. } | Open::Live { proc: on } => on == proc,
                Open::Wanted => was_current,
                Open::Idle => false,
            } || run.running.as_ref().is_some_and(|running| running.proc == proc))
            .map(|(&chat, _)| chat)
            .collect();
        let learning = self.conns.get(&conn).and_then(|each| each.learn);
        let learned_on = match learning {
            Some(Learn::Asked { proc: on, .. }) => on == proc,
            Some(Learn::Wanted) => was_current,
            None => false,
        };
        // Every request to it ends: what each was for is settled through
        // its chat below.
        self.pending.retain(|&(on, _), _| on != proc);
        self.drop_asks(|ask| ask.proc == proc, out);
        let lost = match cause {
            Cause::Restarted => Lost::Restarted,
            Cause::Died => self.died_why(conn),
            Cause::Failed(ref lost) => lost.clone(),
        };
        let reopen = match cause {
            Cause::Restarted => true,
            Cause::Died => initialized,
            Cause::Failed(_) => false,
        };
        if !chats.is_empty() {
            report(
                out,
                Level::Info,
                format!("process {} lost: {lost:?}", proc.0),
            );
        }
        for chat in chats {
            self.session_lost(now, chat, lost.clone(), reopen, out);
        }
        if learned_on {
            self.learn_failed(conn, &super::settings::lost_why(&lost));
        }
        self.advance(now, conn, out);
    }

    /// Why a connection's process went by itself.
    fn died_why(&self, conn: ConnectionId) -> Lost {
        match self.conns.get(&conn) {
            Some(each) if each.sign_in => Lost::SignInNeeded,
            Some(each) => Lost::AgentStopped {
                stderr: each.stderr.lines.iter().cloned().collect(),
            },
            None => Lost::AgentStopped { stderr: Vec::new() },
        }
    }

    /// Lets go of a connection nothing uses.
    pub(super) fn tidy(&mut self, conn: ConnectionId) {
        let unused = self.conns.get(&conn).is_some_and(|each| {
            each.chats.is_empty() && each.current.is_none() && each.learn.is_none()
        });
        if unused && !self.procs.values().any(|each| each.conn == conn) {
            self.conns.remove(&conn);
        }
    }

    /// Takes in what the adapter wrote to its error output.
    fn stderr(&mut self, proc: Ticket, bytes: &[u8]) {
        let limits = &self.host.limits;
        let Some(conn) = self.procs.get(&proc).map(|each| each.conn) else {
            return;
        };
        let Some(each) = self.conns.get_mut(&conn) else {
            return;
        };
        let lines = each.stderr.push(
            bytes,
            limits.stderr_lines,
            limits.stderr_line_bytes,
            limits.biggest_message,
        );
        let profile = profile_of(each, &self.agents);
        if lines.iter().any(|line| profile.wants_sign_in(line)) {
            each.sign_in = true;
        }
    }

    /// The profile `chat`'s connection reads its adapter with.
    pub(super) fn profile_of_chat(&self, chat: ChatId) -> ProfileChoice {
        let conn = self
            .runs
            .get(&chat)
            .and_then(|run| self.conns.get(&run.conn));
        conn.map_or(ProfileChoice::Cautious, |conn| {
            profile_of(conn, &self.agents)
        })
    }
}

/// The profile a connection reads its adapter with: as checked, once the
/// adapter said who it is, or as chosen until then.
pub(super) fn profile_of(
    conn: &Conn,
    agents: &std::collections::BTreeMap<crate::ids::AgentName, crate::agent::AgentSpec>,
) -> ProfileChoice {
    match &conn.profile {
        Some(report) => report.profile.clone(),
        None => agents
            .get(&conn.agent)
            .map_or(ProfileChoice::Cautious, |spec| spec.profile.clone()),
    }
}

/// What an adapter said it can do, in its answer to `initialize`.
fn caps(result: &Value) -> Caps {
    let caps = result.get("agentCapabilities");
    let on = |path: &str| caps.and_then(|caps| caps.pointer(path));
    let yes = |path: &str| on(path).and_then(Value::as_bool).unwrap_or(false);
    // A session capability is advertised by an object being there.
    let has = |path: &str| on(path).is_some_and(Value::is_object);
    Caps {
        load: yes("/loadSession"),
        resume: has("/sessionCapabilities/resume"),
        close: has("/sessionCapabilities/close"),
        mcp_http: yes("/mcpCapabilities/http"),
    }
}

/// A directive sending `signal` to `proc`'s group.
fn signal<A: AppItem, D: AppData>(proc: Ticket, signal: Signal) -> Directive<A, D> {
    Directive::Agent(AgentDirective::Signal {
        process: proc,
        signal,
    })
}
