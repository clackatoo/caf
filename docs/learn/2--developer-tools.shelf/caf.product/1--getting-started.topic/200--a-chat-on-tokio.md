---
title: A Chat on Tokio
type: how
description: Run CAF in a tokio program with caf-driver-tokio. Start it, open a chat, watch it change, send messages, answer questions, stop a turn, shut down, and record a run to replay as a test.
related:
  - caf/getting-started/what-is-caf
  - caf/concepts/the-core
  - caf/concepts/chats-turns-and-items
---

`caf-driver-tokio` runs CAF in a tokio program. It has the drivers the core
needs and the wiring that joins them up, so most programs use only one type
from it: `Caf`.

## Start CAF and open a chat

Describe your program with a `Host`, list the agents it offers, and start
CAF. It runs in a task of its own on the current tokio runtime.

```rust
let claude = AgentSpec::new(
    "claude",
    SpawnCommand::new("/home/me/.local/share/caf/agents/claude-0.98.0/bin/claude-agent-acp"),
    ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
);
let (caf, running) = Caf::<NoAppItems>::start(Config::new(Host::new("eyespy", "0.4.0"), vec![claude]));
let chat = caf.open_chat(OpenChat::new("claude", "/home/me/project")).await?;
```

A chat takes messages at once. Messages sent before its session is open wait
for it.

## Watch it

A watch gives you the whole chat first, then what changes:

```rust
let mut watch = chat.watch();
chat.send(UserMessage::text("Fix the tests")).await?;
while let Some(update) = watch.next().await {
    match update {
        Update::Picture(chat) => draw_all(&chat),
        Update::Changes(changes) => changes.iter().for_each(draw_change),
        _ => {}
    }
}
```

A watch reads only when you ask it to, so nothing piles up for one you don't
read. If it falls further behind than the chat's list of changes is long
(`Limits::kept_changes`, 4096 by default), it gets the whole chat again.

`next` returns `None` once the chat is closed or CAF has shut down. A watch
that hadn't yet seen how shutting down left the chat gets it as one last
picture first.

## Answer questions and stop turns

Questions wait on the chat, in `Chat::questions`. The agent asks permission
to do things, and CAF asks when a session is slow to open. Answer each with
`ChatHandle::answer`. A question is answered once, and only while it stands:
a second answer, or an answer to a question that's gone, is refused.

The default policy, `AlwaysAsk`, puts every permission request to the user.
A program can give `Config::policy` its own, which may take as long as it
needs. If it panics, the request is refused.

`ChatHandle::stop` stops the running turn and drops the ones waiting.

## Shut down

```rust
caf.shutdown().await;
running.await;
```

`shutdown` returns once every adapter has stopped, and everything they
started with them. From the moment it's called, anything else asked of CAF
fails with `Error::Gone`. If every handle on CAF is dropped, it shuts down by
itself.

## Logs

The core's reports go to the [`log`](https://docs.rs/log) facade, with target
`caf`, at the level the core gave them: debug, info or warn. They never hold
the account email Claude's adapter sends.

## Record a run

Set `Config::record` to a file, and CAF writes down everything the core is
given, with its time, as JSON lines. Replayed into a fresh `Core`, a recording
ends in the same state, so a real session becomes a test.

```rust
let mut config = Config::new(host, agents);
config.record = Some("/home/me/caf-run.jsonl".into());
```

The first line holds the host and the agents:

```json
{"caf_recording": 1, "host": {…}, "agents": [{…}]}
```

Each line after it is one event, in order:

```json
{"now": {"wall": 1795600000000, "mono": 1953000000}, "event": {"Host": {"Send": {…}}}}
```

To replay it, make a `Core` from the first line with `Core::new`, and give it
each event with `Core::handle` at its time.

Two things are left out, and the core needs neither to end in the same
state:

- **Claude's sign-in status** (`_auth/status_update`), which holds the
  account's email.
- **The agents' environment changes**, which may hold keys.

Everything else goes in as it came: what the user sent, what the agent said,
its error output, folders and MCP servers. Treat a recording as private, and
read it through before you share it.

The file is made afresh, readable only by you, and never through a symbolic
link. It's written on a thread of its own, so the disk never holds CAF up,
and it's complete once `shutdown` returns. If it can't be made, CAF logs a
warning and runs on without recording.

## What goes where

Inside, one task owns the core and its drivers. Every directive the core
gives goes to its driver:

| Directive | Where it goes |
|---|---|
| Agent | `AgentDriver::apply` |
| Timer | `TimerDriver::wake_at` |
| Policy: decide | The policy, whose answer comes back to the core |
| Policy: withdrawn | Nowhere: the core ignores the answer when it comes |
| Viewer | Wakes the chat's watches |
| Report | The `log` facade |

A program with its own event loop can use `AgentDriver`, `TimerDriver` and a
`Policy` on their own, and wire them to the core itself.
