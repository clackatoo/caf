//! The inspector channel: what the debugger's server and its app say to
//! each other about CAF's steps, over the WebSocket `/inspect`.
//!
//! **Unstable.** Only the debugger's own app reads it, and it changes
//! with the app, in any release. What a host builds on is the viewer wire
//! ([`caf_core::viewer`]), which the debugger serves apart, on `/viewer`.
//!
//! It's JSON in CAF's one shape: every enum is `{"type": …, "value": …}`.
//! Its TypeScript types are generated from these, into
//! `ts/debugger/src/inspector/types/`, and import CAF's own from
//! `@clackatoo/caf`. Regenerate them with
//! `CAF_BLESS=1 cargo test -p caf-debugger --features ts inspector_types`;
//! without `CAF_BLESS`, the same test fails while they're stale.
//!
//! # Steps and positions
//!
//! A **step** is one event given to the core and the directives it gave
//! out ([`caf_driver_tokio::Step`]). Steps are numbered from 1, in order.
//!
//! A **position** is how many steps have been taken: at 0 the core is
//! fresh, and at `n` it's as it was just after step `n`. `end` is the
//! last step's number, so a position runs from 0 to `end`.
//!
//! # A link, in order
//!
//! 1. The server sends [`ToInspector::Hello`]: the channel's
//!    [`PROTOCOL`], live or replay, and which file. An app built for
//!    another protocol says it needs rebuilding, and goes no further.
//! 2. Every step so far, in one or more [`ToInspector::Steps`], then
//!    [`ToInspector::Position`]. If a check has failed already,
//!    [`ToInspector::Diverged`] says where.
//! 3. In replay, the chats as they are at the position, as
//!    [`ToInspector::Past`].
//! 4. Live, each new step comes in a [`ToInspector::Steps`] as it
//!    happens, and moves `end` on. While following, `at` moves with it.
//!
//! The app asks for what it needs with [`FromInspector`]. Every
//! [`FromInspector::StateAt`] is answered with one [`ToInspector::State`],
//! in the order asked. Of the moves ([`FromInspector::Seek`] and
//! [`FromInspector::Follow`]) waiting together when the server gets to
//! them, only the last is answered, before the states: a seek the app has
//! already moved on from isn't worth a replay.
//!
//! # Following and looking back
//!
//! Live, the app **follows** at first: it shows the chats through
//! `/viewer`, as any host's viewer does, and the position is the end.
//! When it seeks ([`FromInspector::Seek`]), it **looks back**: the
//! position stays where it was put while steps go on arriving, and the
//! chats as they were come as [`ToInspector::Past`], from a core of the
//! server's own given the steps up to there. [`FromInspector::Follow`]
//! goes back to following. A replay always looks back: there's nothing
//! live to follow.
//!
//! # Secrets
//!
//! Live, steps are exactly what CAF's core was given and gave out, so
//! they hold what a recording hides, such as the values of MCP servers'
//! variables and headers and the account's email in Claude's sign-in
//! status. So does [`ToInspector::State`]. The channel is only for the
//! user's own browser, on 127.0.0.1, behind the launch's token.

use caf_core::time::Now;
use caf_core::viewer::ToViewer;

/// The version of this channel, in [`ToInspector::Hello`]: one more each
/// time it changes. The app is built apart from the server, from the
/// same checkout, so after a pull one may be older than the other: an
/// app that meets a protocol it wasn't built for asks to be rebuilt.
pub const PROTOCOL: u32 = 2;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// A message from the server to the app.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
#[expect(
    clippy::large_enum_variant,
    reason = "its big variant holds a chat, sent and dropped: boxing it changes no wire, only Rust code matching it, for one word of size"
)]
pub enum ToInspector {
    /// The first message: what the server is doing.
    Hello {
        /// The channel's version, [`PROTOCOL`].
        protocol: u32,
        /// Live or replaying.
        mode: Mode,
        /// Live, the recording this run is written to, which `GET
        /// /recording` downloads, or null if it couldn't be made. In
        /// replay, the file being replayed.
        file: Option<String>,
        /// Live, this run of CAF, as `/viewer`'s `Welcome` names it. Null
        /// in replay.
        run: Option<String>,
        /// Whether CAF serves its own tools, so New chat may offer them:
        /// false in replay.
        tools: bool,
    },
    /// Steps, in order, with no gaps: the steps so far once the link
    /// starts, then each new one as it happens. The first is step `from`,
    /// one after the last step sent before. Each moves `end` on to its
    /// number.
    Steps {
        /// The first one's number.
        from: u64,
        /// The steps.
        steps: Vec<Step>,
    },
    /// Where the app is looking: sent as the link starts, and in answer
    /// to [`FromInspector::Seek`] and [`FromInspector::Follow`]. After
    /// it, live, a [`ToInspector::Steps`] moves `end` on, and `at` too if
    /// following.
    Position {
        /// Following live: the chats are `/viewer`'s, and `at` is the
        /// end. Always `false` in replay.
        live: bool,
        /// The position: see the module docs.
        at: u64,
        /// The last step's number.
        end: u64,
    },
    /// The core's whole state at a position, as [`caf_core::Core::inspect`]
    /// gives it: the answer to [`FromInspector::StateAt`]. Its shape
    /// changes with the core, without notice.
    ///
    /// Readings of the steady clock in it are nanoseconds, and pass 2^53
    /// after about 104 days of the machine running, when JavaScript would
    /// round them.
    State {
        /// The position.
        at: u64,
        /// The state.
        core: serde_json::Value,
    },
    /// A viewer message from the chats as they were at a position, for
    /// the app to give its copies of them, just as a viewer takes the
    /// same messages from `/viewer`. They come after a
    /// [`ToInspector::Position`] when looking back, and in replay:
    ///
    /// 1. first a [`ToViewer::Chats`]: every chat the core had at `at`,
    ///    oldest first. Unlike on `/viewer`, it lists closed chats too, so
    ///    the app can show how they ended. A chat not in it wasn't opened
    ///    yet: the app drops its copy;
    /// 2. then, for each of those chats, a [`ToViewer::Picture`] or a
    ///    [`ToViewer::Changes`], in the same order.
    ///
    /// Moving back gives every chat a Picture. Moving forward gives the
    /// changes since the last position, in Changes, or a Picture for a
    /// chat new since then or further behind than the core keeps
    /// changes for ([`caf_core::host::Limits::kept_changes`]). The first
    /// seek after the link starts or after [`FromInspector::Follow`]
    /// gives every chat a Picture, so the app needs no copies of its
    /// own beforehand. A chat with no changes since gets no message.
    ///
    /// The app should drop a Past whose `at` isn't its latest position:
    /// it was on its way before a later seek.
    Past {
        /// The position.
        at: u64,
        /// The message.
        message: ToViewer,
    },
    /// A replay check failed: at step `at`, a core of the server's own,
    /// given the step's event as a replay would give it, gave out other
    /// directives than CAF's own core did. That's a bug in CAF: something
    /// it does isn't decided by its events alone, or isn't in them, or
    /// isn't in a recording of them. See [`Check`] for the two checks.
    ///
    /// Each check sends only its first, as the link starts or when it
    /// happens, and checks no more after it. After a [`Check::Steps`]
    /// divergence, a replay of the steps no longer matches what CAF did,
    /// so what the server shows of later positions, as
    /// [`ToInspector::State`] and [`ToInspector::Past`], may be wrong.
    Diverged {
        /// Which check.
        check: Check,
        /// The step.
        at: u64,
        /// What CAF's core gave out.
        #[cfg_attr(feature = "ts", ts(as = "Vec<caf_core::Directive>"))]
        expected: Box<RawValue>,
        /// What the server's core gave out, or null if the event didn't
        /// read back from its JSON, so the core couldn't take it.
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<caf_core::Directive>>"))]
        got: Option<Box<RawValue>>,
    },
}

/// The replay checks, live: each is a core of the server's own, given
/// every step's event as some replay gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Check {
    /// The steps' own events, read back from JSON, as time travel replays
    /// them: it must give out exactly CAF's directives.
    Steps,
    /// The recording's lines, read back, as a replay of a bug report
    /// does. The recording hides secrets, so it must give out CAF's
    /// directives but for what those are part of: the values of MCP
    /// servers' variables and headers, in the messages that open and load
    /// sessions. It runs only while CAF records.
    Recording,
}

/// What the server is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Mode {
    /// Running CAF, with agents started for real: `/viewer` serves its
    /// chats.
    Live,
    /// Replaying a recording: nothing is started, and there's no
    /// `/viewer`.
    Replay,
}

/// One step: an event the core was given, and what it gave out.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Step {
    /// Its number, from 1.
    pub no: u64,
    /// When, as the core was told.
    pub now: Now,
    /// The event.
    #[cfg_attr(feature = "ts", ts(as = "caf_core::Event"))]
    pub event: Box<RawValue>,
    /// The directives the core gave out, in order. Live, CAF's own core's;
    /// in replay, the server's core's, since a recording holds none.
    #[cfg_attr(feature = "ts", ts(as = "Vec<caf_core::Directive>"))]
    pub directives: Box<RawValue>,
}

/// A message from the app to the server.
///
/// Every `StateAt` is answered with one `State`, in the order asked, even
/// when several wait together. Of the moves (`Seek` and `Follow`) waiting
/// together when the server gets to them, only the last is answered,
/// before the states.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum FromInspector {
    /// Look at a position: the answer is a [`ToInspector::Position`],
    /// then the chats as they were there, as [`ToInspector::Past`]. A
    /// position past the end is taken as the end. Live, this stops
    /// following. Of the moves waiting together, only the last is
    /// answered.
    Seek {
        /// The position.
        step: u64,
    },
    /// Follow live again: the answer is a [`ToInspector::Position`].
    /// In replay, nothing changes but the answer. Of the moves waiting
    /// together, only the last is answered.
    Follow,
    /// Ask for the core's state at a position. Each is answered with one
    /// [`ToInspector::State`], in the order asked, even when several wait
    /// together, such as a step's state and the one before it. A position
    /// past the end is taken as the end, and the State says which it was.
    StateAt {
        /// The position.
        step: u64,
    },
}

#[cfg(all(test, feature = "ts"))]
mod ts {
    //! The TypeScript types, and the check that they're current.

    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use ts_rs::{Config, TS};

    use super::{Check, FromInspector, Mode, Step, ToInspector};

    /// Where the app keeps them.
    fn dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ts/debugger/src/inspector/types")
    }

    /// One file of a type, as ts-rs writes it, but with every type that
    /// isn't one of `ours` imported from the TypeScript caf-sdk, which
    /// has them all.
    fn file<T: TS + 'static>(config: &Config, ours: &[String]) -> String {
        let text = T::export_to_string(config)
            .unwrap_or_else(|error| panic!("{}: {error}", T::ident(config)));
        let mut from_sdk = Vec::new();
        let mut lines = Vec::new();
        for line in text.lines() {
            match line
                .strip_prefix("import type { ")
                .and_then(|rest| rest.split_once(" }"))
            {
                Some((name, _)) if !ours.iter().any(|our| our == name) => {
                    from_sdk.push(name.to_owned());
                }
                _ => lines.push(line),
            }
        }
        let mut out = String::new();
        for line in lines {
            out += line;
            out += "\n";
            // The SDK's import goes after ts-rs's first line, which says
            // where the file came from.
            if line.starts_with("// This file was generated") && !from_sdk.is_empty() {
                from_sdk.sort();
                out += &format!(
                    "import type {{ {} }} from \"@clackatoo/caf\";\n",
                    from_sdk.join(", ")
                );
            }
        }
        out
    }

    /// Every file, by its name.
    fn files() -> BTreeMap<String, String> {
        let config = Config::new()
            .with_large_int("number")
            .with_import_extension(Some("ts"));
        let ours: Vec<String> = vec![
            ToInspector::ident(&config),
            Mode::ident(&config),
            Check::ident(&config),
            Step::ident(&config),
            FromInspector::ident(&config),
        ];
        let mut files = BTreeMap::new();
        files.insert("ToInspector.ts".into(), file::<ToInspector>(&config, &ours));
        files.insert("Mode.ts".into(), file::<Mode>(&config, &ours));
        files.insert("Check.ts".into(), file::<Check>(&config, &ours));
        files.insert("Step.ts".into(), file::<Step>(&config, &ours));
        files.insert(
            "FromInspector.ts".into(),
            file::<FromInspector>(&config, &ours),
        );
        let index: String = (ours.iter())
            .map(|name| format!("export type * from \"./{name}.ts\";\n"))
            .collect();
        files.insert(
            "index.ts".into(),
            format!(
                "// The inspector channel's types, generated from caf-debugger's `inspector.rs`:\n\
                 // don't edit. The channel is unstable: it changes with the app, and its\n\
                 // version is `PROTOCOL`, in `Hello`. Regenerate them from the repository's\n\
                 // root with\n\
                 // `CAF_BLESS=1 cargo test -p caf-debugger --features ts inspector_types`.\n{index}"
            ),
        );
        files
    }

    /// What's in the folder now.
    fn on_disk() -> BTreeMap<String, String> {
        let Ok(entries) = std::fs::read_dir(dir()) else {
            return BTreeMap::new();
        };
        (entries.map(|entry| entry.expect("a directory entry").path()))
            .map(|path| {
                let name = path
                    .file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned();
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                (name, text)
            })
            .collect()
    }

    #[test]
    fn inspector_types_are_current() {
        let files = files();
        if std::env::var_os("CAF_BLESS").is_some() {
            let dir = dir();
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("the folder is made");
            for (name, text) in &files {
                std::fs::write(dir.join(name), text).expect("a file is written");
            }
        }
        assert!(
            on_disk() == files,
            "ts/debugger/src/inspector/types is stale: regenerate it with \
             `CAF_BLESS=1 cargo test -p caf-debugger --features ts inspector_types`"
        );
    }
}
