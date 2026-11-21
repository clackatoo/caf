---
title: What Is CAF
type: concept
description: CAF, the Clackatoo Agent Framework, is a Rust library for building chats with coding agents such as Claude Code into your own program, robustly, without learning each agent's quirks.
related:
  - caf/concepts/the-core
  - caf/concepts/viewers
  - caf/getting-started/the-debugger
  - caf/concepts/chats-turns-and-items
  - caf/concepts/agents-and-profiles
  - caf/getting-started/saving-and-restoring-chats
---

**CAF**, the Clackatoo Agent Framework, is a Rust library for putting chats
with coding agents, such as Claude Code and Codex, into your own program. It
grew out of the agent backend of
[clackatoo](https://github.com/clackatoo/clackatoo), and clackatoo is its
first user.

## What it does for you

Coding agents speak the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP), through an
*adapter*: a small program that turns the agent into an ACP server. ACP says
what messages exist, but not how each adapter really behaves. Adapters
differ, and what you declare to one can change how it behaves for the whole
connection. Getting a chat right means knowing all of that.

CAF holds that knowledge so your program doesn't have to:

- **It talks to the agent.** It starts the adapter, opens sessions, sends
  messages, follows the reply as it streams in, and stops a turn when asked.
- **It keeps its own record of every chat**, in one format for every agent,
  so your program draws a chat the same way whichever agent it's with.
- **It absorbs each agent's quirks** in a profile per adapter, so nothing
  past that profile knows which agent it's talking to.
- **It survives things going wrong**: an adapter that dies, hangs, floods
  its output, or never answers. Every request it sends has an end, and every
  request the agent sends is answered exactly once.

Your program describes what it can show, in plain terms, and draws the chat
from CAF's record. CAF itself draws nothing.

## How it's built

CAF's heart is [the core](~caf/concepts/the-core), which holds every chat's
state and the rules that change it, and does no I/O at all. Small drivers do
the I/O around it: starting adapter programs, keeping time, asking whether
the agent may do something, showing chats. `caf-driver-tokio` has drivers
for programs built on tokio; a program with its own event loop, such as
clackatoo, writes its own.

## What exists today

CAF is being built a feature at a time, each feature designed in full before
it's built. The first, *talking to one agent*, covers starting an adapter,
opening a session or continuing one, sending messages, seeing the reply
stream in, stopping a turn, and surviving the adapter dying or hanging. It
supports Claude Code, through its adapter `claude-agent-acp`.

The second, *the reference app and inspector*, puts chats in other places:

- **[Viewers](~caf/concepts/viewers)**: a small, versioned set of messages
  that any window or web page can use to follow chats and act on them.
  `caf_driver_tokio::viewer::serve` is CAF's side of it.
- **A TypeScript caf-sdk**, `@clackatoo/caf`, for
  [a chat in the browser](~caf/getting-started/a-chat-in-the-browser).
  Its types are generated from CAF's own, and shared test cases hold it to
  exactly what the core does.
- **[The debugger](~caf/getting-started/the-debugger)**, `caf-debugger`: a
  chat view to copy, beside an inspector of every step CAF takes. It records
  every run, replays recordings, and moves back and forth through them.

The third, *installing and signing in to agents*, means a program no longer
has to find an agent itself:

- **[Installing](~caf/getting-started/installing-and-signing-in)**: CAF
  installs each agent at the version it has checked, in one folder every
  CAF program shares, asking the user first. Old versions go once nothing
  has used them for two weeks.
- **Signing in**: CAF runs each agent's own sign-in and shows its link, so a
  web page can sign the user in without a terminal. It knows whether each
  agent is signed in, and how, but never who.
- **Built for any agent**: everything about one agent is data in its
  [profile](~caf/concepts/agents-and-profiles), so adding an agent is
  adding a profile.

The fourth, *saving and restoring chats*, lets a chat outlive the program
that opened it:

- **[Saving](~caf/getting-started/saving-and-restoring-chats)**: CAF saves
  each chat as it goes, whole, with the agent's session and your own data
  on it, on shelves your program names, in files or a store of your own.
- **Restoring**: a chat comes back with its record, tidied of whatever was
  under way, and carries on the agent's session when a message is sent.
  Chats can be put away, closed, deleted and moved, from your program or a
  viewer.
- **Readable forever**: a save from any version of CAF stays readable by
  every later one, and an older CAF never writes over a newer one's.
