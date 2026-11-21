//! Deadlines: the watchdogs and waits the core keeps, and what happens
//! when one passes.
//!
//! The core asks the timer driver for one wake-up, at its earliest
//! deadline, whenever that changes. When woken, it does whatever is due.

use super::procs::Cause;
use super::state::{Group, Init, Open, Purpose};
use super::{Core, report};
use crate::chat::AppData;
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
    /// A gone process must be let go now.
    Forget(Ticket),
    /// A request is overdue.
    Request(Ticket, i64),
    /// A chat's agent hasn't stopped in time.
    Stop(ChatId),
    /// A turn a chat's agent started by itself has been quiet a while.
    Quiet(ChatId),
    /// A chat has waited a while for its session to be let go.
    Held(ChatId),
    /// A sign-in nobody finished in time.
    SignIn(Ticket),
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// The earliest deadline the core has, if any.
    pub(super) fn next_deadline(&self) -> Option<Mono> {
        self.deadlines().map(|(at, _)| at).min()
    }

    /// Every deadline, with what's due then.
    fn deadlines(&self) -> impl Iterator<Item = (Mono, Due)> + '_ {
        let procs = self.procs.iter().flat_map(|(&proc, each)| {
            let init = match each.init {
                Init::Waiting { deadline: Some(at) } if !each.retired => {
                    Some((at, Due::Init(proc)))
                }
                _ => None,
            };
            let gone = (each.gone.as_ref())
                .filter(|_| !each.finalized)
                .map(|gone| (gone.until, Due::Gone(proc)));
            let step = (each.ending.as_ref())
                .and_then(|ending| ending.next)
                .map(|(_, at)| (at, Due::EndStep(proc)));
            let group = match each.group {
                Group::Again(at) => Some((at, Due::Group(proc))),
                _ => None,
            };
            // Before the group: once let go, there's nothing to ask.
            let forget = self.forget_by(proc).map(|at| (at, Due::Forget(proc)));
            [init, gone, step, forget, group].into_iter().flatten()
        });
        let requests = (self.pending.iter())
            .filter_map(|(&(proc, id), pending)| Some((pending.deadline?, Due::Request(proc, id))));
        let stops = (self.runs.iter())
            .filter_map(|(&chat, run)| Some((run.running.as_ref()?.stop_by?, Due::Stop(chat))));
        let quiet =
            (self.runs.iter()).filter_map(|(&chat, run)| Some((run.quiet_by?, Due::Quiet(chat))));
        let held = (self.runs.iter())
            .filter(|(_, run)| run.open == Open::Wanted)
            .filter_map(|(&chat, run)| Some((run.held_by?, Due::Held(chat))));
        let sign_ins = (self.setup.jobs.iter())
            .filter(|(_, job)| !job.stopped)
            .filter_map(|(&ticket, job)| Some((job.deadline?, Due::SignIn(ticket))));
        procs
            .chain(requests)
            .chain(stops)
            .chain(quiet)
            .chain(held)
            .chain(sign_ins)
    }

    /// The timer driver woke the core: whatever is due is done.
    pub(super) fn woken(&mut self, now: Now, out: &mut Vec<Directive<A, D>>) {
        if self.wake.is_some_and(|wake| wake <= now.mono) {
            self.wake = None;
        }
        for _ in 0..MOST_ROUNDS {
            let due: Vec<Due> = self
                .deadlines()
                .filter(|&(at, _)| at <= now.mono)
                .map(|(_, due)| due)
                .collect();
            if due.is_empty() {
                return;
            }
            for due in due {
                self.fire(now, due, out);
            }
        }
    }

    /// Does what's due.
    fn fire(&mut self, now: Now, due: Due, out: &mut Vec<Directive<A, D>>) {
        match due {
            Due::Init(proc) => self.init_slow(now, proc),
            Due::Gone(proc) => self.finalize(now, proc, out),
            Due::EndStep(proc) => self.end_step(now, proc, out),
            Due::Group(proc) => self.check_group(proc, out),
            Due::Forget(proc) => self.forget_now(now, proc, out),
            Due::Request(proc, id) => self.overdue(now, proc, id, out),
            Due::Stop(chat) => self.stop_overdue(now, chat, out),
            Due::Quiet(chat) => self.quiet(now, chat),
            Due::SignIn(ticket) => self.sign_in_overdue(ticket, out),
            Due::Held(chat) => {
                if let Some(run) = self.runs.get_mut(&chat) {
                    run.held_by = None;
                }
                self.slow(now, chat, true);
            }
        }
    }

    /// `proc` hasn't answered `initialize` in time: each chat waiting on
    /// it is asked whether to keep waiting.
    fn init_slow(&mut self, now: Now, proc: Ticket) {
        let Some(each) = self.procs.get_mut(&proc) else {
            return;
        };
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
    fn overdue(&mut self, now: Now, proc: Ticket, id: i64, out: &mut Vec<Directive<A, D>>) {
        let Some(pending) = self.pending.get_mut(&(proc, id)) else {
            return;
        };
        pending.deadline = None;
        match pending.purpose {
            // Nobody waits for these any more: they just end.
            Purpose::Open {
                abandoned: true, ..
            }
            | Purpose::Prompt {
                abandoned: true, ..
            } => {
                let open = match &pending.purpose {
                    Purpose::Open { session, .. } => {
                        Some(session.as_ref().map(|session| session.0.clone()))
                    }
                    _ => None,
                };
                self.pending.remove(&(proc, id));
                // A session it opens even so is still closed.
                if let (Some(each), Some(session)) = (self.procs.get_mut(&proc), open) {
                    each.given_up.insert(id, session);
                }
                report(
                    out,
                    Level::Debug,
                    format!(
                        "gave up on request {id} of process {}, which nobody waited for",
                        proc.0
                    ),
                );
            }
            // Slow, not failed: the user chooses.
            Purpose::Open { chat, .. } => {
                if self
                    .runs
                    .get(&chat)
                    .is_some_and(|run| run.open == Open::Asked { proc, id })
                {
                    self.slow(now, chat, true);
                }
            }
            Purpose::Close { .. } => {
                self.pending.remove(&(proc, id));
                report(
                    out,
                    Level::Warn,
                    format!("process {} didn't answer closing a session", proc.0),
                );
                // Stuck: restarted, unless that would cut a turn off.
                let busy = (self.runs.values()).any(|run| {
                    run.running
                        .as_ref()
                        .is_some_and(|running| running.proc == proc)
                });
                if !busy {
                    self.lose_process(now, proc, Cause::Restarted, out);
                }
            }
            // These have no watchdog of their own.
            Purpose::Initialize | Purpose::Prompt { .. } => {}
        }
    }
}
