//! Account limits: what agents say of them, kept per profile, given to
//! every agent's status, forgotten as their windows reset, cleared as the
//! account is signed out of or in to, and kept by the usage driver.
//!
//! The core only changes what [`Core::agents`](crate::Core::agents)
//! gives: the driver, which compares it after each call, tells viewers.

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
use crate::time::{Mono, Now, Wall};

/// The longest the core waits on the steady clock for a window to
/// reset. The steady clock stops while the machine sleeps, so a reset
/// that passed meanwhile is found within this long of waking.
const MOST_WAIT: Duration = Duration::from_secs(5 * 60);

/// Every profile's account limits, and when the core next wakes to forget
/// one.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Limits {
    /// Each profile's, by its [slug](Builtin::slug).
    kept: BTreeMap<&'static str, AccountLimits>,
    /// When to wake next, on the steady clock: at the earliest reset of
    /// any window, or [`MOST_WAIT`] from when it was worked out if that's
    /// sooner. One of the core's deadlines.
    pub(super) wake: Option<Mono>,
    /// The earliest reset `wake` was worked out for. It's worked out
    /// again only when that changes, or at `wake`: worked out on every
    /// call, the two clocks' readings, never quite in step, would move it
    /// by a little each time.
    wake_for: Option<Wall>,
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// An agent's account limits, as its profile's are now.
    pub(super) fn agent_limits(&self, agent: &AgentName) -> AccountLimits {
        (self.slugs.get(agent))
            .and_then(|slug| self.limits.kept.get(slug))
            .cloned()
            .unwrap_or_default()
    }

    /// The usage driver gave what it keeps, `limits`, at `now`: each is
    /// merged into its profile's, as news from the agent is, so what
    /// [`Core::agents`](crate::Core::agents) gives changes if any did.
    /// Never saved back.
    pub(super) fn limits_given(&mut self, now: Now, limits: Vec<ProfileLimits>) {
        for mut given in limits {
            not_after(&mut given.limits, now.wall);
            let kept = self.limits.kept.entry(given.profile.slug()).or_default();
            kept.merge(&given.limits);
            kept.forget_passed(now.wall);
        }
    }

    /// An agent of `profile` said something of its limits, in a
    /// `usage_update`, `update`: merged in, then any window already reset
    /// forgotten, and saved if that changed anything.
    pub(super) fn limits_heard(
        &mut self,
        now: Now,
        profile: Builtin,
        update: &Value,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(news) = ProfileChoice::Builtin(profile).limits(update, now.wall) else {
            return;
        };
        let kept = self.limits.kept.entry(profile.slug()).or_default();
        let before = kept.clone();
        kept.merge(&news);
        kept.forget_passed(now.wall);
        if *kept != before {
            save(profile, kept, out);
        }
    }

    /// `profile`'s account was signed out of, or a sign-in to it
    /// finished: its limits are cleared as of `now`, and saved.
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

    /// `agent` was signed out of, or in to, or found signed out: its
    /// profile's limits are cleared, if it has one.
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

    /// A call ends: every window whose reset has passed is forgotten, and
    /// the wake-up worked out again if the earliest reset left changed.
    pub(super) fn limits_tidied(&mut self, now: Now) {
        let next = self.forget_passed(now);
        if next != self.limits.wake_for {
            self.wake_for_reset(now, next);
        }
    }

    /// The core woke for the limits: every window whose reset has passed
    /// is forgotten, and the wake-up worked out again, as the wall clock
    /// may say the earliest hasn't yet.
    pub(super) fn limits_woken(&mut self, now: Now) {
        let next = self.forget_passed(now);
        self.wake_for_reset(now, next);
    }

    /// Forgets every window whose reset has passed: the earliest reset
    /// left.
    fn forget_passed(&mut self, now: Now) -> Option<Wall> {
        for kept in self.limits.kept.values_mut() {
            kept.forget_passed(now.wall);
        }
        (self.limits.kept.values())
            .filter_map(AccountLimits::next_reset)
            .min()
    }

    /// Wakes the core for the reset at `next`, measured on the steady
    /// clock from now, but never more than [`MOST_WAIT`] away.
    fn wake_for_reset(&mut self, now: Now, next: Option<Wall>) {
        self.limits.wake_for = next;
        self.limits.wake = next.map(|resets| {
            let left = u64::try_from(resets.0.saturating_sub(now.wall.0)).unwrap_or(0);
            now.mono.after(Duration::from_millis(left).min(MOST_WAIT))
        });
    }
}

/// Takes `limits` as heard no later than `now`: news from a clock ahead
/// would otherwise outrank every later report.
fn not_after(limits: &mut AccountLimits, now: Wall) {
    for window in &mut limits.windows {
        window.heard = window.heard.min(now);
    }
    if let Some(extra) = limits.extra_usage.as_mut() {
        extra.heard = extra.heard.min(now);
    }
    limits.cleared = limits.cleared.map(|cleared| cleared.min(now));
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
