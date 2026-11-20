---
title: A Chat in the Browser
type: how
description: Show and drive CAF's chats from a web page. A Rust backend serves each browser with caf_driver_tokio::viewer::serve over a WebSocket, and the page follows chats and acts on them with the TypeScript caf-sdk's Connection.
related:
  - caf/getting-started/a-chat-on-tokio
  - caf/concepts/chats-turns-and-items
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

`serve` runs until either side closes or CAF shuts down. A viewer doesn't
keep CAF running: once the host drops its own `Caf`, CAF shuts down, and
each viewer sees its chats end.

Keep the channel to the viewer bounded. While it's full, `serve` stops
taking that viewer's changes. A viewer that falls far behind gets the chat
whole again rather than every change it missed, so a slow browser never
makes CAF hold more than one picture of each chat for it.

### What viewers may do

`ViewerRules` says what viewers may do:

- **`open`** turns a viewer's request to open a chat into an `OpenChat`, or
  refuses it. The default refuses every one, so viewers can only watch and
  use chats the host opened.
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
caf.agents; // the agents a chat may be opened with
caf.chats; // the open chats: id, agent and folder
caf.onChats((chats) => drawList(chats));
```

`Connection.open` takes any `Transport`, an object that sends and hears one
JSON text per message. `webSocket` is the one the SDK ships. If CAF speaks
no version of the wire the SDK does, `open` rejects with a `CafError` whose
`why` is `NoCommonVersion`.

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
comparing objects, and redraw only that.

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

Each resolves once CAF has done it. Questions waiting for an answer are on
the chat, in `chat.questions`. When something isn't done, the promise
rejects with a `CafError`, whose `why` says why:

| `why.type` | What it means |
|---|---|
| `Refused` | The core refused it, and nothing changed. `why.value` says why, such as `QueueFull`. |
| `NotAllowed` | The host doesn't let viewers do this. |
| `Gone` | CAF has shut down, or is shutting down: nothing was done. |
| `Disconnected` | The link ended before the answer came. |

A reason added in a later version of CAF still comes through, with its
name: show it as unknown, with that name.

### When the link drops

When the link ends, for any reason, every request still waiting rejects with
`Disconnected`, and `onClose` is called. Reconnecting is the page's choice.
To pick up where it left off, connect again and watch each chat `from` the
copy its last watch had: CAF then sends only what changed since, if it still
can, and the chat whole if not.

```ts
caf.onClose(async () => {
  const again = await Connection.open(await webSocket(url));
  again.watch(chatId, listener, watching.copy ?? undefined);
});
```

`disconnect` ends the link from the page's side.

### What the SDK does with what it doesn't expect

Nothing CAF sends makes the SDK throw:

- a message of a type it doesn't know, from a later version of the wire, is
  ignored;
- a change it can't apply asks for the chat whole again, as above;
- anything else that can't be CAF's ends the link, as if it had dropped:
  text that isn't JSON, a message missing what it needs, or an answer that
  doesn't fit what was asked. Ignoring it could leave a request waiting
  forever or a chat silently wrong; ending the link tells the page, and
  rejects every request still waiting.

A listener of the page's own that throws doesn't end the link. Its error is
reported as uncaught, and the other listeners are still called.
