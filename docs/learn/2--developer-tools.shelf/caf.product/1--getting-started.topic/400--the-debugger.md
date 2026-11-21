---
title: The Debugger
type: how
description: Run caf-debugger to chat with an agent beside an inspector of every step CAF takes, install agents and sign in to them, save chats and restore them in a later run, record each run, replay a recording, and move back and forth through it, with what its token is for, what a recording holds, and what "replay diverged" means.
related:
  - caf/getting-started/a-chat-in-the-browser
  - caf/getting-started/installing-and-signing-in
  - caf/getting-started/saving-and-restoring-chats
  - caf/concepts/the-core
  - caf/concepts/viewers
---

`caf-debugger` is CAF's own debugging tool. It opens a page in your browser
with two halves:

- **the chat**, on the left: CAF's reference chat view, showing the chats as
  any host's page would, through the TypeScript caf-sdk;
- **the inspector**, on the right: every **step** CAF takes (one event given
  to the core, and the directives it gave out), the core's whole state after
  each one, and each chat's numbered changes.

A timeline along the bottom moves through the steps. Because the core does
no I/O and decides everything from its events, any moment of a run can be
rebuilt by replaying the events up to it. So every bug report can be a
recording, stepped through to the moment it went wrong.

It's a tool for working on CAF, not part of CAF's API: what it shows and
how it talks to its page change without notice.

## Run it

The debugger is a Rust program, `crates/debugger` in
[CAF's repository](https://github.com/clackatoo/caf). Its page is a web app
in `ts/debugger`, which has to be built first:

```sh
cd ts
npm ci
npm run build --workspace debugger
cd ..
cargo run --release -p caf-debugger
```

Build it with `--release`: a debug build replays steps about ten times
slower, which a long run makes noticeable when moving through time (see
[Moving through time](#moving-through-time)).

It prints where it records the run, where it keeps chats, and a URL to
open:

```text
Running CAF with claude, fake.
Recording to /home/me/.local/state/caf/debugger/2026-11-20T14-03-22Z-4182.jsonl.
Keeping chats in /home/me/.local/share/caf-debugger/chats.
Open http://127.0.0.1:7320/#token=5f0c…
```

Open that URL. Ctrl-C shuts CAF down, which ends each chat cleanly and
finishes the recording; so do `SIGTERM`, and `SIGHUP` as its terminal
closes. A second one stops the debugger at once, without waiting for CAF.

`--port` picks another port; `--port 0` picks a free one. If the app isn't
built, the page says how to build it. The page and the server are built
apart, so after pulling a new CAF, build the app again: a page built for
another version of the server says so, rather than half working.

## The agents

With no `--agents`, the debugger offers:

- **`claude`**, Claude's adapter, installed by CAF when you ask (see
  [Installing and signing in](#installing-and-signing-in));
- **`fake`**, if it can be found (below): CAF's fake agent, which has no
  model behind it, so the debugger is useful with no real agent and no
  cost. It answers any message
  at once with a few words. Send it exactly `ask`, and it asks permission
  to run a command: answer it, or try Stop while it waits. Send it exactly
  `fail`, and the turn fails, the first time in each chat, so sending it
  again shows Send again working. Send it exactly `sign in`, and the turn
  fails for want of signing in, the first time in each chat.

The fake agent is built by Cargo as the debugger starts, so start the
debugger with `cargo run`. Started any other way, it offers the fake agent
only if `CAF_FAKE_AGENT_BIN` names the `caf-fake-agent` program, and says
why not otherwise.

To offer other agents, list them in a file and pass it with `--agents`.
Then those are all it offers: not `claude` or `fake`, unless the file names
them.

```toml
[[agent]]
name = "claude"
installed = true
profile = "claude-agent-acp"

[[agent]]
name = "my-claude"
program = "/home/me/.local/bin/claude-agent-acp"
args = []
profile = "claude-agent-acp"

[[agent]]
name = "mine"
program = "my-adapter"
profile = "cautious"
```

```sh
cargo run -p caf-debugger -- --agents agents.toml
```

`profile` is one of:

- `claude-agent-acp`, CAF's profile for Claude's adapter;
- `cautious`, plain ACP with nothing risky switched on, for any other
  adapter. CAF can't turn off such an agent's own tools, so it's listed,
  but a chat with it is refused;
- `fake`, a profile for CAF's fake agent, for trying installs and
  sign-ins with no real agent. Installed, its install always fails at
  once, as if offline, so nothing is ever installed. Started from
  `caf-fake-agent` (with its script as its argument), it signs in, out and
  says whether it's signed in with `caf-fake-agent auth`, which does what
  the file `CAF_FAKE_AGENT_AUTH` names says. Its sign-in link must be to
  `caf.example`.

Each agent is either:

- **`installed = true`**: CAF installs it, and starts it from its install.
  Only an agent with one of CAF's own profiles can be, since the profile
  says how, so not `cautious`. It has no `program` or `args`.
- **started by its `program`**: an absolute path, or a bare name looked up
  on `PATH` as the debugger starts, with any `args`. CAF signs in to one
  only once it has answered as the adapter its profile is for, so open a
  chat with it first.

In the chat view, "New chat" opens a chat with any of these agents in any
folder, given as an absolute path. The debugger gives chats no MCP servers,
asks you about every permission request, and starts an adapter of its own for
each chat.

## Installing and signing in

The **Agents** panel, under the list of chats, shows each agent the
debugger offers: whether it's installed, and whether it's signed in.

- **Install** installs an agent CAF installs itself, showing its size
  first and its progress as it goes; **Stop** stops it. If it can't be
  installed, the panel says why, such as a missing Node or npm.
- **Sign in** runs the agent's own sign-in, from CAF's profile for it. It
  shows only where CAF can sign in now: for an agent the debugger starts
  itself, once it has answered in a chat as the adapter its profile is
  for. It shows while signed in too, to switch accounts.
  The sign-in then shows a link to open in a new tab, a code to type there
  if the agent shows one, and a box to paste a code into, for when the
  sign-in page gives you one instead. **Stop signing in** stops it. It
  shows in the chat beside the panel, if that chat is with the agent and
  isn't closed, and the panel says "Signing in: see the chat."; otherwise
  in the panel.
- **Sign out** signs out of it.

The debugger lets its page do all of this. Installs go where every CAF
program keeps them, `~/.local/share/caf/agents/` (under `$XDG_DATA_HOME`
if that's set), so an agent installed here is installed for them all.
Claude's adapter is about 627 MB. Claude keeps its sign-in in its own
folder, `~/.claude`, so signing in here signs in Claude Code too.

A chat opened with an agent that isn't installed waits for it, and says so:
Install starts it, and Not now stops the message that waited. A chat whose
turn failed for want of signing in offers Sign in, and once you're signed
in, says so; send the message again then.

Nothing is shown in the panel while looking back or replaying: a recording
holds what CAF was told, not each agent's status.

## Saved chats

Every chat the debugger opens is saved, on the shelf `debugger`, in
`~/.local/share/caf-debugger/chats` (under `$XDG_DATA_HOME` if that's set),
or in the folder `--store` names:

```sh
cargo run -p caf-debugger -- --store /tmp/chats
```

As it starts, it checks it can write there, making the folder, private, if
it isn't there yet, and stops if it can't, saying why. Then it lists the
chats saved there, under **Saved**, below the
list of open chats: each with its title or first message, its agent and
folder, when it last changed, and how many turns it has. **Restore** brings
one back, with its record, idle until you send to it; its session then
carries on, as the fake agent resumes sessions. Closing a chat puts it
away, closed, and it stays under Saved, to restore again. A chat another
program has open, or the debugger can't read, says so, and is left as it
was.

Ctrl-C saves every chat whole before the debugger stops, so the next run
finds them as you left them. Replaying keeps no chats: nothing is saved or
listed.

## The token

The debugger starts agents in your folders, and any web page you visit can
try to reach a port on your machine. So it's locked down:

- It listens on `127.0.0.1` only, so nothing else on the network can reach
  it, and answers only requests addressed to it by that name or
  `localhost`, so a site that points its own name at your machine gets
  nothing.
- Each launch makes a new random **token**, in the URL it prints. It's in
  the part after `#`, which a browser never sends anywhere, so it stays out
  of logs. The page passes it on to the server, which refuses anything
  without it.
- The server only accepts the page's own connections: a page from any
  other site is refused, even with the token.

Treat the URL as you would a password while the debugger runs: anyone with
it can chat with your agents as you.

## Recordings

Every live run is recorded, from start to end, to a new file named for when
it started, in UTC, and the debugger's process:

```text
~/.local/state/caf/debugger/2026-11-20T14-03-22Z-4182.jsonl
```

That's under `$XDG_STATE_HOME/caf/debugger/` if `XDG_STATE_HOME` is set.
The debugger makes the folder readable only by you, each time it starts,
and CAF makes each file so too. **Save bug report**, in the top bar,
downloads the run's recording so far. If CAF can't make the file, the
debugger says so as it starts, runs unrecorded, and offers nothing to
download.

To replay a recording, pass it with `--replay`:

```sh
cargo run -p caf-debugger -- --replay 2026-11-20T14-03-22Z.jsonl
```

A replay starts nothing and writes nothing: it rebuilds the run from its
events alone. The chat view is read-only, and the timeline starts at the
end.

A recording copied while CAF was still writing it may end partway through
a line: the replay stops before it. Any other line that isn't one of CAF's
events, such as one from a newer CAF, makes the debugger refuse the file,
naming the line, since the replay would be wrong from there on.

### What a recording holds

A recording is every event CAF's core was given, in order: what you sent,
everything each agent said, and its error output, and each chat restored,
whole, as it was read. That's enough to rebuild every chat exactly, so **a recording holds the whole conversation**, and
whatever the agent read out of your files along the way. Read one through
before sharing it.

A few things are left out, and replaying doesn't need them:

- the account's email, which Claude's adapter reports in its sign-in status,
  and all of that status but how you're signed in;
- a code you pasted into a sign-in;
- each agent's arguments, and the values of its environment variables;
- the values of each MCP server's environment variables and HTTP headers;
- what you paste into a sign-in, such as a code from the sign-in page.

A sign-in's link, and any code it showed you to type, are kept: each works
once, so it's no use by the time anyone reads the recording, and a replay
needs it.

The inspector itself shows everything, those too: it shows exactly what the
core was given. Only your browser can see it, but take care with
screenshots.

The debugger keeps every step in memory for as long as it runs, so moving
back through time can reach any of them. The steps take about what the
recording does. The copies of its core it keeps along the way (see
below) take more: at most 64 of them, each the whole core as it was then,
every chat with all its turns, so up to 64 times what the chats hold at
the end. So a long live run grows; restart the debugger to start afresh.

## Moving through time

Live, the page **follows** the run: the chat view shows the chats as they
are, through the same viewer wire any host uses, and the timeline sits at
the end as steps arrive.

Drag the timeline back, or pick a step in the inspector, and the page
**looks back**: the chat view and the core's state show the moment just
after that step, and the chat view is read-only. Steps go on arriving
meanwhile. Go back to the end to follow again.

Looking back works by replay: the debugger keeps a core of its own, and
gives it the run's events up to the step you chose. Every streamed chunk
of words is a step, so a long run has tens of thousands. So the debugger
keeps copies of its core along the way, and a move starts from the
nearest copy before the step:

- for the first 64,000 steps, a copy every 1000 steps, so a move replays
  at most 1000;
- after that, still at most 64 copies: each time there would be more,
  every other one goes and the spacing doubles, so a move replays at most
  a 32nd of the run.

The chat view gets those chats the way any viewer gets a chat: whole when
moving back, and as numbered changes when moving forward. So moving
through time also checks that the chat view's SDK and the core build the
same chat.

A move should take less than 50 ms, which holds, up to 64,000 steps,
while replaying runs at 50 µs a step or quicker. A release build replays
at a few µs a step, a debug build at a few tens, so in a release build it
holds far longer: a run of a million steps moves in about 60 ms. The first
time a move is slower, the debugger logs it, once a run.

## "Replay diverged"

The core does no I/O, so given the same events it must always give out the
same directives. Live, the debugger checks this at every step, twice:

- **the steps:** a core of its own is given each event exactly as CAF's
  core was, read back from JSON, and must give out exactly what CAF's own
  core did. That's what moving through time replays.
- **the recording:** another is given each event as the recording holds
  it, with its secrets hidden, as a replay of a bug report would be. It
  must give out what CAF's core did, but for the hidden secrets
  themselves: the values of MCP servers' variables and headers, which
  reach the agent in the message that opens its session, and a code
  pasted into a sign-in, which a replay passes on as empty.

If either ever differs, the inspector shows **"replay diverged at step
N"**, saying which check, with both lists of directives. That's a bug in
CAF: something it does isn't decided by its events alone, such as a clock
read or a random number, or something that affects it isn't in its
events, or isn't in a recording of them. Save the recording and report it.

After a divergence, that check stops. If it was the steps', the
debugger's replay no longer matches what CAF did, so what it shows when
looking back at later steps may be wrong. The run itself goes on.
