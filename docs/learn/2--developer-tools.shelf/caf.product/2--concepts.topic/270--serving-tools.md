---
title: Serving Tools
type: concept
description: How CAF gives an agent its own tools, Read, Edit, Write and Bash, and the app's, over MCP. The one line every app adds to its main, choosing a chat's tools, how a connection is bound to its chat and what that does and doesn't guarantee, keeping long calls alive, cancelling, background jobs, and losing the tools mid-session.
related:
  - caf/concepts/tool-calls
  - caf/concepts/cafs-own-tools
  - caf/concepts/your-own-tools
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

What the tools themselves do is [CAF's own tools](~caf/concepts/cafs-own-tools).
The app can add tools of its own, written in Rust, which CAF serves beside
its own, on the same connection, and treats as it treats its own: see
[Your own tools](~caf/concepts/your-own-tools).

> [!NOTE]
> Windows isn't supported yet: serving needs Unix sockets.

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
`relay_if_asked`, `Caf::try_start` returns `StartError::NoRelayHook`, whose
message says to add it, rather than every chat's tools failing later. So
does anything else that keeps CAF from serving, as another `StartError`:
`SocketPathTooLong` for a socket path that's too long, and `Serving` for
the rest. Nothing is started then. `Caf::start` panics with the same
message.

CAF listens on a socket in `$XDG_RUNTIME_DIR/caf`, or a folder of its own
in the temp folder if there's no runtime folder, readable only by the user
(`ToolsConfig::folder` says another). The socket is named after your program
and its process, numbered if the process starts more than one CAF, and
removed when CAF shuts down. A socket's path is
limited to about 108 bytes; the error for a longer one names the path.

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

Give it the app's own tools by name, after CAF's, with
`Tools::caf(..).and_app(["trim_clip"])`. A chat may have the app's and
none of CAF's. A name the app didn't register refuses the chat, as it
opens and as it's restored (`Refused::UnknownTool`).

A chat's tools aren't saved: give them again when you restore it
(`RestoreChat::tools`), and in what `ViewerRules::restore` returns, for
chats a viewer restores. A chat restored without them has none.

Giving a chat tools while serving is off (`Relay::None`) is refused
(`Refused::ToolsNotServed`).

For a chat with tools, CAF adds its `caf` server to the MCP servers it gives
the agent, and tells the agent about them in its instructions, in place of
saying it has none. The server lists CAF's tools first, then the app's, in
the order given, each with its hints, all but the kind, as MCP's
annotations. The agent may call only the tools the chat was given: a call
to any other is answered as an error, and never runs.

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
| `User` | The user stopped the turn, or closed or deleted the chat: CAF cancels the call, stops its work, and answers the model that the user stopped it. Or the agent gave up on the call as the user stopped the turn: CAF stops its work, and answers nothing, as MCP says. |
| `Agent` | The agent cancelled it while its turn wasn't being stopped, or its relay went: it closed, or a newer one replaced it. CAF stops its work, and answers nothing, as MCP says. |

A turn that ends by itself with a call still open doesn't cancel it: the
call ends Failed, and `stopped_by` stays empty. So does a turn cut off
as the chat is put away or CAF shuts down: its calls end Failed, stopped
by no one.

Stopping a call's work drops it, so a tool still running is stopped too.
For one of the app's tools, whose work runs as a task of its own, CAF
aborts the task, and answers the model only once its future has been
dropped, or five seconds have passed (see
[Your own tools](~caf/concepts/your-own-tools#a-call-step-by-step)).

**A preview that fails** (an edit to a file not read first, say) ends its
call Failed with the reason, and no one is asked: the tool's driver has
already given the model the reason. So does a call to one of the app's
tools whose input doesn't fit its type: the model is told what was wrong
with it.

**The policy** is given the hints of every call CAF asks the policy
about, to a tool it serves (`PermissionRequest::hints`): whether the tool
only reads, may change or delete what was there, or reaches beyond the
app and the chat's folder, and its kind. Every tool CAF serves has them: CAF's own
from `CafTool::hints`, the app's from its spec.
A call the agent reports itself, to a tool CAF doesn't serve, has none.

## Background jobs

A call can carry on after the model has been answered, as a **job**: for
example, Bash run with `run_in_background`, or a command still running when
its timeout is reached. Its driver reports the call `Detached` before
answering the model. It stays Running, and its `job` gives its number among
the chat's jobs (1, 2, …), which is how the model names it. A job keeps
running when:

- its turn ends, or the user stops it (Stop stops the turn, not its
  background work);
- its relay goes;
- a turn the agent started by itself goes quiet.

It ends in one of these ways:

| How | Ends |
| --- | --- |
| By itself | Done or Failed, and the agent hears of it in a **notice turn**, worded by the driver: at once, or after the turn running. A job of one of the app's tools is told with its answer as the notice's result (see [Your own tools](~caf/concepts/your-own-tools#jobs)) |
| TaskStop, from the model | Cancelled, `stopped_by` `Agent`. TaskStop needs no permission, and stops only jobs of its own chat: for any other number, the model is told "No task {id} in this conversation" |
| The chat closed or deleted | Cancelled, `stopped_by` `User` |
| The chat put away, or CAF shutting down | Failed, stopped by no one |

A restored chat has no jobs: one saved while running ends Failed. TaskStop
is served with Bash automatically, and with any of the app's tools, which
can go on as jobs too. A notice turn is `Starter::Notice`: the
app can send its own with `Core::notify`, and it's queued like a message,
but never counts against the host's limit on messages waiting
(`queued_messages`): only the user's messages do.
The user's Stop drops the messages still waiting, but not the notices:
dropping a notice would mean the agent never learns that its job ended,
and sending it at once would set the agent working again just after the
user stopped it. So from the Stop on, every notice is **held**: those
waiting, and any job's that comes after, without starting the agent for
it. They're all sent just before the user's next message, or the app's
next notice, each as its own turn, in order.

Each chat also keeps a **shell folder**: where its last command's shell
ended, starting at its folder. Its next Bash command runs there. With
commands run side by side, the one that finishes last sets it. A job's
end doesn't change it, a folder that isn't an absolute path of at most
4096 bytes is ignored, and a restored chat starts again at its folder.

The driver must keep a job running when its relay goes. It reports a job
that ends by itself with a notice. When TaskStop stops a job, it reports
the job without one, before the TaskStop's own end. If the job ended by
itself first, it ends as it did, and TaskStop answers "No task {id} in
this conversation".

## Losing the tools

If a chat's relay goes while its session is still open, as when the
adapter's MCP server crashes, the chat loses CAF's tools until its session
is opened again. CAF ends the calls that were open, and adds a note to the
chat, once per session, so the user knows why the agent can't use them:

> CAF's tools stopped working for this conversation; they'll be back when
> it's opened again.

Nothing is noted when the chat is closing anyway.
