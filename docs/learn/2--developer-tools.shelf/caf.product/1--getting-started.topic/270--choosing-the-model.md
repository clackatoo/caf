---
title: Choosing the Model
type: how
description: Let the user choose an agent's model, how hard it thinks and fast mode, for any agent, from a tokio program. Read what the agent offers, choose for a chat or as your program's default, follow what the agent changes by itself, and learn what an agent offers before any chat.
related:
  - caf/getting-started/a-chat-on-tokio
  - caf/concepts/agents-and-profiles
  - caf/concepts/chats-turns-and-items
---

An agent says which models it has, how hard each can think, and whether it
has a fast mode, and the list differs by account, gateway and version. CAF
reads it from whichever way the agent says it, and gives you one shape for
every agent: three settings, `Model`, `Effort` and `Fast`.

## See what the agent offers

Once a chat's session opens, its `settings` hold what the agent offers and
what was chosen:

```rust
for setting in &chat.settings.offered {
    match &setting.form {
        SettingForm::Choices(choices) => draw_menu(setting, choices), // models, effort levels
        SettingForm::Switch => draw_toggle(setting),                  // fast mode
        _ => {}
    }
}
```

Each setting has the agent's own `name` for it and what the agent says it
is now (`current`). Each choice has the agent's id, its name, and maybe a
description and a group. A choice such as Claude's `default` model, which
stands for whatever the agent would pick, is passed on as it is.

What the agent offers changes as it goes: Claude's effort levels depend on
the model, and fast mode is offered only while the model has it. Read
`settings` again whenever the chat changes.

## Choose

```rust
chat.set_setting(SettingChoice::model("sonnet")).await?;
```

What's chosen is in `settings.chosen`, apart from what the agent says is
current: where they differ, the change is waiting. It's sent at once if no
turn is running, and otherwise as the turn ends, so it applies from the
next message. Only the latest choice of each setting counts.

Choices are matched exactly, by the agent's own ids: `opus` never quietly
becomes `opus[1m]`. What the agent says it uses counts as offered, even
if it isn't in its list. Once the agent has said what it offers, in this
run, a choice it doesn't offer is refused (`Refused::NoSuchSetting`). A
choice is checked against what's offered when it's made, so an effort
level only a new model has can be chosen once the model change is
answered, not with it.

A message never goes with a choice silently replaced: either it fails, or
the chat says what changed. If a choice made for the chat itself can't be
applied, because the agent doesn't offer it or refuses it, every message
waiting fails (`FailureReason::SettingNotApplied`), with Send again, and
the chat says so whether or not any waits (`Note::NotOffered`, or
`Note::NotApplied` for one refused). The setting falls back to your
program's default, if the agent offers it, and otherwise to what the
agent uses. Your program's own default that can't be applied is dropped
quietly.

A change the agent never answers doesn't fail anything: as with any
request it leaves unanswered for `Timers::request` (60 s), its program is
taken to be stuck and restarted, or, while another chat is mid-turn on
it, only this chat's session is closed. The session opens again at once
if a message waits, and otherwise with the next, and every choice is
sent again before the messages go. If the agent had lost the session,
it's opened again and the choice sent to it, as a message would be.

A choice already applied that the agent stops offering, such as fast mode
when the model changes to one without it, falls back the same way, and
the chat says so (`Note::NotOffered`); the messages go on.

## Choose before the chat opens

A chat can choose as it's opened, and your program can choose for every
chat with an agent:

```rust
let mut claude = AgentSpec::installed("claude", Builtin::ClaudeAgentAcp);
claude.settings = vec![SettingChoice::model("haiku")]; // the cheapest, for every chat

let mut open = OpenChat::new("claude", "/home/me/project");
open.settings = vec![SettingChoice::effort("high")]; // this chat's own
```

The chat's own choice comes first, then your program's default, then the
agent's own. They're sent as the session opens, before any message. A
default the agent doesn't offer is dropped quietly; a chat's own choice
that it doesn't offer fails every message waiting, saying so.

Which choices are the chat's own is saved with it (`settings.own`). A
restored chat keeps them, and takes your program's defaults as they are
then for the rest: what the agent chose by itself in an earlier run never
outranks them.

Claude reads the user's own default model even with none of their
settings, so CAF sends a chat's choices again each time its session
opens.

## What the agent changes by itself

An agent may change a setting itself, as Claude falls back to another
model when one refuses. The choice follows it, and the chat says so with a
note (`Note::AgentChanged`), in the turn it happened in. Either way, the
choice is the agent's now, not the chat's own.

Each turn records the model it was sent on (`Turn::model`).

## Learn what an agent offers before any chat

To offer models before the user opens a chat:

```rust
caf.learn_settings(&AgentName("claude".into())).await?;
```

CAF starts the agent if it isn't running, opens a session with none of
the agent's tools, reads what it offers, and closes it. No message is
sent, so it costs nothing. Where it stands is in the agent's status from
`watch_agents`, as `settings`: `Unknown`, `Learning`, `Known(settings)`
or `Failed(why)`. What was learned before stays shown while it's learned
again, and if that fails. If the agent's adapter serves chats and can't
close sessions, the session stays until the adapter stops. Every chat's
session tells it too. It isn't saved: it depends on the account and the
agent's version, so it's learned again each run.

## Saving

A chat's settings are saved with it, and a restored chat sends its choices
as its session opens, like any other.

## Viewers

A viewer chooses with `FromViewer::Set`, allowed as the rest of acting on
chats is, and learns an agent's settings with `AgentAction::LearnSettings`,
allowed as installing is.
