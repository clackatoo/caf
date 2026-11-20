//! The setup driver: installing agents and signing in to them, as the core
//! directs ([`SetupDirective`]), on tokio.
//!
//! It keeps the contract written on [`SetupDirective`]: installs built in a
//! hidden folder and renamed into place whole; one install of a folder at
//! a time across every CAF program on the machine; nothing removed while a
//! process runs from it; `https` downloads checked before they're unpacked;
//! and nothing a sign-in or status command prints reaches the core except
//! what its recipe names.
//!
//! npm installs run `npm ci --ignore-scripts --no-audit --no-fund` from the
//! profile's lockfile. Downloads are fetched by the driver itself, not by
//! `curl`. Sign-in commands that open a browser are given a `BROWSER` of
//! the driver's own, which hands their link back instead of opening
//! anything.

use std::path::PathBuf;

use caf_core::directive::SetupDirective;
use caf_core::event::SetupEvent;
use caf_core::ids::Ticket;

/// Installs agents and runs their sign-in commands.
pub struct SetupDriver {
    _todo: (),
}

impl SetupDriver {
    /// A driver keeping installs under `root`, which it makes if it isn't
    /// there, readable only by the user.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        let _ = root;
        todo!()
    }

    /// The folder installs go in by default: `$XDG_DATA_HOME/caf/agents`,
    /// or `~/.local/share/caf/agents`. `None` if neither can be found.
    #[must_use]
    pub fn default_root() -> Option<PathBuf> {
        todo!()
    }

    /// Starts carrying out a directive. It never blocks: the work runs on
    /// tasks and threads of the driver's own, and reports come from
    /// [`SetupDriver::next`].
    pub fn apply(&mut self, directive: SetupDirective) {
        let _ = directive;
        todo!()
    }

    /// The next report, quoting the ticket of the directive it's about.
    ///
    /// Cancel-safe: dropped before it resolves, it loses nothing.
    pub async fn next(&mut self) -> (Ticket, SetupEvent) {
        todo!()
    }
}
