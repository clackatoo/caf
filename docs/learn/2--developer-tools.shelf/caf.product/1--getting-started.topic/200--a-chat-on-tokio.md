---
title: A Chat on Tokio
type: how
description: Run CAF in a tokio program with caf-driver-tokio. Start it, open a chat, watch it change, send messages, answer questions, stop a turn, shut down, and record a run to replay as a test.
related:
  - caf/getting-started/what-is-caf
  - caf/getting-started/installing-and-signing-in
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
let claude = AgentSpec::command(
    "claude",
    SpawnCommand::new("/opt/claude-agent-acp/bin/claude-agent-acp"),
    ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
);
let (caf, running) = Caf::<NoAppItems>::start(Config::new(Host::new("eyespy", "0.4.0"), vec![claude]));
let chat = caf.open_chat(OpenChat::new("claude", "/home/me/project")).await?;
```

A chat takes messages at once. Messages sent before its session is open wait
for it.

The agent has none of its own tools: only those you give it, in
`OpenChat::mcp_servers`. `OpenChat::instructions` tells it about your
program and the work, after CAF's own short instructions. See
[Agents and profiles](~caf/concepts/agents-and-profiles#an-agent-has-only-your-tools).

Here your program says how to start the agent's adapter. CAF can also
install it, and sign in to it, itself: see
[Installing and signing in](~caf/getting-started/installing-and-signing-in).

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
that hasn't seen how shutting down left the chat gets it as one last picture
first, even one that never read before.

A watch doesn't keep CAF running: see [Shut down](#shut-down).

## Answer questions and stop turns

Questions wait on the chat, in `Chat::questions`. The agent asks permission
to do things, and CAF asks when a session is slow to open. Answer each with
`ChatHandle::answer`. A question is answered once, and only while it stands:
a second answer, or an answer to a question that's gone, is refused.

The default policy, `AlwaysAsk`, puts every permission request to the user.
A program can give `Config::policy` its own. Its `decide` should return at
once, with a future that does the work: that future may take as long as it
needs, and is dropped if the request is withdrawn, such as when the user
stops the turn. If either panics, the request is refused.

`ChatHandle::stop` stops the running turn and drops the ones waiting.

## Shut down

```rust
caf.shutdown().await;
running.await;
```

`shutdown` returns once every adapter has exited and been reaped, and what
each started has been sent SIGKILL. From the moment it's called, anything
else asked of CAF fails with `Error::Gone`.

If every `Caf` and `ChatHandle` is dropped, CAF shuts down by itself. Watches
don't count: a task still waiting on a watch gets the chat's last picture,
then `None`.

## Logs

The core's reports go to the [`log`](https://docs.rs/log) facade, with target
`caf`, at the level the core gave them: debug, info or warn. They never hold
the account email Claude's adapter sends.

## Record a run

Set `Config::record` to a file that doesn't exist yet, and CAF writes down
everything the core is given, with its time, as JSON lines. Replayed into a
fresh `Core`, a recording ends in the same state, so a real session becomes a
test.

```rust
let mut config = Config::new(host, agents);
config.record = Some("/home/me/caf-run.jsonl".into());
```

The first line holds the host and the agents:

```json
{"caf_recording": 3, "host": {…}, "agents": [{…}]}
```

Each line after it is one event, in order:

```json
{"now": {"wall": 1795600000000, "mono": 1953000000}, "event": {"type": "Host", "value": {"type": "Send", "value": {…}}}}
```

Every enum is written as its variant's name as `type`, with what it holds, if
anything, as `value`, as everywhere in CAF: `{"type": "Woken"}`. This is
version 3 of the format: check `caf_recording` before reading the rest.
Until CAF is released, an older version's recording isn't read: its
events change shape with no migration, as version 3's did for archiving
and the names on notes about settings.
Each `type` comes before its `value`, and must stay there: what the agent
sent is kept exactly as it came, and reads back only in that order.

To replay it, make a `Core` from the first line with `Core::new`, read each
line's event as an `Event`, and give it to `Core::handle` at its time. A
recording cut short ends with a `stopped` line (see below): stop there.

### What's hidden

None of this changes what the core does with the recording:

- **Claude's sign-in status** (`_auth/status_update`) holds the account's
  email. It's written with its params as the kind of sign-in alone, such as
  `{"authStatus": {"kind": "account"}}`, which is all the core reads.
- **A code the user pasted** into a sign-in (`Caf::paste`) is written as
  `null`. A replay takes it as text it doesn't have.
- **Each agent's arguments** are left out, and **its environment variables**
  are written with their names but not their values.
- **Each MCP server's environment variables and HTTP headers** are written
  with their names but not their values.

### What's kept

Everything else goes in as it came:

- the host, and each agent's name, program, folder and profile;
- what the user sent, and everything the agent said;
- the agent's error output, exactly: as text where a piece of it is UTF-8,
  and as bytes where it isn't, such as one that ends partway through a
  character;
- folders;
- each MCP server's name, program, arguments and URL;
- what the setup driver reported, with the folder installs live in, and a
  sign-in's link and code, each good for one sign-in only.

So don't put secrets in MCP arguments or URLs. Names of variables and
arguments are written as arrays of bytes, as Rust writes them. Treat a
recording as private, and read it through before you share it.

### How it's written

The file is made new, readable only by you. If anything is at that path
already, even a link, CAF logs a warning and doesn't record. It's written on
a thread of its own, so the disk never holds CAF up, and it's complete once
`shutdown` returns.

If a write fails, CAF logs a warning and stops recording. If more than
256 MiB waits to be written because the disk has fallen behind, it stops too,
and once what came before is written, it ends the file with a line saying
why:

```json
{"stopped": "the disk has fallen behind"}
```

Either way, what's in the file still replays, as far as it goes.

## What goes where

Inside, one task owns the core and its drivers. Every directive the core
gives goes to its driver:

| Directive | Where it goes |
|---|---|
| Agent | `AgentDriver::apply` |
| Timer | `TimerDriver::wake_at` |
| Policy: decide | The policy, whose answer comes back to the core |
| Policy: withdrawn | Stops that decision |
| Viewer | Wakes the chat's watches |
| Setup | `SetupDriver::apply`, whose reports come back to the core |
| Report | The `log` facade |

A program with its own event loop can use `AgentDriver`, `TimerDriver`,
`SetupDriver` and a `Policy` on their own, and wire them to the core itself.
