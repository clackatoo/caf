---
title: Account Limits
type: how
description: Show how much of the user's subscription each agent has used, window by window, as the agent reports it, such as Claude's five hours, its week and each model's week. Where limits come from, whose they are, how they change over time, where they're kept, and how a viewer shows them with the SDK's helpers.
related:
  - caf/getting-started/usage
  - caf/getting-started/installing-and-signing-in
  - caf/getting-started/a-chat-in-the-browser
---

A subscription allows so much use in each of a few windows of time:
Claude's, five hours from the first turn in them, a week, and a week for
each of some models. When a window's allowance is used up, the agent
refuses turns until it resets. CAF keeps what the agent last said of
each, its **account limits**, so your program can show the user how much
is left before they run out.

## Where they come from

Only Claude says, for now, and only, as the Claude Agent SDK documents
its `rate_limit_info`, for "claude.ai subscription users": an account
signed in with an API key should expect none. Its adapter tells a session
of them in a
`usage_update`, whenever a window's whole percent, reset or state
changes, once the turn has had some of the agent's reply. An agent that
never says has none.

Each window has:

- **`kind`**: `FiveHours`, `Week`, `ModelWeek` with the model's name in
  lower case (`opus`, `sonnet`), `ExtraWeek` (the week with extra usage
  counted in), or `Other`, with the agent's own name for a window CAF
  doesn't know;
- **`used`**: how much of it is used, a whole percent, 0–100;
- **`resets`**: when it starts again, if the agent said;
- **`state`**: `Fine`, `Warning` (the agent warns it's near the end), or
  `Refused` until it resets, always 100% used;
- **`heard`**: when CAF last heard of it.

`extra_usage` says whether extra usage, paid for beyond the plan as it
goes, is in use, if the agent has said.

## Whose they are

An account's, not a chat's: every chat with an agent, and every agent with
the same profile, shares them, as they share one sign-in. They come with
the agent's status, as `AgentStatus::limits`, the same for every viewer:

```rust
use std::time::{SystemTime, UNIX_EPOCH};
use caf_core::time::Wall;

let mut agents = caf.watch_agents();
while let Some(statuses) = agents.next().await {
    let since = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let now = Wall(i64::try_from(since.as_millis())?); // milliseconds since the epoch
    for status in &statuses {
        for window in &status.limits.windows {
            draw_limit(&status.name, window, window.pace(now)); // see "Showing them"
        }
    }
}
```

They're information only. CAF never holds a message back for them, nor
warns before sending: a turn the agent refuses for its limits fails as
it does without them, as `FailureReason::UsageLimit`.

## Over time

- **The latest news wins, window by window.** A report changes only the
  windows it names, each replacing the one of its kind if it was heard
  later, whichever chat, app or saved copy it came from.
- **A window is forgotten once its reset has passed**, and the statuses
  are told again. One with no reset stays until another replaces it.
- **Signing out, and a sign-in finishing, clears them**, as the account
  may have changed. A window heard before that is never taken back.
- **A limit reset early** by the provider, or raised by a new plan, shows
  as it was until the agent next says, with the next turn: CAF can't ask.
  `heard` lets you say how old a window is.

## Where they're kept

In the usage folder every CAF app on the machine shares
([Usage](~caf/getting-started/usage)), a file per profile in `limits/`,
such as `limits/claude-agent-acp.json`. So an app has them as it starts,
before the agent says again. Each save is merged with the file, so two
apps never undo each other's later news, and an app takes in what another
kept as it starts and whenever it saves news of its own. With no usage
folder (`config.usage = None`), limits are still kept while CAF runs, but
not across restarts.

## Showing them

A window's **pace** is how much of it has gone by, as a whole percent:
where `used` would be at an even pace. Used above it is ahead of pace, on
the way to running out before the reset. A window CAF doesn't know the
length of (`Other`), or with no reset, has none.

The TypeScript SDK has the rest, so every viewer says the same of them:

```ts
import { label, left, lowest, lowness, pace } from "@clackatoo/caf";

for (const agent of caf.agents) {
  const worst = lowest(agent.limits); // the window with least left, for a glance
  if (worst !== null) drawDot(agent.name, lowness(worst)); // "fine", "low" or "out"
  for (const window of agent.limits.windows) {
    drawBar(label(window.kind), left(window), pace(window, Date.now()));
  }
}
```

- `label` names a window for showing: `5-hour`, `Weekly`, `Weekly · Opus`,
  `Weekly · extra included`, or the agent's own name for it.
- `left` is how much is left, as a whole percent: none once refused.
- `lowness` is `low` at 20% left or less (`LOW_LEFT`), and `out` at 10%
  or less (`OUT_LEFT`), or once refused; otherwise `fine`.
- `lowest` is the window with the least left, the first of them on a tie,
  or `null` with none.
- `pace` and `length` match caf-core's `LimitWindow::pace` and
  `WindowKind::length` exactly.

Each takes an agent's status as a watch gives it, read-only. A kind of
window newer than the SDK has no length and no pace, and is named by its
type.

Pace and resets move with the clock, not with news, so draw them again as
time passes: a viewer has no other word that a window is nearer its
reset.
