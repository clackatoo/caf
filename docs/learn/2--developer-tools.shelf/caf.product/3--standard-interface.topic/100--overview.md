---
title: The Standard Interface
type: concept
description: How a chat with an agent should look and behave in any app built on CAF, so a user moving between CAF apps finds the same thing. What CAF SI standardises and what it leaves to the app, how firm each part is, the colour roles, the words, and how to use these pages, or point an agent at them, when building a UI.
related:
  - caf/standard-interface/composer
  - caf/standard-interface/transcript
  - caf/standard-interface/agents
  - caf/standard-interface/chats
  - caf/standard-interface/usage
  - caf/concepts/viewers
---

CAF SI, the Standard Interface, is how a chat with an agent should look and
behave in any app built on CAF. CAF itself draws nothing: an app draws its
chats from what CAF tells its viewers. These pages say what to draw, so every
CAF app draws the same thing, and nobody has to invent it.

They're written to be followed exactly, by a person or by an agent building a
UI. To have an agent build a chat UI on CAF, point it at this section: every
page here is also published as plain Markdown, and the section as a whole is
one Markdown page too, which the site's `llms.txt` lists.

## What it covers

**CAF SI standardises a single chat, and says nothing about how an app
arranges its chats.**

The test for any part is whether a user moving between two CAF apps would be
confused or misled if it differed. If they would, it's standard. Everything
inside a chat passes that test: what the agent is doing, what's waiting on
the user, what went wrong and what to do about it.

How an app finds and arranges its chats doesn't. Every app already has its
own way of navigating its things: a sidebar, tabs, clackatoo's panes and
workspaces. Guidance there would only push an app into growing a second way,
which is exactly the awkward integration CAF SI exists to prevent.

## How firm each part is

Every part on these pages is marked with how firm it is:

- **Required.** Everything inside a chat; how a chat is identified wherever
  it appears; the words and actions for what happens to a chat; the colour
  roles. Copy these.
- **Suggested.** Where the app should choose, such as how the usage page is
  laid out. Take the way shown, or another the page names; don't invent one.
- **Up to the app.** The app's own frame: where chats are listed, panes,
  windows, workspaces, projects (CAF's shelves), search, keyboard shortcuts
  beyond the composer's, empty lists. Use what the app already has.
- **Later.** Drawn for something CAF doesn't provide yet, so the design is
  ready when it does. Don't build it until CAF does.

Nothing is enforced: an app that ignores CAF SI still works. But an app that
follows it looks and behaves like every other CAF app, which is the point.

## The pages

- [The composer](~caf/standard-interface/composer): where the user writes.
  Its settings menu, the context ring, waiting messages, and where questions
  and the agent banner attach.
- [The transcript](~caf/standard-interface/transcript): turns as a summary
  line that opens into a timeline; failures and their remedies, questions,
  CAF's notes, a chat that lost its agent, and the agent's own history.
- [Agents](~caf/standard-interface/agents): installing and signing in; the
  agent banner and the agents panel.
- [A chat's identity and lifecycle](~caf/standard-interface/chats): how a
  chat is named and marked wherever it appears; restoring, archiving,
  deleting and starting chats.
- [Usage](~caf/standard-interface/usage): context, cost and account limits in
  the chat, and the usage page.

Each page links to its mock-up, live: an HTML page drawing every state, in
dark and light, which you can click through. The pictures on these pages are
taken from those mock-ups, in the dark theme. **Where the words on a page
and a picture differ, the words are right.**

## Colour roles

**Required.** The mock-ups use clackatoo's colours. An app may use its own
palette, but keeps these roles, and uses each colour only for its role:

| Role | Means | Used for |
| --- | --- | --- |
| Accent | Act here, or happening now. | For example: the lit Send button, the caret, primary buttons, what's chosen (a tick's row, a lit switch, the slider, a filter's mark, a favourite's star, the focused search box), the edge of something waiting on the user (a question, the agent banner), and what's running now (a spinner, the *Working* mark). Kept to those kinds of thing, so it still means "here". |
| Warning | It went wrong, or can't go on. | Failed turns and steps; refusals; restores that can't happen; a limit that's out; context past 95%; an action that can't be undone, such as Delete. |
| Caution | Held, getting close, or needs the user soon. | A setting change waiting for the turn to end; context or limits filling up; a chat that lost its agent; a note that the agent remembers nothing; extra usage in use; the mark on an agent that still needs installing or signing in, in a list or the agents panel (the agent banner itself, waiting on the user, has its own tone). |
| Link | Something to follow. | Links that open a page. Never something to act on: *Copy link* and *Learn them now* are buttons. |
| CAF's voice | CAF is speaking. | The CAF badge on what CAF says in the transcript (its notes, failures, and questions it asks itself), so it never reads as the agent. A box that says what to do next, such as a lost chat's, is the app's own frame and carries none. |
| Muted | Matters less. | Times, counts, the agent's thinking, steps once they're done, the agent's own history. |

Anything else is **calm**: the ordinary text colour, or muted, with no role.
Buttons come in three looks: **primary**, filled with the accent, for the
one thing to do; **plain**, raised but with no role colour; and **quiet**,
text only, in the muted colour, for a secondary action or a way out, such
as *Not now*.
An agent's or an app's own colour, such as Claude's orange, is identity, not
a role: it appears only in their marks and to tell them apart in charts, and
never on text, controls or edges.

clackatoo's values, which the mock-ups use:

| Role | Dark | Light |
| --- | --- | --- |
| Accent | `#d9b24a` | `#d9b24a`, and `#8a6a12` for text |
| Warning | `#e06c75` | `#c0404b` |
| Caution | `#e89a3c` | `#b0660f` |
| Link | `#7cb7f5` | `#2d6db5` |
| CAF's voice | `#6fd3c9` | `#1f9488` |
| Muted | `#8a9099` | `#666c75` |

## Words

**Required.**

- **Users never see CAF's own words:** *shelf*, *loaded*, *put away*,
  *session*, *adapter*. Use the app's words, or none.
- **CAF's *close* is shown as *Archive*.** Closing a tab or a pane never
  archives a chat: see [A chat's identity and lifecycle](~caf/standard-interface/chats)
  for what it does.
- **CAF speaks in its own voice**: its notes, failures and questions are
  marked as CAF, and the agent's words are quoted as the agent's. Neither is
  ever presented as the other.
