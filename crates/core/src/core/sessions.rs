//! Chats and their sessions: opening one, continuing one the agent
//! knows, waiting for a slow one, losing one, and closing.
//!
//! How a session is opened is decided from what the adapter said it can
//! do, never sent blind and rewritten: a session to continue is resumed
//! if the adapter can, loaded (its history replayed and imported) if it
//! can only load, and otherwise a new one is opened and the chat says so.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::clean::agent_text;
use super::frames::Reply;
use super::procs::Cause;
use super::record::Log;
use super::settings::{Choosing, sendable};
use super::state::{Conn, How, Open, Purpose, Run};
use super::{Core, OpenChat, Refused, SessionChoice, report};
use crate::agent::{EnvChange, McpServer};
use crate::change::Change;
use crate::chat::AppData;
use crate::chat::{
    AppItem, Chat, ChatState, Failure, FailureReason, ItemKind, Lost, Note, Remedy, Starter, Turn,
    TurnLife,
};
use crate::directive::{Directive, Level};
use crate::host::Grouping;
use crate::ids::{AgentName, AgentSessionId, ChatId, ConnectionId, QuestionId, Ticket};
use crate::profiles::{AUTH_REQUIRED, CAF_SERVER, claude_server_name, own_instructions};
use crate::question::{QuestionKind, SlowChoice, SlowToOpen};
use crate::time::{Now, Window};

/// The longest session id CAF keeps, in bytes.
const MOST_SESSION_ID: usize = 256;

/// The longest instructions an app may give an agent, in bytes: 64 KiB,
/// some 16,000 words, sent with every session opened.
pub(crate) const MOST_INSTRUCTIONS: usize = 64 << 10;

impl<A: AppItem, D: AppData> Core<A, D> {
    /// Opens chat `chat`.
    pub(super) fn open_chat_as(
        &mut self,
        now: Now,
        chat: ChatId,
        open: OpenChat,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        let spec = self.agents.get(&open.agent).ok_or(Refused::NoSuchAgent)?;
        if !spec.profile.strips() {
            return Err(Refused::OwnTools);
        }
        if !open.cwd.is_absolute() {
            return Err(Refused::RelativeFolder);
        }
        let cwd = open.cwd.to_str().ok_or(Refused::Unsendable)?.to_owned();
        check_servers(&open.mcp_servers)?;
        let mut mcp_sent = open
            .mcp_servers
            .iter()
            .map(mcp_server)
            .collect::<Option<Vec<_>>>()
            .ok_or(Refused::Unsendable)?;
        let app_tools = self.app_tools_given(open.tools.app)?;
        let caf_tools = crate::tool::Tools::served(open.tools.caf, !app_tools.is_empty());
        mcp_sent.extend(self.caf_server(chat, &caf_tools, &app_tools)?);
        check_instructions(open.instructions.as_deref())?;
        if !open.settings.iter().all(sendable) {
            return Err(Refused::Unsendable);
        }
        if let Some(shelf) = &open.shelf {
            super::saving::check_shelf(shelf)?;
        }
        if self.chats.contains_key(&chat) {
            report(out, Level::Warn, format!("chat {} is already open", chat.0));
            return Ok(());
        }
        // A numbered id, as recordings made before chats had UUIDs hold,
        // came from the counter every other id does.
        if let Ok(number) = u64::try_from(chat.0.as_u128()) {
            self.ids.seen(number);
        }
        self.made_chat(chat);
        self.data.insert(chat, D::default());
        self.call_off_shutdown(now);
        let conn = self.conn_for(chat, &open.agent);
        let settings = self.first_settings(&open.agent, &open.settings);
        self.chats.insert(
            chat,
            Chat {
                id: chat,
                agent: open.agent,
                cwd: open.cwd.clone(),
                state: ChatState::Opening,
                turns: Vec::new(),
                usage: None,
                questions: Vec::new(),
                settings,
                spend: Box::default(),
                archived: None,
            },
        );
        self.logs.insert(chat, Log::new(self.ids.first));
        self.dirty.insert(chat);
        self.runs.insert(
            chat,
            Run {
                conn,
                shell: cwd.clone(),
                notices_held: false,
                shell_warned: false,
                cwd,
                mcp: open.mcp_servers,
                mcp_sent: super::state::SentServers(mcp_sent),
                instructions: open.instructions,
                session: match open.session {
                    SessionChoice::New => None,
                    SessionChoice::Continue(session) => Some(session),
                },
                open: Open::Wanted,
                running: None,
                agent_turn: None,
                quiet_by: None,
                held_until: None,
                dropped_tools: std::collections::BTreeSet::new(),
                tools: BTreeMap::new(),
                served: BTreeMap::new(),
                import: None,
                replaying: false,
                slow: None,
                held_by: None,
                note: None,
                resent: None,
                restored: false,
                choosing: Choosing::default(),
                cost: None,
                caf_tools,
                app_tools,
                tools_lost: false,
            },
        );
        self.serve(chat, out);
        if let Some(shelf) = open.shelf {
            self.keep_new(now, chat, shelf);
        }
        // Its first picture shows it waiting for its agent, if it must.
        let state = self.wanting(chat);
        if let Some(whole) = self.chats.get_mut(&chat) {
            whole.state = state;
        }
        self.advance(now, conn, out);
        Ok(())
    }

    /// The connection a new chat with `agent` joins, as the host's
    /// grouping says.
    pub(super) fn conn_for(&mut self, chat: ChatId, agent: &AgentName) -> ConnectionId {
        let per_chat = self.host.grouping == Grouping::PerChat;
        let shared = (self.conns.iter())
            .find(|(_, each)| !per_chat && each.only.is_none() && each.agent == *agent);
        let conn = match shared {
            Some((&conn, _)) => conn,
            None => {
                let conn = ConnectionId(self.ids.take());
                self.conns.insert(
                    conn,
                    Conn {
                        agent: agent.clone(),
                        only: per_chat.then_some(chat),
                        chats: std::collections::BTreeSet::new(),
                        current: None,
                        profile: None,
                        stderr: super::clean::Tail::default(),
                        sign_in: false,
                        learn: None,
                    },
                );
                conn
            }
        };
        if let Some(each) = self.conns.get_mut(&conn) {
            each.chats.insert(chat);
        }
        conn
    }

    /// The chat wants its session open again.
    pub(super) fn want_open(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        run.open = Open::Wanted;
        let conn = run.conn;
        let state = self.wanting(chat);
        self.set_state(chat, state);
        self.advance(now, conn, out);
    }

    /// Asks `proc` for `chat`'s session, which has said what it can do.
    pub(super) fn ask_session(
        &mut self,
        now: Now,
        chat: ChatId,
        proc: Ticket,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(caps) = self.procs.get(&proc).and_then(|each| each.caps().cloned()) else {
            return;
        };
        let Some(run) = self.runs.get(&chat) else {
            return;
        };
        if let Some(session) = &run.session {
            match self.holder(chat, proc, session) {
                Some(Hold::Chat) => return self.session_lost(now, chat, Lost::InUse, false, out),
                // It waits for whatever holds it to let it go, and is asked
                // whether to keep waiting if that takes long.
                Some(Hold::Going) => {
                    let wait = now.mono.after(self.host.timers.session_open);
                    if let Some(run) = self.runs.get_mut(&chat)
                        && run.held_by.is_none()
                        && run.slow.is_none()
                    {
                        run.held_by = Some(wait);
                    }
                    return;
                }
                None => {}
            }
        }
        // An adapter that turned out not to be the one its profile is
        // for is read cautiously, and CAF can't turn off its tools.
        let profile = self.profile_of_chat(chat);
        if !profile.strips() {
            return self.session_lost(
                now,
                chat,
                Lost::Incompatible(crate::chat::Incompatible::OwnTools),
                false,
                out,
            );
        }
        let Some(run) = self.runs.get(&chat) else {
            return;
        };
        if !caps.mcp_http
            && run
                .mcp
                .iter()
                .any(|server| matches!(server, McpServer::Http { .. }))
        {
            return self.session_lost(
                now,
                chat,
                Lost::Incompatible(crate::chat::Incompatible::HttpMcp),
                false,
                out,
            );
        }
        let how = match &run.session {
            None => How::New,
            Some(_) if caps.resume => How::Resume,
            Some(_) if caps.load => How::Load,
            Some(_) => How::New,
        };
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        if how == How::New && run.session.take().is_some() {
            run.note = Some(Note::CannotContinue);
        }
        if how == How::Load && run.restored {
            report(
                out,
                Level::Debug,
                format!(
                    "chat {}: what the agent replays of its session is thrown away, as CAF has its record",
                    chat.0
                ),
            );
        }
        let mut params = json!({
            "cwd": run.cwd,
            "mcpServers": run.mcp_sent.0,
        });
        if let Some(session) = &run.session {
            params["sessionId"] = json!(session.0);
        }
        let own = own_instructions(&self.host.name, &run.caf_tools, &run.app_tools);
        if let Some(meta) = profile.stripped(&own, run.instructions.as_deref()) {
            params["_meta"] = meta;
        }
        let purpose = Purpose::Open {
            chat,
            how,
            session: run.session.clone(),
            abandoned: false,
        };
        let wait = self.host.timers.session_open;
        if let Some(id) = self.request(now, proc, (how.method(), params), purpose, Some(wait), out)
            && let Some(run) = self.runs.get_mut(&chat)
        {
            run.open = Open::Asked { proc, id };
            run.replaying = how == How::Load;
            run.held_by = None;
            // A new wait starts, with a deadline of its own.
            if let Some(question) = run.slow {
                self.unask_slow(chat, question);
            }
        }
    }

    /// What keeps `chat` from opening `session` on `proc`, if anything.
    /// A session is held, across every connection of the chat's agent, by
    /// another chat that has it or asked for it, by any request still
    /// opening it (even one nobody waits for, or has given up on) or
    /// closing it, and by any other process
    /// that had it and isn't let go yet.
    fn holder(&self, chat: ChatId, proc: Ticket, session: &AgentSessionId) -> Option<Hold> {
        let agent = &self.chats.get(&chat)?.agent;
        let agents_proc = |proc: &Ticket| {
            let conn = self
                .procs
                .get(proc)
                .and_then(|each| self.conns.get(&each.conn));
            conn.is_some_and(|conn| &conn.agent == agent)
        };
        let chat_holds = (self.runs.iter()).any(|(&other, run)| {
            let agents = self
                .chats
                .get(&other)
                .is_some_and(|whole| &whole.agent == agent);
            let holds = matches!(run.open, Open::Asked { .. } | Open::Live { .. });
            other != chat && agents && holds && run.session.as_ref() == Some(session)
        });
        if chat_holds {
            return Some(Hold::Chat);
        }
        let opening = (self.pending.iter()).any(|((on, _), pending)| {
            let about = match &pending.purpose {
                Purpose::Open {
                    session: Some(asked),
                    ..
                }
                | Purpose::Close { session: asked } => asked == session,
                _ => false,
            };
            about && agents_proc(on)
        });
        let given_up = (self.procs.iter()).any(|(on, each)| {
            each.given_up
                .values()
                .any(|asked| asked.as_ref() == Some(&session.0))
                && agents_proc(on)
        });
        let elsewhere = (self.procs.iter())
            .any(|(on, each)| *on != proc && each.sessions.contains(&session.0) && agents_proc(on));
        (opening || given_up || elsewhere).then_some(Hold::Going)
    }

    /// The agent answered request `id` of `proc`, which asked for `chat`'s
    /// session `how`, continuing `asked` if not a new one.
    pub(super) fn opened(
        &mut self,
        now: Now,
        (proc, id): (Ticket, i64),
        chat: ChatId,
        (how, asked, abandoned): (How, Option<AgentSessionId>, bool),
        reply: Reply,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let waiting = !abandoned
            && self
                .runs
                .get(&chat)
                .is_some_and(|run| run.open == Open::Asked { proc, id });
        if waiting {
            self.end_replay(chat);
        }
        match reply {
            Reply::Result(result) => {
                let made = result
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(|id| AgentSessionId(id.to_owned()));
                let session = made.or(asked).filter(|session| keepable(&session.0));
                match (waiting, session) {
                    (true, Some(session)) => {
                        self.session_open(now, chat, proc, session, &result, out);
                    }
                    (true, None) => {
                        report(
                            out,
                            Level::Warn,
                            "the agent gave a session id CAF can't keep".to_owned(),
                        );
                        self.session_lost(
                            now,
                            chat,
                            Lost::Incompatible(crate::chat::Incompatible::BadAnswer),
                            false,
                            out,
                        );
                    }
                    (false, Some(session)) => self.close_late(now, proc, session, out),
                    (false, None) => {}
                }
            }
            Reply::Error(_) | Reply::Unreadable if !waiting => {}
            Reply::Error(error)
                if how != How::New && self.profile_of_chat(chat).session_gone(&error) =>
            {
                // Gone from the agent: a new session, and the chat says so.
                if let Some(run) = self.runs.get_mut(&chat) {
                    run.session = None;
                    run.note = Some(Note::SessionGone);
                    run.open = Open::Wanted;
                }
                self.ask_session(now, chat, proc, out);
            }
            Reply::Error(error) => {
                let lost = if error.code == AUTH_REQUIRED {
                    Lost::SignInNeeded
                } else {
                    Lost::Refused {
                        message: agent_text(&error.message, self.host.limits.failure_text),
                    }
                };
                self.session_lost(now, chat, lost, false, out);
            }
            Reply::Unreadable => {
                self.session_lost(
                    now,
                    chat,
                    Lost::Incompatible(crate::chat::Incompatible::BadAnswer),
                    false,
                    out,
                );
            }
        }
    }

    /// `chat`'s session is open, as `session` on `proc`, the agent having
    /// answered `result`.
    fn session_open(
        &mut self,
        now: Now,
        chat: ChatId,
        proc: Ticket,
        session: AgentSessionId,
        result: &Value,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        run.open = Open::Live { proc };
        run.session = Some(session.clone());
        // A new session is told afresh if CAF's tools are lost.
        run.tools_lost = false;
        // A session opened, new or again, says its cost afresh.
        run.cost = None;
        if let Some(each) = self.procs.get_mut(&proc) {
            each.sessions.insert(session.0.clone());
        }
        let note = run.note.take();
        if let Some(question) = run.slow {
            self.unask_slow(chat, question);
        }
        self.set_state(chat, ChatState::Open { session });
        if let Some(note) = note {
            self.say(now, chat, note);
        }
        // What it offers, and the choices to send before any message.
        self.settings_opened(chat, result);
        self.mode_opened(chat, result);
        self.pump(now, chat, out);
    }

    /// A session nobody waits for any more opened: it's closed, if no
    /// chat holds it now and the agent can close sessions. Otherwise it
    /// lives until its adapter stops.
    pub(super) fn close_late(
        &mut self,
        now: Now,
        proc: Ticket,
        session: AgentSessionId,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if let Some(each) = self.procs.get_mut(&proc) {
            each.sessions.insert(session.0.clone());
        }
        let held = (self.runs.values()).any(|run| {
            matches!(run.open, Open::Asked { .. } | Open::Live { .. })
                && run.session.as_ref() == Some(&session)
        });
        if held {
            return;
        }
        if self
            .procs
            .get(&proc)
            .and_then(|each| each.caps())
            .is_some_and(|caps| caps.close)
        {
            let params = json!({ "sessionId": session.0 });
            let wait = self.host.timers.request;
            self.request(
                now,
                proc,
                ("session/close", params),
                Purpose::Close { session },
                Some(wait),
                out,
            );
        } else {
            report(out, Level::Info, "a session opened after its chat stopped waiting stays open until its adapter stops".to_owned());
        }
    }

    /// The history a `session/load` replayed is all in: the turn being
    /// built is added.
    pub(super) fn end_replay(&mut self, chat: ChatId) {
        if let Some(run) = self.runs.get_mut(&chat) {
            run.replaying = false;
        }
        self.flush_import(chat);
    }

    /// Adds the imported turn being built, if there is one.
    pub(super) fn flush_import(&mut self, chat: ChatId) {
        let Some(mut turn) = self.runs.get_mut(&chat).and_then(|run| run.import.take()) else {
            return;
        };
        super::updates::settle_imported(&mut turn);
        self.add_turn_before_waiting(chat, turn);
    }

    /// `chat` lost its session, or never got it, because of `lost`. Its
    /// running turn fails. If `reopen`, it opens again at once when it was
    /// opening or has messages waiting; otherwise its waiting messages
    /// fail, visibly, rather than wait for something that won't come.
    pub(super) fn session_lost(
        &mut self,
        now: Now,
        chat: ChatId,
        lost: Lost,
        reopen: bool,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        let opening = matches!(run.open, Open::Wanted | Open::Asked { .. });
        run.open = Open::Idle;
        run.held_by = None;
        let running = run.running.take();
        let slow = run.slow;
        self.end_replay(chat);
        if let Some(question) = slow {
            self.unask_slow(chat, question);
        }
        let mut failure = failure_for(&lost);
        self.sign_in_failure(chat, &mut failure);
        if let Some(running) = running {
            // A turn being stopped ends as the user asked.
            let life = if running.stop_by.is_some() {
                TurnLife::Stopped
            } else {
                TurnLife::Failed(failure.clone())
            };
            self.end_turn(now, chat, running.turn, life);
        }
        self.end_agent_turn(now, chat);
        if self
            .chats
            .get(&chat)
            .is_none_or(|whole| whole.state == ChatState::Closed)
        {
            return;
        }
        self.set_state(chat, ChatState::Lost { reason: lost });
        let waiting = self.waiting_turns(chat);
        if reopen && (opening || !waiting.is_empty()) {
            self.want_open(now, chat, out);
        } else {
            for turn in waiting {
                self.set_life(now, chat, turn, TurnLife::Failed(failure.clone()));
            }
        }
    }

    /// Adds a turn of CAF's own to `chat`, saying `note`: before any
    /// message waiting to go, as what it says comes first.
    pub(super) fn say(&mut self, now: Now, chat: ChatId, note: Note) {
        let turn = crate::ids::TurnId(self.ids.take());
        let item = crate::chat::Item {
            id: crate::ids::ItemId(self.ids.take()),
            window: Window::instant(now),
            parent: None,
            kind: ItemKind::Note(note),
        };
        self.add_turn_before_waiting(
            chat,
            Turn {
                id: turn,
                starter: Starter::Caf,
                life: TurnLife::Finished,
                window: Window::instant(now),
                items: vec![item],
                imported: false,
                model: None,
                spent: None,
            },
        );
    }

    /// Asks the user whether to keep waiting for `chat`'s session, if
    /// they aren't being asked already.
    pub(super) fn slow(&mut self, now: Now, chat: ChatId, adapter_answered: bool) {
        if self.runs.get(&chat).is_none_or(|run| run.slow.is_some()) {
            return;
        }
        let question = self.ask(
            now,
            chat,
            QuestionKind::SlowToOpen(SlowToOpen { adapter_answered }),
        );
        if let Some(run) = self.runs.get_mut(&chat) {
            run.slow = Some(question);
        }
    }

    /// Withdraws the question whether to keep waiting.
    pub(super) fn unask_slow(&mut self, chat: ChatId, question: QuestionId) {
        if let Some(run) = self.runs.get_mut(&chat) {
            run.slow = None;
        }
        self.unask(chat, question);
    }

    /// The user chose what to do about `chat`'s slow session.
    pub(super) fn slow_chosen(
        &mut self,
        now: Now,
        chat: ChatId,
        question: QuestionId,
        choice: SlowChoice,
        out: &mut Vec<Directive<A, D>>,
    ) {
        self.unask_slow(chat, question);
        let Some(run) = self.runs.get(&chat) else {
            return;
        };
        let conn = run.conn;
        let proc = match run.open {
            Open::Asked { proc, .. } => Some(proc),
            Open::Wanted => self.conns.get(&conn).and_then(|each| each.current),
            _ => None,
        };
        match choice {
            SlowChoice::KeepWaiting => self.keep_waiting(now, chat),
            SlowChoice::Stop => self.stop_opening(now, chat, out),
            SlowChoice::Retry => match proc {
                Some(proc) if self.busy_besides(proc, chat) => {
                    // Restarting would cut another chat's turn off: this
                    // session is just asked for again.
                    report(
                        out,
                        Level::Info,
                        format!("chat {}: asking again, as another chat is busy", chat.0),
                    );
                    self.abandon_open(now, chat);
                    self.want_open(now, chat, out);
                }
                Some(proc) => {
                    report(
                        out,
                        Level::Info,
                        format!("chat {}: restarting its adapter, as asked", chat.0),
                    );
                    self.lose_process(now, proc, Cause::Restarted, out);
                }
                None => self.want_open(now, chat, out),
            },
        }
    }

    /// Whether a chat other than `chat` has a turn running on `proc`.
    pub(super) fn busy_besides(&self, proc: Ticket, chat: ChatId) -> bool {
        (self.runs.iter()).any(|(&other, run)| {
            other != chat
                && run
                    .running
                    .as_ref()
                    .is_some_and(|running| running.proc == proc)
        })
    }

    /// Waits as long again for `chat`'s session, or its adapter.
    fn keep_waiting(&mut self, now: Now, chat: ChatId) {
        let timers = self.host.timers.clone();
        let Some(run) = self.runs.get(&chat) else {
            return;
        };
        match run.open {
            Open::Asked { proc, id } => {
                if let Some(pending) = self.pending.get_mut(&(proc, id)) {
                    pending.deadline = Some(now.mono.after(timers.session_open));
                }
            }
            // Waiting for the adapter itself: every chat waiting on it
            // waits again, and is asked again together. Waiting on a hold,
            // the wait starts again as the chat is next asked for.
            Open::Wanted => {
                if let Some(run) = self.runs.get_mut(&chat) {
                    run.held_by = None;
                }
                let Some(run) = self.runs.get(&chat) else {
                    return;
                };
                let conn = run.conn;
                let current = self.conns.get(&conn).and_then(|each| each.current);
                if let Some(each) = current.and_then(|proc| self.procs.get_mut(&proc))
                    && let super::state::Init::Waiting { deadline } = &mut each.init
                {
                    *deadline = Some(now.mono.after(timers.initialize));
                }
                let asked: Vec<(ChatId, QuestionId)> = (self.runs.iter())
                    .filter(|(_, run)| run.conn == conn && run.open == Open::Wanted)
                    .filter_map(|(&chat, run)| Some((chat, run.slow?)))
                    .collect();
                for (chat, question) in asked {
                    self.unask_slow(chat, question);
                }
            }
            _ => {}
        }
    }

    /// Stops waiting for `chat`'s session: its waiting messages end as
    /// stopped, and its notices are held.
    pub(super) fn stop_opening(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        self.abandon_open(now, chat);
        // The user stopped it: messages waiting are stopped, and notices
        // held, as the user's Stop does.
        for turn in self.waiting_turns(chat) {
            let notice = matches!(
                self.turn(chat, turn).map(|each| &each.starter),
                Some(crate::chat::Starter::Notice(_))
            );
            if !notice {
                self.set_life(now, chat, turn, TurnLife::Stopped);
            }
        }
        if let Some(run) = self.runs.get_mut(&chat) {
            run.notices_held = true;
        }
        self.set_state(
            chat,
            ChatState::Lost {
                reason: Lost::Stopped,
            },
        );
        let conn = self.runs.get(&chat).map(|run| run.conn);
        if let Some(conn) = conn {
            self.advance(now, conn, out);
        }
    }

    /// Stops waiting for `chat`'s session to open. No answer is sent;
    /// one that opens anyway is closed (see [`Core::close_late`]). The
    /// request keeps a watchdog, so it ends even if the agent never
    /// answers.
    pub(super) fn abandon_open(&mut self, now: Now, chat: ChatId) {
        let watchdog = now.mono.after(self.host.timers.request);
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        if let Open::Asked { proc, id } = run.open
            && let Some(pending) = self.pending.get_mut(&(proc, id))
        {
            pending.deadline = Some(watchdog);
            if let Purpose::Open { abandoned, .. } = &mut pending.purpose {
                *abandoned = true;
            }
        }
        if matches!(run.open, Open::Asked { .. } | Open::Wanted) {
            run.open = Open::Idle;
        }
        run.held_by = None;
        let slow = run.slow;
        self.end_replay(chat);
        if let Some(question) = slow {
            self.unask_slow(chat, question);
        }
    }

    /// Closes `chat`: the user is done with it. A saved chat is then put
    /// away, marked closed.
    pub(super) fn close(
        &mut self,
        now: Now,
        chat: ChatId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        let whole = self.chats.get(&chat).ok_or(Refused::NoSuchChat)?;
        // One archived already, as a restored one may be, keeps when it
        // was archived, and is put away all the same.
        if whole.state != ChatState::Closed {
            self.close_session(now, chat, out);
            self.record(chat, Change::Archived(Some(now.wall)));
        }
        if self.saving.kept.contains_key(&chat) {
            self.put_away_now(now, chat, out);
        }
        Ok(())
    }

    /// Winds `chat` down, closes its session and marks it closed.
    pub(super) fn close_session(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        self.wind_down(now, chat, TurnLife::Stopped, out);
        let conn = self.let_go(now, chat, out);
        self.set_state(chat, ChatState::Closed);
        if let Some(conn) = conn {
            self.advance(now, conn, out);
        }
    }

    /// Lets `chat`, wound down, go of its session and connection: the
    /// session is closed if its adapter goes on serving others, and the
    /// connection, which the caller advances, stops its adapter if not.
    pub(super) fn let_go(
        &mut self,
        now: Now,
        chat: ChatId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Option<ConnectionId> {
        let run = self.runs.get_mut(&chat)?;
        let conn = run.conn;
        let live = match run.open {
            Open::Live { proc } => Some(proc),
            _ => None,
        };
        run.open = Open::Idle;
        // A setting being sent is waited for no more: its answer, or its
        // watchdog, changes nothing of the chat.
        run.choosing.asked = None;
        run.choosing.unsent.clear();
        let session = run.session.clone();
        if let Some(each) = self.conns.get_mut(&conn) {
            each.chats.remove(&chat);
        }
        let others = self
            .conns
            .get(&conn)
            .is_some_and(|each| !each.chats.is_empty());
        // Each of Claude's sessions is a program of its own, so it's
        // closed if the adapter goes on serving others. If not, the
        // adapter is stopped, which ends every session it has.
        if let (Some(proc), Some(session), true) = (live, session, others)
            && self
                .procs
                .get(&proc)
                .and_then(|each| each.caps())
                .is_some_and(|caps| caps.close)
        {
            let wait = self.host.timers.request;
            self.request(
                now,
                proc,
                ("session/close", json!({ "sessionId": session.0 })),
                Purpose::Close { session },
                Some(wait),
                out,
            );
        }
        Some(conn)
    }

    /// Ends what `chat` has going: its questions, its running turn (as
    /// `life`, telling the agent to stop), its waiting turns (as stopped),
    /// its background jobs (cancelled by the user if `life` is Stopped)
    /// and any session it's opening.
    pub(super) fn wind_down(
        &mut self,
        now: Now,
        chat: ChatId,
        life: TurnLife,
        out: &mut Vec<Directive<A, D>>,
    ) {
        self.withdraw_asks(chat, out);
        let stopped = life == TurnLife::Stopped;
        if let Some(running) = self.give_up_prompt(now, chat) {
            if let Some(session) = self.runs.get(&chat).and_then(|run| run.session.clone()) {
                self.send_notification(
                    running.proc,
                    "session/cancel",
                    json!({ "sessionId": session.0 }),
                    out,
                );
            }
            self.end_turn(now, chat, running.turn, life);
        }
        for turn in self.waiting_turns(chat) {
            self.set_life(now, chat, turn, TurnLife::Stopped);
        }
        self.release_notices(chat);
        self.end_agent_turn(now, chat);
        self.stop_jobs(now, chat, stopped, out);
        self.abandon_open(now, chat);
    }

    /// Stops everything.
    pub(super) fn shut_down(&mut self, now: Now, out: &mut Vec<Directive<A, D>>) {
        self.shutting_down = true;
        self.stop_setup(out);
        // An idle chat has nothing going, and stays idle.
        let chats: Vec<ChatId> = (self.chats.iter())
            .filter(|(_, whole)| !matches!(whole.state, ChatState::Closed | ChatState::Idle))
            .map(|(&chat, _)| chat)
            .collect();
        for chat in chats {
            self.wind_down(now, chat, TurnLife::CutOff, out);
            if let Some(run) = self.runs.get_mut(&chat) {
                run.open = Open::Idle;
            }
            self.set_state(
                chat,
                ChatState::Lost {
                    reason: Lost::ShutDown,
                },
            );
        }
        let learning: Vec<ConnectionId> = (self.conns.iter())
            .filter(|(_, each)| each.learn.is_some())
            .map(|(&conn, _)| conn)
            .collect();
        for conn in learning {
            self.learn_failed(conn, crate::settings::LearnFailure::ShutDown);
        }
        // An idle chat's jobs stop too: it stays idle.
        let jobs: Vec<ChatId> = (self.chats.keys().copied())
            .filter(|&chat| self.has_jobs(chat))
            .collect();
        for chat in jobs {
            self.stop_jobs(now, chat, false, out);
        }
        // Closing an adapter's input ends every session it has, so they
        // aren't closed one by one first.
        let procs: Vec<Ticket> = self
            .conns
            .values()
            .filter_map(|each| each.current)
            .collect();
        for proc in procs {
            if let Some(each) = self.procs.get_mut(&proc) {
                each.retired = true;
            }
            self.end_proc(now, proc, false, out);
        }
        for each in self.conns.values_mut() {
            each.current = None;
        }
        self.save_all(now);
    }
}

/// What keeps a chat from opening a session.
enum Hold {
    /// Another chat has it: it's in use.
    Chat,
    /// Something going away has it: wait.
    Going,
}

/// How a chat's lost session fails a turn that needed it.
fn failure_for(lost: &Lost) -> Failure {
    let (reason, agent_said) = match lost {
        Lost::SignInNeeded => (FailureReason::SignInNeeded, None),
        Lost::Refused { message } => (FailureReason::Other, Some(message.clone())),
        _ => (FailureReason::AgentStopped, None),
    };
    Failure {
        reason,
        agent_said,
        remedy: Some(Remedy::SendAgain),
    }
}

/// Checks what the app tells the agent ([`OpenChat::instructions`]): at
/// most [`MOST_INSTRUCTIONS`] bytes, else [`Refused::Unsendable`].
pub(super) fn check_instructions(instructions: Option<&str>) -> Result<(), Refused> {
    match instructions {
        Some(text) if text.len() > MOST_INSTRUCTIONS => Err(Refused::Unsendable),
        _ => Ok(()),
    }
}

/// Checks the MCP servers the app gives a chat: none may be named so that
/// an agent writes its tools' names as CAF's own, `mcp__caf__…`, else
/// [`Refused::ReservedServer`]: one Claude Code writes as `caf`, in any
/// case, or as `caf_` or anything starting `caf__`, such as `caf.` or
/// `CAF__x` (`caf.x`, written `caf_x`, is fine). An agent
/// couldn't tell the app's tools from CAF's. Nor may two be named alike,
/// as Claude Code writes them, else [`Refused::SameServerName`].
pub(super) fn check_servers(servers: &[McpServer]) -> Result<(), Refused> {
    let reserved = servers.iter().any(|server| {
        let (McpServer::Stdio { name, .. } | McpServer::Http { name, .. }) = server;
        format!("{}__", claude_server_name(name))
            .to_ascii_lowercase()
            .starts_with(&format!("{CAF_SERVER}__"))
    });
    if reserved {
        return Err(Refused::ReservedServer);
    }
    let mut written = std::collections::BTreeSet::new();
    for server in servers {
        let (McpServer::Stdio { name, .. } | McpServer::Http { name, .. }) = server;
        if !written.insert(claude_server_name(name)) {
            return Err(Refused::SameServerName);
        }
    }
    Ok(())
}

/// The names of `run`'s MCP servers, as the app gave them.
pub(super) fn server_names<A: AppItem>(run: &Run<A>) -> Vec<String> {
    (run.mcp.iter())
        .map(|server| {
            let (McpServer::Stdio { name, .. } | McpServer::Http { name, .. }) = server;
            name.clone()
        })
        .collect()
}

/// Whether CAF keeps `session` as a session id: at most 256 bytes, with
/// no control characters.
pub(super) fn keepable(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= MOST_SESSION_ID
        && !session.chars().any(char::is_control)
}

/// An MCP server as ACP sends it: `None` if it can't be sent exactly.
/// ACP can only set a server's variables, and has no folder to start it
/// in.
pub(super) fn mcp_server(server: &McpServer) -> Option<Value> {
    let pair = |name: &str, value: &str| json!({ "name": name, "value": value });
    Some(match server {
        McpServer::Stdio { name, command } => {
            if command.cwd.is_some() {
                return None;
            }
            let env = (command.env.iter())
                .map(|change| match change {
                    EnvChange::Set(name, value) => Some(pair(name.to_str()?, value.to_str()?)),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let args = (command.args.iter())
                .map(|arg| arg.to_str())
                .collect::<Option<Vec<_>>>()?;
            json!({ "name": name, "command": command.program.to_str()?, "args": args, "env": env })
        }
        McpServer::Http { name, url, headers } => json!({
            "type": "http",
            "name": name,
            "url": url,
            "headers": headers.iter().map(|(name, value)| pair(name, value)).collect::<Vec<_>>(),
        }),
    })
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// Unarchives `chat`, as [`Core::unarchive`] says.
    pub(super) fn unarchive_chat(
        &mut self,
        _now: Now,
        chat: ChatId,
        _out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        let whole = self.chats.get(&chat).ok_or(Refused::NoSuchChat)?;
        if whole.state != ChatState::Closed {
            return Ok(());
        }
        let agent = whole.agent.clone();
        // It's on no connection while archived: back on one, idle, as a
        // restored chat is, its agent started when a message is sent; but
        // for an agent the host no longer offers, which no message can be
        // sent to.
        if self.agents.contains_key(&agent) {
            let conn = self.conn_for(chat, &agent);
            let old = (self.runs.get_mut(&chat)).map(|run| std::mem::replace(&mut run.conn, conn));
            if let Some(old) = old.filter(|old| *old != conn) {
                self.tidy(old);
            }
        }
        // What its last session said of its settings is forgotten, as a
        // restored chat's is: a choice is checked as its session opens.
        if let Some(run) = self.runs.get_mut(&chat) {
            run.choosing = Choosing::default();
        }
        self.set_state(chat, ChatState::Idle);
        self.record(chat, Change::Archived(None));
        Ok(())
    }
}
