//! Turns: sending messages one at a time, how a turn ends, and Stop.
//!
//! A chat runs one turn at a time. A message sent while a turn runs, or
//! while the session opens, waits as a turn of its own, in order, and is
//! never dropped: it's sent, stopped, or fails visibly.

use serde_json::{Value, json};

use super::clean::agent_text;
use super::frames::Reply;
use super::procs::Cause;
use super::state::{Open, Purpose, Running};
use super::{Core, Refused, report};
use crate::acp::RpcError;
use crate::change::Change;
use crate::chat::AppData;
use crate::chat::{
    AppItem, ChatState, Failure, FailureReason, ItemKind, Lost, Note, Part, Remedy, Starter,
    ToolStatus, Turn, TurnLife, UserMessage,
};
use crate::directive::{Directive, Level};
use crate::ids::{ChatId, Ticket, TurnId};
use crate::time::{End, Now, Window};

impl<A: AppItem, D: AppData> Core<A, D> {
    /// Sends `message` in `chat` as turn `turn`.
    pub(super) fn send_as(
        &mut self,
        now: Now,
        chat: ChatId,
        turn: TurnId,
        message: UserMessage,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        let whole = self.chats.get(&chat).ok_or(Refused::NoSuchChat)?;
        if whole.state == ChatState::Closed {
            return Err(Refused::Closed);
        }
        // A restored chat whose agent the host no longer offers, or whose
        // own tools CAF can't turn off.
        let spec = self.agents.get(&whole.agent).ok_or(Refused::NoSuchAgent)?;
        if !spec.profile.strips() {
            return Err(Refused::OwnTools);
        }
        if message.parts.is_empty() {
            return Err(Refused::Unsupported);
        }
        let command =
            (message.parts.iter()).any(|part| matches!(part, Part::Text(text) if is_command(text)));
        if command && message.parts.len() > 1 {
            return Err(Refused::CommandNotAlone);
        }
        if self.waiting_turns(chat).len() >= self.host.limits.queued_messages {
            return Err(Refused::QueueFull);
        }
        self.ids.seen(turn.0);
        self.call_off_shutdown(now);
        self.end_agent_turn(now, chat);
        self.add_turn(
            chat,
            Turn {
                id: turn,
                starter: Starter::User(message),
                life: TurnLife::Waiting,
                window: Window::open(now),
                items: Vec::new(),
                imported: false,
                model: None,
                usage: None,
            },
        );
        if self
            .runs
            .get(&chat)
            .is_some_and(|run| run.open == Open::Idle)
        {
            self.want_open(now, chat, out);
        }
        self.pump(now, chat, out);
        Ok(())
    }

    /// The turns of `chat` waiting to be sent, oldest first.
    pub(super) fn waiting_turns(&self, chat: ChatId) -> Vec<TurnId> {
        let Some(whole) = self.chats.get(&chat) else {
            return Vec::new();
        };
        (whole.turns.iter())
            .filter(|turn| turn.life == TurnLife::Waiting)
            .map(|turn| turn.id)
            .collect()
    }

    /// Sends `chat`'s next waiting turn, if its session is open and no
    /// turn is running: once what's chosen of its settings is sent, and
    /// answered.
    pub(super) fn pump(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let Some(run) = self.runs.get(&chat) else {
            return;
        };
        let (Open::Live { proc }, None, Some(session)) = (&run.open, &run.running, &run.session)
        else {
            return;
        };
        let (proc, session) = (*proc, session.clone());
        if !self.send_settings(now, chat, proc, &session, out) {
            return;
        }
        let Some(turn) = self.waiting_turns(chat).first().copied() else {
            return;
        };
        let Some(Starter::User(message)) = self.turn(chat, turn).map(|each| each.starter.clone())
        else {
            return;
        };
        let prompt: Vec<Value> = (message.parts.iter())
            .map(|part| match part {
                Part::Text(text) => json!({ "type": "text", "text": text }),
            })
            .collect();
        let params = json!({ "sessionId": session.0, "prompt": prompt });
        let purpose = Purpose::Prompt {
            chat,
            turn,
            abandoned: false,
        };
        // A prompt has no watchdog: an agent may work on one for an hour.
        let Some(id) = self.request(now, proc, ("session/prompt", params), purpose, None, out)
        else {
            return;
        };
        if let Some(run) = self.runs.get_mut(&chat) {
            run.running = Some(Running {
                turn,
                proc,
                id,
                stop_by: None,
            });
        }
        self.set_life(now, chat, turn, TurnLife::Running);
        if let Some(model) = self.model_used(chat) {
            self.record(chat, Change::TurnModel { turn, model });
        }
    }

    /// The agent answered the prompt of `chat`'s turn `turn`.
    pub(super) fn prompt_answered(
        &mut self,
        now: Now,
        chat: ChatId,
        turn: TurnId,
        reply: Reply,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        if run
            .running
            .as_ref()
            .is_none_or(|running| running.turn != turn)
        {
            return;
        }
        let stopping = run
            .running
            .take()
            .is_some_and(|running| running.stop_by.is_some());
        if let Reply::Error(error) = &reply
            && self.profile_of_chat(chat).session_gone(error)
        {
            return self.prompt_lost_session(now, chat, turn, stopping, out);
        }
        let life = match reply {
            Reply::Result(result) => {
                // The session works: a setting that finds it gone may have
                // it opened again.
                if let Some(run) = self.runs.get_mut(&chat) {
                    run.choosing.reopened = false;
                }
                ended_by(&result)
            }
            Reply::Error(error) => {
                let mut failure = self.failure(chat, turn, &error);
                self.sign_in_failure(chat, &mut failure);
                TurnLife::Failed(failure)
            }
            Reply::Unreadable => {
                TurnLife::Failed(failure(FailureReason::BadAnswer, Some(Remedy::SendAgain)))
            }
        };
        // The agent's questions end with its turn.
        self.withdraw_asks(chat, out);
        self.end_turn(now, chat, turn, life);
        self.pump(now, chat, out);
    }

    /// The agent no longer has the session `chat`'s turn `turn` was sent
    /// to, as when Claude's program under the adapter died. The session is
    /// opened again, resumed where the agent can: its conversation may be
    /// kept even so, and if it isn't, a new one opens and the chat says so.
    ///
    /// The turn goes to it again if the agent did nothing with it, once
    /// only: Claude's adapter says so before anything reaches the model.
    /// A turn the user was stopping ends stopped, and one the agent did
    /// something with, or that was already sent again, fails.
    fn prompt_lost_session(
        &mut self,
        now: Now,
        chat: ChatId,
        turn: TurnId,
        stopping: bool,
        out: &mut Vec<Directive<A, D>>,
    ) {
        self.withdraw_asks(chat, out);
        let heard = self
            .turn(chat, turn)
            .is_some_and(|each| !each.items.is_empty());
        let resent = self
            .runs
            .get(&chat)
            .is_some_and(|run| run.resent == Some(turn));
        if stopping {
            self.end_turn(now, chat, turn, TurnLife::Stopped);
        } else if heard || resent {
            let failed = failure(FailureReason::AgentStopped, Some(Remedy::SendAgain));
            self.end_turn(now, chat, turn, TurnLife::Failed(failed));
        } else {
            self.set_life(now, chat, turn, TurnLife::Waiting);
            if let Some(run) = self.runs.get_mut(&chat) {
                run.resent = Some(turn);
            }
        }
        self.end_agent_turn(now, chat);
        self.want_open(now, chat, out);
    }

    /// What the agent's error `error` for `chat`'s turn `turn` means, as
    /// its profile reads it.
    fn failure(&self, chat: ChatId, turn: TurnId, error: &RpcError) -> Failure {
        let said = self.profile_of_chat(chat).failure(error);
        let most = self.host.limits.failure_text;
        let words = said
            .words
            .map(|words| agent_text(&words, most))
            .filter(|words| !words.is_empty());
        // Claude's adapter says a failure's text as words first, perhaps
        // after words of the answer: it's shown once, as those words.
        let said_already = |words: &String| {
            let last = self.turn(chat, turn).and_then(|each| each.items.last());
            let ends = |text: &str| {
                agent_text(text, usize::MAX)
                    .trim_end()
                    .ends_with(words.trim())
            };
            last.is_some_and(|item| matches!(&item.kind, ItemKind::Words(text) if ends(text)))
        };
        Failure {
            reason: said.reason,
            agent_said: words.filter(|words| !said_already(words)),
            remedy: said.remedy,
        }
    }

    /// Ends `chat`'s turn `turn` as `life`: its words stop growing, and
    /// tools still running fail.
    pub(super) fn end_turn(&mut self, now: Now, chat: ChatId, turn: TurnId, life: TurnLife) {
        let items: Vec<_> = (self.turn(chat, turn).into_iter())
            .flat_map(|each| each.items.iter())
            .filter(|item| item.window.end == End::Open)
            .map(|item| (item.id, item.kind.clone()))
            .collect();
        for (item, kind) in items {
            if let ItemKind::Tool(mut tool) = kind
                && matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)
            {
                tool.status = ToolStatus::Failed;
                self.set_tool(chat, item, tool);
            }
            self.end_item(now, chat, item);
        }
        self.set_life(now, chat, turn, life);
        if let Some(run) = self.runs.get_mut(&chat)
            && run.agent_turn == Some(turn)
        {
            run.agent_turn = None;
            run.quiet_by = None;
        }
    }

    /// Ends the turn the agent started by itself, if one is open.
    pub(super) fn end_agent_turn(&mut self, now: Now, chat: ChatId) {
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        run.quiet_by = None;
        let Some(turn) = run.agent_turn.take() else {
            return;
        };
        self.end_turn(now, chat, turn, TurnLife::Finished);
    }

    /// The turn open for the agent's output: the running turn, or one it
    /// started by itself.
    pub(super) fn open_turn(&self, chat: ChatId) -> Option<TurnId> {
        let run = self.runs.get(&chat)?;
        run.running
            .as_ref()
            .map(|running| running.turn)
            .or(run.agent_turn)
    }

    /// The turn the agent's live output goes into: the open turn, or a new
    /// one it started by itself, unless one was just stopped and this was
    /// in flight.
    pub(super) fn live_turn(
        &mut self,
        now: Now,
        chat: ChatId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Option<TurnId> {
        if let Some(turn) = self.open_turn(chat) {
            return Some(turn);
        }
        if self
            .runs
            .get(&chat)?
            .held_until
            .is_some_and(|until| now.mono < until)
        {
            report(
                out,
                Level::Debug,
                format!(
                    "chat {}: dropped what the agent had in flight as its turn was stopped",
                    chat.0
                ),
            );
            return None;
        }
        let turn = TurnId(self.ids.take());
        self.add_turn(
            chat,
            Turn {
                id: turn,
                starter: Starter::Agent,
                life: TurnLife::Running,
                window: Window::open(now),
                items: Vec::new(),
                imported: false,
                model: None,
                usage: None,
            },
        );
        let run = self.runs.get_mut(&chat)?;
        run.agent_turn = Some(turn);
        run.quiet_by = Some(now.mono.after(self.host.timers.quiet));
        Some(turn)
    }

    /// `chat`'s agent-started turn has been quiet long enough: it ends,
    /// unless a tool of it is still running or waiting to, or the agent
    /// waits on a question.
    pub(super) fn quiet(&mut self, now: Now, chat: ChatId) {
        let Some(turn) = self.runs.get(&chat).and_then(|run| run.agent_turn) else {
            if let Some(run) = self.runs.get_mut(&chat) {
                run.quiet_by = None;
            }
            return;
        };
        let tools = (self.turn(chat, turn).into_iter())
            .flat_map(|each| each.items.iter())
            .any(|item| matches!(&item.kind, ItemKind::Tool(tool) if matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)));
        let busy = tools || self.asks.values().any(|ask| ask.chat == chat);
        if busy {
            let quiet_by = now.mono.after(self.host.timers.quiet);
            if let Some(run) = self.runs.get_mut(&chat) {
                run.quiet_by = Some(quiet_by);
            }
        } else {
            self.end_agent_turn(now, chat);
        }
    }

    /// Stops `chat`'s running turn and its waiting ones, or its opening.
    pub(super) fn stop_chat(
        &mut self,
        now: Now,
        chat: ChatId,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        let whole = self.chats.get(&chat).ok_or(Refused::NoSuchChat)?;
        let run = self.runs.get(&chat).ok_or(Refused::NoSuchChat)?;
        let waiting = self.waiting_turns(chat);
        let opening = matches!(run.open, Open::Wanted | Open::Asked { .. })
            && whole.state != ChatState::Closed;
        let running = run
            .running
            .as_ref()
            .map(|running| (running.turn, running.proc, running.stop_by.is_some()));
        let agent_turn = run.agent_turn;
        let for_agent = whole.state == ChatState::WaitingForAgent;
        if waiting.is_empty() && running.is_none() && agent_turn.is_none() && !opening {
            return Err(Refused::NothingToStop);
        }
        // ACP: open permission requests are answered "cancelled" before
        // the agent is told to stop.
        self.withdraw_asks(chat, out);
        if let Some(turn) = agent_turn {
            self.stop_agent_turn(now, chat, turn, out);
        }
        if let Some((turn, proc, false)) = running {
            let session = self.runs.get(&chat).and_then(|run| run.session.clone());
            match session.filter(|_| self.procs.get(&proc).is_some_and(|each| each.askable())) {
                Some(session) => {
                    self.notify(
                        proc,
                        "session/cancel",
                        json!({ "sessionId": session.0 }),
                        out,
                    );
                    let stop_by = now.mono.after(self.host.timers.stop);
                    if let Some(running) = self
                        .runs
                        .get_mut(&chat)
                        .and_then(|run| run.running.as_mut())
                    {
                        running.stop_by = Some(stop_by);
                    }
                    self.set_life(now, chat, turn, TurnLife::Stopping);
                }
                // Its process is going: nothing more will come of it.
                None => {
                    self.give_up_prompt(now, chat);
                    self.end_turn(now, chat, turn, TurnLife::Stopped);
                }
            }
        }
        let stopped = !waiting.is_empty();
        for turn in waiting {
            self.set_life(now, chat, turn, TurnLife::Stopped);
        }
        if opening {
            self.stop_opening(now, chat, out);
        }
        if for_agent {
            self.stopped_waiting(now, chat, stopped);
        }
        Ok(())
    }

    /// Stops a turn the agent started by itself. ACP doesn't say when such a
    /// turn ends, so it ends now, and what the agent had in flight opens
    /// no new turn for a moment.
    fn stop_agent_turn(
        &mut self,
        now: Now,
        chat: ChatId,
        turn: TurnId,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(run) = self.runs.get_mut(&chat) else {
            return;
        };
        run.held_until = Some(now.mono.after(IN_FLIGHT));
        if let (Open::Live { proc }, Some(session)) = (run.open, run.session.clone()) {
            self.notify(
                proc,
                "session/cancel",
                json!({ "sessionId": session.0 }),
                out,
            );
        }
        self.end_turn(now, chat, turn, TurnLife::Stopped);
    }

    /// Stops waiting for `chat`'s running prompt: its answer, if it comes,
    /// changes nothing. The request keeps a watchdog, so it ends even if
    /// the agent never answers.
    pub(super) fn give_up_prompt(&mut self, now: Now, chat: ChatId) -> Option<Running> {
        let watchdog = now.mono.after(self.host.timers.request);
        let running = self.runs.get_mut(&chat)?.running.take()?;
        if let Some(pending) = self.pending.get_mut(&(running.proc, running.id))
            && let Purpose::Prompt { abandoned, .. } = &mut pending.purpose
        {
            *abandoned = true;
            pending.deadline = Some(watchdog);
        }
        Some(running)
    }

    /// The agent didn't stop `chat`'s turn in time: the turn fails, and
    /// the session is restarted ([`Core::restart_stuck`]).
    pub(super) fn stop_overdue(&mut self, now: Now, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let Some(running) = self.give_up_prompt(now, chat) else {
            return;
        };
        report(
            out,
            Level::Warn,
            format!("chat {}: the agent didn't stop when asked", chat.0),
        );
        let failed = failure(FailureReason::DidNotStop, Some(Remedy::SendAgain));
        self.end_turn(now, chat, running.turn, TurnLife::Failed(failed));
        self.restart_stuck(now, chat, running.proc, out);
    }

    /// `chat`'s session on `proc` is stuck: the adapter is restarted if no
    /// other chat is mid-turn on it; otherwise only this chat's session is
    /// closed, where the agent can, and opened again with its next
    /// message, or at once if one waits.
    pub(super) fn restart_stuck(
        &mut self,
        now: Now,
        chat: ChatId,
        proc: Ticket,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if !self.busy_besides(proc, chat) {
            return self.lose_process(now, proc, Cause::Restarted, out);
        }
        let session = self.runs.get(&chat).and_then(|run| run.session.clone());
        let closes = self
            .procs
            .get(&proc)
            .and_then(|each| each.caps())
            .is_some_and(|caps| caps.close);
        match (session, closes) {
            (Some(session), true) => {
                let wait = self.host.timers.request;
                let params = json!({ "sessionId": session.0 });
                self.request(
                    now,
                    proc,
                    ("session/close", params),
                    Purpose::Close { session },
                    Some(wait),
                    out,
                );
            }
            // It can't be closed, and may still be running the stuck turn:
            // never continued, a new one is opened instead.
            _ => {
                if let Some(run) = self.runs.get_mut(&chat) {
                    run.session = None;
                    run.note = Some(Note::SessionStuck);
                }
            }
        }
        self.session_lost(now, chat, Lost::Restarted, true, out);
    }
}

/// How long after a turn the agent started is stopped what it says opens
/// no new turn: it was in flight. 2 s, as clackatoo.
const IN_FLIGHT: std::time::Duration = std::time::Duration::from_secs(2);

/// How a turn ends, by the stop reason in the agent's answer `result`.
fn ended_by(result: &Value) -> TurnLife {
    match result.get("stopReason").and_then(Value::as_str) {
        Some("cancelled") => TurnLife::Stopped,
        Some("max_tokens") => TurnLife::Failed(failure(FailureReason::TooLong, None)),
        Some("max_turn_requests") => TurnLife::Failed(failure(
            FailureReason::TooManySteps,
            Some(Remedy::SendAgain),
        )),
        // The agent forgets the message: nothing to send again as it is.
        Some("refusal") => TurnLife::Failed(failure(FailureReason::Refused, None)),
        // `end_turn`, and any reason newer than CAF, read leniently.
        Some(_) => TurnLife::Finished,
        None => TurnLife::Failed(failure(FailureReason::BadAnswer, Some(Remedy::SendAgain))),
    }
}

/// A failure CAF describes alone, with nothing from the agent.
fn failure(reason: FailureReason, remedy: Option<Remedy>) -> Failure {
    Failure {
        reason,
        agent_said: None,
        remedy,
    }
}

/// Whether `text` is a slash command: a `/` and a name with no other `/`
/// in it, so a path isn't one.
fn is_command(text: &str) -> bool {
    let Some(rest) = text.trim_start().strip_prefix('/') else {
        return false;
    };
    let name = rest.split(char::is_whitespace).next().unwrap_or("");
    !name.is_empty() && !name.contains('/')
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_command_is_a_slash_and_a_name_but_not_a_path() {
        assert!(super::is_command("/compact"));
        assert!(super::is_command("  /review the tests"));
        assert!(!super::is_command("/home/me is broken"));
        assert!(!super::is_command("/ nothing"));
        assert!(!super::is_command("compact"));
    }
}
