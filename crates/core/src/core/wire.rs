//! Writing JSON-RPC to an adapter: CAF's requests and notifications, and
//! its answers to the agent's requests.
//!
//! Nothing is written to a process that has gone or is being killed, and
//! no new request goes to one that's been retired.

use std::time::Duration;

use serde_json::{Value, json};

use super::Core;
use super::state::{Pending, Purpose};
use crate::acp::RequestId;
use crate::chat::AppItem;
use crate::directive::{AgentDirective, Directive};
use crate::ids::Ticket;
use crate::time::Now;

/// JSON-RPC's error for a method the other side doesn't have.
pub(super) const METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC's error for a request that can't be read.
pub(super) const INVALID_REQUEST: i64 = -32600;

/// ACP's error for a request the asker withdrew.
pub(super) const CANCELLED: i64 = -32800;

/// ACP's error for something that doesn't exist, such as a session.
pub(super) const NOT_FOUND: i64 = -32002;

impl<A: AppItem> Core<A> {
    /// Sends a request to `proc` and remembers it as `purpose`, overdue
    /// after `watchdog` if given. Returns its id, or `None` if `proc` may
    /// not be asked anything.
    pub(super) fn request(
        &mut self,
        now: Now,
        proc: Ticket,
        (method, params): (&str, Value),
        purpose: Purpose,
        watchdog: Option<Duration>,
        out: &mut Vec<Directive>,
    ) -> Option<i64> {
        let each = self.procs.get_mut(&proc).filter(|each| each.askable())?;
        let id = each.next_id;
        each.next_id += 1;
        write(out, proc, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = watchdog.map(|watchdog| now.mono.after(watchdog));
        self.pending.insert((proc, id), Pending {
            purpose,
            sent: now.mono,
            deadline,
        });
        Some(id)
    }

    /// Sends a notification to `proc`, if it may be written to.
    pub(super) fn notify(&mut self, proc: Ticket, method: &str, params: Value, out: &mut Vec<Directive>) {
        if self.procs.get(&proc).is_some_and(|each| each.askable()) {
            write(out, proc, &json!({ "jsonrpc": "2.0", "method": method, "params": params }));
        }
    }

    /// Answers the agent's request `id` on `proc` with `result`, or an
    /// error's code and message. Returns whether it could be written.
    pub(super) fn reply(&mut self, proc: Ticket, id: &RequestId, result: Result<Value, (i64, String)>, out: &mut Vec<Directive>) -> bool {
        if !self.procs.get(&proc).is_some_and(|each| each.writable()) {
            return false;
        }
        let id = id_json(id);
        let message = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        };
        write(out, proc, &message);
        true
    }
}

/// Adds a directive writing `message` as one line to `proc`.
fn write(out: &mut Vec<Directive>, proc: Ticket, message: &Value) {
    let mut bytes = serde_json::to_vec(message).unwrap_or_default();
    bytes.push(b'\n');
    out.push(Directive::Agent(AgentDirective::Write { process: proc, bytes }));
}

/// A request id as JSON.
pub(super) fn id_json(id: &RequestId) -> Value {
    match id {
        RequestId::Number(number) => json!(number),
        RequestId::String(text) => json!(text),
        RequestId::Null => Value::Null,
    }
}
