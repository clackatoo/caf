---
title: CAF's Own Tools
type: concept
description: What CAF's own Read, Edit, Write, Bash and TaskStop do, as the model sees them and as the user does. Reading before changing and what counts as stale, encodings and line endings, Bash's folder, timeout and background, the files a long output goes to, background jobs and CAF's notice of each one's end, and giving CAF your editor's buffers and your terminals in place of the disk and its own pty.
related:
  - caf/concepts/tool-calls
  - caf/concepts/serving-tools
  - caf/standard-interface/transcript
---

CAF gives an agent its own tools in place of the agent's: **Read**,
**Edit**, **Write** and **Bash**, and **TaskStop** with Bash. How they
reach the agent is [Serving tools](~caf/concepts/serving-tools); what CAF
records of each call is [Tool calls](~caf/concepts/tool-calls). This page
is what the tools themselves do.

The model was trained on Claude Code's tools, so CAF's take the same
inputs, behave the same way and answer in the same words, Claude Code's
own, wherever CAF's design doesn't say otherwise. Where it does, this page
says so.

Give a chat all of them with `Tools::caf(CafTool::ALL)`, or some:

```rust
let open = OpenChat::new("claude", folder).with_tools(Tools::caf(CafTool::ALL));
```

A chat given Bash gets TaskStop too, as Bash's background jobs need it.

## Read

`{file_path, offset?, limit?}`, and Claude's `allow_large` and `pages`,
which are taken. Its answer is the file's lines, numbered from 1, each as
`{n}\t{line}`.

- **How much:** 2,000 lines unless `limit` says. A read of more than about
  25,000 tokens (a quarter of its bytes) is refused, in Claude Code's
  words, telling the model to read part of it; so is a read of a whole
  file bigger than 256 KiB with no `limit`. `allow_large` raises the
  tokens to 100,000.
- **Lines** are split at `\n`. A `\r` before it is dropped, so the model
  never sees a Windows line ending; Edit puts them back (below). A UTF-8
  byte order mark is skipped.
- **An empty file**, or an `offset` past the end, is answered with Claude
  Code's reminder saying so.
- **A picture** (`.png`, `.jpg`, `.jpeg`, `.gif` or `.webp` whose first
  bytes say it is one) is given to the model as a picture, and the record
  has it as an `Image` piece.
- **Refused:** a PDF ("Reading PDFs isn't supported yet."), anything
  that isn't UTF-8 text, a folder, and a file that isn't there, whose
  answer names the chat's folder, as Claude Code's does. A notebook
  (`.ipynb`) is read as its JSON.

The record has what was read as a `Text` piece, and the file and first
line as its location.

## Edit

`{file_path, old_string, new_string, replace_all?}`, with `path`,
`old_str` and `new_str` taken for them. It replaces `old_string` with
`new_string`, once, or everywhere with `replace_all`.

- **It's refused, in Claude Code's words,** if the strings are the same;
  if `old_string` isn't in the file, or is there more than once without
  `replace_all`; or if the file is over 16 MiB.
- **An empty `old_string` makes a new file,** and is refused for a file
  that has text.
- **Matching** is exact, then with curly quotes (‘’“”) taken as straight
  ones, as models write them straight. A match through curly quotes keeps
  them: straight quotes in `new_string` are curled as the file's are.
- **An empty `new_string`** deletes the match, and the line ending after it.

Before anything is asked, CAF works the change out, so the question the
user answers, and the record, show the `Diff` of exactly what would be
written (see [Tool calls](~caf/concepts/tool-calls)). An edit that can't
be made, such as one whose string isn't there, ends the call at once,
*Failed*, with why, and asks nobody.

## Write

`{file_path, content}`. It writes `content` as given, line endings and
all, as Claude Code's Write does, making the file and any folders it's in.
A folder, or anything that isn't a regular file (a device, a FIFO, a
socket), is refused. Its diff is worked out and shown before it's asked
about, as Edit's is, *New file* when there was none.

## Read before you change

The model may change a file only as it has seen it, so it never writes
over something it didn't know was there. CAF keeps, for each chat, the
files its model read: each one's version, and the hash of what was read.

- **A file the model hasn't read** can't be edited or written: "File has
  not been read yet. Read it first before writing to it." A new file needs
  no read. A read of any part of a file counts, as in Claude Code, so a
  file too big to read whole can still be changed.
- **A file that changed since** is *stale*: its version moved, and its
  bytes aren't the ones read, or were read only in part, so the model
  can't know they're the same. Someone else, the user or a formatter,
  changed it. Write is
  refused: "File has been modified since read, either by the user or by a
  linter. Read it again before attempting to write it." Edit, as Claude
  Code's does, still makes the change if it applies to the file as it is
  now, and its answer tells the model the file has changes
  it hasn't seen, so it reads it before an edit that depends on them.
  Otherwise it's refused as stale.
- **Between the question and the write,** the file may change again: the
  write is made only if the file's version is still the one the change
  was worked out from, else it fails as stale, and nothing is written.
- **After a change,** the model has seen the file whole, as it wrote it.
  Its answer says so, so it needn't read it back.

What a chat has read is kept for one run of the app: a chat restored in the
next run starts with none, and the model reads again.

## Encodings and line endings

CAF reads and writes UTF-8 text, and leaves the rest of a file's bytes as
they were.

- **A byte order mark** stays: Read skips it, Edit keeps it.
- **Line endings stay.** Each line Edit doesn't touch keeps its own
  ending. The lines the model's `new_string` puts in end as most of the
  file's do (the endings in its first 4,096 characters), so the model's
  `\n` becomes `\r\n` in a Windows file.
- **Write writes what it's given,** as Claude Code's does: no endings are
  changed.
- **Anything else,** a file in UTF-16 or Latin-1, or a binary one, can't be
  read or edited: "This tool cannot read binary files."

## Bash

`{command, timeout?, description?, run_in_background?}`. It runs the
command, in a shell, in a terminal of its own (a pty), so a program that
draws progress bars and colours does, and answers with what it printed.

**The folder.** The first command runs in the chat's folder. Each runs in
the folder the last one ended in: `cd src` in one call leaves the next in
`src`, as the model expects from Claude Code. Variables and functions
don't carry over. A restored chat starts in its folder again.

**What the user sees.** The command's output comes into the record as it
runs (an `Output` piece, its lines with their colours, the way the
terminal showed them, a progress bar redrawn in place), and ends with how
the command ended. The call's title is the command's first line.

**What the model gets.** The output without colours, blank lines at its
start and end dropped. A command that failed starts with `Exit code {n}`,
or `Killed by signal {n}` (exit 1 from `grep`, `find`, `diff`, `test`
and the like means "no", so isn't a failure, as in Claude Code). One the
user stopped ends with `<error>Command was aborted before completion</error>`.
Output over 30,000 characters is saved to a file, and the model is given
its path and its first 2,000 characters, to read the rest with Read.

**The timeout** is 2 minutes unless the model asks for another, at most
10. A command still running then **isn't killed:** it's moved to the
background, as Claude Code does now, and the model is told so.

**Its environment** is your program's, with `TERM=xterm-256color`, and
`PAGER` and `GIT_PAGER` set to `cat` so nothing waits for a key. `CLAUDECODE`
and CAF's own relay variables are taken out.

**Stopping** a command (the user's Stop, while its turn runs) sends
`SIGTERM` to it and everything it started, then `SIGKILL` 3 seconds later.

## Background jobs

A command run with `run_in_background`, or one still running at its
timeout, becomes a **job**: the model is answered at once, with the job's
number and the file its output is being written to, and the command runs
on, past the model's answer and its turn.

- **It runs until it ends,** or the model stops it with TaskStop, or its
  chat is closed, deleted or put away, or the app shuts down. **The user's
  Stop doesn't stop it:** Stop stops the turn, not the work it left in the
  background, as in Claude Code.
- **Its call stays running in the record,** with its job's number
  (`ToolCall::job`), in its turn, however many turns later it ends. Apps
  mark it *Background* (see [the transcript](~caf/standard-interface/transcript#background-jobs)).
- **When it ends, CAF tells the agent** with a notice, as Claude Code
  does: `<task-notification>` with the job's number, where its output is,
  and how it ended (completed, with its exit code, failed, or was
  stopped). It's sent as a message is, in a turn of its own
  (`Starter::Notice`): at once if the chat is idle, else once the running
  turn ends. Its call ends *Done*, *Failed* or *Cancelled* where it is.
- **At most 8 run at once** in a chat; a ninth is refused, telling the
  model to stop one with TaskStop first.
- **Jobs don't outlive the app.** A restored chat has none: a call saved
  running ends *Failed*.

**TaskStop** `{task_id}` stops one of the chat's own jobs: "Stopped task
3", or "No task 3 in this conversation" for any other number. It never
asks anyone, as all it can do is stop what its own chat started. The job's
call ends *Cancelled*, stopped by the agent.

**Output files** (a job's, and a long output's) are kept in a folder of the
chat's own under CAF's tools folder (`ToolsConfig::folder`, by default
`$XDG_RUNTIME_DIR/caf`), readable by the user alone, and removed when the
chat is let go or the app ends.

## Your editor and your terminals

CAF reads and writes files, and runs commands, through two backends, which
your app may replace in its `ToolsConfig`:

```rust
let mut config = Config::new(host, agents);
config.tools.relay = Relay::ThisProgram;
config.tools.files = Arc::new(EditorBuffers::new(editor));      // your `Files`
config.tools.commands = Arc::new(VisibleTerminals::new(panes)); // your `Commands`
```

**`Files`** (default `DiskFiles`, the disk) has two methods. `read` gives a
file's raw bytes and its `Version`, anything that tells whether it changed;
`write` replaces them, if the version is still the one given, else fails
with `FileError::Changed`. An editor's would read its open buffer, unsaved
changes and all, so the model edits what the user sees, and make each write
one undoable change. CAF does the decoding, the line endings and the
read-before-change rules itself, so a backend only moves bytes.

`DiskFiles` reads only regular files, at most 256 MiB, never through a FIFO.
Its version is the file's modification time, size, inode and device, and its
content's hash. It writes atomically: to a new file beside it, with its
mode, synced, then renamed over it, once its version is checked again. A
symlink's target is written, and a file with other hard links is written in
place, so it keeps them.

**`Commands`** (default `PtyCommands`, a pty of CAF's own for each command)
starts a command, given its line, folder and variables, and hands CAF what it
prints: raw terminal bytes, which CAF works into lines on its own screen, or
lines already worked out, for a terminal emulator of your own. What it
starts can be waited for, interrupted (Ctrl-C) and stopped, and says where
the user can watch it, such as *terminal 7*: the model's answer then starts
*Ran in terminal 7*, so the model knows the user saw it. Dropping it must stop the command.

`PtyCommands` runs `bash -c`, or `sh -c` where there's no bash, in a
session and process group of its own, its pty the terminal. The folder its
shell ended in comes back with how it ended, which is how Bash's folder
carries over. It runs on Linux and macOS.

A backend's methods return at once, and do their work in the future they
give back, as CAF's other drivers' do.
