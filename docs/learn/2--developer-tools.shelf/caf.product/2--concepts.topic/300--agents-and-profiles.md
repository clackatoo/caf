---
title: Agents and Profiles
type: concept
description: A program describes itself to CAF in plain terms, and CAF works out what to tell each agent from that agent's profile, which holds everything CAF knows about one adapter at one version, down to how to turn off the agent's own tools, install it and sign in to it.
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
- how to turn off its own tools;
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
| A newer version than any checked | The last checked profile, with its risky features off, and its tools turned off the same way |
| A different adapter, or none named | The cautious profile, and no session is opened |

The **cautious profile** is for agents CAF doesn't know: plain ACP, with
nothing risky switched on. CAF can't turn off such an agent's own tools,
so it never runs one: see below.

Profiles are CAF's own: a program chooses one for each agent but never
changes one. Today CAF has profiles for Claude Code's adapter,
`claude-agent-acp`, checked at 0.98.0, and the cautious one.

Everything that's particular to one agent is data in its profile. Nothing
else in CAF is written for one agent, so adding an agent is adding a
profile.

## An agent has only your tools

Every agent runs with **none of its own tools**. Claude Code, for example,
can usually read and edit files, run commands and search the web, and it
loads the user's own setup: their MCP servers, skills, plugins, hooks,
slash commands, and instruction files such as `CLAUDE.md` and `AGENTS.md`.
CAF turns all of that off, as the agent's profile says how. The only tools
the agent has are the ones your program gives it, as MCP servers
(`OpenChat::mcp_servers`).

Each of the agent's own tools will come back, one at a time, once CAF
supports it fully: what it does, in CAF's own terms, the permission it
needs, and how it shows in the chat. CAF serves its own tools to the agent
as an MCP server named `caf`, so no server of yours may have that name, in
any case, nor one the agent would write as `caf`, or as `caf_` or
anything starting `caf__`: Claude Code writes each character but a
letter, a digit, `_` and `-` as `_`, so `caf.` and `caf__x` would name
tools as CAF's do (`caf.x`, written `caf_x`, is fine). Opening or restoring a chat with
one is refused (`Refused::ReservedServer`). Nor may two of your servers have
names the agent would write alike, such as `my tools` and `my.tools`, both
`my_tools`: a chat with them is refused (`Refused::SameServerName`), as
the agent couldn't tell their tools apart. The agent never asks permission for CAF's own
tools; CAF asks your policy, and the user, itself.

Each session starts in the agent's ordinary permission mode, in which it
asks before using a tool not yet allowed. Claude's adapter would start it
in the user's own default mode, from their `~/.claude/settings.json`, such
as plan mode, in which no tool runs; CAF sets it back to `default` as the
session opens, before anything is sent, unless the session says it's in
`default` already. If the agent won't, CAF logs a warning and carries on:
it still asks about its own tools itself, but your MCP servers' tools may
then run without the agent asking.

In place of the agent's built-in instructions, which describe tools it no
longer has, CAF gives it short ones of its own. For a program called
`framewright` they say:

> You are working inside framewright, the app the user is talking to you
> in. You have no tools of your own: you can't read or change files, run
> commands or browse the web, except through tools framewright gives you.
> Answer in Markdown.

Your program can add its own after them, such as a description of the
project open, with `OpenChat::instructions`. They're sent each time the
chat's session is opened, so they're kept to 64 KiB. They aren't saved:
give them again when you restore the chat (`RestoreChat::instructions`).

**CAF never runs an agent it can't strip.** An agent with the cautious
profile is refused (`Refused::OwnTools`): opening a chat with it fails, as
does sending a message to a restored chat with it. Its status says so
beforehand: `AgentStatus::own_tools_off` is false. An agent whose adapter
turns out not to be the one its profile is for falls to the cautious
profile, so its chats get no session: they're lost, as CAF can't turn off
its tools, and their waiting messages fail. From then on, in that run, its
status says `own_tools_off` is false too, until an adapter of it answers
as the one its profile is for. A status read from an older CAF, which
didn't turn tools off, says false.

For Claude, a few things stay as they are: settings an organisation
manages on the machine, the plugins built into Claude Code, and the
adapter's own `/mcp` command, which the user can send as a message (the
agent can't call it) to see the program's MCP servers and reconnect them.
The adapter offers `/remote-control` too, but it fails: CAF turns Remote
Control off, so a chat is never reachable from claude.ai. The user's sign-in is
untouched, but one that lives in their own Claude Code settings, such as
an API key helper, isn't read, so the chat asks them to sign in.

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
Each version has its own folder there, named after the agent's profile,
never after what a program calls the agent, such as
`claude-agent-acp-0.98.0`. So every program that offers Claude shares one
install of it, whatever it calls it. An install
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
asking again. Older versions are removed once the new one has started,
but only those no program has started an agent from for 14 days: another
program on the machine may pin an older version, and still use it. A
version is never removed while any program runs from it, and a program
never removes a version newer than its own.

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
   `claude.com`. A link anywhere else stops the sign-in, so CAF never
   shows the user a link to a site the agent's profile doesn't name.
4. The user approves the sign-in in their browser, and it finishes by
   itself. If their browser is on another machine, the page gives them a
   code to paste back, and the program passes it on (`Core::paste`).

Once it's done, each chat whose last turn failed for want of signing in
says so, and its message can be sent again. CAF never sends it again by
itself.

While a sign-in to an agent that was signed out is under way, new chats
with it **wait for their agent**, as they do for an install, since
starting it would only fail: its status shows the sign-in, with its link.
A sign-in nobody finishes is stopped after a while (10 minutes, unless the
program sets otherwise), and the chats waiting on it go ahead.

Agents CAF installs from one profile share one sign-in too, whatever
the program calls them: signing in to one signs in to all of them.

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
that profile is for. The exception is an agent that keeps its sign-in in a
private folder CAF gives it: only CAF's own install of it is given one, so
CAF doesn't run commands that need it for the program's own.

## Settings

The user can choose an agent's model, how hard it thinks (its effort) and
fast mode, for any agent. The agent says what it offers, never CAF: once a
session opens, the agent's profile reads its list from however the agent
gives it (ACP's session config options, or an older agent's list of
models) into one shape, and a chat holds it beside what was chosen. What
the agent offers that CAF doesn't support yet, such as Claude's modes,
stays as the agent set it.

A chat's choices are sent before any message, one at a time and the model
first, as which effort levels there are depends on it; a change made while
a turn runs waits for it to end. A message never goes on a setting nobody
chose without saying so: a choice made for the chat itself that can't
be applied fails the messages waiting, and the chat says so; one that
stops being offered, or that the agent changes by itself, is said in the
chat too. The agent's status
says what it offers before any chat, once CAF has learned it
(`Core::learn_settings`). See
[Choosing the model](~caf/getting-started/choosing-the-model).

## Sharing an adapter

One adapter process can serve many chats. The program chooses whether each
agent's chats share one process, which saves memory, or each chat has its
own, so chats never affect each other. Either way, when a process is
restarted or lost, every chat it served is told.
