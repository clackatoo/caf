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
//! let trim = tool("trim_clip", |_call, input: Trim| async move {
//!     // Ask the app's UI thread to do it, through a handle the closure
//!     // captured, and await its answer: never block on it.
//!     Done::answer(format!("Trimmed clip {} to {}s", input.clip, input.seconds))
//! })
//! .called("Trim a clip")
//! .title(|input: &Trim| format!("Trim clip {}", input.clip))
//! .describe("Trims a clip to a length in seconds.")
//! .destructive();
//!
//! config.tools.app = AppTools::new().with(trim)?;
//! ```

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use caf_core::chat::{AppItem, NoAppItems};
use caf_core::ids::{CallId, ChatId};
use caf_core::mcp::Answer;
use caf_core::tool::{AppToolSpec, CafTool, Hints, MOST_TOOL_NAME, Piece, Text, ToolKind};
use schemars::JsonSchema;
use serde::Deserialize as _;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::tools::{Call, Print};

/// A boxed future, `Send`, as a tool's work is.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// One of the app's own tools. `A` is the app's item type, which a call
/// may leave on its record ([`Done::item`]).
///
/// The tool is the app's own value: it holds whatever handles it needs to
/// reach the app's state, such as a channel to framewright's UI thread.
/// Only its futures must be `Send`; its state needn't move between
/// threads.
///
/// Its futures run on threads the app's tools have to themselves, two of
/// them. One that awaits, however long, holds nothing up. One that
/// blocks anyway ties up one of those threads: it can hold up the app's
/// other tools, but never CAF, its own tools or its other drivers (if
/// those threads can't be started, which is logged, they share CAF's).
///
/// A task the tool starts with `tokio::spawn` runs on those threads too,
/// and is dropped when CAF shuts down, wherever it's got to. Work that
/// must outlive CAF, or the call, belongs on the app's own runtime,
/// through a handle the tool holds, such as a
/// [`Handle`](tokio::runtime::Handle).
pub trait AppTool<A: AppItem = NoAppItems>: Send + Sync + 'static {
    /// What it's given: serde reads it from the model's JSON, and
    /// schemars describes it to the model, its doc comments as the
    /// fields' descriptions.
    type Input: DeserializeOwned + JsonSchema + Send + 'static;

    /// Its name, title, description and hints. Asked once, as it's
    /// registered.
    fn spec(&self) -> ToolSpec;

    /// A call's title in the transcript, such as "Trim clip 3". It's
    /// worked out as the call comes, on CAF's own task, so it must be
    /// cheap, and never block or wait for anything.
    fn title(&self, input: &Self::Input) -> String;

    /// What the call would do, without doing it, shown before anyone's
    /// asked about it (such as the change it would make), or what it
    /// ends with, failed, if it can't be done. `None` (the default) for a
    /// tool that doesn't preview: [`ToolSpec::previews`] says which.
    ///
    /// A preview that fails ends the call, Failed, even if its `Done`
    /// says it was done: what it ends with is otherwise the call's, its
    /// answer, record and item ([`Done::item`]) included. The future
    /// runs as a task of its own, but `preview` itself is called on CAF's
    /// own task: it must give its future at once, never blocking.
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
    /// ([`Call::answer_now`]). A job's end is told the agent in a
    /// notice, with what it ended with as its result, as the model never
    /// heard `Done`'s answer. Dropping the future cancels it: it must
    /// stop its work then.
    ///
    /// The future runs as a task of its own, on the app's tools' own
    /// threads; but `run` itself is called on CAF's own task: it must
    /// give its future at once, never blocking. A future should await,
    /// not block: one that blocks a thread ties it up, and can't be
    /// aborted. When its call is stopped, the model is told how it ended
    /// once the future has been dropped, or after five seconds, whichever
    /// comes first: if the future ran to its end first, as one blocking
    /// its thread may, with what it gave, and else that it was stopped.
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
        let name = name.into();
        Self {
            title: name.clone(),
            name,
            description: String::new(),
            hints: Hints::default(),
            previews: false,
            schema: None,
        }
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
        Self {
            done: true,
            answer: Answer::text(text),
            record: Vec::new(),
            item: None,
        }
    }

    /// It failed, and the model is told why.
    #[must_use]
    pub fn failed(why: impl Into<String>) -> Self {
        let why = why.into();
        Self {
            done: false,
            record: vec![Piece::Text(Text::new(why.clone()))],
            answer: Answer::error(why),
            item: None,
        }
    }

    /// With these pieces for the transcript.
    #[must_use]
    pub fn with_record(mut self, record: Vec<Piece>) -> Self {
        self.record = record;
        self
    }

    /// With the app's own item for the call.
    #[must_use]
    pub fn with_item(mut self, item: A) -> Self {
        self.item = Some(item);
        self
    }
}

/// The app's own tools, registered once, as CAF starts.
pub struct AppTools<A: AppItem = NoAppItems> {
    /// In the order registered.
    tools: Vec<Registered<A>>,
}

/// A tool as it's registered.
struct Registered<A: AppItem> {
    /// Its spec, with its schema worked out.
    spec: ToolSpec,
    tool: Arc<dyn Erased<A>>,
}

impl<A: AppItem> AppTools<A> {
    /// None.
    #[must_use]
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// With `tool` too.
    ///
    /// # Errors
    ///
    /// [`ToolError`] if its name isn't one a model can call, is taken
    /// in any case (by another of the app's, or by one of CAF's own), or
    /// its schema isn't an object's.
    pub fn with(mut self, tool: impl AppTool<A>) -> Result<Self, ToolError> {
        let mut spec = tool.spec();
        let name = spec.name.clone();
        if !name_ok(&name) {
            return Err(ToolError::BadName(name));
        }
        if (CafTool::ALL.iter()).any(|caf| caf.name().eq_ignore_ascii_case(&name)) {
            return Err(ToolError::Cafs(name));
        }
        if (self.tools.iter()).any(|known| known.spec.name.eq_ignore_ascii_case(&name)) {
            return Err(ToolError::Taken(name));
        }
        let schema = match spec.schema.take() {
            Some(schema) => schema,
            None => schema_of(&tool),
        };
        if schema.get("type").and_then(Value::as_str) != Some("object") {
            return Err(ToolError::BadSchema(name));
        }
        spec.schema = Some(schema);
        self.tools.push(Registered {
            spec,
            tool: Arc::new(Typed(tool)),
        });
        Ok(self)
    }

    /// What the core knows of each, in the order served.
    #[must_use]
    pub fn specs(&self) -> Vec<AppToolSpec> {
        (self.tools.iter())
            .map(|known| {
                let spec = &known.spec;
                AppToolSpec::new(
                    spec.name.clone(),
                    spec.title.clone(),
                    spec.hints,
                    spec.previews,
                )
            })
            .collect()
    }

    /// Whether there are none.
    pub(crate) fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The tool named `name`, if there's one.
    pub(crate) fn named(&self, name: &str) -> Option<&Arc<dyn Erased<A>>> {
        (self.tools.iter())
            .find(|known| known.spec.name == name)
            .map(|known| &known.tool)
    }

    /// The tool named `name`'s spec, its schema worked out, if there's
    /// one.
    pub(crate) fn spec(&self, name: &str) -> Option<&ToolSpec> {
        (self.tools.iter())
            .find(|known| known.spec.name == name)
            .map(|known| &known.spec)
    }
}

impl<A: AppItem> Default for AppTools<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: AppItem> std::fmt::Debug for AppTools<A> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (formatter.debug_list())
            .entries(self.tools.iter().map(|known| &known.spec.name))
            .finish()
    }
}

/// `tool`'s input's schema, as schemars works it out: without `$schema`,
/// which says only which draft it is.
fn schema_of<A: AppItem, T: AppTool<A>>(tool: &T) -> Value {
    let _ = tool;
    let mut schema = schemars::schema_for!(T::Input).to_value();
    if let Value::Object(fields) = &mut schema {
        fields.remove("$schema");
    }
    schema
}

/// Why a tool can't be registered.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolError {
    /// Its name has a character other than `[A-Za-z0-9_-]`, or is empty,
    /// or is longer than [`MOST_TOOL_NAME`] bytes.
    BadName(String),
    /// Another of the app's tools has its name, in some case.
    Taken(String),
    /// One of CAF's own tools has its name, in some case.
    Cafs(String),
    /// Its schema isn't an object's.
    BadSchema(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadName(name) => write!(
                formatter,
                "the tool name {name:?} isn't one a model can call: it must be 1 to \
                 {MOST_TOOL_NAME} of A-Z, a-z, 0-9, _ and -"
            ),
            Self::Taken(name) => write!(
                formatter,
                "the app has two tools named {name:?}, in some case"
            ),
            Self::Cafs(name) => {
                write!(formatter, "the tool name {name:?} is one of CAF's own")
            }
            Self::BadSchema(name) => write!(
                formatter,
                "the tool {name:?} has an input schema that isn't an object's"
            ),
        }
    }
}

impl std::error::Error for ToolError {}

/// How a [`Built`] tool titles a call, from its input.
type Titler<I> = Box<dyn Fn(&I) -> String + Send + Sync>;

/// A tool [`tool`] built from a closure.
pub struct Built<I, F, A: AppItem = NoAppItems> {
    spec: ToolSpec,
    run: F,
    title: Option<Titler<I>>,
    item: std::marker::PhantomData<fn() -> A>,
}

/// A tool named `name` that runs `run`, for tools too small for a type of
/// their own: give it a title, a description and hints with the
/// builder's methods.
///
/// Rust doesn't take the app's item type from its default: a tool is
/// best built where it's registered, as in
/// `config.tools.app = AppTools::new().with(tool(..))?`, which says it.
/// Built on its own, by an app with no items of its own, its closure
/// names it: `Done::<NoAppItems>::answer(..)`.
pub fn tool<I, F, Fut, A>(name: impl Into<String>, run: F) -> Built<I, F, A>
where
    I: DeserializeOwned + JsonSchema + Send + 'static,
    F: Fn(Call, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Done<A>> + Send + 'static,
    A: AppItem,
{
    Built {
        spec: ToolSpec::new(name),
        run,
        title: None,
        item: std::marker::PhantomData,
    }
}

impl<I, F, A: AppItem> Built<I, F, A> {
    /// What it's called in general, such as "Trim a clip"
    /// ([`ToolSpec::title`]). Its name by default.
    #[must_use]
    pub fn called(mut self, title: impl Into<String>) -> Self {
        self.spec.title = title.into();
        self
    }

    /// Each call's title, from its input, such as "Trim clip 3". The
    /// tool's name by default. It must be cheap ([`AppTool::title`]).
    #[must_use]
    pub fn title(mut self, title: impl Fn(&I) -> String + Send + Sync + 'static) -> Self {
        self.title = Some(Box::new(title));
        self
    }

    /// What it does, for the model.
    #[must_use]
    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.spec.description = description.into();
        self
    }

    /// It only reads.
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.spec.hints.read_only = true;
        self
    }

    /// It may change or delete what was there.
    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.spec.hints.destructive = true;
        self
    }

    /// It reaches beyond the app and the chat's folder.
    #[must_use]
    pub fn open_world(mut self) -> Self {
        self.spec.hints.open_world = true;
        self
    }

    /// The kind of tool its calls are.
    #[must_use]
    pub fn kind(mut self, kind: ToolKind) -> Self {
        self.spec.hints.kind = kind;
        self
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
        match &self.title {
            Some(title) => title(input),
            None => self.spec.name.clone(),
        }
    }

    fn run(&self, call: Call, input: I) -> BoxFuture<Done<A>> {
        Box::pin((self.run)(call, input))
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
    /// What the model was answered early, if it went on as a job
    /// ([`Call::answer_now`]): the model never hears `done`'s answer then.
    pub answered_now: Option<Answer>,
}

/// Calls `tool` with `input` the way CAF would, with no agent and no CAF:
/// reads the input (failing as CAF would, if it doesn't fit), works out
/// the title, previews, and runs it, in a chat of its own whose folder is
/// `cwd`.
pub async fn try_tool<A: AppItem>(
    tool: &impl AppTool<A>,
    input: serde_json::Value,
    cwd: &Path,
) -> Tried<A> {
    let spec = tool.spec();
    let read = match serde_json::from_value::<_>(input.clone()) {
        Ok(read) => read,
        Err(error) => {
            return Tried {
                title: spec.title,
                preview: None,
                done: Done::failed(bad_input(&error.to_string())),
                printed: Vec::new(),
                answered_now: None,
            };
        }
    };
    let title = tool.title(&read);
    let (prints, mut printed) = mpsc::unbounded_channel();
    let call = Call::new(
        ChatId::from_number(1),
        CallId(1),
        cwd.to_owned(),
        input,
        None,
        prints,
    );
    let preview = match spec.previews {
        true => Some(match tool.preview(&call, &read) {
            Some(preview) => preview.await,
            None => Ok(Vec::new()),
        }),
        false => None,
    };
    // What it printed as it previewed isn't kept, as CAF keeps none.
    while printed.try_recv().is_ok() {}
    let done = match &preview {
        Some(Err(done)) => done.clone(),
        _ => tool.run(call, read).await,
    };
    let mut lines = Vec::new();
    let mut answered_now = None;
    while let Ok(print) = printed.try_recv() {
        match print {
            Print::Lines {
                from, lines: new, ..
            } => {
                let from = usize::try_from(from).unwrap_or(usize::MAX);
                if from > lines.len() {
                    lines.resize(from, String::new());
                }
                lines.truncate(from);
                lines.extend(new);
            }
            Print::Detach { answer, .. } => answered_now = answered_now.or(Some(answer)),
        }
    }
    Tried {
        title,
        preview,
        done,
        printed: lines,
        answered_now,
    }
}

/// What the model is told of a call whose input didn't fit, as `why`
/// (serde's message) says: as the tools driver answers it.
pub(crate) fn bad_input(why: &str) -> String {
    format!("Refused: {why}")
}

/// A tool with its input's type forgotten, as [`AppTools`] keeps it: its
/// input is read again from JSON for each step, as the core gives it back.
pub(crate) trait Erased<A: AppItem>: Send + Sync {
    /// Reads `input`, giving the call's title: or why it doesn't fit, as
    /// serde says.
    fn title(&self, input: &Value) -> Result<String, String>;
    /// [`AppTool::preview`], of `input` read: `None` if it doesn't
    /// preview, or `input` doesn't fit.
    fn preview(&self, call: &Call, input: &Value)
    -> Option<BoxFuture<Result<Vec<Piece>, Done<A>>>>;
    /// [`AppTool::run`], of `input` read: failed if it doesn't fit.
    fn run(&self, call: Call, input: &Value) -> BoxFuture<Done<A>>;
}

/// A tool, to forget its input's type behind [`Erased`].
struct Typed<T>(T);

impl<A: AppItem, T: AppTool<A>> Erased<A> for Typed<T> {
    fn title(&self, input: &Value) -> Result<String, String> {
        let read = T::Input::deserialize(input).map_err(|error| error.to_string())?;
        Ok(self.0.title(&read))
    }

    fn preview(
        &self,
        call: &Call,
        input: &Value,
    ) -> Option<BoxFuture<Result<Vec<Piece>, Done<A>>>> {
        let read = T::Input::deserialize(input).ok()?;
        self.0.preview(call, &read)
    }

    fn run(&self, call: Call, input: &Value) -> BoxFuture<Done<A>> {
        match T::Input::deserialize(input) {
            Ok(read) => self.0.run(call, read),
            Err(error) => Box::pin(std::future::ready(Done::failed(bad_input(
                &error.to_string(),
            )))),
        }
    }
}

/// The name rules, shared with the core's (`MOST_TOOL_NAME`).
fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MOST_TOOL_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[cfg(test)]
mod tests {
    use caf_core::mcp::AnswerPart;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;

    /// A clip to trim, as a tool's input.
    #[derive(Deserialize, JsonSchema)]
    struct Trim {
        /// The clip to trim.
        clip: u64,
        /// Its new length, in seconds.
        seconds: f64,
        /// Whether to fade it out.
        #[serde(default)]
        #[allow(dead_code)] // only its schema is looked at
        fade: Option<bool>,
    }

    /// An app's item, for the tests.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Trimmed(u64);

    impl AppItem for Trimmed {
        fn summary(&self) -> String {
            format!("Trimmed clip {}", self.0)
        }
    }

    /// A tool trimming clips, answering what it did.
    fn trim() -> impl AppTool<Trimmed> {
        tool("trim_clip", |_call, input: Trim| async move {
            Done::answer(format!("Trimmed clip {} to {}s", input.clip, input.seconds))
                .with_item(Trimmed(input.clip))
        })
        .title(|input: &Trim| format!("Trim clip {}", input.clip))
        .describe("Trims a clip to a length in seconds.")
        .destructive()
    }

    /// A tool named `name` that does nothing.
    fn named(name: &str) -> impl AppTool {
        tool(name, |_call, _input: Trim| async { Done::answer("") })
    }

    fn text(answer: &Answer) -> &str {
        match answer.content.as_slice() {
            [AnswerPart::Text(text)] => text,
            other => panic!("not one text: {other:?}"),
        }
    }

    #[test]
    fn a_name_must_be_one_a_model_can_call() {
        for bad in [
            "",
            "trim clip",
            "trim.clip",
            "trimé",
            &"a".repeat(MOST_TOOL_NAME + 1),
        ] {
            assert_eq!(
                AppTools::new().with(named(bad)).err(),
                Some(ToolError::BadName(bad.into())),
                "{bad:?}"
            );
        }
        let longest = "a".repeat(MOST_TOOL_NAME);
        for good in ["trim_clip", "Trim-Clip-2", &longest] {
            assert!(AppTools::new().with(named(good)).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn a_name_of_cafs_own_in_any_case_is_refused() {
        for cafs in ["Read", "read", "EDIT", "write", "bAsH", "taskstop"] {
            assert_eq!(
                AppTools::new().with(named(cafs)).err(),
                Some(ToolError::Cafs(cafs.into())),
            );
        }
    }

    #[test]
    fn a_name_taken_by_another_of_the_apps_in_any_case_is_refused() {
        let tools = AppTools::new().with(named("trim")).unwrap();
        for taken in ["trim", "Trim", "TRIM"] {
            assert_eq!(
                AppTools::new()
                    .with(named("trim"))
                    .unwrap()
                    .with(named(taken))
                    .err(),
                Some(ToolError::Taken(taken.into()))
            );
        }
        assert!(tools.with(named("trim_clip")).is_ok());
    }

    #[test]
    fn a_schema_must_be_an_objects() {
        let number = tool("count", |_call, _input: u32| async {
            Done::<NoAppItems>::answer("")
        });
        assert_eq!(
            AppTools::new().with(number).err(),
            Some(ToolError::BadSchema("count".into()))
        );
        let mut spec = ToolSpec::new("count");
        spec.schema = Some(json!({ "type": "string" }));
        assert_eq!(
            AppTools::new().with(Specced(spec)).err(),
            Some(ToolError::BadSchema("count".into()))
        );
    }

    #[test]
    fn a_schema_given_is_served_in_place_of_schemars() {
        let schema = json!({ "type": "object", "properties": { "clip": { "type": "integer" } } });
        let mut spec = ToolSpec::new("count");
        spec.schema = Some(schema.clone());
        let tools = AppTools::new().with(Specced(spec)).unwrap();
        assert_eq!(tools.spec("count").unwrap().schema, Some(schema));
    }

    #[test]
    fn schemars_schema_has_the_fields_doc_comments() {
        let tools = AppTools::new().with(trim()).unwrap();
        let schema = tools.spec("trim_clip").unwrap().schema.clone().unwrap();
        assert_eq!(
            schema,
            json!({
                "title": "Trim",
                "description": "A clip to trim, as a tool's input.",
                "type": "object",
                "properties": {
                    "clip": {
                        "description": "The clip to trim.",
                        "type": "integer",
                        "format": "uint64",
                        "minimum": 0,
                    },
                    "seconds": {
                        "description": "Its new length, in seconds.",
                        "type": "number",
                        "format": "double",
                    },
                    "fade": {
                        "description": "Whether to fade it out.",
                        "type": ["boolean", "null"],
                        "default": null,
                    },
                },
                "required": ["clip", "seconds"],
            })
        );
    }

    #[test]
    fn the_core_knows_each_in_the_order_registered_with_its_hints() {
        let read = tool("word_count", |_call, _input: Trim| async {
            Done::answer("")
        })
        .read_only()
        .kind(ToolKind::Read);
        let tools = AppTools::new().with(trim()).unwrap();
        let tools = tools.with(read).unwrap();
        let mut destructive = Hints::default();
        destructive.destructive = true;
        let mut reads = Hints::default();
        reads.read_only = true;
        reads.kind = ToolKind::Read;
        assert_eq!(
            tools.specs(),
            vec![
                AppToolSpec::new("trim_clip", "trim_clip", destructive, false),
                AppToolSpec::new("word_count", "word_count", reads, false),
            ]
        );
    }

    #[test]
    fn a_tool_by_default_says_nothing_of_itself() {
        let spec = named("trim").spec();
        assert_eq!(spec.title, "trim");
        assert_eq!(spec.description, "");
        assert_eq!(spec.hints, Hints::default());
        assert_eq!(spec.hints.kind, ToolKind::Other);
        assert!(!spec.previews);
        assert_eq!(spec.schema, None);
        let mut open = Hints::default();
        open.open_world = true;
        assert_eq!(named_open().spec().hints, open);
    }

    fn named_open() -> impl AppTool {
        tool("fetch", |_call, _input: Trim| async { Done::answer("") }).open_world()
    }

    #[test]
    fn why_a_tool_cant_be_registered_names_it() {
        let bad = ToolError::BadName("trim clip".into()).to_string();
        assert!(bad.contains("\"trim clip\"") && bad.contains("54"), "{bad}");
        assert!(
            ToolError::Taken("trim".into())
                .to_string()
                .contains("two tools")
        );
        assert!(
            ToolError::Cafs("read".into())
                .to_string()
                .contains("CAF's own")
        );
        assert!(
            ToolError::BadSchema("trim".into())
                .to_string()
                .contains("object")
        );
    }

    #[test]
    fn done_answers_and_failed_answers_an_error_and_records_why() {
        let done: Done = Done::answer("Trimmed");
        assert!(done.done && !done.answer.is_error && done.record.is_empty());
        assert_eq!(text(&done.answer), "Trimmed");
        let failed: Done = Done::failed("No such clip");
        assert!(!failed.done && failed.answer.is_error);
        assert_eq!(failed.record, vec![Piece::Text(Text::new("No such clip"))]);
        let record = vec![Piece::Text(Text::new("3s"))];
        let done = Done::answer("Trimmed")
            .with_record(record.clone())
            .with_item(Trimmed(3));
        assert_eq!((done.record, done.item), (record, Some(Trimmed(3))));
    }

    #[tokio::test]
    async fn a_tool_tried_gives_its_title_answer_and_item() {
        let tried = try_tool(
            &trim(),
            json!({ "clip": 3, "seconds": 2.5 }),
            Path::new("/"),
        )
        .await;
        assert_eq!(tried.title, "Trim clip 3");
        assert!(tried.preview.is_none());
        assert!(tried.done.done);
        assert_eq!(text(&tried.done.answer), "Trimmed clip 3 to 2.5s");
        assert_eq!(tried.done.item, Some(Trimmed(3)));
        assert!(tried.printed.is_empty() && tried.answered_now.is_none());
    }

    #[tokio::test]
    async fn a_tool_tried_with_input_that_doesnt_fit_fails_saying_why() {
        let tried = try_tool(&trim(), json!({ "clip": "three" }), Path::new("/")).await;
        assert_eq!(tried.title, "trim_clip");
        assert!(!tried.done.done && tried.done.answer.is_error);
        let why = text(&tried.done.answer);
        assert!(why.starts_with("Refused: invalid type"), "{why}");
    }

    #[tokio::test]
    async fn a_tool_tried_prints_and_answers_early_as_a_job() {
        let job = tool("render", |mut call: Call, _input: Trim| async move {
            call.print(0, vec!["one".into(), "two".into()]);
            let job = call.answer_now(|job| Answer::text(format!("Rendering as job {}", job.0)));
            call.print(1, vec!["2".into(), "three".into()]);
            Done::<NoAppItems>::answer(format!("Rendered, as job {}", job.0))
        });
        let tried = try_tool(&job, json!({ "clip": 1, "seconds": 1 }), Path::new("/")).await;
        assert_eq!(tried.printed, ["one", "2", "three"]);
        assert_eq!(text(&tried.answered_now.unwrap()), "Rendering as job 1");
        assert_eq!(text(&tried.done.answer), "Rendered, as job 1");
    }

    #[tokio::test]
    async fn a_tool_tried_previews_and_isnt_run_if_its_preview_fails() {
        let tried = try_tool(
            &Previewing,
            json!({ "clip": 1, "seconds": 1 }),
            Path::new("/a"),
        )
        .await;
        assert!(matches!(&tried.preview, Some(Ok(pieces)) if pieces.len() == 1));
        assert_eq!(text(&tried.done.answer), "Ran in /a");
        let tried = try_tool(
            &Previewing,
            json!({ "clip": 0, "seconds": 1 }),
            Path::new("/a"),
        )
        .await;
        assert!(matches!(&tried.preview, Some(Err(done)) if !done.done));
        assert_eq!(text(&tried.done.answer), "There's no clip 0");
    }

    #[test]
    fn a_built_tool_may_be_called_something_in_general() {
        let named = tool("trim_clip", |_call, _input: Trim| async {
            Done::<NoAppItems>::answer("")
        })
        .called("Trim a clip");
        assert_eq!(named.spec().title, "Trim a clip");
        assert_eq!(named.spec().name, "trim_clip");
    }

    #[tokio::test]
    async fn a_tool_tried_keeps_only_what_it_printed_as_it_ran() {
        let tried = try_tool(
            &Printing,
            json!({ "clip": 1, "seconds": 1 }),
            Path::new("/"),
        )
        .await;
        assert!(tried.printed.is_empty(), "{:?}", tried.printed);
    }

    /// A tool that prints as it previews, and not as it runs.
    struct Printing;

    impl AppTool for Printing {
        type Input = Trim;

        fn spec(&self) -> ToolSpec {
            let mut spec = ToolSpec::new("print");
            spec.previews = true;
            spec
        }

        fn title(&self, _input: &Trim) -> String {
            String::new()
        }

        fn preview(
            &self,
            call: &Call,
            _input: &Trim,
        ) -> Option<BoxFuture<Result<Vec<Piece>, Done>>> {
            call.print(0, vec!["previewing".into(), "more".into()]);
            Some(Box::pin(async { Ok(Vec::new()) }))
        }

        fn run(&self, _call: Call, _input: Trim) -> BoxFuture<Done> {
            Box::pin(async { Done::answer("") })
        }
    }

    /// A tool of its own spec, doing nothing.
    struct Specced(ToolSpec);

    impl AppTool for Specced {
        type Input = Trim;

        fn spec(&self) -> ToolSpec {
            self.0.clone()
        }

        fn title(&self, _input: &Trim) -> String {
            String::new()
        }

        fn run(&self, _call: Call, _input: Trim) -> BoxFuture<Done> {
            Box::pin(async { Done::answer("") })
        }
    }

    /// A tool that previews: failing for clip 0.
    struct Previewing;

    impl AppTool for Previewing {
        type Input = Trim;

        fn spec(&self) -> ToolSpec {
            let mut spec = ToolSpec::new("trim");
            spec.previews = true;
            spec
        }

        fn title(&self, input: &Trim) -> String {
            format!("Trim clip {}", input.clip)
        }

        fn preview(
            &self,
            _call: &Call,
            input: &Trim,
        ) -> Option<BoxFuture<Result<Vec<Piece>, Done>>> {
            let clip = input.clip;
            Some(Box::pin(async move {
                match clip {
                    0 => Err(Done::failed("There's no clip 0")),
                    clip => Ok(vec![Piece::Text(Text::new(format!("Clip {clip}")))]),
                }
            }))
        }

        fn run(&self, call: Call, _input: Trim) -> BoxFuture<Done> {
            let cwd = call.cwd().display().to_string();
            Box::pin(async move { Done::answer(format!("Ran in {cwd}")) })
        }
    }
}
