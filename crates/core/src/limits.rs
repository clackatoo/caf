//! Account limits: how much of an agent's subscription allowance is used,
//! window by window, as the agent reports it. Claude's five hours and its
//! week, say.
//!
//! # Where they come from
//!
//! Claude's adapter tells a session of them in a `usage_update`, in
//! `_meta._claude/rateLimit` (the Claude Agent SDK's `rate_limit_info`),
//! whenever any window's whole percent, reset or status changes:
//! - **`unifiedWindows`** holds the five-hour window, the week and the
//!   week with extra usage included, each with its own share used, reset
//!   and status;
//! - **the limit itself** (`rateLimitType`, `utilization`, `resetsAt`,
//!   `status`) is the one that matters most just then, and fills in any
//!   window `unifiedWindows` doesn't hold, such as a model's own week.
//!   For a window both hold, `unifiedWindows`' wins.
//!
//! `overage` is never a window: it says whether extra usage is in use
//! ([`AccountLimits::extra_usage`]), as `isUsingOverage` does. A window
//! with no share that reads is skipped; a share is kept to 0–100% and
//! rounded, and a reset only between 2020 and 2100. Agents whose profile
//! says nothing of limits have none.
//!
//! Claude's adapter passes a report on only once the turn has had some of
//! the agent's reply, so a new Claude process's first one may be lost.
//! That's why limits are kept (below).
//!
//! # Whose they are
//!
//! An account's, not a chat's: every chat with an agent, and every agent
//! with the same [profile](crate::profiles::Builtin), shares them, as
//! they share one sign-in. They come with the agent's status
//! ([`AgentStatus::limits`](crate::agent::AgentStatus::limits)), to every
//! viewer. They're information only: CAF never holds a message back for
//! them, and a turn refused for its limits fails as it does without them
//! ([`FailureReason::UsageLimit`](crate::chat::FailureReason::UsageLimit)).
//!
//! # Over time
//!
//! - **A report changes only the windows it names**, each replacing the
//!   one of its kind if it was heard later ([`AccountLimits::merge`]).
//! - **A window is forgotten once its reset has passed**: the core wakes
//!   for the earliest, drops those passed, and tells the agents' statuses
//!   again ([`AccountLimits::forget_passed`]). One with no reset stays
//!   until another replaces it.
//! - **Signing out, and a sign-in finishing, clears them**, as the account
//!   may change: [`AccountLimits::cleared`] says when, so a window heard
//!   before it is never taken back from a saved copy.
//! - **A limit reset early** by the provider, or raised by a new plan,
//!   shows as it was until the agent next says, with the next turn: CAF
//!   can't ask. [`LimitWindow::heard`] lets a viewer say how old it is.
//!
//! # Kept across restarts
//!
//! The usage driver keeps each profile's limits in a file every CAF app
//! on the machine shares, beside the usage ledger. The core tells it what
//! they are whenever the agent changes them or they're cleared
//! ([`UsageDirective::SaveLimits`](crate::directive::UsageDirective::SaveLimits));
//! the driver merges that with the file, so two apps never undo each
//! other's later news, and tells the core what the file holds as it starts
//! and whenever a save found news of another app's
//! ([`Event::Limits`](crate::Event::Limits)).
//!
//! # Guarantees
//!
//! Each of these has a test.
//!
//! 1. **The latest news wins, window by window**, whichever session, app
//!    or saved copy it came from, and merging is the same in any order.
//! 2. **No window outlives its reset**, nor a clearing of the account it
//!    was heard before.
//! 3. **Refused is always 100% used.**
//! 4. **Limits never stop a message being sent.**

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::profiles::Builtin;
use crate::time::Wall;

/// An account's limits, as its agent last said them.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AccountLimits {
    /// The windows, in the order [`WindowKind`] lists them, then the
    /// others by name. Empty until the agent says, and for an agent that
    /// never does.
    pub windows: Vec<LimitWindow>,
    /// Whether extra usage, beyond the plan, is in use, paid for as it
    /// goes (Claude's `overage`), if the agent has said.
    pub extra_usage: Option<ExtraUsage>,
    /// When the account's limits were last cleared, as it was signed out
    /// of or in to: a window heard before it is never taken back.
    pub cleared: Option<Wall>,
}

/// One window of an account's limits.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct LimitWindow {
    /// Which window it is.
    pub kind: WindowKind,
    /// How much of it is used, as a whole percent, 0–100. 100 when
    /// [`Refused`](LimitState::Refused).
    pub used: u8,
    /// When it starts again, if the agent said.
    pub resets: Option<Wall>,
    /// Whether the agent still takes turns in it.
    pub state: LimitState,
    /// When CAF last heard of it, from any session or app.
    pub heard: Wall,
}

/// Whether extra usage is in use, and when CAF last heard.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ExtraUsage {
    /// Whether it is.
    pub in_use: bool,
    /// When CAF last heard, from any session or app.
    pub heard: Wall,
}

/// Which window a limit is for.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum WindowKind {
    /// Five hours from the first turn in it. Claude's `five_hour`.
    FiveHours,
    /// A week, for every model. Claude's `seven_day`.
    Week,
    /// A week for one model, by the agent's name for it, lower case:
    /// `opus`, `sonnet`. Claude's `seven_day_<name>`, for any name, as
    /// its CLI knows more models than its SDK lists (`fable`), but
    /// `seven_day_overage_included`, which is [`ExtraWeek`](WindowKind::ExtraWeek).
    ModelWeek(String),
    /// A week with extra usage counted in. Claude's
    /// `seven_day_overage_included`.
    ExtraWeek,
    /// A window CAF doesn't know, by the agent's own name for it. It has
    /// no length, so no pace.
    Other(String),
}

/// Whether the agent still takes turns in a window.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum LimitState {
    /// It does. Claude's `allowed`.
    Fine,
    /// It does, but the agent warns it's near the end. Claude's
    /// `allowed_warning`.
    Warning,
    /// It doesn't, until the window resets. Claude's `rejected`.
    Refused,
}

/// A profile's limits, as the usage driver keeps them.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ProfileLimits {
    /// The profile whose account they are.
    pub profile: Builtin,
    /// The limits.
    pub limits: AccountLimits,
}

impl ProfileLimits {
    /// `profile`'s account's limits.
    #[must_use]
    pub fn new(profile: Builtin, limits: AccountLimits) -> Self {
        Self { profile, limits }
    }
}

impl WindowKind {
    /// How long the window is: five hours, or a week; `None` for
    /// [`Other`](WindowKind::Other).
    #[must_use]
    pub fn length(&self) -> Option<Duration> {
        todo!()
    }
}

impl LimitWindow {
    /// How much of the window has gone by at `now`, as a whole percent,
    /// 0–100: where `used` would be at an even pace. Above it is ahead of
    /// pace. `None` with no reset or no [length](WindowKind::length).
    #[must_use]
    pub fn pace(&self, now: Wall) -> Option<u8> {
        let _ = now;
        todo!()
    }
}

impl AccountLimits {
    /// The window of this kind, if any.
    #[must_use]
    pub fn window(&self, kind: &WindowKind) -> Option<&LimitWindow> {
        let _ = kind;
        todo!()
    }

    /// Take in `news`: each of its windows replaces this one's of its
    /// kind if heard later, or is added; the later `cleared` is kept, and
    /// every window, and `extra_usage`, heard before it dropped. `extra_usage`
    /// is replaced as a window is. Whether anything changed.
    ///
    /// Two of a kind heard at the same moment, as two apps may, are told
    /// apart the cautious way: the more used wins, then the worse state
    /// (`Refused`, then `Warning`, then `Fine`), then the later reset; and
    /// extra usage in use wins over not. So it's the same in any order:
    /// `a.merge(b)` and `b.merge(a)` end alike.
    pub fn merge(&mut self, news: &AccountLimits) -> bool {
        let _ = news;
        todo!()
    }

    /// Drop every window whose reset is at or before `now`. Whether any
    /// was.
    pub fn forget_passed(&mut self, now: Wall) -> bool {
        let _ = now;
        todo!()
    }

    /// The earliest reset of any window: when the core next has one to
    /// forget.
    #[must_use]
    pub fn next_reset(&self) -> Option<Wall> {
        todo!()
    }
}
