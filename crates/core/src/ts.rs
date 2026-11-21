//! TypeScript types for CAF's own, through ts-rs: what the TypeScript
//! caf-sdk's `ts/sdk/src/types/` is generated from.
//!
//! Every type CAF serialises derives `TS` under the feature `ts`. Whole
//! numbers, `u64` and `i64` too, are `number`, as `JSON.parse` reads them:
//! every number CAF itself writes in a chat or on the viewer wire is below
//! 2^53, and the agent's raw JSON carried inside (`Unknown.raw`,
//! `ToolCall.input`) is the agent's, read as any JSON is (see the crate
//! docs). The generic types are exported with no app items of their own
//! (`NoAppItems`, which is `never`): ts-rs can't export them generic, as
//! it fills a type's parameters with its own placeholder, which isn't an
//! `AppItem`.
//!
//! Beside them, `ts/sdk/test/shapes.gen.ts` holds real JSON, each value
//! typed with them: the shape tests' values and everything in the
//! recorded traces (see `shape.rs`'s `Samples`). The SDK's `tsc` checks
//! the types describe what CAF writes.
//!
//! Regenerate both with
//! `CAF_BLESS=1 cargo test -p caf-core --features ts typescript_types`.
//! Without `CAF_BLESS`, the same test fails if they're stale. Its
//! neighbours check the generated types have no enum in serde's default
//! shape, and that every type deriving `TS` is exported and sampled.

/// How serde writes a [`std::time::Duration`].
#[expect(dead_code, reason = "only its TypeScript type is used")]
#[derive(ts_rs::TS)]
#[ts(rename = "Duration")]
pub(crate) struct Duration {
    /// Whole seconds.
    secs: u64,
    /// Nanoseconds past them.
    nanos: u32,
}

/// How serde writes a [`std::ffi::OsString`]: its bytes on Unix, its
/// UTF-16 units on Windows.
#[expect(dead_code, reason = "only its TypeScript type is used")]
#[derive(ts_rs::TS)]
#[ts(rename = "OsString")]
pub(crate) enum OsString {
    /// On Unix.
    Unix(Vec<u8>),
    /// On Windows.
    Windows(Vec<u16>),
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test that writes and reads the generated files: the core itself still does no I/O"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use ts_rs::{Config, TS};

    use crate::acp::{Frame, Message, RequestId, RpcError, Shape};
    use crate::agent::{
        AgentSpec, AgentStatus, CannotInstall, EnvChange, InstallFailure, InstallState, Launch,
        McpServer, SignInLink, SignInState, SpawnCommand,
    };
    use crate::change::{Change, Mismatch, Numbered};
    use crate::chat::{
        Chat, ChatState, Compacted, Cost, Failure, FailureReason, Incompatible, Item, ItemKind,
        Lost, NoAppData, NoAppItems, Note, Part, Remedy, Starter, ToolCall, ToolKind, ToolStatus,
        Turn, TurnLife, Unknown, Usage, UserMessage,
    };
    use crate::core::{ConnectionInfo, OpenChat, Refused, SessionChoice};
    use crate::directive::{
        AgentDirective, Directive, Level, PermissionRequest, PolicyDirective, Report,
        SetupDirective, Signal, StorageDirective, TimerDirective, ViewerDirective,
    };
    use crate::event::{
        AgentEvent, Event, Exit, HostCommand, PolicyAnswer, SetupEvent, StorageEvent,
    };
    use crate::host::{Grouping, Host, Limits, Shows, Timers};
    use crate::ids::{
        AgentName, AgentSessionId, ChangeNo, ChatId, ConnectionId, ItemId, QuestionId, Shelf,
        Ticket, TurnId,
    };
    use crate::install::{
        Artifact, Bundled, EnvValue, Install, LinkFrom, Needs, NpmPackage, Platform, Program,
        Recipe, SignIn, SignedInAs, Status, StatusCommand, Unpack,
    };
    use crate::profiles::{AdapterInfo, Builtin, Checked, ProfileChoice, ProfileReport};
    use crate::question::{
        Answer, OptionId, OptionKind, Permission, PermissionOption, Question, QuestionKind,
        SlowChoice, SlowToOpen,
    };
    use crate::save::{CannotRestore, Entry, Header, RestoreChat, Saved, SavedChat, SavedState};
    use crate::settings::{
        Choice, KnownSettings, ModelUsed, Setting, SettingChoice, SettingForm, SettingKind,
        SettingValue, Settings,
    };
    use crate::shape::{Samples, every_type, traces};
    use crate::time::{End, Mono, Now, Wall, Window};
    use crate::viewer::{
        AgentAction, ChatSummary, FromViewer, OpenRequest, Outcome, RequestNo, Rights, ToViewer,
    };

    /// The TypeScript caf-sdk, which keeps the generated files.
    fn sdk() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ts/sdk")
    }

    /// The types that have no values, so have no samples.
    const UNINHABITED: [&str; 1] = ["NoAppItems"];

    /// Writes every type, each with what it uses, to `dir`, and an
    /// `index.ts` exporting them all. Returns their Rust names.
    fn export(dir: &Path) -> Vec<&'static str> {
        let config = Config::new()
            .with_large_int("number")
            .with_import_extension(Some("ts"))
            .with_out_dir(dir);
        let mut names = Vec::new();
        macro_rules! export {
            ($($ty:ident),* $(,)?) => {
                $(<$ty as TS>::export_all(&config)
                    .unwrap_or_else(|error| panic!("{}: {error}", stringify!($ty)));
                names.push(stringify!($ty));)*
            };
        }
        export!(
            // acp
            Frame,
            Message,
            RequestId,
            RpcError,
            Shape,
            // agent
            AgentSpec,
            AgentStatus,
            CannotInstall,
            EnvChange,
            InstallFailure,
            InstallState,
            Launch,
            McpServer,
            SignInLink,
            SignInState,
            SpawnCommand,
            // change
            Change,
            Mismatch,
            Numbered,
            // chat
            Chat,
            ChatState,
            Compacted,
            Cost,
            Failure,
            FailureReason,
            Incompatible,
            Item,
            ItemKind,
            Lost,
            NoAppData,
            NoAppItems,
            Note,
            Part,
            Remedy,
            Starter,
            ToolCall,
            ToolKind,
            ToolStatus,
            Turn,
            TurnLife,
            Unknown,
            Usage,
            UserMessage,
            // settings
            Choice,
            KnownSettings,
            ModelUsed,
            Setting,
            SettingChoice,
            SettingForm,
            SettingKind,
            SettingValue,
            Settings,
            // save
            CannotRestore,
            Entry,
            Header,
            RestoreChat,
            Saved,
            SavedChat,
            SavedState,
            // core
            ConnectionInfo,
            OpenChat,
            Refused,
            SessionChoice,
            // directive
            AgentDirective,
            Directive,
            Level,
            PermissionRequest,
            PolicyDirective,
            Report,
            SetupDirective,
            Signal,
            StorageDirective,
            TimerDirective,
            ViewerDirective,
            // event
            AgentEvent,
            Event,
            Exit,
            HostCommand,
            PolicyAnswer,
            SetupEvent,
            StorageEvent,
            // host
            Grouping,
            Host,
            Limits,
            Shows,
            Timers,
            // install
            Artifact,
            Bundled,
            EnvValue,
            Install,
            LinkFrom,
            Needs,
            NpmPackage,
            Platform,
            Program,
            Recipe,
            SignIn,
            SignedInAs,
            Status,
            StatusCommand,
            Unpack,
            // ids
            AgentName,
            AgentSessionId,
            ChangeNo,
            ChatId,
            ConnectionId,
            ItemId,
            QuestionId,
            Shelf,
            Ticket,
            TurnId,
            // profiles
            AdapterInfo,
            Builtin,
            Checked,
            ProfileChoice,
            ProfileReport,
            // question
            Answer,
            OptionId,
            OptionKind,
            Permission,
            PermissionOption,
            Question,
            QuestionKind,
            SlowChoice,
            SlowToOpen,
            // time
            End,
            Mono,
            Now,
            Wall,
            Window,
            // viewer
            AgentAction,
            ChatSummary,
            FromViewer,
            OpenRequest,
            Outcome,
            RequestNo,
            Rights,
            ToViewer,
        );
        let index: String = files(dir)
            .keys()
            .map(|file| format!("export type * from \"./{file}\";\n"))
            .collect();
        std::fs::write(
            dir.join("index.ts"),
            format!("// Every type generated from caf-core: see `ts.rs` there.\n{index}"),
        )
        .expect("index.ts is written");
        names
    }

    /// Every sample (see [`Samples`]), once each, as TypeScript constants
    /// of their types: `tsc` checks the generated types take real JSON.
    fn shapes() -> String {
        let mut samples = Samples::default();
        every_type(&mut samples);
        traces(&mut samples);
        let mut seen = BTreeSet::new();
        let mut out = String::from(
            "// Generated by caf-core's `ts.rs`: don't edit. Values as CAF writes them, and as\n\
             // recordings hold them, each typed with the generated types, so `tsc` checks the\n\
             // types describe real JSON. Regenerate it as the types are.\n\
             import type * as T from \"../src/types/index.ts\";\n\n",
        );
        for (ty, json) in samples.values {
            if seen.insert((ty.clone(), json.clone())) {
                out += &format!("export const sample{}: {ty} = {json};\n", seen.len());
            }
        }
        out
    }

    /// Every file under `dir`, by its path from there, with what it holds.
    fn files(dir: &Path) -> BTreeMap<String, String> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    let name = path.strip_prefix(root).expect("under the root");
                    let text = std::fs::read_to_string(&path).expect("a file reads");
                    out.insert(name.to_string_lossy().replace('\\', "/"), text);
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    /// A fresh folder for this test to write in.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("caf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// What's generated, by its path in the SDK: the types and the samples.
    fn generated() -> BTreeMap<String, String> {
        let dir = scratch("ts");
        export(&dir);
        let mut made: BTreeMap<String, String> = (files(&dir).into_iter())
            .map(|(name, text)| (format!("src/types/{name}"), text))
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        made.insert("test/shapes.gen.ts".to_owned(), shapes());
        made
    }

    /// What the SDK has of what's generated.
    fn kept() -> BTreeMap<String, String> {
        let mut kept: BTreeMap<String, String> = (files(&sdk().join("src/types")).into_iter())
            .map(|(name, text)| (format!("src/types/{name}"), text))
            .collect();
        if let Ok(text) = std::fs::read_to_string(sdk().join("test/shapes.gen.ts")) {
            kept.insert("test/shapes.gen.ts".to_owned(), text);
        }
        kept
    }

    #[test]
    fn typescript_types_are_current() {
        let made = generated();
        if std::env::var_os("CAF_BLESS").is_some() {
            let _ = std::fs::remove_dir_all(sdk().join("src/types"));
            for (name, text) in &made {
                let path = sdk().join(name);
                std::fs::create_dir_all(path.parent().expect("a folder")).expect("folders");
                std::fs::write(path, text).expect("a file is written");
            }
            return;
        }
        let kept = kept();
        let stale: BTreeSet<&String> = (made.keys().chain(kept.keys()))
            .filter(|name| made.get(*name) != kept.get(*name))
            .collect();
        assert!(
            stale.is_empty(),
            "ts/sdk is stale: {stale:?}. Regenerate it with \
             `CAF_BLESS=1 cargo test -p caf-core --features ts typescript_types`"
        );
    }

    /// What in generated TypeScript `text` isn't in the one shape: an
    /// enum written as a bare string (`"A" | "B"`), as `{ "A": … }`, with
    /// its fields beside its tag (`{ "type": "A", field: … }`), or with a
    /// variant renamed from Rust's. The only string literals are the tags,
    /// `"type": "A"`, each followed by `}` or `"value"`, and the only
    /// quoted keys `type` and `value`, and the std types' own.
    fn untagged(text: &str) -> Vec<String> {
        let mut code = String::new();
        let mut rest = text;
        while let Some(at) = rest.find("/**") {
            code += &rest[..at];
            rest = rest[at..]
                .find("*/")
                .map_or("", |end| &rest[at + end + 2..]);
        }
        code += rest;
        let code: Vec<&str> = code
            .lines()
            .filter(|line| {
                !["/", "import ", "export type * from "]
                    .iter()
                    .any(|start| line.starts_with(start))
            })
            .collect();
        let code = code.join("\n");
        let pieces: Vec<&str> = code.split('"').collect();
        let mut found = Vec::new();
        for at in (1..pieces.len()).step_by(2) {
            let literal = pieces[at];
            let after = pieces.get(at + 1).map_or("", |after| after.trim_start());
            if after.starts_with(':') {
                if !["type", "value", "Ok", "Err", "Unix", "Windows"].contains(&literal) {
                    found.push(format!("the key \"{literal}\""));
                }
            } else if !(at >= 2 && pieces[at - 1].trim() == ":" && pieces[at - 2] == "type") {
                found.push(format!("the string \"{literal}\""));
            } else if !literal.starts_with(char::is_uppercase) {
                found.push(format!("the renamed variant \"{literal}\""));
            } else if !(after.starts_with('}')
                || (pieces[at + 1].trim() == "," && pieces.get(at + 2) == Some(&"value")))
            {
                found.push(format!(
                    "the variant \"{literal}\", with no value beside it"
                ));
            }
        }
        found
    }

    #[test]
    fn typescript_types_are_in_the_one_shape() {
        let dir = scratch("ts-shape");
        export(&dir);
        let made = files(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        let wrong: Vec<String> = (made.iter())
            .flat_map(|(name, text)| {
                untagged(text)
                    .into_iter()
                    .map(move |what| format!("{name}: {what}"))
            })
            .collect();
        assert!(wrong.is_empty(), "not in the one shape: {wrong:#?}");
        // It finds each way an enum can be out of it: a bare string, serde's
        // default shape, a tag with no `value` beside it, and a renamed
        // variant.
        assert_eq!(untagged(r#"type L = "Warn" | "Info";"#).len(), 2);
        assert_eq!(
            untagged(r#"type E = { "At": Wall } | { "type": "Open" };"#).len(),
            1
        );
        assert_eq!(
            untagged(r#"type I = { "type": "Lost", reason: Lost } | { "type": "Open" };"#).len(),
            1
        );
        assert_eq!(
            untagged(r#"type R = { "type": "words", "value": string } | { "type": "At", "value": Wall };"#)
                .len(),
            1
        );
    }

    /// Every type in caf-core's source that derives `TS`, by name.
    fn derived() -> BTreeSet<String> {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = BTreeSet::new();
        for (name, text) in files(&src) {
            if name == "ts.rs" || name.contains("tests/") {
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            for (at, line) in lines.iter().enumerate() {
                if !line.contains("derive(ts_rs::TS)") {
                    continue;
                }
                // The item: past its attributes, which may span lines.
                let item = (lines[at + 1..].iter())
                    .find(|line| {
                        (line.split_whitespace()).any(|word| ["struct", "enum"].contains(&word))
                    })
                    .expect("an item");
                let words: Vec<&str> = item.split_whitespace().collect();
                let name = words
                    .windows(2)
                    .find(|pair| ["struct", "enum"].contains(&pair[0]))
                    .map(|pair| {
                        pair[1]
                            .split(['<', '(', '{', ';'])
                            .next()
                            .unwrap_or(pair[1])
                    })
                    .expect("a struct or enum");
                if name == "$name" {
                    // The ids, each made by `id!`: its name is the line
                    // after its docs.
                    for (at, line) in lines.iter().enumerate() {
                        if line.trim() == "id! {" {
                            let id = (lines[at + 1..].iter())
                                .find(|line| !line.trim_start().starts_with("///"))
                                .expect("an id");
                            found.insert(id.trim().to_owned());
                        }
                    }
                } else {
                    found.insert(name.to_owned());
                }
            }
        }
        found
    }

    #[test]
    fn typescript_types_are_all_exported_and_sampled() {
        let dir = scratch("ts-list");
        let exported: BTreeSet<String> = (export(&dir).into_iter()).map(str::to_owned).collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            derived(),
            exported,
            "every type deriving TS is in `export!`"
        );
        let mut samples = Samples::default();
        every_type(&mut samples);
        let sampled: BTreeSet<String> = (samples.values.iter())
            .filter_map(|(ty, _)| ty.strip_prefix("T.").map(str::to_owned))
            .collect();
        let mut want = exported.clone();
        want.retain(|name| !UNINHABITED.contains(&name.as_str()));
        assert_eq!(sampled, want, "`every_type` has one of every type");
    }
}
