---
title: The Core
type: concept
description: CAF's core holds every chat's state and rules and does no I/O. Drivers report what happened as events, and the core answers with directives, so the same events always give the same result.
related:
  - caf/getting-started/what-is-caf
  - caf/concepts/chats-turns-and-items
  - caf/concepts/agents-and-profiles
---

CAF's **core**, the `caf-core` crate, holds the state of every chat and the
rules that change it. It does no I/O at all: it never reads a clock, starts a
process, sets a timer, touches a file, or decides whether the agent may do
something. Clippy checks that rule on every build.

## Events in, directives out

Everything that happens reaches the core as an **event**, with the time it
happened. Everything the core wants done leaves it as a **directive**:

```text
driver ──event──▶ core (rules, reading and updating state) ──directives──▶ drivers
```

Both lists are closed. Drivers report only the events the core knows, and the
core gives only the directives drivers know. One event can lead to no
directives or to several, for any driver.

The host's own requests, such as opening a chat or sending a message, are
events too. A program calls methods like `Core::send`, which record them, so
a recording of a session holds everything that happened in it.

## Drivers

Drivers are small, and each does one kind of job:

| Driver | Its job |
|---|---|
| Agent | Starts adapter programs and carries their bytes, knowing nothing of which agent it runs |
| Timer | Wakes the core at the one time it asked for |
| Policy | Answers "may the agent do this?" |
| Viewer | Tells the program's views that a chat changed |
| Storage | Saves and loads chats (a later feature) |
| Setup | Installs agents and runs their sign-in commands, as each one's [recipe](~caf/concepts/agents-and-profiles#installing) says |

Drivers never hear each other: only the core listens to them, and only the
core gives them work. A directive whose result the core waits for has a
**ticket**, and the result quotes it. Tickets are never reused, so a result
that arrives after the core stopped waiting is simply ignored. Each adapter
process is known by the ticket of the directive that started it, so output
from a process the core has replaced can't be mistaken for the new one's.

## Why no I/O

Given the same events, the core always gives the same directives and ends in
the same state. So:

- **Any session can be replayed as a test.** A bug becomes a recording of
  events, and its fix a check on what comes out.
- **It fits any program.** clackatoo runs it on its own event loop, under
  strict rules about never stalling; a tokio program uses the drivers in
  `caf-driver-tokio`.
- **Nothing can quietly block it.** Slow work happens in drivers, and its
  result comes back as one more event.

## What the core promises

1. Every request the agent sends is answered, exactly once.
2. Every request CAF sends ends. Every one has a deadline except a prompt,
   which may run as long as the agent works; once the user stops it, it has
   one too.
3. Nothing the agent sends is silently dropped.
4. Every event names what it belongs to, so a late update lands on its own
   row.
5. A result for a ticket the core no longer waits on is ignored.
6. A question is answered once, and only while it still stands.
7. Nothing an agent replays from a session's history acts.
8. When an adapter is restarted or lost, every chat it served is told.
9. The agent's words are never passed off as CAF's.
