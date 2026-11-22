---
title: Installing and Signing In
type: how
description: Let CAF install an agent's adapter and sign in to it from a tokio program. Offer an agent CAF installs, follow where each agent stands, install it with the user's consent, run its sign-in, and let viewers do the same.
related:
  - caf/getting-started/a-chat-on-tokio
  - caf/concepts/agents-and-profiles
  - caf/concepts/viewers
---

CAF can install an agent's adapter itself and sign in to it, so your program
needs no terminal and no `npm` of its own. Everything about one agent (which
version, how to install it, how to sign in) is in its profile, so this works
the same for every agent CAF has a profile for. Today that's Claude.

## Offer an agent CAF installs

```rust
let claude = AgentSpec::installed("claude", Builtin::ClaudeAgentAcp);
let (caf, running) = Caf::<NoAppItems>::start(Config::new(Host::new("eyespy", "0.4.0"), vec![claude]));
```

CAF installs the adapter at exactly the version its profile was checked at,
in a folder of its own, named after its profile whatever you call the
agent: `claude-agent-acp-0.98.0`. Every CAF program on the machine shares
the folder these live in, and so one install of each agent: by default `~/.local/share/caf/agents`
(`$XDG_DATA_HOME/caf/agents` if that's set). `Config::installs` names
another.

An agent offered with `AgentSpec::command` is started with your own command
instead. CAF doesn't install it, but it can still sign in to it if its
profile says how.

## Follow where each agent stands

```rust
let mut agents = caf.watch_agents();
while let Some(statuses) = agents.next().await {
    for status in &statuses {
        draw_agent(status); // its install state and sign-in state
    }
}
```

`watch_agents` gives every agent's status at once, then again each time any
of it changes. If it changes faster than you read, you get only the latest.
Each `AgentStatus` has two parts.

Its **install state**:

| State | Meaning |
|---|---|
| `Host` | You start it with your own command |
| `Unknown` | CAF hasn't looked yet |
| `Missing { size }` | Not installed: installing takes about `size` bytes |
| `Installing { done, size }` | Being installed |
| `Installed { version }` | Installed |
| `Cannot(why)` | It can't be: no Node, or one too old; no npm; no build for this machine; or the last try failed, and why |

Its **sign-in state**:

| State | Meaning |
|---|---|
| `Unknown` | Nothing has said yet |
| `SignedOut` | Signed out |
| `SigningIn(link)` | The user has something to do: open `link.url`, maybe type `link.code` there, maybe paste a code back |
| `SignedIn(how)` | Signed in, with an account, an API key or a gateway |

CAF keeps how an agent is signed in, never who: the account's email never
reaches its state, its logs or a recording.

It also says whether CAF can turn off the agent's own tools
(`own_tools_off`), which it must to run it: see
[Agents and profiles](~caf/concepts/agents-and-profiles#an-agent-has-only-your-tools).

CAF looks as it starts. Call `caf.check_agents()` to look again, such as
after the user installs Node.

## Install it

Installing Claude's adapter takes about 630 MB, so it's the user's decision.
A chat opened with an agent that isn't installed waits for it
(`ChatState::WaitingForAgent`), with its messages waiting too. Show the
agent's status beside the chat, with an Install button:

```rust
caf.install(&AgentName("claude".into())).await?;
```

Progress shows in `watch_agents`. Once it's installed, every chat waiting
for it starts. If it fails, their waiting messages stop, with a note, and
the agent's status says why. `caf.stop_install` stops it, and stopping a
waiting chat stops its messages.

What CAF does to install it:

- **npm packages** are installed with `npm ci --ignore-scripts` from a
  lockfile CAF ships, so every dependency comes at the exact version and
  hash it was checked with, and no install script runs. This needs Node 22
  or later, and npm, on `PATH`. CAF never installs Node itself.
- **Downloads** come over `https` only, and are checked against their
  SHA-256 before anything is unpacked. They take their proxy from the
  environment variables npm reads (`npm_config_https_proxy`, then
  `HTTPS_PROXY` or `HTTP_PROXY`), unless `NO_PROXY` names the host or a
  domain it's in. A proxy set only in `.npmrc` is npm's alone.
- **An agent that keeps its sign-in in a folder CAF gives it** gets one,
  named after its profile like its installs, such as
  `claude-agent-acp-private`, beside them, readable only by the user. It
  outlives every version, and CAF never removes it.
- **Each install** is built in a hidden folder and renamed into place whole,
  so one cut off partway leaves nothing that looks installed. Two programs
  installing the same agent at once install it once: a lock in the folder
  makes the second wait, and it then finds the first's.
- **Old versions** are removed once the version your program pins has
  started, but only once no process, in any CAF program, has started from
  the old one for 14 days, and never while one still runs from it. So two
  programs pinning different versions don't remove each other's.

## Patched adapters

Sometimes an adapter needs a fix its upstream hasn't made yet. Rather than
wait, CAF patches it as it installs it: a profile's recipe may carry exact
edits to files npm installed, or a download unpacked, kept in CAF's
repository and compiled in, never downloaded. CAF doesn't send these
fixes upstream for now.

A patched install is of a version of its own: the upstream version, then
`+caf.` and the patch's level. Claude's adapter is installed patched, as
`0.98.0+caf.1`, in `claude-agent-acp-0.98.0+caf.1`, and
`Installed { version }` says `0.98.0+caf.1`.

Patching is an install's last step, and each file is checked three ways:

- it must be exactly the file the patch was written for, by its SHA-256;
- each edit's text must be in it exactly once, as the edits before it left
  it;
- the result must be exactly the file CAF tested, by its SHA-256 again.

The original is kept beside it, as `<file>.caf-original`, and the patched
file written whole and renamed into place. If any check fails, the install
fails with `InstallFailure::Patch`, saying which file and which check, and
nothing of it is left: CAF never runs a half-patched adapter. Trying again
downloads it afresh. If that fails the same way, the patch doesn't fit
what upstream serves, and only a newer CAF fixes it.

A later CAF may patch the same upstream version again, at a higher level.
If an earlier level of it is installed, CAF makes the new one from it,
since nothing is downloaded: it links the earlier install's files into a
new folder, puts back each original, and patches that. Nothing is changed
in place, so the earlier install, and any program still running from it,
is untouched.

- **As CAF looks at what's installed**, it does so without asking,
  unless the user stopped installing it this run (`caf.stop_install`).
  The agent shows `Installing`, then `Installed`. If this fails, the agent
  reads as `Missing`, its waiting chats go on waiting, and installing it,
  with the user's leave, downloads it as any other.
- **When you call `caf.install`**, it's made this way too, if it can be,
  even if CAF had already started making it by itself. The user has agreed
  to install it, so if this fails, CAF downloads it at once, and the agent
  goes on `Installing`.
- **A `caf.install` before CAF has first looked** shows `Installing`, and
  waits for the look to choose: made from an earlier level, downloaded,
  already installed, or, if the machine can't install it, `Cannot` and why,
  with the messages waiting for it stopped, as for an install that failed.
  If CAF can't look, it downloads.

CAF never tries to make one level from an earlier one twice in a run.

An earlier patch level of the version your program pins counts as an older
version, and is removed as one is. A later level, which another program
may pin, never is.

## Sign in

```rust
let claude = AgentName("claude".into());
caf.sign_in(&claude).await?;
```

CAF runs the sign-in the agent's profile names, never one the agent
suggests. For Claude that's its adapter's own `--cli auth login`, which
needs no terminal. Its state becomes `SigningIn`, and once the command gives
its link, `link.url` has it. Show it to the user to open. CAF has already
checked it points at one of the sites the profile allows, such as
`claude.ai`, and a sign-in whose link points anywhere else is stopped.

The user approves in their browser, and the sign-in finishes by itself. If
their browser is on another machine, the page gives them a code to paste
back instead. When `link.paste` is true, take it from them and pass it on:

```rust
caf.paste(&claude, code).await?;
```

When the sign-in ends, the state becomes `SignedIn`. A chat whose last turn
failed because the agent was signed out gets a note saying the user signed
in. Its Send again stays: CAF never resends by itself. A failed or stopped
sign-in (`caf.stop_sign_in`) goes back to how it was.

`caf.sign_out` runs the profile's sign-out command.

A failed turn whose remedy is `Remedy::SignIn` is a button for exactly this
call.

## Let viewers do it

A viewer hears every agent's status, as a `ToViewer::Agents` message right
after the welcome and again each time any of it changes. Installing and
signing in from a viewer is your decision, as opening chats is. It's off by
default:

```rust
let mut rules = ViewerRules::default();
rules.agents = true;
```

With it off, a viewer's `FromViewer::Agent` request is answered
`NotAllowed`, and it sees a sign-in under way without its link or code.
Whoever opens the link and approves it, or types the code on the
provider's site, chooses whose account the agent signs in to: a viewer
that may only watch mustn't be able to sign your agent in to its own.

## Recordings

A recording (`Config::record`) keeps what it needs to replay the run, and
nothing a sign-in should keep private:

- a pasted code is written as `null`;
- Claude's sign-in status is written as its kind alone, such as
  `{"authStatus": {"kind": "account"}}`;
- the sign-in link and the code shown on the site are kept as they came.
  Each is good for one sign-in only.

## On your own loop

A program that wires the core up itself uses `SetupDriver` like the other
drivers: hand it each `Setup` directive with `apply`, and give the core each
report from `next` as `Event::Setup`. Start each adapter process with a
shared lock on its install folder, as `AgentDriver` does with
`AgentDirective::Start`'s `hold`, so no CAF program removes the folder while
the process runs.
