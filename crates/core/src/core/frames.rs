//! What an adapter sends, sorted by shape: the agent's requests, its
//! notifications, and its answers to CAF's requests.
//!
//! Every request the agent sends is answered exactly once: here at once,
//! or once the policy or the user decides (see `asks`). The one
//! exception is a request from a process that has gone or is being
//! killed, which nothing can be written to: it's dropped, and reported.

use serde_json::Value;
use serde_json::value::RawValue;

use super::wire::{INVALID_REQUEST, METHOD_NOT_FOUND};
use super::{Core, report};
use crate::acp::{Frame, Message, RequestId, RpcError, Shape};
use crate::chat::AppData;
use crate::chat::AppItem;
use crate::directive::{Directive, Level};
use crate::ids::Ticket;
use crate::time::Now;

/// The longest method name repeated in a report or an error, in bytes.
const MOST_METHOD: usize = 64;

/// An answer to one of CAF's requests.
#[derive(Debug)]
pub(super) enum Reply {
    /// Its result.
    Result(Value),
    /// Its error.
    Error(RpcError),
    /// It couldn't be read: too big, or its result isn't JSON.
    Unreadable,
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// Takes in one line from an adapter.
    pub(super) fn frame(
        &mut self,
        now: Now,
        proc: Ticket,
        frame: Frame,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if self.procs.get(&proc).is_some_and(|each| each.finalized) {
            report(
                out,
                Level::Debug,
                "ignored a message from a process after its end".to_owned(),
            );
            return;
        }
        self.heard(now, proc);
        match frame {
            Frame::Message(Message::Request { id, method, params }) => {
                self.agent_request(now, proc, &id, &method, params, out)
            }
            Frame::Message(Message::Notification { method, params }) => {
                self.notification(now, proc, &method, params, out)
            }
            Frame::Message(Message::Response { id, outcome }) => {
                let reply = match outcome {
                    Ok(result) => {
                        serde_json::from_str(result.get()).map_or(Reply::Unreadable, Reply::Result)
                    }
                    Err(error) => Reply::Error(error),
                };
                self.answered(now, proc, id, reply, out);
            }
            // A line too big to read: its head says whether it was an answer
            // or a request, by the keys found there, so a request whose
            // method lies past the head is still refused rather than taken
            // for an answer. Only where the head says neither does the
            // method decide.
            Frame::TooBig {
                id: Some(id),
                method,
                len,
                shape,
                ..
            } => {
                let request = match shape {
                    Shape::Call => true,
                    Shape::Answer => false,
                    _ => method.is_some(),
                };
                if request {
                    let what = method
                        .as_deref()
                        .map_or_else(|| "its method unknown".to_owned(), shown);
                    report(
                        out,
                        Level::Warn,
                        format!("refused a request of {len} bytes, too big to read: {what}"),
                    );
                    if !self.reply(
                        proc,
                        &id,
                        Err((INVALID_REQUEST, "too big to read".to_owned())),
                        out,
                    ) {
                        report(
                            out,
                            Level::Debug,
                            "couldn't answer it: its process has gone".to_owned(),
                        );
                    }
                } else {
                    report(
                        out,
                        Level::Warn,
                        format!("an answer of {len} bytes was too big to read"),
                    );
                    self.answered(now, proc, id, Reply::Unreadable, out);
                }
            }
            Frame::TooBig { id: None, len, .. } => {
                report(
                    out,
                    Level::Warn,
                    format!("dropped a message of {len} bytes, too big to read, with no id"),
                );
            }
            Frame::Junk { len } => {
                report(
                    out,
                    Level::Debug,
                    format!("skipped a line of {len} bytes that isn't JSON-RPC"),
                );
            }
        }
    }

    /// The agent asks CAF something.
    fn agent_request(
        &mut self,
        now: Now,
        proc: Ticket,
        id: &RequestId,
        method: &str,
        params: Option<Box<RawValue>>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if !self.procs.get(&proc).is_some_and(|each| each.writable()) {
            report(
                out,
                Level::Debug,
                format!(
                    "dropped a request from a process that has gone: {}",
                    shown(method)
                ),
            );
            return;
        }
        match method {
            "session/request_permission" => {
                self.permission_asked(now, proc, id, &parse(params.as_deref()), out)
            }
            // Forms are declared, so Claude keeps its question tool, but no
            // host shows them yet: declining is what ACP has a client do
            // with a form it can't show.
            "elicitation/create" => {
                self.reply(
                    proc,
                    id,
                    Ok(serde_json::json!({ "action": "decline" })),
                    out,
                );
            }
            // Files and terminals aren't declared, and are refused if asked
            // for anyway. So is anything else, extensions too.
            _ => {
                let message = format!("{} isn't supported", shown(method));
                self.reply(proc, id, Err((METHOD_NOT_FOUND, message)), out);
            }
        }
    }

    /// The agent tells CAF something.
    fn notification(
        &mut self,
        now: Now,
        proc: Ticket,
        method: &str,
        params: Option<Box<RawValue>>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        match method {
            "session/update" => self.update(now, proc, &parse(params.as_deref()), out),
            "$/cancel_request" => self.cancel_request(proc, &parse(params.as_deref()), out),
            // Claude's adapter says who is signed in here, with the
            // account's email: only how is read, and the rest is never
            // kept, logged or passed on.
            "_auth/status_update" => self.status_pushed(proc, &parse(params.as_deref()), out),
            _ => report(
                out,
                Level::Debug,
                format!("ignored a notification: {}", shown(method)),
            ),
        }
    }

    /// The agent answered one of CAF's requests.
    fn answered(
        &mut self,
        now: Now,
        proc: Ticket,
        id: RequestId,
        reply: Reply,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let retired = self.procs.get(&proc).is_some_and(|each| each.retired);
        let level = if retired { Level::Debug } else { Level::Warn };
        let RequestId::Number(id) = id else {
            report(out, level, "an answer with an id CAF never uses".to_owned());
            return;
        };
        if self
            .procs
            .get_mut(&proc)
            .is_some_and(|each| each.given_up.remove(&id).is_some())
        {
            let session = match &reply {
                Reply::Result(result) => result.get("sessionId").and_then(Value::as_str),
                _ => None,
            };
            if let Some(session) = session {
                self.close_late(
                    now,
                    proc,
                    crate::ids::AgentSessionId(session.to_owned()),
                    out,
                );
            }
            return;
        }
        let Some(pending) = self.pending.remove(&(proc, id)) else {
            report(
                out,
                level,
                format!("an answer to request {id}, which CAF never sent or has ended"),
            );
            return;
        };
        match pending.purpose {
            super::state::Purpose::Initialize => self.initialized(now, proc, reply, out),
            super::state::Purpose::Open {
                chat,
                how,
                session,
                abandoned,
            } => self.opened(now, (proc, id), chat, (how, session, abandoned), reply, out),
            super::state::Purpose::Prompt {
                chat,
                turn,
                abandoned,
            } => {
                if abandoned {
                    report(
                        out,
                        Level::Debug,
                        "the agent answered a prompt CAF had given up on".to_owned(),
                    );
                } else {
                    self.prompt_answered(now, chat, turn, reply, out);
                }
            }
            super::state::Purpose::Set {
                chat,
                given_up: true,
                ..
            } => self.late_set_answered(now, chat, &reply, out),
            super::state::Purpose::Set { chat, choice, .. } => {
                self.set_answered(now, (proc, id), chat, choice, reply, out);
            }
            super::state::Purpose::Learn { conn } => {
                self.learned(now, (proc, id), conn, reply, out);
            }
            super::state::Purpose::Close { session } => match reply {
                Reply::Result(_) => {
                    if let Some(each) = self.procs.get_mut(&proc) {
                        each.sessions.remove(&session.0);
                    }
                }
                Reply::Error(error) => report(
                    out,
                    Level::Debug,
                    format!("the agent couldn't close a session: error {}", error.code),
                ),
                Reply::Unreadable => report(
                    out,
                    Level::Debug,
                    "the agent's answer to closing a session couldn't be read".to_owned(),
                ),
            },
        }
    }
}

/// `params` as JSON: null if there are none or they can't be read.
fn parse(params: Option<&RawValue>) -> Value {
    params
        .and_then(|params| serde_json::from_str(params.get()).ok())
        .unwrap_or(Value::Null)
}

/// A method's name as it's repeated: cleaned and capped, as the agent
/// chose it.
pub(super) fn shown(method: &str) -> String {
    super::clean::agent_text(method, MOST_METHOD)
}
