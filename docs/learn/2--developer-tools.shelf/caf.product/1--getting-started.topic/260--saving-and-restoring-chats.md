---
title: Saving and Restoring Chats
type: how
description: Keep chats from one run of your program to the next. Give CAF a store, keep chats on shelves, list and restore them, put them away, close (archive), unarchive, delete and move them, keep your own data on each, and write a store of your own, with what CAF saves, when, and its promise that a save stays readable.
related:
  - caf/getting-started/a-chat-on-tokio
  - caf/getting-started/a-chat-in-the-browser
  - caf/concepts/chats-turns-and-items
  - caf/getting-started/the-debugger
---

CAF can save every chat as it goes, so the user can close your program, open
it again tomorrow, and carry on where they were. Your program says where
chats are kept; CAF decides what to write and when, and brings a chat back
with its whole record, its own data, and the agent's session to carry on.

## Give CAF a store

A **store** is where chats are kept. CAF comes with one that keeps them in
files, `FileStore` in the crate `caf-save`:

```rust
use caf_save::FileStore;

let mut config = Config::new(Host::new("eyespy", "0.4.0"), vec![claude]);
let root = FileStore::default_root("eyespy").expect("a home folder");
config.store = Some(Box::new(FileStore::new(root)));
let (caf, running) = Caf::<NoAppItems>::start(config);
```

`FileStore::default_root("eyespy")` is `~/.local/share/eyespy/chats`
(`$XDG_DATA_HOME/eyespy/chats` if that's set). The store runs on a thread of
its own, so the disk never holds CAF up.

With no store, CAF saves nothing: opening a chat on a shelf, listing,
restoring, deleting and moving are all refused with `Refused::NotSaved`.

## Keep a chat on a shelf

Each saved chat is kept on a **shelf**, a name your program chooses: one per
project, per user, or just one. The store decides what a shelf is; to
`FileStore` it's a folder. A chat is saved if it's opened with a shelf:

```rust
let mut open = OpenChat::new("claude", "/home/me/project");
open.shelf = Some(Shelf("project-x".into()));
let chat = caf.open_chat(open).await?;
```

A chat opened without one lives in memory only, and is gone when CAF stops.
A shelf's name may be up to 256 bytes long, but not empty, and with no
control characters in it.

## What's saved, and when

**Everything a chat is, is saved**: its whole record, the agent's session to
carry on, and your program's data on it ([below](#your-own-data-on-each-chat)).
What belongs to one run isn't: adapter processes, requests in flight,
timers, and the chat's MCP servers, which hold tokens and sockets, so your
program gives them again when it restores the chat. Questions waiting for an
answer are saved with the record, and dropped when it's restored, as what
asked them is gone.

CAF writes a chat:

- **a second after it changes** (`host.timers.save`), with every change since
  it was last written: streamed words to one item are one change;
- **at once when a turn ends**;
- **whole** the first time, when it's put away or closed, as CAF shuts down,
  and whenever its changes outgrow the last whole copy.

A write that fails, such as on a full disk, is tried again later, whole,
with a warning in the log each time, waiting longer each time up to a
minute. Shutting down waits until it's written, so check the store can be
written to as your program starts.

`caf.shutdown()` returns once every saved chat is written whole and let go,
so the next run, or another program, can have it.

## List what's saved

```rust
let mut saved = caf.watch_saved();
caf.list_saved(Shelf("project-x".into())).await?;
while let Some(chats) = saved.next().await {
    draw_saved(&chats);
}
```

`watch_saved` follows every shelf you've listed, with their chats in the
order they were made, whatever shelf each is on: the whole list at once,
then again each time it changes. Once listed, a shelf is kept up to date as chats on it are saved,
put away and deleted. List it again to see chats another program saved
since.

Each `SavedChat` has its `shelf`, its `header`, which is what to show
without loading it (its id, agent, folder, when it was made and last
changed, your `label` for it, the first line of its first message, how many
turns it has, whether it was closed, and when it was archived), and its
`state`: `Saved`,
`Restoring`, `Loaded`, or `Failed` with why the last restore failed.

## Restore a chat

```rust
let chat = caf.restore(RestoreChat::new(id, Shelf("project-x".into()))).await?;
```

`restore` returns the chat's handle once it's loaded, with the same id it
had. Give it its tools in `RestoreChat::mcp_servers`, as you would to a new
chat. The shelf needn't have been listed.

**Restoring is lazy.** A restored chat is `Idle`: no agent is started until
a message is sent. Then CAF carries on the agent's session: resumed where
the agent can, or loaded, with what the agent replays thrown away, as CAF
has the record already. An agent that can do neither opens a new session,
and the chat says so.

**What was under way is tidied**, as a save may have come at any moment: a
turn that was running is cut off, its unfinished tool calls have failed,
and a message waiting to be sent is stopped, never sent by itself, as what
the user meant then may not stand now.

A chat whose agent your program no longer offers still comes back, with its
record, and sending to it is refused with `Refused::NoSuchAgent`. If you
renamed the agent, and exactly one agent you offer has its built-in
profile, the chat comes back with that one.

A chat that can't be restored is left as it was, and `restore` fails with
`Error::CannotRestore`, saying why:

| Why | Meaning |
|---|---|
| `NotFound` | It isn't on that shelf. |
| `InUse` | Another program, or another `Caf`, has it open: a chat is never open twice. |
| `Newer { version }` | A newer CAF saved it, in a version of the format this one can't read. It's left untouched for that one. |
| `Damaged(why)` | Its save is damaged. |
| `AppData(why)` | Your program can't read the data it saved with it: see [What your program reads, it must keep](#what-your-program-reads-it-must-keep). |
| `Store(why)` | The store failed, such as a disk error. |

Trying again may help after `InUse` or `Store`. The core may also refuse it:
`Refused::Loaded` for a chat that's loaded or being restored already.

## Put away, close, unarchive, delete and move

**Put away** a chat to save it whole and let it go from memory, such as when
the user hasn't looked at it for a while:

```rust
chat.put_away().await?;
```

Its session is closed, its adapter stopped if nothing else uses it, and it's
let go in the store, so another program can have it. A turn running is cut
off first, and messages waiting are stopped. It stays on its shelf, and
restoring it carries on. A watch of it gets how it was left, one last time,
and ends. Until it's written, which is at once unless a move of its shelf is
under way, restoring it is refused as `Loaded`, though it's listed as
saved: try again in a moment.

**Close** a chat when the user is done with it: it's **archived**, as of
then (`Chat::archived`), and a watch of it sees it archived. Its session is
closed, and its adapter stopped if nothing else uses it. A saved chat is
then put away, marked closed, with when it was archived in its header
(`Header::archived`), and can still be restored. One that isn't saved
stays, archived, until CAF stops, and its watches go on. An archived
chat's settings can't be chosen (`Refused::Closed`).

**Archiving sticks.** A closed chat restored is still archived, as it was
left, with no agent started, however many times it's put away and
restored, so your program can keep it in an archive rather than among the
user's chats. Putting it away, or closing it again, keeps when it was
archived. It's unarchived only when your program says so, or when the user
sends it a message:

```rust
chat.unarchive().await?; // or send to it: that unarchives it first
```

Unarchived, it's `Idle`, and its agent starts when a message is sent, as a
restored chat's does. Unarchiving a chat that isn't archived does nothing.
A message sent to an archived chat goes as to any other; one refused, such
as an empty message, leaves it archived. While it's archived, its settings
can't be chosen (`Refused::Closed`). Putting an archived chat away, or
closing it again, keeps when it was archived. A chat saved closed by a CAF
from before archiving is restored archived, as of when it's restored.

**Delete** a chat for good, loaded or not: one that's loaded is closed and
let go first.

```rust
caf.delete(id, Shelf("project-x".into())).await?;
```

**Move** a shelf to move every chat on it, loaded or not, such as when the
user renames a project:

```rust
caf.move_shelf(Shelf("project-x".into()), Shelf("project-y".into())).await?;
```

All of it moves, or none: a move onto a shelf that has chats already isn't
supported, and a chat on it that another program has open stops the move.
Either way the move fails, says so in the log, and every chat stays where it
was. Meanwhile, CAF holds back everything else for chats on either shelf, so
nothing is written in the wrong place.

## Your own data on each chat

Each chat has a slot for your program's own data about it, such as its
title, saved and restored with it. Give CAF your type, which says what one
line to show for the chat, its **label**:

```rust
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Notes {
    title: String,
    pinned: bool,
}

impl AppData for Notes {
    fn label(&self) -> Option<String> {
        Some(self.title.clone())
    }
}

let (caf, running) = Caf::<NoAppItems, Notes>::start(config);
chat.set_data(Notes { title: "Fixing the tests".into(), pinned: true }).await?;
let notes = chat.data().await?;
```

A chat's data is the default until you set it, and is set whole. Its label
shows in `watch_chats`, in each saved chat's header, and to viewers, who
never see the data itself. A recording ([`Config::record`](~caf/getting-started/the-debugger#what-a-recording-holds))
holds the data each time it's set, so keep secrets out of it.

### What your program reads, it must keep

A save stays readable while your program changes, so CAF checks your data
as it reads it back. Your data comes back only if what your type writes
back holds all of what was saved: every key of every map, with what it
held, every list as long, and everything else equal. Keys your type adds,
as a newer version of your program does, are fine. Anything else, such as a
field your type no longer has, would be lost when the chat is next saved, so
CAF refuses to restore it (`AppData`) and leaves the save as it was, for a
version of your program that can read it.

So add new fields with `#[serde(default)]`, and never remove or rename one.

Items of your program's own kind in the record are checked the same way,
except that one your program can't read doesn't stop the chat: it comes
back as an `Unknown` item with the summary it had, and is saved back
exactly as it was, so a version of your program that can read it gets it
back. Items in a tool call have no such wrapping, so they stop the chat as
your data does. Through JSON, an item keeps only text keys and no raw
bytes.

## Viewers

A viewer, such as a page using the
[TypeScript caf-sdk](~caf/getting-started/a-chat-in-the-browser#saved-chats),
gets the saved chats on every shelf you've listed, right after the agents,
and again whenever they change. Each chat in its list of chats loaded says
its label and shelf, and when it was archived if it is: an archived chat
restored, or closed and not saved, is listed, marked. A viewer may close and unarchive chats (`FromViewer::Close`,
`FromViewer::Unarchive`) as it may act on them (`ViewerRules::act`). Its
request to restore a chat goes to
`ViewerRules::restore`, which gives it its tools, or refuses it; by default
it refuses every one:

```rust
let mut rules = ViewerRules::default();
rules.restore = Box::new(|chat, shelf| {
    (shelf.0 == "project-x").then(|| RestoreChat::new(chat, shelf.clone()))
});
```

## The files

`FileStore` keeps each shelf in a folder of its own, and each chat as four
files in it:

```text
<root>/<shelf>/<chat>.head      what lists show
<root>/<shelf>/<chat>.caf       the chat whole, as last written
<root>/<shelf>/<chat>.journal   what changed since
<root>/<shelf>/<chat>.lock      held while a program has it loaded
```

The folders and files `FileStore` makes are readable only by the user; a
folder that's there already, such as the root you give it, keeps its own
permissions: make a root of your own private yourself. A whole copy replaces the
old one in one step, and each change is synced before it counts as written,
so a crash leaves the chat as it was last written, and at worst loses a
change being written. The locks are the system's, so a program that dies
lets go of its chats; for that, the root must be on a local disk, not a
network one.

Chats are compressed as they're written, quickly. `FileStore::squeeze`
compresses again, as hard as it can, every chat nobody has changed for a
while: run it now and then, such as once a day for chats a week old, on a
thread of its own, as it's slow.

## Writing your own store

A program that keeps its data somewhere else, such as eyespy in its own
SQLite database, can keep chats there too, by writing a `caf_save::Store`.
A store only keeps bytes: CAF turns chats into bytes and back, so the format
stays CAF's. For each chat it keeps three things, by shelf and chat id: its
header, its snapshot (the chat whole) and its journal (records added to the
end).

```rust
impl Store for MyStore {
    fn list(&mut self, shelf: &Shelf) -> Result<Vec<(ChatId, Vec<u8>)>, StoreError> { … }
    fn load(&mut self, shelf: &Shelf, chat: ChatId) -> Result<Stored, StoreError> { … }
    fn snapshot(&mut self, shelf: &Shelf, chat: ChatId, header: &[u8], snapshot: &[u8]) -> Result<(), StoreError> { … }
    fn append(&mut self, shelf: &Shelf, chat: ChatId, header: &[u8], record: &[u8]) -> Result<(), StoreError> { … }
    fn release(&mut self, shelf: &Shelf, chat: ChatId) { … }
    fn delete(&mut self, shelf: &Shelf, chat: ChatId) -> Result<(), StoreError> { … }
    fn move_shelf(&mut self, from: &Shelf, to: &Shelf) -> Result<(), StoreError> { … }
}
```

Its calls block, and come from one thread at a time, in the order CAF gives
them. It must:

- **lock a chat for one program at a time**, from when it's loaded or first
  written until it's released, so a second gets `StoreError::InUse`;
- **replace a snapshot and empty the journal in one step**, so a crash
  leaves one or the other, whole;
- **add each record after everything before it**: one cut off by a crash
  may be left, as the format finds it;
- **replace a header whole**;
- **never remove or change a chat it wasn't told to.**

A store that can say whether it holds a chat's lock already
(`Store::holds`) lets CAF let go of a chat it loaded and couldn't read only
if loading it took the lock. A write that fails is tried again until it's
written, and shutting down waits for it, so fail only when you must. A
store that panics fails what it was doing, as if it had said so, lets go
of a chat it was loading, and is used again. On a runtime other than tokio, `caf_save::carry_out` does one
of the core's directives to your store and gives back its answer: the
storage driver's whole job.

## A save stays readable

**Saves stay readable forever, and a CAF never changes one it can't read.**
The format's version, `caf_save::VERSION`, goes up with every change to
what's saved, even a field added. So a CAF meeting a save from a newer one
always knows, and leaves it untouched (`Newer`), rather than read it, lose
what it doesn't know, and write it back without it. A newer CAF reads every
older version's saves, and the first time it writes one, writes it whole in
its own version. A change of shape, rather than an addition, is migrated
from every older version as a save is read: version 5's notes about
settings, which name their choice, read from an older save with no name.
Every version has a sample chat, saved by it, that each release of CAF is
tested against.
