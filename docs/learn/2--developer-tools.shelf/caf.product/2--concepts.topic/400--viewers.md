---
title: Viewers
type: concept
description: A viewer is anything that shows chats, such as a window or a web page. It speaks to CAF in a small versioned set of messages, follows each chat as a picture and then changes, and asks the host, never CAF, to open a chat.
related:
  - caf/concepts/chats-turns-and-items
  - caf/concepts/the-core
---

A **viewer** is anything that shows chats: clackatoo's window, a web page
using the TypeScript caf-sdk, or CAF's own debugger. It may be in another
process, or another language, from the program running CAF, so it talks to
CAF in messages: the **viewer wire**, in `caf_core::viewer`.

The wire is only the messages. It does no I/O, so every transport (a
WebSocket, a socket, a pipe) carries the same ones, and the program running
CAF chooses how. Rust viewers such as clackatoo's window send them as
MessagePack with field names; TypeScript sends them as JSON.

## A link, in order

1. The viewer says **Hello**, with the oldest and newest versions of the
   wire it speaks. Anything else first ends the link.
2. CAF answers **Welcome**, with the version both will speak, the agents a
   viewer may open chats with, and the open chats. If there's no version in
   common, it says so and ends the link.
3. The viewer **watches** chats. For each, it gets the chat whole, then its
   changes as they happen.
4. It may act on them: send a message, stop a turn, answer a question, close
   a chat, or ask to open one. Each request carries a number the viewer
   chose, and CAF quotes it back with how it went: done, refused with the
   core's reason, or not allowed by the host.

## Pictures and changes

Each chat keeps a numbered list of small changes (see
[Following a chat](~caf/concepts/chats-turns-and-items#following-a-chat)).
A viewer following a chat keeps a copy of it, and the number of the last
change it has.

- A **picture** is the chat whole, with the number of the last change it
  includes. A new viewer gets one first. So does one that fell too far
  behind, or reconnected after CAF dropped the changes it missed. A picture
  may come at any time, and replaces the copy.
- **Changes** come after it, oldest first, each naming what it changes by
  id: a turn added, words appended, a tool finished, a question asked or
  gone.

A viewer that reconnects can say which change it has, and gets only what
followed, if CAF still has it. Otherwise it gets a picture.

### Applying a change

`Chat::apply` takes one change and updates a chat with it. It's the only way
a chat's record changes: the core builds its own record with it, and every
viewer's copy is built with it, or with its equal in another language. So
they can't drift apart.

A change that names something the chat doesn't have, such as words for an
item it never saw, doesn't fit. The chat is left as it was. In a viewer that
means it missed something, and needs a fresh picture. Inside the core it's a
bug, reported as one.

`ChatCopy` is a viewer's copy of one chat. It skips changes it already has,
and asks for a picture when a change is missing or doesn't fit, keeping what
it applied before then.

Every SDK's `apply` must turn the same changes into the same chat. The
**conformance fixtures** in `fixtures/apply/` check it: each is one chat from
CAF's tests or recorded sessions, as the core saw it before its first change,
then each change and the chat after it.

## Versions

The two ends speak the older of their newest versions, and what a version
sends never changes. A later version may add messages, kinds of change,
reasons and fields, so a viewer skips what it doesn't know:

- a message it doesn't know, it ignores;
- a kind of change it doesn't know, it can't apply, so it asks for a fresh
  picture;
- a reason it doesn't know, such as why a chat lost its session, it shows as
  unknown, with its name;
- a field it doesn't know, it doesn't read. A field added later is always one
  an older viewer can do without.

`samples/viewer/v1.json` holds one of every message of version 1, and a test
fails if what CAF writes no longer matches it.

Every number on the wire is below 2^53, so JavaScript reads it exactly: CAF's
ids count up from 1, and times are milliseconds.

## Opening is the host's decision

A viewer can't open a chat by itself. It asks, with an agent CAF offered, a
folder, and a new session or one to continue, and the **host**, the program
running CAF, decides.

That's because opening a chat starts programs. The full request to open a
chat includes the chat's MCP servers, the tools the agent is given, and each
of those is a command to run on the machine. A viewer may be a web page, and
a web page that could send one could run anything. So the host turns a
viewer's request into a chat with the tools it chooses, or refuses it.
