//! TypeScript types for CAF's own, through ts-rs: what the TypeScript
//! caf-sdk's `ts/sdk/src/types/` is generated from.
//!
//! Every type CAF serialises derives `TS` under the feature `ts`. Whole
//! numbers, `u64` and `i64` too, are `number`, as `JSON.parse` reads them:
//! every one a viewer is sent is below 2^53 (see the crate docs). The
//! generic types are exported with no app items of their own
//! (`NoAppItems`, which is `never`): ts-rs can't export them generic, as
//! it fills a type's parameters with its own placeholder, which isn't an
//! `AppItem`.
//!
//! Regenerate them with
//! `CAF_BLESS=1 cargo test -p caf-core --features ts typescript_types`.
//! Without `CAF_BLESS`, the same test fails if they're stale.

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
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use ts_rs::{Config, TS};

    use crate::acp::{Frame, Message, RequestId, RpcError, Shape};
    use crate::agent::{AgentSpec, EnvChange, McpServer, SpawnCommand};
    use crate::change::{Change, Numbered};
    use crate::chat::{
        Chat, ChatState, Compacted, Cost, Failure, FailureReason, Incompatible, Item, ItemKind,
        Lost, NoAppItems, Note, Part, Remedy, Starter, ToolCall, ToolKind, ToolStatus, Turn,
        TurnLife, Unknown, Usage, UserMessage,
    };
    use crate::core::{ConnectionInfo, OpenChat, Refused, SessionChoice};
    use crate::directive::{
        AgentDirective, Directive, Level, PermissionRequest, PolicyDirective, Report, Signal,
        StorageDirective, TimerDirective, ViewerDirective,
    };
    use crate::event::{AgentEvent, Event, Exit, HostCommand, PolicyAnswer, StorageEvent};
    use crate::host::{Grouping, Host, Limits, Shows, Timers};
    use crate::ids::{
        AgentName, AgentSessionId, ChangeNo, ChatId, ConnectionId, ItemId, QuestionId, Ticket,
        TurnId,
    };
    use crate::profiles::{AdapterInfo, Builtin, Checked, ProfileChoice, ProfileReport};
    use crate::question::{
        Answer, OptionId, OptionKind, Permission, PermissionOption, Question, QuestionKind,
        SlowChoice, SlowToOpen,
    };
    use crate::time::{End, Mono, Now, Wall, Window};
    use crate::viewer::{ChatSummary, FromViewer, OpenRequest, Outcome, RequestNo, ToViewer};

    /// Where the TypeScript caf-sdk keeps them.
    fn checked_in() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ts/sdk/src/types")
    }

    /// Writes every type, each with what it uses, to `dir`, and an
    /// `index.ts` exporting them all.
    fn export(dir: &Path) {
        let config = Config::new()
            .with_large_int("number")
            .with_import_extension(Some("ts"))
            .with_out_dir(dir);
        macro_rules! export {
            ($($ty:ty),* $(,)?) => {
                $(<$ty as TS>::export_all(&config)
                    .unwrap_or_else(|error| panic!("{}: {error}", stringify!($ty)));)*
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
            EnvChange,
            McpServer,
            SpawnCommand,
            // change
            Change,
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
            StorageEvent,
            // host
            Grouping,
            Host,
            Limits,
            Shows,
            Timers,
            // ids
            AgentName,
            AgentSessionId,
            ChangeNo,
            ChatId,
            ConnectionId,
            ItemId,
            QuestionId,
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
            ChatSummary,
            FromViewer,
            OpenRequest,
            Outcome,
            RequestNo,
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

    #[test]
    fn typescript_types_are_current() {
        let fresh = std::env::temp_dir().join(format!("caf-ts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&fresh);
        export(&fresh);
        let made = files(&fresh);
        let kept = checked_in();
        if std::env::var_os("CAF_BLESS").is_some() {
            let _ = std::fs::remove_dir_all(&kept);
            for (name, text) in &made {
                let path = kept.join(name);
                std::fs::create_dir_all(path.parent().expect("a folder")).expect("folders");
                std::fs::write(path, text).expect("a type is written");
            }
        } else {
            let have = files(&kept);
            let stale: Vec<&String> = (made.keys().chain(have.keys()))
                .filter(|name| made.get(*name) != have.get(*name))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            assert!(
                stale.is_empty(),
                "ts/sdk/src/types is stale: {stale:?}. Regenerate it with \
                 `CAF_BLESS=1 cargo test -p caf-core --features ts typescript_types`"
            );
        }
        let _ = std::fs::remove_dir_all(&fresh);
    }
}
