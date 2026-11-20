---
title: Agents and Profiles
type: concept
description: A program describes itself to CAF in plain terms, and CAF works out what to tell each agent from that agent's profile, which holds everything CAF knows about one adapter at one version, down to how to install it and sign in to it.
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
- how it describes what it's doing;
- its **recipe**: how to install it, how to sign in and out, and how to tell
  whether it's signed in.

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

Everything that's particular to one agent is data in its profile. Nothing
else in CAF is written for one agent, so adding an agent is adding a
profile.

## Where an agent stands

A program offers each agent in one of two ways:

- **installed by CAF** (`AgentSpec::installed`), from its profile's recipe;
- **started by the program's own command** (`AgentSpec::command`), such as
  an adapter the user installed themselves.

Either way, each agent has a status that every chat with it shares, since
they share one install and one sign-in (`Core::agents`). It says whether
the agent is installed, being installed (with progress), or can't be, and
why; and whether it's signed in, signed out, being signed in to (with the
link to open), or not known yet. CAF keeps only *how* an agent is signed
in, such as by subscription or with an API key, never *who*.

## Installing

CAF installs an agent's adapter at exactly the version its profile was
checked at. Claude's comes from npm, with a lockfile CAF ships, so every
dependency, Claude Code itself included, comes at the version and checksum
in it, and no install script runs. It needs Node 22 or later and npm on the
machine; CAF checks, and says exactly what's missing. An agent that's
downloaded instead is checked against its SHA-256 before it's unpacked.

Installs live in one folder that every CAF program on the machine shares:
`$XDG_DATA_HOME/caf/agents/`, by default `~/.local/share/caf/agents/`.
Each version has its own folder there, such as `claude-0.98.0`. An install
is built in a hidden folder and renamed into place whole, so one cut off
halfway leaves nothing that looks installed, and two programs installing at
once can't collide.

The **setup driver** does the work: the core decides, from the recipe,
what to install, and the setup driver downloads, checks and writes. It's
one of the core's [drivers](~caf/concepts/the-core), so every install is
recorded and replays like everything else.

Installing is the user's choice, as an adapter can be large (Claude's is
about 630 MB). A chat opened with an agent that isn't installed **waits for
its agent**: its messages wait with it, and its agent's status says it can
be installed, and how big it is. Then:

- **Install**: once it's done, the chat starts, and its messages go.
- **Not now**: stopping the chat stops its waiting messages, and the chat
  says the agent wasn't installed.
- **It fails**, say because the machine is offline: the waiting messages
  stop, the chat says the install failed, and the agent's status says why.
  Installing again tries again.

A CAF release that moves to a newer version installs it on next use,
asking again. A version is never removed while any program runs from it.
The one before is kept until the new one has started once, and then
removed.

## Signing in

CAF signs in to an agent with the agent's own command, as its recipe says,
never with anything the agent suggests. For Claude, that's the Claude Code
that came with its adapter, so the user needs no copy of their own, and
no terminal:

1. The program calls `Core::sign_in`, or the user chooses **Sign in** on a
   turn that failed for want of it.
2. CAF runs the sign-in command and catches the link it would open in a
   browser. The agent's status shows it, for the user to open.
3. CAF checks the link goes to one of the sites the recipe names, such as
   `claude.com`. A link anywhere else stops the sign-in, so a tampered
   package can't send the user to a fake sign-in page.
4. The user approves the sign-in in their browser, and it finishes by
   itself. If their browser is on another machine, the page gives them a
   code to paste back, and the program passes it on (`Core::paste`).

Once it's done, each chat whose last turn failed for want of signing in
says so, and its message can be sent again. CAF never sends it again by
itself.

CAF learns whether an agent is signed in from what the agent says when it
starts and as it works, from a status command where the recipe has one,
and from failures. Of what the agent says about its account, CAF reads only
how it's signed in. The rest, such as the account's email, is never kept,
logged or recorded.

Sign-ins are kept by the agents themselves, as Claude keeps its own, so
every CAF program on the machine, and the user's own Claude Code, share one
sign-in. CAF stores no credentials. Signing out runs the agent's own
command too.

An agent started by the program's own command can be signed in to the same
way, if its profile says how, once it has started and said it's the adapter
that profile is for.

## Sharing an adapter

One adapter process can serve many chats. The program chooses whether each
agent's chats share one process, which saves memory, or each chat has its
own, so chats never affect each other. Either way, when a process is
restarted or lost, every chat it served is told.
