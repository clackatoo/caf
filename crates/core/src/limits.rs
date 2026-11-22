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
//!   Claude may leave its `utilization` out: then the threshold of it
//!   the account has passed (`surpassedThreshold`), if it says, is read
//!   as its share, though at least that much may be used.
//!   For a window both hold, `unifiedWindows`' share, reset and status
//!   win, and the limit's stand in for any it doesn't say; but the limit
//!   refused is never hidden: the window is refused, and all used.
//!
//! `overage` is never a window: it says whether extra usage is in use
//! ([`AccountLimits::extra_usage`]), as `isUsingOverage` does. A window
//! with no share that reads is skipped, unless refused; a share, from 0
//! to 1, is kept to 0–100% and rounded. A reset is in seconds, or in
//! milliseconds if it's above 10^11, as no time in seconds is before the
//! year 5000, and is kept only between 2020 and 2100. A status CAF
//! doesn't know reads as fine, and a window whose name isn't plain
//! (letters, digits, `_`, `-` and `.`, 64 bytes at most) is skipped. Two
//! of a kind, such as a model's week named in two cases, are told apart
//! as a merge does, and one report gives 16 windows at most: the limit's
//! own, then the rest in the order [`WindowKind`] lists them. Limits are
//! heard only from what an agent says as it works, never from a
//! session's history replayed. Agents whose profile says nothing of
//! limits have none.
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
//!   News is merged in, then any window already reset forgotten, from
//!   the agent and the usage driver alike; news heard, or cleared, after
//!   now, by a clock ahead, is taken as of now. A profile keeps 32
//!   windows at most.
//! - **A window is forgotten once its reset has passed**: the core wakes
//!   for the earliest, drops those passed, and the agents' statuses change
//!   ([`AccountLimits::forget_passed`]). One with no reset stays until
//!   another replaces it. The core waits on the steady clock, which stops
//!   while the machine sleeps, so it wakes at least every five minutes
//!   while any window has a reset: one missed asleep is forgotten within
//!   five minutes of waking.
//! - **Signing out, and a sign-in finishing, clears them**, as the account
//!   may change: [`AccountLimits::cleared`] says when, so a window heard
//!   before it is never taken back from a saved copy. So does a status
//!   check finding the agent signed out once it was known signed in, as
//!   with `claude /logout`. An account changed outside CAF with no
//!   sign-out it sees, such as `claude /login` to another account while
//!   signed in, isn't noticed: its limits show the old account's until
//!   the agent next says, window by window.
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

use std::collections::BTreeMap;
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

/// The earliest reset CAF keeps: 2020-01-01, UTC.
pub(crate) const EARLIEST_RESET: Wall = Wall(1_577_836_800_000);

/// The first reset CAF doesn't keep: 2100-01-01, UTC.
pub(crate) const LATEST_RESET: Wall = Wall(4_102_444_800_000);

/// The most windows a profile's limits keep.
pub(crate) const MOST_WINDOWS: usize = 32;

/// Five hours.
const FIVE_HOURS: Duration = Duration::from_secs(5 * 60 * 60);

/// A week.
const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

impl WindowKind {
    /// How long the window is: five hours, or a week; `None` for
    /// [`Other`](WindowKind::Other).
    #[must_use]
    pub fn length(&self) -> Option<Duration> {
        match self {
            Self::FiveHours => Some(FIVE_HOURS),
            Self::Week | Self::ModelWeek(_) | Self::ExtraWeek => Some(WEEK),
            Self::Other(_) => None,
        }
    }
}

impl LimitState {
    /// How bad it is, for telling two windows heard at once apart.
    fn rank(self) -> u8 {
        match self {
            Self::Fine => 0,
            Self::Warning => 1,
            Self::Refused => 2,
        }
    }
}

impl LimitWindow {
    /// How much of the window has gone by at `now`, as a whole percent,
    /// 0–100: where `used` would be at an even pace. Above it is ahead of
    /// pace. `None` with no reset or no [length](WindowKind::length).
    #[must_use]
    pub fn pace(&self, now: Wall) -> Option<u8> {
        let length = i128::try_from(self.kind.length()?.as_millis()).ok()?;
        let resets = i128::from(self.resets?.0);
        let gone = i128::from(now.0) - (resets - length);
        // Rounded half up, as JavaScript's `Math.round` does: the SDKs'
        // pace is the same to the percent.
        let pace = if gone <= 0 {
            0
        } else if gone >= length {
            100
        } else {
            (200 * gone + length) / (2 * length)
        };
        u8::try_from(pace).ok()
    }

    /// What tells two of a kind apart, the later or more cautious
    /// greater: when it was heard, then how much is used, how bad its
    /// state is, and when it resets.
    pub(crate) fn rank(&self) -> (Wall, u8, u8, Option<Wall>) {
        (self.heard, self.used, self.state.rank(), self.resets)
    }

    /// Made whole, whoever wrote it: a share of at most 100, all of it
    /// if refused, and a reset only between 2020 and 2100.
    fn tidied(mut self) -> Self {
        self.used = if self.state == LimitState::Refused {
            100
        } else {
            self.used.min(100)
        };
        self.resets =
            (self.resets).filter(|resets| (EARLIEST_RESET..LATEST_RESET).contains(resets));
        self
    }

    /// What keeps a window when a profile has more than
    /// [`MOST_WINDOWS`], the greater kept: windows of the kinds CAF knows
    /// first, then models' weeks, then others; each by when it was
    /// heard, then the first by name.
    fn keeping(&self) -> (u8, Wall, std::cmp::Reverse<&WindowKind>) {
        let kind = match self.kind {
            WindowKind::Other(_) => 0,
            WindowKind::ModelWeek(_) => 1,
            _ => 2,
        };
        (kind, self.heard, std::cmp::Reverse(&self.kind))
    }
}

impl AccountLimits {
    /// The window of this kind, if any.
    #[must_use]
    pub fn window(&self, kind: &WindowKind) -> Option<&LimitWindow> {
        self.windows.iter().find(|window| &window.kind == kind)
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
    ///
    /// Whoever wrote them, windows are made whole: a share of at most
    /// 100, all of it if refused, and a reset only between 2020 and 2100.
    /// At most 32 are kept: past that, the others CAF doesn't know are
    /// dropped first, then models' weeks, each the oldest heard first.
    pub fn merge(&mut self, news: &AccountLimits) -> bool {
        // Both sides are taken as they come, each window against every
        // other of its kind, so limits read from anywhere, unsorted or
        // with two of a kind, end the same whichever side they're on.
        let mut windows: BTreeMap<WindowKind, LimitWindow> = BTreeMap::new();
        for window in self.windows.iter().chain(&news.windows) {
            let window = window.clone().tidied();
            match windows.get(&window.kind) {
                Some(kept) if kept.rank() >= window.rank() => {}
                _ => {
                    windows.insert(window.kind.clone(), window);
                }
            }
        }
        let cleared = self.cleared.max(news.cleared);
        let fresh = |heard: Wall| cleared.is_none_or(|cleared| heard >= cleared);
        let mut windows: Vec<LimitWindow> = (windows.into_values())
            .filter(|window| fresh(window.heard))
            .collect();
        // The cap picks among each kind's latest, by an order of its own:
        // so it's the same in any order too.
        if windows.len() > MOST_WINDOWS {
            windows.sort_by(|one, other| other.keeping().cmp(&one.keeping()));
            windows.truncate(MOST_WINDOWS);
            windows.sort_by(|one, other| one.kind.cmp(&other.kind));
        }
        let extra_usage = (self.extra_usage.into_iter().chain(news.extra_usage))
            .max_by_key(|extra| (extra.heard, extra.in_use))
            .filter(|extra| fresh(extra.heard));
        let merged = AccountLimits {
            windows,
            extra_usage,
            cleared,
        };
        let changed = *self != merged;
        *self = merged;
        changed
    }

    /// Drop every window whose reset is at or before `now`. Whether any
    /// was.
    pub fn forget_passed(&mut self, now: Wall) -> bool {
        let before = self.windows.len();
        (self.windows).retain(|window| window.resets.is_none_or(|resets| resets > now));
        self.windows.len() != before
    }

    /// The earliest reset of any window: when the core next has one to
    /// forget.
    #[must_use]
    pub fn next_reset(&self) -> Option<Wall> {
        self.windows.iter().filter_map(|window| window.resets).min()
    }
}

#[cfg(test)]
mod tests;
