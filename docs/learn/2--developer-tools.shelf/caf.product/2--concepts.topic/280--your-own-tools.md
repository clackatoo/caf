---
title: Your Own Tools
type: concept
description: How an app gives its agents tools of its own, written in Rust, served beside CAF's on the same connection. A tool from a closure or the AppTool trait, its name and schema, hints for the policy, giving a chat its tools, a call step by step, input that doesn't fit, previews, jobs and their notices, the app's own item on a call, and trying a tool in a test.
related:
  - caf/concepts/serving-tools
  - caf/concepts/cafs-own-tools
  - caf/concepts/tool-calls
  - caf/standard-interface/transcript
---

An app can give its agents tools of its own: trim a clip in a video
editor, move the map in a mapping app, count the words in a text. Each is
a Rust function with a typed input, and CAF serves it beside its own
**Read**, **Edit**, **Write** and **Bash**, on the same `caf` connection
(see [Serving tools](~caf/concepts/serving-tools)). The agent sees it as
`mcp__caf__trim_clip`, and CAF treats a call to it as it treats a call to
its own: it reads the input, asks the policy, and the user if need be,
runs it, and records it, so the app draws it as it draws any call.

## A tool from a closure

`tool` builds a tool from a name and an async closure, for tools too
small for a type of their own:

```rust
use caf_driver_tokio::{AppTools, Done, tool};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Trim {
    /// The clip to trim.
    clip: u64,
    /// Its new length, in seconds.
    seconds: f64,
}

let trim = tool("trim_clip", |_call, input: Trim| async move {
    // Ask the app's UI thread to do it, through a handle the closure
    // captured, and await its answer: never block on it.
    Done::answer(format!("Trimmed clip {} to {}s", input.clip, input.seconds))
})
.called("Trim a clip")
.title(|input: &Trim| format!("Trim clip {}", input.clip))
.describe("Trims a clip to a length in seconds.")
.destructive();

config.tools.app = AppTools::new().with(trim)?;
```

The app registers its tools once, as CAF starts (`ToolsConfig::app`), and
they're fixed for the run. The builder's methods:

| Method | What it says |
| --- | --- |
| `called(..)` | What it's called in general, such as *Trim a clip*: what a call whose input doesn't fit is titled. Its name by default. |
| `title(..)` | Each call's title in the transcript, from its input, such as *Trim clip 3*. The tool's name by default. |
| `describe(..)` | What it does, for the model. |
| `read_only()`, `destructive()`, `open_world()` | Its hints (below). |
| `kind(..)` | The kind of tool its calls are, for an icon: `ToolKind::Other` by default. |

Rust doesn't take a type from its default, so a tool is best built where
it's registered, as above, which says the app's item type. Built on its
own, by an app with no items of its own, its closure names it:
`Done::<NoAppItems>::answer(..)`.

## A tool of its own type

A tool with more to it, such as one that previews, is a type of its own,
implementing `AppTool`:

```rust
use caf_driver_tokio::{AppTool, BoxFuture, Call, Done, ToolSpec};
use caf_core::tool::{Piece, Text, ToolKind};

struct TrimClip {
    /// A channel to the app's UI thread.
    ui: Ui,
}

impl AppTool<Edit> for TrimClip {
    type Input = Trim;

    fn spec(&self) -> ToolSpec {
        let mut spec = ToolSpec::new("trim_clip");
        spec.title = "Trim a clip".into();
        spec.description = "Trims a clip to a length in seconds.".into();
        spec.hints.destructive = true;
        spec.hints.kind = ToolKind::Edit;
        spec.previews = true;
        spec
    }

    fn title(&self, input: &Trim) -> String {
        format!("Trim clip {}", input.clip)
    }

    fn preview(&self, _call: &Call, input: &Trim) -> Option<BoxFuture<Result<Vec<Piece>, Done<Edit>>>> {
        let ui = self.ui.clone();
        let (clip, seconds) = (input.clip, input.seconds);
        Some(Box::pin(async move {
            match ui.length(clip).await {
                Some(length) => Ok(vec![Piece::Text(Text::new(format!("{length}s → {seconds}s")))]),
                None => Err(Done::failed(format!("There's no clip {clip}"))),
            }
        }))
    }

    fn run(&self, _call: Call, input: Trim) -> BoxFuture<Done<Edit>> {
        let ui = self.ui.clone();
        Box::pin(async move {
            let edit = ui.trim(input.clip, input.seconds).await;
            Done::answer(format!("Trimmed clip {} to {}s", input.clip, input.seconds))
                .with_item(edit)
        })
    }
}
```

`A`, here `Edit`, is the app's item type: what a call may leave on its
record (below). The tool is the app's own value, holding whatever handles
it needs to reach the app's state, such as a channel to its UI thread.
Only the futures it gives must be `Send`: its state needn't move between
threads.

`ToolSpec::new` names a tool titled as named, with no description, every
hint off, of kind Other, not previewing, with schemars' schema. `spec` is
asked once, as the tool is registered.

**Never block.** `title`, `preview` and `run` are called on CAF's own
task, so each must give its answer, or its future, at once: `title` must
be cheap, and `preview` and `run` must do their work in the future they
give, not before. Each future runs as a task of its own, on two threads
the app's tools have to themselves, named `caf-app-tool-N`. One that
awaits what it waits for, such as the UI thread's answer, holds nothing
up, however long it takes. One that blocks anyway ties up one of those
threads: it can hold up the app's other tools, but never CAF, its own
tools or its other drivers (if those threads can't be started, which is
logged, they share CAF's). And a future blocking a thread can't be
stopped (below).

A task the tool starts with `tokio::spawn` runs on those threads too,
and is dropped when CAF shuts down, wherever it's got to. Work that must
outlive CAF, or the call, belongs on the app's own runtime: give the
tool a handle to it (`tokio::runtime::Handle`), and spawn it there.

## Names and schemas

`AppTools::with` checks each tool as it's registered, and refuses one with
a `ToolError`:

| `ToolError` | Its name or schema |
| --- | --- |
| `BadName` | Has a character other than `A-Z`, `a-z`, `0-9`, `_` and `-`, or is empty, or longer than 54 bytes: Claude names a tool `mcp__caf__` and its name, and Claude's API takes names of at most 64 bytes. |
| `Taken` | Is another of the app's tools', in any case: `trim` and `Trim` can't both be. |
| `Cafs` | Is one of CAF's own, in any case: no tool may be called `read` or `TaskStop`. |
| `BadSchema` | Isn't an object's. |

The tool's input is its `Input` type: serde reads it from the model's
JSON, and schemars describes it to the model, each field's doc comment as
its description. CAF serves that schema without its `$schema`, which says
only which draft it is. `ToolSpec::schema` gives one of the app's own in
its place, which must be an object's too.

A description is the model's only guide to the tool, so it says what the
tool does and when to use it. Agents cut one longer than 4,096
characters.

## Hints

A tool's hints say what it does, for the policy, and what kind of tool its
calls are:

| `Hints` | Means |
| --- | --- |
| `read_only` | It only reads: it changes nothing anywhere. |
| `destructive` | It may change or delete what was there, not only add to it. |
| `open_world` | It reaches beyond the app and the chat's folder, such as the web. |
| `kind` | The kind of tool its calls are, for icons and counting. |

Each is off by default, and the kind is Other. The policy is given them
with every call CAF asks the policy about, to a tool it serves
(`PermissionRequest::hints`): an app's tool's from its spec, and CAF's
own from `CafTool::hints`, so a policy can allow every read-only call
without knowing the tools by name. A call the agent reports itself, to a
tool CAF doesn't serve, has none. The agent is given them too, all but
the kind, as MCP's annotations.

## Giving a chat its tools

A chat is given the app's tools by name, after CAF's:

```rust
let tools = Tools::caf([CafTool::Read]).and_app(["trim_clip", "word_count"]);
let chat = caf.open_chat(OpenChat::new("claude", folder).with_tools(tools)).await?;
```

A chat may have the app's tools and none of CAF's; it's served TaskStop
with them, so their jobs can be stopped. The agent is listed CAF's tools
first, then the app's, in the order given, each once, and is told of
both in its instructions: "framewright gives you tools to read files
(Read) and stop background tasks (TaskStop), in the folder you're working
in, and tools of its own (trim_clip, word_count), asking the user's leave
where they must." What each of the app's tools does is its description's
to say.

A chat given a tool the app didn't register is refused, as it opens and as
it's restored (`Refused::UnknownTool`). A chat's tools aren't saved: give
them again as it's restored (`RestoreChat::tools`), as for CAF's own. A
call to one of the app's tools that the chat wasn't given is refused, and
never runs.

## A call, step by step

1. **Its input is read** with serde, and its title worked out with
   `title`, before CAF records the call: so the transcript never shows a
   call without its title. A call whose input doesn't fit, or whose
   `title` is empty, gets the tool's own title (`called`, or
   `ToolSpec::title`). Its origin is `Origin::App` with the tool's name,
   its action `Action::Tool`, its kind its hints'.
2. **It's previewed,** if the tool previews (`ToolSpec::previews`), before
   anyone's asked: what it would do, such as the change it would make,
   which the question shows. A preview that fails, as `Err(Done)`, ends the
   call Failed, with what it says, and asks no one.
3. **The policy decides,** with the tool's hints, and asks the user if it
   must.
4. **It runs:** `run` is given the `Call` and the input. It may print a
   running command's output (`Call::print`) and answer early, going on as
   a job (`Call::answer_now`, below).
5. **It ends** with a `Done`: Done or Failed, the model's answer, and what
   the transcript keeps. The call's record is what it previewed, then
   what it recorded.

`Call` also gives the chat's folder (`cwd`), the input as the model wrote
it (`input`), and the agent's own id for the call (`agent_call`).

| `Done` | Is |
| --- | --- |
| `Done::answer(text)` | It did it, and the model is told `text`. |
| `Done::failed(why)` | It failed: the model is told why, as an error, and the transcript keeps it. |
| `.with_record(pieces)` | What the transcript keeps, such as a diff or a command's output. |
| `.with_item(item)` | The app's own item for the call. |

**Input that doesn't fit** its type, such as text where a number goes,
ends the call Failed at once, titled as the tool is ("Trim a clip"), and
asks no one, as there's nothing to ask about. The model is told what serde
said, so it can call again: *Refused: invalid type: string "three",
expected u64*.

**Stopping** a call drops its future: the user stopped the turn, or
closed the chat; the policy or the user refused it; the agent gave up on
it; or the chat was put away, or CAF shut down. A tool stops its work
when its future is dropped. CAF aborts the future's task first, and tells
the model the call was stopped or refused only once the future has been
dropped, so the model never hears it was while the work goes on. A tool
that doesn't stop within five seconds, such as one blocking its thread,
has its call answered all the same, and CAF logs that the app's tool
didn't stop.

A call stopped with its turn ends only once its task has: CAF waits, for
the same five seconds, for what the task gives. Aborted before it
finished, the call ends Cancelled, and the model is told it was
cancelled. If its future finished first, as one does that had finished
already, or that blocks its thread until its end and so can't be stopped,
it ends as it went instead, with its record and your item, and the model
is told what it gave: the record never says Cancelled for work your tool
did, and your item is never lost. A panic, even as the future is dropped,
fails the call, as any panic does. And if five seconds pass first, the
call ends Cancelled all the same.

**A panic** in `title` titles the call as the tool is; one in `preview`
or `run`, in their futures, or as a future is dropped, ends the call
Failed: the model is told *The app's tool failed unexpectedly*, and the
log names the tool.

## Jobs

A tool can answer the model early and go on, as Bash in the background
does. `Call::answer_now` answers the model with what it makes of the job's
number, and the run's future carries on as that job:

```rust
let render = tool("render", |mut call: Call, input: Render| async move {
    call.answer_now(|job| Answer::text(format!("Rendering, as task {}", job.0)));
    let file = render_scene(&input.scene).await;
    Done::answer(format!("Rendered {file}"))
})
.called("Render a scene")
.title(|input: &Render| format!("Render the {}", input.scene));

config.tools.app = AppTools::new().with(render)?;
```

The job ends as any does (see
[Serving tools](~caf/concepts/serving-tools#background-jobs)): a chat
given any of the app's tools is served TaskStop too, so the agent can
stop it, and a job's call is cancelled when its chat is closed. TaskStop
is answered once the job's future has been dropped, or five seconds have
passed, as any stopped call is. When it ends by itself, the model never
heard the `Done`'s answer, so CAF tells the agent in a notice, laid out
as its own are, with the answer as its result:

```text
<task-notification>
<task-id>1</task-id>
<tool-use-id>toolu_01</tool-use-id>
<status>completed</status>
<summary>Background task "Render the intro" completed</summary>
<result>Rendered intro.mp4</result>
</task-notification>
```

The task's id is the job's number, and `<tool-use-id>` is there only if
the agent gave an id for the call. The summary names the call by its
title, and the status is `failed`, as the summary says, for a `Done` that
failed. The result is the answer's text parts, joined by newlines:
pictures are dropped, and an answer with no text has no `<result>`. Its
`&`, `<` and `>` are written as `&amp;`, `&lt;` and `&gt;`, as is
everything in a notice. A result longer than an answer may be, 30,000
characters, is cut there, and ends saying so: *[Cut here: the first
30000 of its 45210 characters were kept]*.

## The app's own item on a call

A call may leave the app's own item on its record: `Done::with_item`. It
arrives with the call's end, and becomes `ToolCall::app`, which the app's
own view draws, such as the edit a trim made, with a button to undo it.
It's set once, as the call ends: no updates while it runs. A preview
that fails ends the call, so the item its `Done` has is the call's. The
model never sees it: it's told the `Done`'s answer.
Only the app's own tools may leave one: an item on a call to one of CAF's
is dropped, with a warning in the log.

It's saved with the chat, sent to viewers, and kept as `Unknown` by a
viewer that can't read it, as the app's items are (see
[Chats, turns, and items](~caf/concepts/chats-turns-and-items)), its
`summary` standing in for it.

## Trying a tool

`try_tool` calls a tool the way CAF would, with no agent and no CAF: it
reads the input, failing as CAF would if it doesn't fit, works out the
title, previews, and runs it, in a chat of its own whose folder is the one
given:

```rust
#[tokio::test]
async fn trimming_answers_and_leaves_the_edit() {
    let tried = try_tool(&trim, json!({ "clip": 3, "seconds": 2.5 }), Path::new("/")).await;
    assert_eq!(tried.title, "Trim clip 3");
    assert_eq!(tried.done.answer, Answer::text("Trimmed clip 3 to 2.5s"));
}
```

| `Tried` | Has |
| --- | --- |
| `title` | The call's title. |
| `preview` | What it previewed, if it previews. |
| `done` | What it ended with, or the answer CAF would give if its input didn't fit. |
| `printed` | The lines it printed as it ran; not what it printed previewing, which CAF doesn't keep. |
| `answered_now` | What the model was answered early, if it went on as a job. |

The debugger has a tool of its own, `word_count`, given to every chat
given CAF's tools: a small example to read (see
[The debugger](~caf/getting-started/the-debugger)).
