---
title: Usage
type: how
description: Show what each turn and each chat spent, in tokens and money, and how much every CAF app on the machine has used over a day, a week or a month. What a turn records, a chat's spend, the ledger every app shares and where it is, prices and their three layers, summaries, letting viewers see them, and turning it off.
related:
  - caf/getting-started/a-chat-on-tokio
  - caf/getting-started/choosing-the-model
  - caf/getting-started/saving-and-restoring-chats
  - caf/concepts/viewers
---

CAF records what every turn spends, if the agent says: its tokens, by
model, and what it cost. Each chat adds its turns up, and every turn also
goes into a **usage ledger** that every CAF app on the machine shares, so
your program can show how much the user has used, across all of them.

## What a turn records

When a turn ends, the agent's answer says how many tokens it used. CAF
keeps them on the turn, as `Turn::spent`:

- **tokens**, by kind: input read fresh, input read from the cache, input
  written to the cache, and output, with how much of it was thinking if the
  agent says;
- **by model**: an agent that splits them by model, as Claude and Codex do,
  counts what its subagents and compaction spent too, so CAF keeps that
  split; otherwise one row, against the turn's model;
- **cost**, as the agent reported it, if it did: Claude says what its
  session has cost so far, and the turn's cost is how much that rose;
- **fast mode**, if it was on, which costs more.

A turn the agent says nothing about has no record. It still happened; it
just isn't counted.

## A chat's spend

Each chat has a `spend`: its turns' records added up, with the tokens, the
cost the agent reported, and the cost CAF estimated for turns it didn't.

```rust
let spend = &chat.spend;
draw_cost(spend.cost(), spend.estimated > 0.0); // say "about" if any was estimated
```

It's sent again (`Change::Spend`) whenever a turn adds to it, or the prices
change, so every viewer shows the same figure. A reported cost is never
replaced by an estimate. An estimate is never stored: it's worked out with
the prices of the time, so a price put right puts every figure right.
Tokens on a model no price covers are counted as `unpriced`, and their cost
is in neither figure. Money is in US dollars: a cost an agent reports in
another currency is kept as it came, but estimated instead in the spend,
as CAF converts no currency.

A chat's spend is saved with it, but a restored chat's is worked out again
from its turns. Turns from before CAF recorded usage have no record, so it
counts from when it began.

## The ledger

Every turn's record also goes, as one line, into the ledger: one folder
for the whole machine, shared by every CAF app on it.

```text
~/.local/share/caf/usage/        ($XDG_DATA_HOME/caf/usage if that's set;
  clackatoo/2026-10.jsonl.zst     %APPDATA%\caf\usage on Windows)
  clackatoo/2026-11.jsonl
  eyespy/2026-11.jsonl
  prices/own.json
  prices/litellm.json
  limits/claude-agent-acp.json
```

Each app has a folder, named for its host (`Host::new("eyespy", …)`), and
a file per month, with a line per turn. A month that has ended is
compressed. Lines are only ever added, each whole, so two copies of an app
never mix theirs. A line holds when the turn ended, the app, the chat and
turn, the agent, and what it spent: never what was said, and never the
folder a chat worked in, so the ledger can't tell which projects you work
on.

It's on by default: `Config::new` keeps it in that folder. Writing it is a
thread's of its own, and it never holds a chat up. A line it can't write
is kept, and tried again with the next and every hour; the first failure
in a row is reported to the log (target `caf`). By the time
`Caf::shutdown` returns, every line is written, or reported lost. A disk
that has hung holds shutting down up for thirty seconds at most: then how
many lines may be lost is reported, and CAF stops without them.

The folder also keeps each agent profile's [account
limits](~caf/getting-started/account-limits), how much of the
subscription's five hours and week is used, as the agent last said:
a file per profile in `limits/`, so CAF has them as it starts, before the
agent says again. Each save is merged with the file, window by window, the
latest heard winning, so two apps never undo each other's news, and an app
that finds news another kept in it takes it in. One that can't be kept is
reported to the log, and costs only keeping them across restarts.

## Prices

CAF estimates cost from a table of prices, in three layers. The first that
has a model prices it:

1. **the user's own**, in `prices/own.json`, so every app agrees;
2. **LiteLLM's table**, as last fetched, in `prices/litellm.json`;
3. **the snapshot bundled with CAF**: what prices are before the first
   fetch, offline, or with fetching off.

[LiteLLM's table](https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json)
is fetched once a day, by whichever app looks first, so every app sharing
the folder shares the fetch. A fetch that fails changes nothing, and is
tried again an hour later. A model is found by the agent's id for it:
`claude-haiku-5-5` is also found as `anthropic/claude-haiku-5-5`, and
`claude-haiku-5-5-20261001` as `claude-haiku-5-5`.

```rust
let prices = caf.prices().await?; // each model's price, and its layer
caf.set_price("claude-haiku-5-5".into(), Some(ModelPrice::new(1.0, 5.0))).await?; // per million tokens
caf.set_price("claude-haiku-5-5".into(), None).await?; // the next layer prices it again
```

`ModelPrice::new` gives input and output prices, with the cache prices the
same as input's; set its fields for the rest. A price set is in force for
every chat once `set_price` returns. Another app sharing the folder takes
it within the hour, as each app reads its prices again every hour, and
before every summary it reads, so a summary is always priced as the
prices are now.

## Summaries

A **summary** is how much was used over a period, from every app's lines:
what a usage page is drawn from.

```rust
let query = UsageQuery::new(since, until, "Europe/London"); // by day, in the user's time zone
let summary = caf.usage(query).await?;
for bucket in &summary.buckets {
    // one per day, app, agent, model and fast mode, with turns in it
    draw_bar(bucket.start, &bucket.app, &bucket.agent, &bucket.model, bucket.reported + bucket.estimated);
}
```

Each bucket has its tokens, its reported and estimated cost, its unpriced
tokens and how many turns. `query.by = Resolution::Hour` cuts it by hour
instead, and `apps` and `agents` keep only those named. Every other view, by agent, app, model or day,
is a sum of buckets. An agent is named by its built-in profile, such as
`claude-agent-acp`, so one agent two apps name differently counts as one.
`chats` says how many chats the period covers, `prices` the price of each
model in it, and `unreadable` any file that couldn't be read: a summary is
never quietly short.

It's read on a thread of its own, so it never holds a chat up. A turn
that ended a moment ago may not be in it yet. A time zone CAF doesn't know,
or a period that's empty or longer than 400 days, is `Error::BadQuery`.

## Viewers

A viewer watching a chat sees its turns' records and its spend, as it sees
the rest of the chat. Summaries and prices cover every app on the machine,
so they're for viewers the host lets: `ViewerRules::usage`, off by default.
The welcome says if it's on (`Rights::usage`). A viewer asks for a summary
with `FromViewer::Usage`, answered with `ToViewer::Usage`, and sets a price
with `FromViewer::SetPrice`, answered `Done`, or `PriceNotSet` with the
system's words if it couldn't be written. One that isn't let is answered
`NotAllowed`.
The link goes on while a summary is read.

## Turning it off

```rust
config.usage = None; // no ledger
```

With no ledger, chats still record their turns and their spend, priced with
the bundled snapshot, but there's no summary and no price of the user's
own: both are `Error::NoLedger`, and `Outcome::NoLedger` to a viewer.

To keep the ledger and never reach the network, turn fetching off:

```rust
let mut usage = UsageConfig::new(folder);
usage.fetch_prices = false; // the user's own prices and the bundled snapshot only
config.usage = Some(usage);
```

Tests should do one or the other, so they never write to the user's
ledger.
