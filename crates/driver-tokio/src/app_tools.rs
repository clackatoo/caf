//! The app's own tools: typed Rust tools CAF serves its agents beside its
//! own (Read, Edit, Write, Bash), on the same `caf` connection.
//!
//! A tool is an [`AppTool`]: a name, a typed input whose JSON schema
//! schemars works out, hints, a title for each call, and an async
//! function. The app registers its tools once, as CAF starts
//! ([`ToolsConfig::app`](crate::ToolsConfig::app)), and gives a chat the
//! ones it should have by name ([`Tools::app`](caf_core::tool::Tools::app)).
//! A call to one goes the way a call to CAF's own tools does: its input is
//! read (and refused, asking no one, if it doesn't fit), it's previewed if
//! the tool previews, the policy decides on it with its hints, and it's
//! run, printing output and answering early as a job if it likes. What it
//! gives the model, and what it gives the transcript, including the app's
//! own item ([`ToolCall::app`](caf_core::tool::ToolCall::app)), are kept
//! apart.
//!
//! [`tool`] builds one from a closure, for tools too small for a type of
//! their own; [`try_tool`] runs one in a test, with no agent and no CAF.
//!
//! ```ignore
//! #[derive(serde::Deserialize, schemars::JsonSchema)]
//! struct Trim {
//!     /// The clip to trim.
//!     clip: u64,
//!     /// Its new length, in seconds.
//!     seconds: f64,
//! }
//!
//! let trim = tool("trim_clip", |call, input: Trim| async move {
//!     // Ask the app's UI thread to do it, through a handle the closure
//!     // captured, and wait for its answer.
//!     Done::answer(format!("Trimmed clip {} to {}s", input.clip, input.seconds))
//! })
//! .title(|input: &Trim| format!("Trim clip {}", input.clip))
//! .describe("Trims a clip to a length in seconds.")
//! .destructive();
//!
//! config.tools.app = AppTools::new().add(trim)?;
//! ```

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use caf_core::chat::{AppItem, NoAppItems};
use caf_core::mcp::Answer;
use caf_core::tool::{AppToolSpec, Hints, MOST_TOOL_NAME, Piece};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::tools::Call;

/// A boxed future, `Send`, as a tool's work is.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// One of the app's own tools. `A` is the app's item type, which a call
/// may leave on its record ([`Done::item`]).
///
/// The tool is the app's own value: it holds whatever handles it needs to
/// reach the app's state, such as a channel to framewright's UI thread.
/// Only its futures must be `Send`; its state needn't move between
/// threads.
pub trait AppTool<A: AppItem = NoAppItems>: Send + Sync + 'static {
    /// What it's given: serde reads it from the model's JSON, and
    /// schemars describes it to the model, its doc comments as the
    /// fields' descriptions.
    type Input: DeserializeOwned + JsonSchema + Send + 'static;

    /// Its name, title, description and hints. Asked once, as it's
    /// registered.
    fn spec(&self) -> ToolSpec;

    /// A call's title in the transcript, such as "Trim clip 3".
    fn title(&self, input: &Self::Input) -> String;

    /// What the call would do, without doing it, shown before anyone's
    /// asked about it (such as the change it would make), or what it
    /// ends with, failed, if it can't be done. `None` (the default) for a
    /// tool that doesn't preview: [`ToolSpec::previews`] says which.
    fn preview(
        &self,
        call: &Call,
        input: &Self::Input,
    ) -> Option<BoxFuture<Result<Vec<Piece>, Done<A>>>> {
        let _ = (call, input);
        None
    }

    /// Does it. It may print a running command's output
    /// ([`Call::print`]) and answer the model early, going on as a job
    /// ([`Call::answer_now`]). Dropping the future cancels it: it must
    /// stop its work then.
    fn run(&self, call: Call, input: Self::Input) -> BoxFuture<Done<A>>;
}

/// What a tool says of itself, as it's registered.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    /// Its name, as the model calls it: `[A-Za-z0-9_-]`, at most
    /// [`MOST_TOOL_NAME`] bytes, and never one of CAF's own, in any case.
    pub name: String,
    /// What it's called in general, such as "Trim a clip".
    pub title: String,
    /// What it does, for the model: at most 4096 characters, as agents
    /// cut longer ones.
    pub description: String,
    /// What it says of itself, for the policy and the call's kind.
    pub hints: Hints,
    /// Whether it previews ([`AppTool::preview`]).
    pub previews: bool,
    /// The input's JSON schema, if not the one schemars works out from
    /// [`AppTool::Input`]: an object schema.
    pub schema: Option<serde_json::Value>,
}

impl ToolSpec {
    /// A tool named `name`, titled as named, with no description, not
    /// read-only, not destructive, not open-world, of kind Other, not
    /// previewing, with schemars' schema.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let _ = name;
        todo!()
    }
}

/// What a call to one of the app's tools ends with.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Done<A: AppItem = NoAppItems> {
    /// Whether it did what it was asked: the call ends Done or Failed.
    pub done: bool,
    /// What the model is told: text and pictures, as an error if it
    /// failed.
    pub answer: Answer,
    /// What the transcript keeps, such as a diff or a command's output.
    pub record: Vec<Piece>,
    /// The app's own item for the call, drawn by the app's own view.
    pub item: Option<A>,
}

impl<A: AppItem> Done<A> {
    /// It did it, and the model is told `text`.
    #[must_use]
    pub fn answer(text: impl Into<String>) -> Self {
        let _ = text;
        todo!()
    }

    /// It failed, and the model is told why.
    #[must_use]
    pub fn failed(why: impl Into<String>) -> Self {
        let _ = why;
        todo!()
    }

    /// With these pieces for the transcript.
    #[must_use]
    pub fn with_record(self, record: Vec<Piece>) -> Self {
        let _ = record;
        todo!()
    }

    /// With the app's own item for the call.
    #[must_use]
    pub fn with_item(self, item: A) -> Self {
        let _ = item;
        todo!()
    }
}

/// The app's own tools, registered once, as CAF starts.
#[allow(dead_code)] // filled in by feature 13's build
pub struct AppTools<A: AppItem = NoAppItems> {
    tools: BTreeMap<String, Box<dyn Erased<A>>>,
}

impl<A: AppItem> AppTools<A> {
    /// None.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    /// With `tool` too.
    ///
    /// # Errors
    ///
    /// [`ToolError`] if its name isn't one a model can call, is taken
    /// (by another of the app's or, in any case, by one of CAF's own), or
    /// its schema isn't an object's.
    pub fn add(self, tool: impl AppTool<A>) -> Result<Self, ToolError> {
        let _ = tool;
        todo!()
    }

    /// What the core knows of each, in the order served.
    #[must_use]
    pub fn specs(&self) -> Vec<AppToolSpec> {
        todo!()
    }
}

impl<A: AppItem> Default for AppTools<A> {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a tool can't be registered.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolError {
    /// Its name has a character other than `[A-Za-z0-9_-]`, or is empty,
    /// or is longer than [`MOST_TOOL_NAME`] bytes.
    BadName(String),
    /// Another of the app's tools has its name.
    Taken(String),
    /// One of CAF's own tools has its name, in some case.
    Cafs(String),
    /// Its schema isn't an object's.
    BadSchema(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = formatter;
        todo!()
    }
}

impl std::error::Error for ToolError {}

/// A tool [`tool`] built from a closure.
#[allow(dead_code)] // filled in by feature 13's build
pub struct Built<I, F, A: AppItem = NoAppItems> {
    spec: ToolSpec,
    run: F,
    title: Option<Box<dyn Fn(&I) -> String + Send + Sync>>,
    item: std::marker::PhantomData<fn() -> A>,
}

/// A tool named `name` that runs `run`, for tools too small for a type of
/// their own: give it a title, a description and hints with the
/// builder's methods.
pub fn tool<I, F, Fut, A>(name: impl Into<String>, run: F) -> Built<I, F, A>
where
    I: DeserializeOwned + JsonSchema + Send + 'static,
    F: Fn(Call, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Done<A>> + Send + 'static,
    A: AppItem,
{
    let _ = (name, run);
    todo!()
}

impl<I, F, A: AppItem> Built<I, F, A> {
    /// Each call's title, from its input. The tool's name by default.
    #[must_use]
    pub fn title(self, title: impl Fn(&I) -> String + Send + Sync + 'static) -> Self {
        let _ = title;
        todo!()
    }

    /// What it does, for the model.
    #[must_use]
    pub fn describe(self, description: impl Into<String>) -> Self {
        let _ = description;
        todo!()
    }

    /// It only reads.
    #[must_use]
    pub fn read_only(self) -> Self {
        todo!()
    }

    /// It may change or delete what was there.
    #[must_use]
    pub fn destructive(self) -> Self {
        todo!()
    }

    /// It reaches beyond the app and the chat's folder.
    #[must_use]
    pub fn open_world(self) -> Self {
        todo!()
    }

    /// The kind of tool its calls are.
    #[must_use]
    pub fn kind(self, kind: caf_core::tool::ToolKind) -> Self {
        let _ = kind;
        todo!()
    }
}

impl<I, F, Fut, A> AppTool<A> for Built<I, F, A>
where
    I: DeserializeOwned + JsonSchema + Send + 'static,
    F: Fn(Call, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Done<A>> + Send + 'static,
    A: AppItem,
{
    type Input = I;

    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn title(&self, input: &I) -> String {
        let _ = input;
        todo!()
    }

    fn run(&self, call: Call, input: I) -> BoxFuture<Done<A>> {
        let _ = (call, input);
        todo!()
    }
}

/// What a test call to a tool gave ([`try_tool`]).
#[non_exhaustive]
#[derive(Debug)]
pub struct Tried<A: AppItem = NoAppItems> {
    /// Its title.
    pub title: String,
    /// What it previewed, if it previews.
    pub preview: Option<Result<Vec<Piece>, Done<A>>>,
    /// What it ended with: or, if its input didn't fit, the model's answer
    /// saying why, failed, as CAF gives it.
    pub done: Done<A>,
    /// The output it printed, as lines.
    pub printed: Vec<String>,
}

/// Calls `tool` with `input` the way CAF would, with no agent and no CAF:
/// reads the input (failing as CAF would, if it doesn't fit), works out
/// the title, previews, and runs it, in a chat of its own whose folder is
/// `cwd`.
pub async fn try_tool<A: AppItem>(
    tool: &impl AppTool<A>,
    input: serde_json::Value,
    cwd: &std::path::Path,
) -> Tried<A> {
    let _ = (tool, input, cwd);
    todo!()
}

/// A tool with its input's type forgotten, as [`AppTools`] keeps it.
#[allow(dead_code)] // filled in by feature 13's build
trait Erased<A: AppItem>: Send + Sync {
    /// Its spec, as registered.
    fn spec(&self) -> &ToolSpec;
}

/// The name rules, shared with the core's (`MOST_TOOL_NAME`).
#[allow(dead_code)] // used once built
fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MOST_TOOL_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
