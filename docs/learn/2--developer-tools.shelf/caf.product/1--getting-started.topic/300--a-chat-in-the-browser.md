---
title: A Chat in the Browser
type: how
description: Show and drive CAF's chats from a web page. A Rust backend serves each browser with caf_driver_tokio::viewer::serve over a WebSocket, and the page follows chats and acts on them with the TypeScript caf-sdk's Connection.
related:
  - caf/getting-started/a-chat-on-tokio
  - caf/concepts/chats-turns-and-items
  - caf/concepts/viewers
  - caf/getting-started/saving-and-restoring-chats
---

CAF runs in Rust, but the chat can be drawn anywhere. A **viewer** is
anything that shows chats, such as a web page. It talks to CAF over the
**viewer wire**: a small set of messages, sent as JSON to a browser, that
let it follow chats and act on them.

A chat in the browser has two halves:

- **the backend**, a Rust program running CAF with `caf-driver-tokio`, which
  serves each browser that connects with `caf_driver_tokio::viewer::serve`;
- **the front end**, a web page using the TypeScript caf-sdk, whose
  `Connection` follows chats and sends what the user does.

The TypeScript caf-sdk is `@clackatoo/caf`, in `ts/sdk` of
[CAF's repository](https://github.com/clackatoo/caf). It isn't on npm yet.

## The backend

Start CAF as in [A Chat on Tokio](~caf/getting-started/a-chat-on-tokio).
Then, for each browser that connects, run `serve` with two channels: one for
the messages the browser sends, one for the messages it's sent. `serve`
doesn't touch the network itself, so it works over any transport. The host
moves messages between the channels and the socket, and encodes them as JSON.

Here it is over a WebSocket with [axum](https://docs.rs/axum):

```rust
async fn viewer(ws: WebSocketUpgrade, State(caf): State<Caf<NoAppItems>>) -> Response {
    ws.on_upgrade(move |socket| link(socket, caf))
}

async fn link(socket: WebSocket, caf: Caf<NoAppItems>) {
    let (to_caf, from) = mpsc::channel(16);
    let (to, mut to_viewer) = mpsc::channel(64);
    tokio::spawn(serve(caf, Arc::new(ViewerRules::default()), from, to));

    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(message) = to_viewer.recv().await {
            let text = serde_json::to_string(&message).expect("CAF's messages are JSON");
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        // serve has ended: close the socket, so the browser hears it.
        let _ = sink.close().await;
    });
    while let Some(Ok(message)) = stream.next().await {
        let Message::Text(text) = message else { continue };
        // A message that isn't the wire's ends the link.
        let Ok(message) = serde_json::from_str::<FromViewer>(&text) else { break };
        if to_caf.send(message).await.is_err() {
            break;
        }
    }
    writer.abort();
}
```

`serve` runs until either side closes or CAF shuts down. When it ends, the
writer closes the socket, so the browser hears the link end; without that,
the page would wait on a socket nothing writes to. A viewer doesn't keep
CAF running: once the host drops its own `Caf`, CAF shuts down, and each
viewer sees its chats end.

Keep the channel to the viewer bounded. While it's full, `serve` stops
taking that viewer's changes. A viewer that falls far behind gets the chat
whole again rather than every change it missed, so a slow browser never
makes CAF hold more than one picture of each chat for it.

### What viewers may do

`ViewerRules` says what viewers may do:

- **`open`** turns a viewer's request to open a chat into an `OpenChat`, or
  refuses it. The default refuses every one, so viewers can only watch and
  use chats the host opened.
- **`restore`** does the same for a request to restore a saved chat: it
  gives the chat its tools, or refuses. The default refuses every one.
- **`act`** says whether viewers may send, stop, answer and close. It's
  `true` by default.

Opening is always the host's decision, because a chat's MCP servers are
programs to run. A viewer only asks for an agent CAF offered, a folder, and
a new session or one to continue. The host chooses the rest, and may limit
the folders:

```rust
let mut rules = ViewerRules::default();
rules.open = Box::new(|request: &OpenRequest| {
    if !request.cwd.starts_with("/home/me/projects") {
        return None;
    }
    let mut open = OpenChat::new(request.agent.0.clone(), request.cwd.clone());
    open.session = request.session.clone();
    Some(open)
});
```

`open` runs on the viewer's task, so it should decide at once. A host that
needs to ask its user, or read a file, decides beforehand or refuses.

A viewer may watch any chat in its `Caf`: one `Caf` is one circle of trust.
A host whose users mustn't see each other's chats runs a `Caf` for each.

A viewer that may open chats starts agents in your folders, and any website
can try to reach a port on your machine. A backend for one person on one
machine should listen on `127.0.0.1` only, check the WebSocket's `Origin`,
and ask for a token it made at start.

## The front end

Connect, and the SDK says hello and agrees a version of the wire with CAF:

```ts
import { Connection, webSocket } from "@clackatoo/caf";

const caf = await Connection.open(await webSocket("ws://127.0.0.1:7320/viewer"));
caf.agents; // every agent on offer: its name, whether it's installed and signed in
caf.chats; // the open chats: id, agent, folder, and the app's label and shelf, if any
caf.onChats((chats) => drawList(chats));
caf.onAgents((agents) => drawAgents(agents));
```

`Connection.open` takes any `Transport`, an object that sends and hears one
JSON text per message. `webSocket` is the one the SDK ships. If CAF speaks
no version of the wire the SDK does, `open` rejects with a `CafError` whose
`why` is `NoCommonVersion`. It waits as long as CAF takes to say hello
back: to give up sooner, pass a signal, as in
`Connection.open(transport, { signal: AbortSignal.timeout(5000) })`.

CAF says where its agents stand right after its hello, and `open` resolves
once it has, so `agents` is right from the start. `onChats` and `onAgents`
each return a function that stops them.

`caf.may` says what the host lets this page do: `act` on chats (send,
stop, answer, choose their settings and close), and on `agents` (install
them, sign in and out, and learn their settings). Offer only what it may: anything else is answered `NotAllowed`. A
page that may not act on agents gets no sign-in's link or code. Whether it
may open chats, or restore saved ones, isn't said, as the host decides each
request.

### Watch a chat

```ts
const watching = caf.watch(chatId, (event) => {
  if (event.type === "Chat") render(event.chat);
  else showEnded();
});
// …and when the page no longer shows it:
watching.stop();
```

The listener is given the chat whole each time it changes. It's a plain
object, of the same types as CAF's Rust ones, generated from them: every
enum is `{ type, value }`, so `switch (item.kind.type)` tells each kind
apart.

Each new chat shares every turn and item that didn't change with the one
before it, so Svelte, React or plain DOM code can tell what changed by
comparing objects, and redraw only that. It also shares them with the SDK's
own copy, so it's typed read-only all the way down (`DeepReadonly<Chat>`):
changing it would change the copy. `apply` takes a read-only chat too.

CAF first sends a watched chat whole, then its numbered changes. The SDK
applies them itself. If one is missing, doesn't fit, or is of a kind this
version of the SDK doesn't know, it asks CAF for the chat whole again, and
the listener only ever sees the right chat. The listener hears `Ended` once
the chat is closed, or CAF has shut down.

### Act

```ts
const chat = await caf.open({ agent: "claude", cwd: "/home/me/projects/app", session: { type: "New" } });
const turn = await caf.send(chat, { parts: [{ type: "Text", value: "Fix the tests" }] });
await caf.stop(chat);
await caf.answer(question.id, { type: "Permission", value: option.id });
await caf.close(chat);
```

`open` resolves once the chat is open, and `send` once the core has taken
the message, with the turn it will be: not once the turn has run, which
the watch shows. `stop`, `answer` and `close` resolve once they're done.
Questions waiting for an answer are on the chat, in `chat.questions`. When
something isn't done, the promise rejects with a `CafError`, whose `why`
says why:

| `why.type` | What it means |
|---|---|
| `Refused` | The core refused it, and nothing changed. `why.value` says why, such as `QueueFull`. |
| `NotAllowed` | The host doesn't let viewers do this. |
| `Gone` | CAF has shut down, or is shutting down: nothing was done. |
| `CannotRestore` | A saved chat couldn't be restored: see [Saved chats](#saved-chats). |
| `Unknown` | It went in a way this version of the SDK doesn't know, from a later CAF. `why.value.outcome` is its name. |
| `Disconnected` | The link ended before the answer came. |

A later CAF may add reasons: to `Refused` and `CannotRestore` here, and to
a chat's `Lost` and a failed turn's `FailureReason`. The types list only
the reasons this version of the SDK knows, but a new one still comes
through, by its name. So give every `switch` on a reason a `default` that
shows it by its `type`:

```ts
function refusal(reason: Refused): string {
  switch (reason.type) {
    case "QueueFull":
      return "Too many messages are waiting.";
    // …
    default:
      return `Refused (${reason.type})`;
  }
}
```

### Saved chats

A host that [saves its chats](~caf/getting-started/saving-and-restoring-chats)
keeps them on **shelves**, names of its own choosing, and lists the shelves
it wants viewers to see. `caf.saved` holds
the chats saved on them, open or not, in the order they were made:

```ts
caf.saved; // each: its shelf, its header, and its state
caf.onSaved((saved) => drawSaved(saved));
const [first] = caf.saved;
const chat = await caf.restore(first.header.chat, first.shelf);
```

CAF sends the list right after the agents, and again, whole, whenever it
changes, so it may come just after `open` resolves: read `saved`, and
follow `onSaved`, which returns a function that stops it. A host that
lists no shelves never sends it, and `saved` stays empty.

Each chat's `header` is what to show without loading it: its `chat` id,
`agent`, `cwd`, when it was `made` and last `changed`, the app's `label`
for it, the `first` line of its first message, how many `turns` it has,
and whether it was `closed`. Its `state` is `Saved`, `Restoring`, `Loaded`,
or `Failed`, with why the last restore failed.

`restore` resolves with the chat's id once it's loaded. It's then in the
next `Chats`, so in `chats` once `onChats` hears it, and watched like any
other. A chat keeps its id when it's saved and
restored. A restored chat is `Idle`: no session is open until a message is
sent, which opens one, continuing the agent's session where the agent can.

The host decides each restore, as it does each open, so `may` doesn't say
whether this page may: one it doesn't allow rejects with `NotAllowed`. One
that can't be restored rejects with `CannotRestore`, and its save is left as
it was. `why.value` says why:

| `why.value.type` | Why |
|---|---|
| `NotFound` | It isn't on that shelf. |
| `InUse` | Another program has it open: a chat is never open twice. |
| `Newer` | A newer CAF saved it, in a version of the format (`why.value.value.version`) this one can't read. |
| `Damaged` | Its save is damaged: the value says how. |
| `AppData` | The app can't read the data it saved with it: the value says why. |
| `Store` | The store failed, such as a disk error: the value says how. |

Trying again may help after `InUse` or `Store`, not after the rest. The
core may also refuse it: `Loaded`, for a chat that's open or being restored
already, or `BadShelf`, for a shelf's name it can't take.

An item of the app's own that this version of the app can't read comes
back as an `Unknown` item whose `what` is `App`, with the `summary` it had:
show that. Its data is kept exactly as it was, and saved back unchanged.

### Install and sign in

Each of `caf.agents` is an `AgentStatus`: its `name`, its `install` state
and its `sign_in` state. These are the agent's, shared by every chat with
it. The `install` state is `Missing` with its size, `Installing` with how
far it has got, `Installed` with its version, `Cannot` with why, `Host` for
an agent the host starts itself, or `Unknown` until CAF has looked. The
`sign_in` state is `SignedOut`, `SigningIn` with what the user must do,
`SignedIn` with how (never who), or `Unknown`. `can_sign_in` says whether
`signIn` would be accepted now, so offer Sign in only while it's true. It
changes as CAF runs: an agent the host starts itself can be signed in to
only once it has answered as the adapter its profile is for.
`own_tools_off` says whether CAF can turn off the agent's own tools, as it
must to run it: while it's false, a chat with the agent is refused, or gets
no session, so don't offer one. It's false for an agent with the cautious
profile, turns false once an adapter answers that isn't the one its
profile is for, and true again once one that is answers. See
[Agents and profiles](~caf/concepts/agents-and-profiles#an-agent-has-only-your-tools).
`limits` are its account's limits, as the agent last said them: how much
of each window of its subscription is used, and when it resets, shared by
every agent with its profile. See [Account
limits](~caf/getting-started/account-limits).

```ts
await caf.install("claude");
await caf.stopInstall("claude");
await caf.signIn("claude");
await caf.paste("claude", code); // the code a sign-in page gave the user
await caf.stopSignIn("claude");
await caf.signOut("claude");
```

Each resolves once CAF has started it: how it goes shows in `agents`.
While signing in, the status's `SigningIn` value holds the `url` to open
(null until the agent's sign-in command gives it), a `code` to type there,
if any, and whether the user may `paste` a code back. Open the link in a
new tab with `rel="noopener noreferrer"`, and only if it starts with
`https://`. CAF has checked its site against the agent's profile, but a
page should check for itself too.

The host decides whether viewers may do any of this, as it does for opening
chats, and `caf.may.agents` says so. When it doesn't let them, each rejects
with `NotAllowed`. The core may refuse too:

- `NoSuchAgent`, for an agent it doesn't offer;
- `NotInstallable` or `AlreadyInstalled`, for an install;
- `NothingToStop`, for stopping an install that isn't under way;
- `CannotSignIn`, for signing in or out where CAF can't now;
- `NotSigningIn`, for a paste or a stop with no sign-in under way;
- `Unsupported`, for a paste the sign-in doesn't take: empty, more than
  one line, or too long.

A chat with an agent that isn't installed yet is `WaitingForAgent`, and its
messages wait. They're sent once the agent is installed. Stopping the chat
stops them, and a note in the chat says so.

### Choose the model

A chat's `settings` say what its agent offers that the user may choose,
and what was chosen. Its `offered` lists each `Setting` in the agent's
order: what it's about (its `kind`: `Model`, `Effort` or `Fast`), what the
agent calls it, its `form`, and what the agent says it is now (`current`).
The form is `Choices`, each with the agent's own `id`, its `name`, and any
`description` and `group`, or a `Switch`, on or off. Draw a setting by its
form, so one of a kind a later CAF adds still shows. A choice such as
Claude's `default`, which stands for whatever model the agent would pick,
comes as the agent gives it. `offered` is empty until the chat's session
first opens, and for an agent that offers none.

```ts
await caf.setSetting(chatId, { kind: { type: "Model" }, value: { type: "Choice", value: "sonnet" } });
await caf.setSetting(chatId, { kind: { type: "Fast" }, value: { type: "Switch", value: true } });
```

Choices are by the agent's own ids, matched exactly. The chat's `chosen`
holds them, with the host's defaults for the agent, and its `own` says
which kinds were chosen for the chat itself. Where a choice differs from
its setting's `current`, the change is waiting. A change made while a turn
runs waits for it to end, and applies from the next message; with none
running, it's sent at once. One made before the session opens is applied
as it opens, before any message goes, and if it can't be, the message
fails, `SettingNotApplied`, offering Send again. A choice the agent
doesn't offer, or refuses, is dropped, with a note in the chat
(`NotOffered`, `NotApplied`), and the setting falls back to the host's
default, if the agent offers it, or else to what the agent uses; a host's
default the agent doesn't offer is dropped quietly. When the agent changes
a setting by itself, such as falling back to another model, the chat
follows it, and a note says so (`AgentChanged`). Each turn's `model` names
the model that answered it, if the agent says.

The core refuses `NoSuchSetting` for a setting, or a value of it, that the
agent has said it doesn't offer.

To offer them before any chat, each agent's status says what CAF knows of
its settings: `Unknown`, `Learning`, `Known` with what a session with it
last offered, or `Failed` with why. Every chat's session tells CAF. To
learn them sooner, ask: CAF opens a session and sends nothing, so it costs
nothing.

```ts
await caf.learnSettings("claude");
```

A chat can't be opened with its settings from the page: open it, then
choose them, before anything is sent.

### Usage

A chat's `spend` is what it has cost so far, over the turns that have a
record: their tokens, the cost the agent `reported`, the cost `estimated`
at API prices for turns it didn't, and the tokens no price covered
(`unpriced`). CAF works it out and sends it again whenever a turn adds to
it or the prices change, so every page shows the same figure. Each turn's
`spent` is its own record. `usage` is how full the agent's context is, as
it last said.

```ts
import { cost, total } from "@clackatoo/caf";

cost(chat.spend); // reported and estimated, in US dollars
total(chat.spend.tokens); // input, both cache counts and output
```

A page the host lets (`caf.may.usage`) can ask for a summary of every
CAF app's usage on the machine over a period, and set prices of the
user's own. A summary is buckets, one per period, app, agent, model and
fast mode; the SDK's helpers add them up, so no page writes its own sums:

```ts
import { group, price, split, totals } from "@clackatoo/caf";

const summary = await caf.usage({
  since, until, zone: Intl.DateTimeFormat().resolvedOptions().timeZone,
  by: { type: "Day" }, apps: [], agents: [],
});
totals(summary); // every bucket added up
group(summary, "agent"); // by agent, biggest cost first; or "app", "model", "fast"
group(summary, "model", "tokens"); // biggest by tokens first
group(summary, "period"); // in time order, every day of the period, empty ones too
split(summary, "app"); // each day's groups: a stacked chart's bars
price(summary, "claude-haiku-5-5"); // the entry that priced it, as the summary's `matched` says
await caf.setPrice("claude-haiku-5-5", { input: 1, cache_read: 0.1, cache_write: 1.25, output: 5, fast: null });
await caf.setPrice("claude-haiku-5-5", null); // the table's price again
```

Periods are worked out in the query's zone, so a day is the user's own,
23 or 25 hours across a change of clocks. A summary isn't pushed: ask
again for newer figures. It rejects with `NotAllowed` for a page the host
doesn't let, `NoLedger` if CAF keeps no ledger, and `BadQuery` saying why
for a zone CAF doesn't know or a period it can't sum. `unreadable` names
any part of the record that couldn't be read: say the figures are short.

### When the link drops

When the link ends, for any reason, every request still waiting rejects with
`Disconnected`, and `onClose` is called. A watch isn't ended: `onClose` is
how the page hears. `watch` on a connection that has ended throws
`Disconnected`.

Reconnecting is the page's choice. To pick up where it left off, connect
again and watch each chat `from` the copy its last watch had: CAF then sends
only what changed since, if it still can, and the chat whole if not.

```ts
const stopReconnecting = caf.onClose(() => {
  reconnect().catch((error) => showOffline(error));
});

async function reconnect() {
  const again = await Connection.open(await webSocket(url));
  if (again.run !== caf.run) return startOver(again); // CAF restarted
  again.watch(chatId, listener, watching.copy ?? undefined);
}
```

`onClose` returns a function that stops it, such as when the page is done
with the connection.

A chat keeps its id for good, but a restarted CAF numbers each chat's
changes from the start again, and a chat open before may not be open now.
Each connection's `run` names the run of CAF it reached, and is different
every time CAF starts. So compare it with the old connection's: if it
changed, the page's copies mean nothing now, and it starts again from the
new `chats`.
`watch` checks too: it only resumes from a copy one of its watches made on
a connection to the same run, and otherwise asks for the chat whole. A
`ChatCopy` the page made itself, such as from storage, isn't resumed from.

`disconnect` ends the link from the page's side.

### What the SDK does with what it doesn't expect

Nothing CAF sends makes the SDK throw:

- a message of a type it doesn't know, from a later version of the wire, is
  ignored;
- a change it can't apply asks for the chat whole again, as above;
- an answer of a kind it doesn't know rejects that request with `Unknown`,
  as above;
- anything else that can't be CAF's ends the link, as if it had dropped:
  text that isn't JSON, a message missing what it needs, or an answer that
  doesn't fit what was asked. Ignoring it could leave a request waiting
  forever or a chat silently wrong; ending the link tells the page, and
  rejects every request still waiting.

A listener of the page's own that throws doesn't end the link. Its error is
reported as uncaught, and the other listeners are still called.

## A chat view to copy

CAF's debugger draws its chats with a reference chat view, in Svelte, in
`ts/debugger/src/chat/` of CAF's repository. It draws everything a chat
holds: its state and why a session was lost, each turn with what started
it and how it ended, words, thinking, tools, CAF's notes, items CAF
couldn't read, the questions waiting, and the context ring.
It says each of CAF's reasons in CAF's words, and names one it doesn't know
by its `type`.

It's a pure renderer: give it a chat and the functions to call when the
user acts, and it calls them. It never talks to CAF itself, so it doesn't
care where the chat came from:

```svelte
<script lang="ts">
  // Kept as it changes, so the view follows an install's progress and a
  // sign-in's link as they come, until the component goes.
  let agents = $state.raw(caf.agents);
  $effect(() => caf.onAgents((now) => (agents = now)));
</script>

<ChatView
  {chat}
  label={caf.chats.find((each) => each.id === chat.id)?.label ?? null}
  agent={agents.find((agent) => agent.name === chat.agent)}
  may={caf.may}
  actions={{
    send: (message) => caf.send(chat.id, message),
    stop: () => caf.stop(chat.id),
    answer: (question, answer) => caf.answer(question, answer),
    sendAgain: (message) => caf.send(chat.id, structuredClone(message)),
    close: () => caf.close(chat.id),
    install: () => caf.install(chat.agent),
    stopInstall: () => caf.stopInstall(chat.agent),
    signIn: () => caf.signIn(chat.agent),
    paste: (text) => caf.paste(chat.agent, text),
    stopSignIn: () => caf.stopSignIn(chat.agent),
    setSetting: (choice) => caf.setSetting(chat.id, choice),
    learnSettings: () => caf.learnSettings(chat.agent),
  }}
/>
```

It offers only what `may` lets it: without `act`, the chat can be watched,
not acted in; without `agents`, it says what's happening to the agent but
offers no Install or sign-in.

Given its agent's status, the view also shows what the agent needs before
the chat can go on. While the chat waits for an agent that isn't
installed, it offers Install, naming the size, and Not now, which stops the
waiting message. It shows progress while the agent installs, and says why
when it can't be. While the agent is being signed in to, it shows the link,
the code, a box to paste a code into if the agent takes one, and Stop. A
turn that failed for want of signing in offers Sign in, and still offers
Send again: once signed in, the user sends it again, since CAF never does
by itself. Sign in shows only while the agent's status says CAF can sign
in to it now.

Beside the composer, the view offers the agent's settings: its model and
effort as menus of the agent's own choices, in its order and groups, each
choice's description shown, and fast mode as a switch, saying it may cost
more. A change waiting shows as such: "Sonnet, from your next message"
while a turn runs or the session isn't open, and "Changing to Sonnet…"
otherwise. Before the session first opens, it offers what the agent's
status knows, or, with `act` and `agents`, to learn it. Each turn names
the model that answered it.

By Send is the context ring: how full the agent's context is, amber
past 80% and red past 95%. Clicking it shows what the chat has used and
spent: the context, its cost so far, marked as an API estimate where any
of it is, with the tokens no price covered, and the tokens processed.
Given `openUsage`, it links to the page's usage page: give it only if
`caf.may.usage`. `UsagePage` is one to copy, after CAF SI's: a period's
total, each agent's or app's share and a chart of it by day (or hour),
tokens by kind and what the cache saved, cost by kind and by speed, and a
breakdown by model, app or day, with each model's price a click away and a
price of the user's own:

```svelte
<UsagePage
  load={(query) => caf.usage(query)}
  setPrice={(model, price) => caf.setPrice(model, price)}
  close={() => dialog.close()}
/>
```

Given its `label`, from the chat's summary in `caf.chats`, the view heads
the chat with it. Beside it, `ChatList` lists the open chats, by their
labels where they have them, and opens new ones. Given the agents'
statuses, it offers choosing the model of an agent whose models are known,
passing the choice to its `open(agent, cwd, settings)` action, for the
page to choose once the chat is open. `openThenChoose` does both, and
resolves once the chat is open even if its settings couldn't be chosen,
so it's never opened twice: say why in the chat, with `ChatView`'s
`notice`. `SavedList` lists `caf.saved`: each
chat's label or first line, its agent, folder, when it last changed and its
turns, whether it's loaded or why it can't be, and Restore. Its actions are
`select(chat)`, to show a loaded one, and `restore(chat, shelf)`:

```svelte
<script lang="ts">
  let saved = $state.raw(caf.saved);
  $effect(() => caf.onSaved((now) => (saved = now)));
</script>

<SavedList
  {saved}
  actions={{
    select: (chat) => show(chat),
    restore: async (chat, shelf) => show(await caf.restore(chat, shelf)),
  }}
/>
```

It imports only the caf-sdk, Svelte and markdown-it, which a check in CI
holds it to, so the folder can be lifted out whole. Copy it, or read it for
what a good chat on CAF shows.

The agent's words are untrusted: an agent may repeat a web page or a file,
and HTML in it could script the page. So the view renders Markdown with
HTML off, links only `http` and `https` addresses, opening each in a new
tab with `rel="noopener noreferrer"`, and links to images rather than
loading them. A sign-in link isn't the agent's words, but it opens one the
same way, and only if it's `https`. Whatever a page built on CAF draws the
agent's words with, it should do the same.
