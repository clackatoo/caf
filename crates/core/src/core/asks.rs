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
use crate::chat::AppData;
use crate::chat::{AppItem, ItemKind};
use crate::directive::{Directive, Level, PermissionRequest, PolicyDirective};
use crate::event::PolicyAnswer;
use crate::ids::{ChatId, ItemId, QuestionId, Ticket};
use crate::question::{Answer, OptionId, OptionKind, Permission, PermissionOption, QuestionKind};
use crate::time::Now;
use crate::tool::{Action, DecidedBy, Decision, Origin, ToolCall, ToolStatus};

/// The most options a permission request keeps: any past it are left
/// out.
const MOST_OPTIONS: usize = 64;

/// The longest option label or permission title kept, in bytes.
const MOST_LABEL: usize = 1024;

/// The longest tool input passed to the policy, in bytes.
const MOST_INPUT: usize = 64 << 10;

impl<A: AppItem, D: AppData> Core<A, D> {
    /// The agent asks permission, as request `id` of `proc`.
    pub(super) fn permission_asked(
        &mut self,
        now: Now,
        proc: Ticket,
        id: &RequestId,
        params: &Value,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let session = params.get("sessionId").and_then(Value::as_str);
        let chat = session.and_then(|session| self.chat_with_session(proc, session));
        let Some(chat) = chat else {
            report(
                out,
                Level::Info,
                "refused a permission request for a session no chat holds".to_owned(),
            );
            self.reply(proc, id, Ok(cancelled()), out);
            return;
        };
        // Nothing replayed acts.
        if self.runs.get(&chat).is_some_and(|run| run.replaying) {
            report(
                out,
                Level::Debug,
                "refused a permission request replayed with a session's history".to_owned(),
            );
            self.reply(proc, id, Ok(cancelled()), out);
            return;
        }
        let options = options(params.get("options"));
        let call = params.get("toolCall").cloned().unwrap_or(Value::Null);
        // CAF asks about its own tools itself, with what it knows of each
        // call: the agent asking too is answered at once. Known by the
        // adapter's metadata, or the call CAF has, never its title.
        let caf = matches!(
            self.profile_of_chat(chat).origin_of(&call),
            Some(Origin::Caf(_))
        ) || (call.get("toolCallId").and_then(Value::as_str))
            .and_then(|id| self.runs.get(&chat)?.tools.get(id).copied())
            .is_some_and(|item| self.is_served(chat, item));
        let tool = self.tool_named(now, chat, &call, out);
        if caf {
            let allow = (options.iter())
                .find(|option| option.kind == OptionKind::AllowOnce)
                .or_else(|| (options.iter()).find(|option| option.kind == OptionKind::AllowAlways));
            let outcome = match allow {
                Some(option) => {
                    json!({ "outcome": { "outcome": "selected", "optionId": option.id.0 } })
                }
                None => cancelled(),
            };
            report(
                out,
                Level::Debug,
                "allowed the agent's asking about one of CAF's own tools: CAF asks itself"
                    .to_owned(),
            );
            self.reply_or_report(proc, id, Ok(outcome), out);
            return;
        }
        let title = (call.get("title").and_then(Value::as_str))
            .map(|title| agent_text(title, MOST_LABEL))
            .or_else(|| match &self.item(chat, tool?)?.kind {
                ItemKind::Tool(tool) => Some(tool.title.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let input = (call.get("rawInput").filter(|input| !input.is_null()))
            .map(|input| super::clean::cut(input.to_string(), MOST_INPUT));
        // Whose tool it is: as CAF has it, else as the adapter says in the
        // request itself. Never from its title.
        let known = tool.and_then(|item| match &self.item(chat, item)?.kind {
            ItemKind::Tool(tool) => Some(tool.origin.clone()),
            _ => None,
        });
        let origin = match known {
            Some(origin) if origin != Origin::NotRecorded => origin,
            _ => (self.profile_of_chat(chat).origin_of(&call)).unwrap_or(Origin::NotRecorded),
        };
        let action = match &origin {
            Origin::App(name) | Origin::Agent(name) | Origin::Server { tool: name, .. } => {
                Some(Action::Tool { name: name.clone() })
            }
            // CAF's own calls are asked about by CAF itself, with their
            // actions; nothing reliable is known of one it hasn't seen.
            Origin::Caf(_) | Origin::NotRecorded => None,
        };
        if let Some(item) = tool {
            self.change_asked(chat, item, |tool| {
                // The record says what the policy is told.
                if tool.origin == Origin::NotRecorded {
                    tool.origin = origin.clone();
                }
                if !tool.status.ended() {
                    tool.decision = Decision::Undecided;
                }
            });
        }
        let ticket = Ticket(self.ids.take());
        self.asks.insert(
            ticket,
            Ask {
                proc,
                id: id.clone(),
                chat,
                question: None,
                options,
                title: title.clone(),
                tool,
            },
        );
        out.push(Directive::Policy(PolicyDirective::Decide {
            ticket,
            chat,
            request: PermissionRequest {
                item: tool,
                title,
                origin,
                action,
                input,
            },
        }));
    }

    /// The policy answered.
    pub(super) fn policy_answered(
        &mut self,
        now: Now,
        ticket: Ticket,
        answer: PolicyAnswer,
        reason: Option<String>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(ask) = self.asks.get(&ticket) else {
            report(
                out,
                Level::Debug,
                "ignored the policy's answer to a request that has ended".to_owned(),
            );
            return;
        };
        if ask.question.is_some() {
            report(
                out,
                Level::Debug,
                "ignored the policy answering twice".to_owned(),
            );
            return;
        }
        let by = DecidedBy::Policy {
            reason: reason.map(|reason| agent_text(&reason, MOST_LABEL)),
        };
        match answer {
            PolicyAnswer::Allow => match option(ask, OptionKind::AllowOnce) {
                Some(option) => self.settle(ticket, Some(option), by, out),
                // It allowed once, which the agent doesn't offer: anything
                // more is the user's to choose.
                None => self.ask_user(now, ticket, out),
            },
            PolicyAnswer::Refuse => self.refuse(ticket, by, out),
            PolicyAnswer::AskUser => self.ask_user(now, ticket, out),
        }
    }

    /// Asks the user, if the host can show permission questions, and
    /// refuses safely otherwise.
    fn ask_user(&mut self, now: Now, ticket: Ticket, out: &mut Vec<Directive<A, D>>) {
        if !self.host.shows.permission_questions {
            // Nobody can answer: CAF refuses, as its policy.
            return self.refuse(ticket, DecidedBy::Policy { reason: None }, out);
        }
        let Some(ask) = self.asks.get(&ticket) else {
            return;
        };
        let kind = QuestionKind::Permission(Permission {
            tool: ask.tool,
            title: ask.title.clone(),
            options: ask.options.clone(),
        });
        let (chat, tool) = (ask.chat, ask.tool);
        let question = self.ask(now, chat, kind);
        if let Some(ask) = self.asks.get_mut(&ticket) {
            ask.question = Some(question);
        }
        if let Some(item) = tool {
            self.change_asked(chat, item, |tool| {
                if !tool.status.ended() {
                    tool.status = ToolStatus::Asking;
                }
            });
        }
    }

    /// Refuses safely: once if the agent offers that, else always, else
    /// "cancelled".
    fn refuse(&mut self, ticket: Ticket, by: DecidedBy, out: &mut Vec<Directive<A, D>>) {
        let Some(ask) = self.asks.get(&ticket) else {
            return;
        };
        let option =
            option(ask, OptionKind::RejectOnce).or_else(|| option(ask, OptionKind::RejectAlways));
        self.settle(ticket, option, by, out);
    }

    /// Answers the request kept under `ticket` with `option`, or
    /// "cancelled", as `by` decided, and forgets it. Its call is allowed
    /// unless that's a refusal: an option of a kind CAF doesn't know may
    /// let it run. A call refused is Refused, and never runs; one allowed
    /// waits for the agent to run it.
    fn settle(
        &mut self,
        ticket: Ticket,
        option: Option<OptionId>,
        by: DecidedBy,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(ask) = self.asks.remove(&ticket) else {
            return;
        };
        if let Some(question) = ask.question {
            self.unask(ask.chat, question);
        }
        let chosen = option
            .as_ref()
            .and_then(|option| ask.options.iter().find(|offered| offered.id == *option));
        let refused = chosen.is_none_or(|option| {
            matches!(
                option.kind,
                OptionKind::RejectOnce | OptionKind::RejectAlways
            )
        });
        if let Some(item) = ask.tool {
            self.change_asked(ask.chat, item, |tool| {
                if tool.status.ended() {
                    return;
                }
                if refused {
                    tool.status = ToolStatus::Refused;
                    tool.decision = Decision::Refused(by);
                } else {
                    if tool.status == ToolStatus::Asking {
                        tool.status = ToolStatus::Pending;
                    }
                    tool.decision = Decision::Allowed(by);
                }
            });
        }
        let outcome = match option {
            Some(option) => json!({ "outcome": { "outcome": "selected", "optionId": option.0 } }),
            None => cancelled(),
        };
        self.reply_or_report(ask.proc, &ask.id, Ok(outcome), out);
    }

    /// Answers the agent's request, or reports that its process has gone
    /// and it can't be.
    fn reply_or_report(
        &mut self,
        proc: Ticket,
        id: &RequestId,
        result: Result<Value, (i64, String)>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if !self.reply(proc, id, result, out) {
            report(
                out,
                Level::Debug,
                "couldn't answer a permission request: its process has gone".to_owned(),
            );
        }
    }

    /// The user answered `question`.
    pub(super) fn answer_question(
        &mut self,
        now: Now,
        question: QuestionId,
        answer: Answer,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        if let Answer::Permission(option) = &answer
            && let Some(offered) = self.served_answer(now, question, option, out)
        {
            return if offered {
                Ok(())
            } else {
                Err(Refused::WrongAnswer)
            };
        }
        if let Some((&ticket, ask)) = self
            .asks
            .iter()
            .find(|(_, ask)| ask.question == Some(question))
        {
            let Answer::Permission(option) = answer else {
                return Err(Refused::WrongAnswer);
            };
            let Some(offered) = ask.options.iter().find(|offered| offered.id == option) else {
                return Err(Refused::WrongAnswer);
            };
            let by = DecidedBy::User {
                option: offered.label.clone(),
            };
            self.settle(ticket, Some(option), by, out);
            return Ok(());
        }
        let slow = self.runs.iter().find(|(_, run)| run.slow == Some(question));
        if let Some((&chat, _)) = slow {
            let Answer::SlowToOpen(choice) = answer else {
                return Err(Refused::WrongAnswer);
            };
            self.slow_chosen(now, chat, question, choice, out);
            return Ok(());
        }
        Err(Refused::NoSuchQuestion)
    }

    /// The agent withdrew one of its requests, `$/cancel_request`: its
    /// question goes, and the request is still answered, as cancelled.
    pub(super) fn cancel_request(
        &mut self,
        proc: Ticket,
        params: &Value,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let id = match params.get("requestId") {
            Some(Value::Number(number)) => number.as_i64().map(RequestId::Number),
            Some(Value::String(text)) => Some(RequestId::String(text.clone())),
            Some(Value::Null) => Some(RequestId::Null),
            _ => None,
        };
        let found =
            (self.asks.iter()).find(|(_, ask)| ask.proc == proc && Some(&ask.id) == id.as_ref());
        let Some((&ticket, _)) = found else {
            report(
                out,
                Level::Debug,
                "the agent withdrew a request CAF isn't waiting on".to_owned(),
            );
            return;
        };
        if let Some(ask) = self.forget_ask(ticket, out) {
            self.reply_or_report(proc, &ask.id, Err((CANCELLED, "cancelled".to_owned())), out);
        }
    }

    /// Answers every request `chat`'s agent is waiting on "cancelled".
    pub(super) fn withdraw_asks(&mut self, chat: ChatId, out: &mut Vec<Directive<A, D>>) {
        let tickets: Vec<Ticket> = (self.asks.iter())
            .filter(|(_, ask)| ask.chat == chat)
            .map(|(&ticket, _)| ticket)
            .collect();
        for ticket in tickets {
            if let Some(ask) = self.forget_ask(ticket, out) {
                self.reply_or_report(ask.proc, &ask.id, Ok(cancelled()), out);
            }
        }
    }

    /// Forgets the requests `which` picks, without answering them: their
    /// process has gone, so they can't be.
    pub(super) fn drop_asks(
        &mut self,
        which: impl Fn(&Ask) -> bool,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let tickets: Vec<Ticket> = (self.asks.iter())
            .filter(|(_, ask)| which(ask))
            .map(|(&ticket, _)| ticket)
            .collect();
        for ticket in tickets {
            self.forget_ask(ticket, out);
        }
    }

    /// Forgets the request kept under `ticket`: its question goes, and the
    /// policy is told if it was still deciding.
    fn forget_ask(&mut self, ticket: Ticket, out: &mut Vec<Directive<A, D>>) -> Option<Ask> {
        let ask = self.asks.remove(&ticket)?;
        // Nobody decided: it waits for the agent again.
        if let Some(item) = ask.tool {
            self.change_asked(ask.chat, item, |tool| {
                if tool.status == ToolStatus::Asking {
                    tool.status = ToolStatus::Pending;
                }
            });
        }
        match ask.question {
            Some(question) => self.unask(ask.chat, question),
            None => out.push(Directive::Policy(PolicyDirective::Withdrawn { ticket })),
        }
        Some(ask)
    }
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// Changes the tool call `item` a request of the agent's is about, as
    /// `change` says: unless it's a call CAF serves, whose record is
    /// CAF's, and which CAF asks about itself.
    fn change_asked(&mut self, chat: ChatId, item: ItemId, change: impl FnOnce(&mut ToolCall<A>)) {
        if !self.is_served(chat, item) {
            self.change_tool(chat, item, change);
        }
    }
}

/// ACP's answer "cancelled" to a permission request.
fn cancelled() -> Value {
    json!({ "outcome": { "outcome": "cancelled" } })
}

/// The first option of `kind` the request offers.
fn option(ask: &Ask, kind: OptionKind) -> Option<OptionId> {
    ask.options
        .iter()
        .find(|option| option.kind == kind)
        .map(|option| option.id.clone())
}

/// The options a permission request offers, read leniently: one without
/// an id is left out, and one of a kind CAF doesn't know is `Other`.
fn options(options: Option<&Value>) -> Vec<PermissionOption> {
    let options = options
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
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
