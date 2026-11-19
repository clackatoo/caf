---
title: Agents and Profiles
type: concept
description: A program describes itself to CAF in plain terms, and CAF works out what to tell each agent from that agent's profile, which holds everything CAF knows about one adapter at one version.
related:
  - caf/concepts/the-core
  - caf/concepts/chats-turns-and-items
---

## The program describes itself

A program using CAF never speaks ACP. It says, in plain terms, what it can
show the user: permission questions, the agent's thinking, and so on. CAF
works out what to declare to each agent.

What a client declares can change how an adapter behaves for the whole
connection, so CAF declares whatever makes the agent behave best, and covers
any gap itself. Forms are declared even to a program that can't show them,
for example, because Claude's adapter drops its question tool otherwise. CAF
then answers any form itself.

## Profiles

A **profile** holds everything CAF knows about one adapter at one version:

- what to declare to it, and what those declarations do to it;
- which of its features have been checked;
- how to read its oddities, such as how it reports a failure;
- how to start and configure it;
- how it describes what it's doing.

The core uses profiles to translate each adapter's way of using ACP into
CAF's own terms, so nothing past the profile knows which agent it's talking
to.

When an adapter starts, it says its name and version, and CAF checks them
against the profile:

| Check | What CAF does |
|---|---|
| This version was checked | Every feature the profile lists is on |
| A newer version than any checked | The last checked profile, with its risky features off |
| A different adapter, or none named | The cautious profile |

The **cautious profile** is for agents CAF doesn't know: plain ACP, with
nothing risky switched on.

Profiles are CAF's own: a program chooses one for each agent but never
changes one. Today CAF has profiles for Claude Code's adapter,
`claude-agent-acp`, checked at 0.98.0, and the cautious one.

## Sharing an adapter

One adapter process can serve many chats. The program chooses whether each
agent's chats share one process, which saves memory, or each chat has its
own, so chats never affect each other. Either way, when a process is
restarted or lost, every chat it served is told.
