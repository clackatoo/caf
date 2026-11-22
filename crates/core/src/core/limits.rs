//! Account limits: what agents say of them, kept per profile, given to
//! every agent's status, forgotten as their windows reset, cleared as the
//! account is signed out of or in to, and kept by the usage driver.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use super::Core;
use crate::chat::{AppData, AppItem};
use crate::directive::{Directive, UsageDirective};
use crate::ids::AgentName;
use crate::limits::{AccountLimits, ProfileLimits};
use crate::profiles::{Builtin, ProfileChoice};
use crate::time::{Mono, Now};

/// Every profile's account limits, and when the core next wakes to forget
/// one.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Limits {
    /// Each profile's, by its [slug](Builtin::slug).
    kept: BTreeMap<&'static str, AccountLimits>,
    /// When the earliest window of any resets, on the steady clock, as of
    /// the last call: one of the core's deadlines.
    pub(super) wake: Option<Mono>,
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// An agent's account limits, as its profile's are now.
    pub(super) fn agent_limits(&self, agent: &AgentName) -> AccountLimits {
        (self.slugs.get(agent))
            .and_then(|slug| self.limits.kept.get(slug))
            .cloned()
            .unwrap_or_default()
    }

    /// The usage driver gave what it keeps, `limits`: each is merged into
    /// its profile's, and the agents' statuses are told again if any
    /// changed. Never saved back.
    pub(super) fn limits_given(&mut self, limits: Vec<ProfileLimits>) {
        for given in limits {
            let kept = self.limits.kept.entry(given.profile.slug()).or_default();
            kept.merge(&given.limits);
        }
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
        let Some(mut news) = ProfileChoice::Builtin(profile).limits(update, now.wall) else {
            return;
        };
        // A window already reset is no news.
        news.forget_passed(now.wall);
        let kept = self.limits.kept.entry(profile.slug()).or_default();
        if kept.merge(&news) {
            save(profile, kept, out);
        }
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
        let kept = self.limits.kept.entry(profile.slug()).or_default();
        kept.merge(&AccountLimits {
            cleared: Some(now.wall),
            ..AccountLimits::default()
        });
        // Saved even if nothing was kept, so every other app clears too.
        save(profile, kept, out);
    }

    /// `agent` was signed out of, or in to: its profile's limits are
    /// cleared, if it has one.
    pub(super) fn account_changed(
        &mut self,
        now: Now,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if let Some(ProfileChoice::Builtin(profile)) =
            self.agents.get(agent).map(|spec| &spec.profile)
        {
            self.limits_cleared(now, *profile, out);
        }
    }

    /// The core woke, or a call ended: every window whose reset has
    /// passed is forgotten, and the agents' statuses told again if any
    /// was. The earliest reset left is one of the core's deadlines.
    pub(super) fn limits_woken(&mut self, now: Now) {
        for kept in self.limits.kept.values_mut() {
            kept.forget_passed(now.wall);
        }
        // Measured on the steady clock from now, as the wall clock may
        // jump: on waking, the wall clock is asked again.
        self.limits.wake = (self.limits.kept.values())
            .filter_map(AccountLimits::next_reset)
            .min()
            .map(|resets| {
                let left = u64::try_from(resets.0.saturating_sub(now.wall.0)).unwrap_or(0);
                now.mono.after(Duration::from_millis(left))
            });
    }
}

/// Tells the usage driver to keep `profile`'s limits, `limits`.
fn save<A: AppItem, D: AppData>(
    profile: Builtin,
    limits: &AccountLimits,
    out: &mut Vec<Directive<A, D>>,
) {
    out.push(Directive::Usage(UsageDirective::SaveLimits(Box::new(
        ProfileLimits {
            profile,
            limits: limits.clone(),
        },
    ))));
}
