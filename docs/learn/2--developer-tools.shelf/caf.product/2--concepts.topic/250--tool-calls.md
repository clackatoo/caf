---
title: Tool Calls
type: concept
description: What CAF records of each tool call, whoever ran it. Where a call came from, what CAF reads it as doing, the pieces it gave back (text, diffs, a command's output in colour, images and links), how much of each is kept, output that grows while it runs, who allowed it, and why CAF runs the tools that matter itself.
related:
  - caf/concepts/chats-turns-and-items
  - caf/concepts/agents-and-profiles
  - caf/concepts/viewers
  - caf/standard-interface/transcript
---

A tool call is one of the [items](~caf/concepts/chats-turns-and-items) of a
turn: the agent reading a file, changing one, running a command, searching
the web. CAF records every call the same way, whichever agent made it and
whoever ran the tool, so an app draws one kind of thing (a `ToolCall`) for
all of them.

## CAF runs the tools that matter

An agent comes with tools of its own for reading and changing files and
running commands, and each reports what they did in its own way, often as
little more than a line of text. CAF turns those off, and gives the agent
its own instead, over MCP: **Read**, **Edit**, **Write** and **Bash**. An
agent whose adapter won't let CAF turn its own off isn't run at all.

Since CAF runs them itself, it knows exactly what they did, for every
agent alike: which file was read, the diff an edit made, a command's output
in colour as it ran, and how it exited. It asks the app's policy, and the
user if need be, before anything runs, and it writes such a call's title
itself, from what the call does: the agent's would be a name like
`mcp__caf__Read`. A read, an edit or a write is *Read src/main.rs*,
*Edit src/main.rs* or *Write src/main.rs*, with the path from the chat's
folder when the file is inside it, and whole otherwise. A command is its
first line, followed by *…* if it has more.

For an edit or a write, CAF works out the change before it asks anyone,
and puts its diff on the call, so whoever is asked sees exactly what they
allow. A refused change keeps its diff, and never happens.

The agent keeps the tools CAF doesn't replace, such as Claude's web search,
and reports them as before. CAF records those too, from what the agent
says.

## Where a call came from

A call's `origin` says whose tool it is:

| `Origin` | The tool |
| --- | --- |
| `Caf` | One of CAF's own: Read, Edit, Write or Bash. |
| `App` | One of the app's own tools, which CAF serves for it. *Later.* |
| `Server` | A tool of an MCP server the app gave the chat, by the server's name and the tool's. |
| `Agent` | The agent's own, by the agent's name for it, such as `WebSearch`. |
| `NotRecorded` | Saved before CAF kept where calls came from. |

The origin is never worked out from the call's title, nor from anything
else the model wrote. A call to a tool CAF serves gets its origin from the
connection it arrived on, which belongs to one chat. Any other gets its
origin from what the agent's adapter says, such as Claude's
`_meta.claudeCode.toolName`.

**One call, one item.** A call to a tool CAF serves arrives twice: the
agent reports that it's calling it, and the call itself reaches CAF's tool
server. Whichever comes first makes the item, and the other joins it, so
the chat has one item per call however many reports there were. For such a
call, CAF's record is the truth: it ran it. The agent's report adds only
its id for the call, and when it started.

## What it does, and what it was given

Once CAF has a call's whole input, it reads what the call does, its
`action`, and never changes it after:

| `Action` | Does |
| --- | --- |
| `Read` | Reads a file, and which lines, if not all. |
| `Edit` | Changes part of a file. |
| `Write` | Writes a whole file, new or over what was there. |
| `Run` | Runs a command, in a folder, and whether in the background. |
| `Tool` | Anything else, by its name: its input says the rest. |

Paths in an action are absolute, worked out against the chat's folder
before anything runs, so an action names exactly the file, whatever `..`
the model wrote. The action also gives a call its `kind` (`ToolKind`),
which an app uses for its icon and to count steps: Read is `Read`, Edit and
Write are `Edit`, Run is `Execute`. A call CAF didn't run has the kind the
agent gave it.

A call also keeps its `input`, as the model wrote it, and its `locations`:
the files, and lines in them, it's about, for an app to open.

## What it gave back

A call's `result` is a list of pieces, in order:

| `Piece` | What it is |
| --- | --- |
| `Text` | Plain text, such as a file read. |
| `Diff` | A file's change, as hunks: runs of changed lines, with three lines either side as they were, as `git diff` shows them. Each line has its number before and after, and the diff counts the lines it adds and removes. |
| `Output` | A command's output, as its terminal showed it: its lines, with their colours, and how it ended (an exit code or a signal), or that it ended without saying how. |
| `Image` | A picture, by its type, such as `image/png`. |
| `Link` | A file or web address the call points to, and its name. |
| `Unknown` | A piece CAF can't read yet, kept as it came. |

CAF works a diff's hunks out itself, from the file before and after,
whether CAF made the change or the agent sent both texts. An app draws them
as they are, and never compares texts.

A command's output keeps its colours, as the terminal's codes for them
(SGR), and nothing else a terminal would act on: CAF works out the
cursor's movements before it keeps the output, so a progress bar is its
latest line, not every redraw of it. An app turns the colour codes into
colour, and shows anything else as text, never acting on it.

What's kept is what's useful to the user: a diff, a command's output in
colour. What the model is answered is what's useful to it, and each of
CAF's tools says what that is. For a tool CAF runs, both come from the same
run.

### Caps

Everything in a call is bounded, so one call can't fill a chat's record.
Anything cut says how much of it was kept, of how much (a `Cut`, in bytes):

| What | Most kept | Which part |
| --- | --- | --- |
| `Text` | 64 KB | its start |
| `Output` | 64 KB, each line's ending counting as a byte | its end, in whole lines of at most 16 KB |
| `Diff` | 256 KB of its lines | whole hunks, then how many more there were (`more`) |
| `Image` | 2 MB | all of it, or none, with its size |
| `locations` | 256 | the first, then how many more (`locations_cut`) |
| `input` | 64 KB of its JSON | its start, as a string (`input_cut`) |

An output keeps its last lines, as a terminal's scrollback does, and says
how many before them weren't kept (`dropped`). An image too big to keep
has no data, only its type and size, so an app can say it was too big.

### Output while it runs

A command's output grows while it runs. A call has at most one `Output`
piece, and while the command runs, `Change::OutputChanged` changes it in
place: the lines from one on are replaced by new ones (the last line may
still be being written, so it can change), then lines dropped from the top
to keep under the cap. Its `ending` is empty until the command ends. So a
viewer sees a build or a test run as it goes, sent only what changed.

## Where it is

A call's `status`:

| `ToolStatus` | Means |
| --- | --- |
| `Pending` | Not started yet: reported, or waiting for the policy. |
| `Asking` | Waiting for the user: its permission question is open. |
| `Running` | Running. |
| `Done` | Finished. |
| `Failed` | Went wrong: why is a `Text` piece, or its turn failed under it. |
| `Refused` | Not allowed, by the policy or the user. It never ran. |
| `Cancelled` | Stopped: the user stopped its turn, or the agent cancelled it. |

Every call ends Done, Failed, Refused or Cancelled. When its turn ends, a
call still Pending, Asking or Running is Cancelled if the user stopped the
turn, and Failed otherwise, and so is one a restored chat was cut off in.

## Who allowed it

A call's `decision` says whether it was allowed, and by whom:

| `Decision` | Means |
| --- | --- |
| `Undecided` | Not decided yet. |
| `NotAsked` | It needed no permission, such as a call the agent made without asking. |
| `Allowed` | Allowed, by the policy, with its reason if it gave one, or by the user, with the option they chose. |
| `Refused` | Refused, likewise. |
| `NotRecorded` | Saved before CAF kept who decided. |

When the user is asked, the question is on the chat (`Chat.questions`), and
names the call it's about (`Permission.tool`), so an app can show the
command or the change it asks about beside it. The agent may ask before CAF
has seen the call, so the question may name it only later.

## Drawing them

[The transcript](~caf/standard-interface/transcript) in CAF's Standard
Interface says how an app draws every part of a tool call: its status in
the colour roles, who decided, each kind of piece, and a question showing
what it asks about. The debugger draws them all.
