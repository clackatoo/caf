//! The agent driver: adapter processes and their pipes.
//!
//! It knows nothing about which agent it runs, or about ACP beyond
//! splitting lines: it starts programs, carries bytes and reports what
//! happens, keeping the contract in
//! [`AgentDirective`](caf_core::directive::AgentDirective)'s docs.
//!
//! Each process is started in a process group of its own and watched
//! through a pidfd. Signals go to the whole group through the pidfd, so
//! the `claude` programs Claude's adapter starts stop with it. Lines of
//! 256 KiB or more are parsed on a blocking thread, and handed on in
//! order.

use std::time::Duration;

use caf_core::Ticket;
use caf_core::directive::AgentDirective;
use caf_core::event::AgentEvent;

/// Limits for the agent driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLimits {
    /// The biggest line accepted. Must match the core's
    /// [`Limits::biggest_message`](caf_core::host::Limits::biggest_message).
    /// 64 MiB.
    pub biggest_line: usize,
    /// Lines this long or longer are parsed off the task. 256 KiB.
    pub parse_elsewhere: usize,
    /// How much may wait to be parsed before reading pauses, so a flood
    /// is held back by the pipe rather than by memory. 64 MiB.
    pub parsing: usize,
    /// How much is read before yielding to other tasks. 1 MiB.
    pub read_step: usize,
    /// How much unsent input may build up for one process before it's
    /// treated as stuck and reported gone. 64 MiB.
    pub unsent: usize,
    /// How long a process may take to let go of its pipes after it
    /// exits, before they're dropped. 10 s.
    pub drain: Duration,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            biggest_line: 64 << 20,
            parse_elsewhere: 256 << 10,
            parsing: 64 << 20,
            read_step: 1 << 20,
            unsent: 64 << 20,
            drain: Duration::from_secs(10),
        }
    }
}

/// Runs adapter processes for the core.
pub struct AgentDriver {
    limits: AgentLimits,
}

impl AgentDriver {
    /// A driver with these limits.
    #[must_use]
    pub fn new(limits: AgentLimits) -> Self {
        Self { limits }
    }

    /// Does one directive. Never blocks: starting, writing and signalling
    /// all return at once, and what they lead to is reported by
    /// [`AgentDriver::next`].
    pub fn apply(&mut self, directive: AgentDirective) {
        todo!()
    }

    /// The next thing that happened to any process, with the process's
    /// ticket. Pending forever while there are no processes.
    pub async fn next(&mut self) -> (Ticket, AgentEvent) {
        todo!()
    }
}
