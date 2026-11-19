//! The agent's permission requests: asked of the policy driver, then of
//! the user if it says so, and answered exactly once.
//!
//! An answer is only ever one of the options the agent offered (Claude's
//! adapter throws on any other), or "cancelled". An option of a kind CAF
//! doesn't know is shown to the user but never chosen by CAF itself.
//! Where nobody can answer, CAF refuses safely: a reject option if the
//! agent offered one, else "cancelled".

use serde_json::{Value, json};

use super::clean::agent_text;
use super::state::Ask;
use super::wire::CANCELLED;
use super::{Core, Refused, report};
use crate::acp::RequestId;
use crate::chat::{AppItem, ItemKind};
use crate::directive::{Directive, Level, PermissionRequest, PolicyDirective};
use crate::event::PolicyAnswer;
use crate::ids::{ChatId, QuestionId, Ticket};
use crate::question::{Answer, OptionId, OptionKind, Permission, PermissionOption, QuestionKind};
use crate::time::Now;

/// The most options a permission request keeps: any past it are left
/// out.
const MOST_OPTIONS: usize = 64;

/// The longest option label or permission title kept, in bytes.
const MOST_LABEL: usize = 1024;

/// The longest tool input passed to the policy, in bytes.
const MOST_INPUT: usize = 64 << 10;

impl<A: AppItem> Core<A> {
    /// The agent asks permission, as request `id` of `proc`.
    pub(super) fn permission_asked(&mut self, now: Now, proc: Ticket, id: &RequestId, params: &Value, out: &mut Vec<Directive>) {
        let session = params.get("sessionId").and_then(Value::as_str);
        let chat = session.and_then(|session| self.chat_with_session(proc, session));
        let Some(chat) = chat else {
            report(out, Level::Info, "refused a permission request for a session no chat holds".to_owned());
            self.reply(proc, id, Ok(cancelled()), out);
            return;
        };
        // Nothing replayed acts.
        if self.runs.get(&chat).is_some_and(|run| run.replaying) {
            report(out, Level::Debug, "refused a permission request replayed with a session's history".to_owned());
            self.reply(proc, id, Ok(cancelled()), out);
            return;
        }
        let options = options(params.get("options"));
        let call = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let tool = self.tool_named(now, chat, &call);
        let title = (call.get("title").and_then(Value::as_str))
            .map(|title| agent_text(title, MOST_LABEL))
            .or_else(|| match &self.item(chat, tool?)?.kind {
                ItemKind::Tool(tool) => Some(tool.title.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let input = (call.get("rawInput").filter(|input| !input.is_null()))
            .map(|input| super::clean::cut(input.to_string(), MOST_INPUT));
        let ticket = Ticket(self.ids.take());
        self.asks.insert(ticket, Ask {
            proc,
            id: id.clone(),
            chat,
            question: None,
            options,
            title: title.clone(),
            tool,
        });
        out.push(Directive::Policy(PolicyDirective::Decide {
            ticket,
            chat,
            request: PermissionRequest { title, input },
        }));
    }

    /// The policy answered.
    pub(super) fn policy_answered(&mut self, now: Now, ticket: Ticket, answer: PolicyAnswer, out: &mut Vec<Directive>) {
        let Some(ask) = self.asks.get(&ticket) else {
            report(out, Level::Debug, "ignored the policy's answer to a request that has ended".to_owned());
            return;
        };
        if ask.question.is_some() {
            report(out, Level::Debug, "ignored the policy answering twice".to_owned());
            return;
        }
        match answer {
            PolicyAnswer::Allow => match option(ask, OptionKind::AllowOnce) {
                Some(option) => self.settle(ticket, Some(option), out),
                // It allowed once, which the agent doesn't offer: anything
                // more is the user's to choose.
                None => self.ask_user(now, ticket, out),
            },
            PolicyAnswer::Refuse => self.refuse(ticket, out),
            PolicyAnswer::AskUser => self.ask_user(now, ticket, out),
        }
    }

    /// Asks the user, if the host can show permission questions, and
    /// refuses safely otherwise.
    fn ask_user(&mut self, now: Now, ticket: Ticket, out: &mut Vec<Directive>) {
        if !self.host.shows.permission_questions {
            return self.refuse(ticket, out);
        }
        let Some(ask) = self.asks.get(&ticket) else { return };
        let kind = QuestionKind::Permission(Permission {
            tool: ask.tool,
            title: ask.title.clone(),
            options: ask.options.clone(),
        });
        let chat = ask.chat;
        let question = self.ask(now, chat, kind);
        if let Some(ask) = self.asks.get_mut(&ticket) {
            ask.question = Some(question);
        }
    }

    /// Refuses safely: once if the agent offers that, else always, else
    /// "cancelled".
    fn refuse(&mut self, ticket: Ticket, out: &mut Vec<Directive>) {
        let Some(ask) = self.asks.get(&ticket) else { return };
        let option = option(ask, OptionKind::RejectOnce).or_else(|| option(ask, OptionKind::RejectAlways));
        self.settle(ticket, option, out);
    }

    /// Answers the request kept under `ticket` with `option`, or
    /// "cancelled", and forgets it.
    fn settle(&mut self, ticket: Ticket, option: Option<OptionId>, out: &mut Vec<Directive>) {
        let Some(ask) = self.asks.remove(&ticket) else { return };
        if let Some(question) = ask.question {
            self.unask(ask.chat, question);
        }
        let outcome = match option {
            Some(option) => json!({ "outcome": { "outcome": "selected", "optionId": option.0 } }),
            None => cancelled(),
        };
        self.reply_or_report(ask.proc, &ask.id, Ok(outcome), out);
    }

    /// Answers the agent's request, or reports that its process has gone
    /// and it can't be.
    fn reply_or_report(&mut self, proc: Ticket, id: &RequestId, result: Result<Value, (i64, String)>, out: &mut Vec<Directive>) {
        if !self.reply(proc, id, result, out) {
            report(out, Level::Debug, "couldn't answer a permission request: its process has gone".to_owned());
        }
    }

    /// The user answered `question`.
    pub(super) fn answer_question(&mut self, now: Now, question: QuestionId, answer: Answer, out: &mut Vec<Directive>) -> Result<(), Refused> {
        if let Some((&ticket, ask)) = self.asks.iter().find(|(_, ask)| ask.question == Some(question)) {
            let Answer::Permission(option) = answer else { return Err(Refused::WrongAnswer) };
            if !ask.options.iter().any(|offered| offered.id == option) {
                return Err(Refused::WrongAnswer);
            }
            self.settle(ticket, Some(option), out);
            return Ok(());
        }
        let slow = self.runs.iter().find(|(_, run)| run.slow == Some(question));
        if let Some((&chat, _)) = slow {
            let Answer::SlowToOpen(choice) = answer else { return Err(Refused::WrongAnswer) };
            self.slow_chosen(now, chat, question, choice, out);
            return Ok(());
        }
        Err(Refused::NoSuchQuestion)
    }

    /// The agent withdrew one of its requests, `$/cancel_request`: its
    /// question goes, and the request is still answered, as cancelled.
    pub(super) fn cancel_request(&mut self, proc: Ticket, params: &Value, out: &mut Vec<Directive>) {
        let id = match params.get("requestId") {
            Some(Value::Number(number)) => number.as_i64().map(RequestId::Number),
            Some(Value::String(text)) => Some(RequestId::String(text.clone())),
            Some(Value::Null) => Some(RequestId::Null),
            _ => None,
        };
        let found = (self.asks.iter()).find(|(_, ask)| ask.proc == proc && Some(&ask.id) == id.as_ref());
        let Some((&ticket, _)) = found else {
            report(out, Level::Debug, "the agent withdrew a request CAF isn't waiting on".to_owned());
            return;
        };
        if let Some(ask) = self.forget_ask(ticket, out) {
            self.reply_or_report(proc, &ask.id, Err((CANCELLED, "cancelled".to_owned())), out);
        }
    }

    /// Answers every request `chat`'s agent is waiting on "cancelled".
    pub(super) fn withdraw_asks(&mut self, chat: ChatId, out: &mut Vec<Directive>) {
        let tickets: Vec<Ticket> = (self.asks.iter()).filter(|(_, ask)| ask.chat == chat).map(|(&ticket, _)| ticket).collect();
        for ticket in tickets {
            if let Some(ask) = self.forget_ask(ticket, out) {
                self.reply_or_report(ask.proc, &ask.id, Ok(cancelled()), out);
            }
        }
    }

    /// Forgets the requests `which` picks, without answering them: their
    /// process has gone, so they can't be.
    pub(super) fn drop_asks(&mut self, which: impl Fn(&Ask) -> bool, out: &mut Vec<Directive>) {
        let tickets: Vec<Ticket> = (self.asks.iter()).filter(|(_, ask)| which(ask)).map(|(&ticket, _)| ticket).collect();
        for ticket in tickets {
            self.forget_ask(ticket, out);
        }
    }

    /// Forgets the request kept under `ticket`: its question goes, and the
    /// policy is told if it was still deciding.
    fn forget_ask(&mut self, ticket: Ticket, out: &mut Vec<Directive>) -> Option<Ask> {
        let ask = self.asks.remove(&ticket)?;
        match ask.question {
            Some(question) => self.unask(ask.chat, question),
            None => out.push(Directive::Policy(PolicyDirective::Withdrawn { ticket })),
        }
        Some(ask)
    }
}

/// ACP's answer "cancelled" to a permission request.
fn cancelled() -> Value {
    json!({ "outcome": { "outcome": "cancelled" } })
}

/// The first option of `kind` the request offers.
fn option(ask: &Ask, kind: OptionKind) -> Option<OptionId> {
    ask.options.iter().find(|option| option.kind == kind).map(|option| option.id.clone())
}

/// The options a permission request offers, read leniently: one without
/// an id is left out, and one of a kind CAF doesn't know is `Other`.
fn options(options: Option<&Value>) -> Vec<PermissionOption> {
    let options = options.and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    (options.iter().take(MOST_OPTIONS))
        .filter_map(|option| {
            let id = option.get("optionId")?.as_str()?;
            let kind = match option.get("kind").and_then(Value::as_str) {
                Some("allow_once") => OptionKind::AllowOnce,
                Some("allow_always") => OptionKind::AllowAlways,
                Some("reject_once") => OptionKind::RejectOnce,
                Some("reject_always") => OptionKind::RejectAlways,
                _ => OptionKind::Other,
            };
            let label = option.get("name").and_then(Value::as_str).unwrap_or(id);
            Some(PermissionOption {
                id: OptionId(id.to_owned()),
                label: agent_text(label, MOST_LABEL),
                kind,
            })
        })
        .collect()
}
