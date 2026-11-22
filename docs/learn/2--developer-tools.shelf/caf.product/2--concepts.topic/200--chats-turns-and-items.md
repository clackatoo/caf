---
title: Chats, Turns, and Items
type: concept
description: CAF keeps its own record of every chat, in one format for every agent. A chat is a list of turns, a turn is a list of items, and views follow a chat through its numbered changes.
related:
  - caf/concepts/the-core
  - caf/concepts/agents-and-profiles
---

CAF keeps its own record of every chat, in one format for every agent. The
agent keeps its own transcript too, as its memory; CAF's record is what
happened and what the user saw. Each adapter's
[profile](~caf/concepts/agents-and-profiles) translates what it sends into
this format as it arrives.

## Chats, turns, and items

A **chat** is the user's conversation with an agent. It's a list of
**turns**, and each turn is a list of **items**.

A turn records what started it (the user, the agent itself, another chat's
agent, or the program) and where it is in its life:

| Life | Meaning |
|---|---|
| Waiting | Not sent yet: the chat is opening, or another turn is running |
| Running | The agent is working on it |
| Stopping | Stop was asked for, and the agent hasn't finished stopping |
| Finished | The agent finished |
| Stopped | The user stopped it |
| Failed | It went wrong, with the reason and what the user can do |
| Cut off | The program restarted while it ran |

An item is one thing the agent did: its words, its thinking, a tool it used,
a summary of its memory to make room, a note from CAF, the program's own kind
of item, or something CAF couldn't read, kept as it came so nothing is lost.

Every turn and item has a permanent id, never a position, and a window of
time: when CAF first heard of it, and when it ended. The times are when CAF
heard things, since ACP carries none.

Questions waiting for an answer, such as the agent asking permission to run
something, aren't items: they're a list on the chat, because they come and
go.

## What a turn spent

When a turn ends, finished or stopped, the agent's answer says what it
spent, and CAF records it on the turn: its tokens by kind (input, cache
read and written, output, and how much of the output was thinking), one row
per model, and its cost if the agent reports one. Claude's adapter splits
the tokens by model, counting the subagents and compaction the turn set off
too; an agent that doesn't gets one row, against the model the turn was
sent on. A turn's cost is how much the agent says its session's cost rose
over it. An agent that says nothing gives no record.

The chat adds up its turns' records as what it has **spent**: tokens,
the cost the agent reported, and an estimate, from a table of prices, for
turns it didn't report. Estimates are never stored, so when the prices
change, every chat's figure is worked out again; a chat restored from its
save works its figure out again from its turns too. Each turn's record also
goes to a ledger every CAF program on the machine shares, so usage can be
summed across them.

## The program's own items

A program can add items of its own kind, such as eyespy's "the agent moved
the map here". Only the program adds them, never the agent. CAF keeps them in
order and saves them with the chat without looking inside. Each gives a
one-line summary for anything that doesn't know its type.

## Following a chat

Each chat keeps a numbered list of small changes, each naming what it changes
by id: a turn added, words appended to an item, a tool finished, a question
asked. A view remembers the number it has read up to and asks for what
follows. A view that's new, or has fallen too far behind, gets the whole chat
first and then the changes after it.

How a view outside the program follows a chat, and why every one ends with
the same chat, is in [Viewers](~caf/concepts/viewers).
