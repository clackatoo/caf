---
title: Serving Tools
type: concept
description: How CAF gives an agent its own tools, Read, Edit, Write and Bash, over MCP. The one line every app adds to its main, choosing a chat's tools, how a connection is bound to its chat and what that does and doesn't guarantee, keeping long calls alive, cancelling, losing the tools mid-session, and what isn't built yet.
related:
  - caf/concepts/tool-calls
  - caf/concepts/agents-and-profiles
  - caf/getting-started/a-chat-on-tokio
  - caf/getting-started/saving-and-restoring-chats
---

CAF turns off an agent's own tools and gives it its own instead:
**Read**, **Edit**, **Write** and **Bash**
(see [Tool calls](~caf/concepts/tool-calls#caf-runs-the-tools-that-matter)).
It serves them to the agent over MCP, as a server named `caf`, from inside
your program, so it knows exactly what each call does and can ask your
policy, and the user, before anything runs.

> [!NOTE]
> This is being built in steps. Today CAF serves the tools, binds each
> connection to its chat, and records each call, but the tools themselves
> aren't built yet: **every call fails**, with "CAF's tools aren't built
> yet". Windows isn't supported yet either: serving needs Unix sockets.

## The relay

An agent's adapter starts an MCP server as a program of its own, talking
over its stdin and stdout. CAF's tools live in your program, so the program
the adapter starts is a *relay*: a small program that connects to your
program over a Unix socket and copies bytes both ways.

The relay is your own program, run again. Make this the first line of your
`main`, before anything else runs:

```rust
fn main() {
    caf_relay::relay_if_asked();
    // ...the rest of your program
}
```

When your program is started as a relay, `relay_if_asked` relays until the
agent is done with it, and exits; it never returns. Otherwise it returns at
once, and costs nothing. It needs nothing but the standard library, and
writes nothing to stdout but what it relays.

Then turn serving on: CAF serves no tools unless you ask it to.

```rust
let mut config = Config::new(Host::new("eyespy", "0.4.0"), vec![claude]);
config.tools.relay = Relay::ThisProgram;
```

| `Relay` | What CAF runs as the relay |
| --- | --- |
| `None` | Nothing: no tools are served. The default. |
| `ThisProgram` | Your program, which must call `relay_if_asked`. |
| `Command(SpawnCommand)` | Another program that relays as `caf-relay` does. |

With `ThisProgram` or `Command`, CAF checks the relay as it starts, by
running it once and checking its answer. If your `main` doesn't call
`relay_if_asked`, `Caf::start` panics, with a message that says to add
it, rather than every chat's tools failing later. So does anything else
that keeps CAF from serving, such as a socket path that's too long.

CAF listens on a socket in `$XDG_RUNTIME_DIR/caf`, or a folder of its own
in the temp folder if there's no runtime folder, readable only by the user
(`ToolsConfig::folder` says another). The socket is named after your program
and its process, and removed when CAF shuts down. A socket's path is
limited to about 108 bytes; the panic for a longer one names the path.

## Choosing a chat's tools

A chat gets no tools unless you give it some:

```rust
let chat = caf
    .open_chat(OpenChat::new("claude", "/home/me/project").with_tools(Tools::caf([
        CafTool::Read,
        CafTool::Edit,
        CafTool::Write,
        CafTool::Bash,
    ])))
    .await?;
```

A chat's tools aren't saved: give them again when you restore it
(`RestoreChat::tools`), and in what `ViewerRules::restore` returns, for
chats a viewer restores. A chat restored without them has none.

Giving a chat tools while serving is off (`Relay::None`) is refused
(`Refused::ToolsNotServed`).

For a chat with tools, CAF adds its `caf` server to the MCP servers it gives
the agent, and tells the agent about them in its instructions, in place of
saying it has none. The agent may call only the tools the chat was given: a
call to any other is answered as an error, and never runs.

## How a connection finds its chat

Every chat's relay connects to the same socket, so CAF must know which chat
each connection is for, and that the relay is one it started:

- **A key per run.** CAF draws a random key from the system each time it
  starts. It's never saved, and recordings and the debugger hide it.
- **A token per chat.** The relay is started with its chat's id and a token
  in its environment: the key's HMAC of the chat's id. CAF checks the token
  before anything else, and closes a connection with a wrong one, or for a
  chat that's unknown or has no tools. A token is no use for any other
  chat, nor in another run.
- **A private folder.** Only the user can reach the socket.
- **The newest connection wins.** When the agent opens a chat's session
  again, a new relay may connect before the old one has gone. The new one is
  bound to the chat, and the old one closed, along with its open calls.

Each call then belongs to the chat whose connection it came on, which is
how CAF knows a call's origin without trusting anything the model wrote.

**What it doesn't guarantee.** This keeps one chat's agent from calling
tools as another chat by accident, or by guessing. It isn't isolation. An
agent that can run commands as the user, such as with Bash, can read other
processes' environments, a relay's token included, and could connect as
another chat. CAF's policy rules, and the user's answers, are guard rails
on what the model asks for, not a sandbox. Keeping agents truly apart needs
each one in a sandbox of its own.

## Long calls, and cancelling

**Long calls stay alive.** An agent may give up on a call that's silent for
too long. While a call is open, CAF sends the agent progress every minute,
if the agent asked for it: "Waiting for permission" while the user decides,
then "Running".

**Cancelling.** A call ends Cancelled (see
[Where it is](~caf/concepts/tool-calls#where-it-is)), and its `stopped_by`
says who stopped it:

| `StoppedBy` | Means |
| --- | --- |
| `User` | The user stopped the turn, or closed, put away or deleted the chat; or the agent gave up on the call as the user stopped the turn. CAF stops its work, and answers the model that the user stopped it. |
| `Agent` | The agent cancelled it while its turn wasn't being stopped, or its relay went. CAF stops its work, and answers nothing, as MCP says. |

A turn that ends by itself with a call still open doesn't cancel it: the
call ends Failed, and `stopped_by` stays empty.

Stopping a call's work drops it, so a tool still running is stopped too.

## Losing the tools

If a chat's relay goes while its session is still open, as when the
adapter's MCP server crashes, the chat loses CAF's tools until its session
is opened again. CAF ends the calls that were open, and adds a note to the
chat, once per session, so the user knows why the agent can't use them:

> CAF's tools stopped working for this conversation; they'll be back when
> it's opened again.

Nothing is noted when the chat is closing anyway.
