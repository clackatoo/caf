//! Account limits: what agents say of them, kept per profile, given to
//! every agent's status, forgotten as their windows reset, cleared as the
//! account is signed out of or in to, and kept by the usage driver.

#![allow(dead_code, reason = "drafted: the builder wires these in")]

use serde_json::Value;

use super::Core;
use crate::chat::{AppData, AppItem};
use crate::directive::Directive;
use crate::ids::AgentName;
use crate::limits::{AccountLimits, ProfileLimits};
use crate::profiles::Builtin;
use crate::time::Now;

impl<A: AppItem, D: AppData> Core<A, D> {
    /// An agent's account limits, as its profile's are now.
    pub(super) fn agent_limits(&self, agent: &AgentName) -> AccountLimits {
        // Drafted: none, until the builder keeps them.
        let _ = agent;
        AccountLimits::default()
    }

    /// The usage driver gave what it keeps, `limits`: each is merged into
    /// its profile's, and the agents' statuses are told again if any
    /// changed. Never saved back.
    pub(super) fn limits_given(
        &mut self,
        limits: Vec<ProfileLimits>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let _ = (limits, out);
        todo!()
    }

    /// An agent of `profile` said something of its limits, in a
    /// `usage_update`, `update`: merged in, saved, and the agents'
    /// statuses told again, if it changed anything.
    pub(super) fn limits_heard(
        &mut self,
        now: Now,
        profile: Builtin,
        update: &Value,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let _ = (now, profile, update, out);
        todo!()
    }

    /// `profile`'s account was signed out of, or a sign-in to it
    /// finished: its limits are cleared as of `now`, saved, and the
    /// agents' statuses told again.
    pub(super) fn limits_cleared(
        &mut self,
        now: Now,
        profile: Builtin,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let _ = (now, profile, out);
        todo!()
    }

    /// The core woke: every window whose reset has passed is forgotten,
    /// and the agents' statuses told again if any was. The earliest
    /// reset left is one of the core's deadlines.
    pub(super) fn limits_woken(&mut self, now: Now, out: &mut Vec<Directive<A, D>>) {
        let _ = (now, out);
        todo!()
    }
}
