//! Deadlines: the watchdogs and waits the core keeps, and what happens
//! when one passes.
//!
//! The core asks the timer driver for one wake-up, at its earliest
//! deadline, whenever that changes. When woken, it does whatever is due.

use super::procs::Cause;
use super::state::{Group, Init, Open, Purpose};
use super::{Core, report};
use crate::chat::AppItem;
use crate::directive::{Directive, Level};
use crate::ids::{ChatId, Ticket};
use crate::time::{Mono, Now};

/// The most rounds of due deadlines one wake-up does: each round may make
/// another due at once, if a host sets a timer to zero.
const MOST_ROUNDS: usize = 16;

/// Something due.
#[derive(Clone, Copy, Debug)]
enum Due {
    /// A process hasn't answered `initialize` in time.
    Init(Ticket),
    /// A gone process's last words have had their time.
    Gone(Ticket),
    /// The next step in stopping a process.
    EndStep(Ticket),
    /// Time to ask again whether a process's group is empty.
    Group(Ticket),
    /// A request is overdue.
    Request(Ticket, i64),
    /// A chat's agent hasn't stopped in time.
    Stop(ChatId),
}

impl<A: AppItem> Core<A> {
    /// The earliest deadline the core has, if any.
    pub(super) fn next_deadline(&self) -> Option<Mono> {
        self.deadlines().map(|(at, _)| at).min()
    }

    /// Every deadline, with what's due then.
    fn deadlines(&self) -> impl Iterator<Item = (Mono, Due)> + '_ {
        let procs = self.procs.iter().flat_map(|(&proc, each)| {
            let init = match each.init {
                Init::Waiting { deadline: Some(at) } if !each.retired => Some((at, Due::Init(proc))),
                _ => None,
            };
            let gone = (each.gone.as_ref()).filter(|_| !each.finalized).map(|gone| (gone.until, Due::Gone(proc)));
            let step = (each.ending.as_ref()).and_then(|ending| ending.next).map(|(_, at)| (at, Due::EndStep(proc)));
            let group = match each.group {
                Group::Again(at) => Some((at, Due::Group(proc))),
                _ => None,
            };
            [init, gone, step, group].into_iter().flatten()
        });
        let requests = (self.pending.iter()).filter_map(|(&(proc, id), pending)| Some((pending.deadline?, Due::Request(proc, id))));
        let stops = (self.runs.iter()).filter_map(|(&chat, run)| Some((run.running.as_ref()?.stop_by?, Due::Stop(chat))));
        procs.chain(requests).chain(stops)
    }

    /// The timer driver woke the core: whatever is due is done.
    pub(super) fn woken(&mut self, now: Now, out: &mut Vec<Directive>) {
        if self.wake.is_some_and(|wake| wake <= now.mono) {
            self.wake = None;
        }
        for _ in 0..MOST_ROUNDS {
            let due: Vec<Due> = self.deadlines().filter(|&(at, _)| at <= now.mono).map(|(_, due)| due).collect();
            if due.is_empty() {
                return;
            }
            for due in due {
                self.fire(now, due, out);
            }
        }
    }

    /// Does what's due.
    fn fire(&mut self, now: Now, due: Due, out: &mut Vec<Directive>) {
        match due {
            Due::Init(proc) => self.init_slow(now, proc),
            Due::Gone(proc) => self.finalize(now, proc, out),
            Due::EndStep(proc) => self.end_step(now, proc, out),
            Due::Group(proc) => self.check_group(proc, out),
            Due::Request(proc, id) => self.overdue(now, proc, id, out),
            Due::Stop(chat) => self.stop_overdue(now, chat, out),
        }
    }

    /// `proc` hasn't answered `initialize` in time: each chat waiting on
    /// it is asked whether to keep waiting.
    fn init_slow(&mut self, now: Now, proc: Ticket) {
        let Some(each) = self.procs.get_mut(&proc) else { return };
        each.init = Init::Waiting { deadline: None };
        let conn = each.conn;
        let waiting: Vec<ChatId> = (self.runs.iter())
            .filter(|(_, run)| run.conn == conn && run.open == Open::Wanted)
            .map(|(&chat, _)| chat)
            .collect();
        for chat in waiting {
            self.slow(now, chat, false);
        }
    }

    /// Request `id` of `proc` is overdue.
    fn overdue(&mut self, now: Now, proc: Ticket, id: i64, out: &mut Vec<Directive>) {
        let Some(pending) = self.pending.get_mut(&(proc, id)) else { return };
        pending.deadline = None;
        match pending.purpose {
            // Slow, not failed: the user chooses.
            Purpose::Open { chat, .. } => {
                if self.runs.get(&chat).is_some_and(|run| run.open == Open::Asked { proc, id }) {
                    self.slow(now, chat, true);
                }
            }
            Purpose::Close { .. } => {
                self.pending.remove(&(proc, id));
                report(out, Level::Warn, format!("process {} didn't answer closing a session", proc.0));
                // Stuck: restarted, unless that would cut a turn off.
                let busy = (self.runs.values()).any(|run| run.running.as_ref().is_some_and(|running| running.proc == proc));
                if !busy {
                    self.lose_process(now, proc, Cause::Restarted, out);
                }
            }
            // These have no watchdog of their own.
            Purpose::Initialize | Purpose::Prompt { .. } => {}
        }
    }
}
