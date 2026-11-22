//! What CAF knows about each adapter.
//!
//! A profile holds everything CAF knows about one adapter at one version:
//! - what to declare to it, and what those declarations do to it;
//! - which of its features have been checked;
//! - how to read its oddities;
//! - how to start and configure it;
//! - how its modes compare;
//! - how it describes actions;
//! - its recipe: how to install it, sign in and out, and tell whether
//!   it's signed in (see [`install`](crate::install)).
//!
//! The core uses profiles to translate each adapter's way of using ACP
//! into CAF's own terms, so nothing past this module knows which agent it
//! is talking to.
//!
//! **Every agent runs with none of its own tools.** Its built-in tools,
//! and the user's MCP servers, skills, plugins, hooks and instruction
//! files, are all turned off, as its profile says how; it has only the
//! tools the app gives it. Each will come back, one at a time, once CAF
//! supports it fully: its typed action, its permissions, and how it shows
//! in the record. CAF never runs an agent it can't strip, so an agent with
//! the cautious profile is refused.
//!
//! In place of the agent's built-in instructions, which describe tools it
//! no longer has, CAF gives it short ones of its own, then the app's
//! ([`OpenChat::instructions`](crate::OpenChat::instructions)). For a
//! host called `framewright`, CAF's say:
//!
//! > You are working inside framewright, the app the user is talking to
//! > you in. You have no tools of your own: you can't read or change
//! > files, run commands or browse the web, except through tools
//! > framewright gives you. Answer in Markdown.
//!
//! Profiles are CAF's own. Apps choose one for each agent but never
//! change one. Users will be able to add profiles as data in CAF's shared
//! folder (*later*).
//!
//! # The Claude profile
//!
//! Checked against `@agentclientprotocol/claude-agent-acp` 0.98.0.
//!
//! Its recipe installs that version with npm, from the same `package.json`
//! and `package-lock.json` clackatoo ships, which bring Claude Code itself
//! as a dependency: about 630 MB on Linux, and it needs Node 22 or later.
//! It signs in and out through the adapter, which hands `--cli` on to the
//! Claude Code it brings (`--cli auth login --claudeai`, whose link is on
//! `claude.com`, and `--cli auth logout`), and tells whether it's signed in
//! from the adapter's own `_auth/status_update`, or from
//! `--cli auth status --json`.
//!
//! It declares:
//! - **forms and links** (`elicitation.form`, `elicitation.url`): without
//!   forms, the adapter takes Claude's AskUserQuestion tool away. CAF
//!   declines forms itself while hosts can't show them.
//! - **notices** (`session.notices`): without them, the adapter says its
//!   notices as words, which can arrive while no turn is open and look
//!   like a turn of their own.
//! - **no files and no terminals**: CAF serves neither yet, and refuses
//!   them if asked.
//!
//! It **strips Claude Code of its own tools** (see below), through the
//! options the adapter hands on to Claude Code (`_meta.claudeCode.options`
//! in `session/new`, `session/load` and `session/resume`):
//! - `tools: []`: no built-in tools (Bash, Read, Edit, Write, Agent…);
//! - `settingSources: []`: none of the user's or project's settings files,
//!   so no CLAUDE.md or AGENTS.md, skills, subagents, plugins, hooks or
//!   permission rules of theirs;
//! - `strictMcpConfig: true`, `settings.disableClaudeAiConnectors`: only
//!   the MCP servers the app gives;
//! - `settings.disableRemoteControl`: the session can't be connected to
//!   claude.ai, even if the user sends the adapter's `/remote-control`;
//! - `systemPrompt`: CAF's own short instructions, saying what the agent
//!   is in and that it has only the app's tools, then the app's own
//!   ([`OpenChat::instructions`](crate::OpenChat::instructions)), in place
//!   of Claude Code's, which describe tools it no longer has;
//! - `promptSuggestions: false`, `verbatimPrompts: true`,
//!   `allowDangerouslySkipPermissions: false`, slash commands off
//!   (`extraArgs: { "disable-slash-commands": null }`), and auto-memory,
//!   bundled skills, CLAUDE.md files and claude.ai's connectors off
//!   through their variables (`env`, which the adapter adds to its own).
//!
//! Every one of them is part of what the adapter compares to tell whether
//! a session it has running can be reused, so a session loaded or resumed
//! with other instructions is started again with them.
//!
//! What can't be turned off this way: settings an organisation manages on
//! the machine, the adapter's own hooks (which only report), the plugins
//! built into Claude Code, and the adapter's own two commands, `/mcp` and
//! `/remote-control`, which it offers whatever Claude Code does and runs
//! itself when the user's message is one of them: they're the user's, not
//! tools the agent can call, and `/remote-control` fails, as Remote
//! Control is off. `/mcp` shows the app's MCP servers and reconnects
//! them. Plugins are off through `settingSources`, which say which are
//! enabled: Claude Code has no variable for it. Sign-in is untouched (its credentials
//! live in `~/.claude`), but a sign-in that lives in the user's
//! `settings.json` (an API key helper, a gateway's variables) isn't seen,
//! so the chat asks the user to sign in. The user's model choice still
//! applies.
//!
//! Its **settings** ([`settings`](crate::settings)) are ACP's session
//! config options: `model` (category `model`), `effort` (category
//! `thought_level`, its levels depending on the model) and fast mode (the
//! option `fast`, offered only while the model supports it), set with
//! `session/set_config_option`; it declares that it takes on/off options
//! (`session.configOptions.boolean`), and an on/off option offered as a
//! choice of `on` and `off` is read as a switch all the same. The rest it
//! offers (its modes, `ultracode`, `agent`) stay hidden, as it set them.
//! It reads the
//! user's own `settings.model` even with no settings sources, and on
//! resume prefers it to the session's own model, so CAF always applies the
//! chat's choice itself once a session opens. A model the agent changes
//! by itself, such as on a refusal, it reports in `config_option_update`.
//!
//! What a turn **spent** ([`usage`](crate::usage)) it says in its answer
//! to the prompt: ACP's `usage`, and the same split by model in
//! `_meta.quota.model_usage`, which counts the subagents and compaction
//! the turn set off too, and so is what CAF keeps; it's empty on a turn
//! stopped, whose `usage` CAF keeps instead, against the model the
//! adapter last named during the turn, in `usage_update`'s
//! `_meta._claude/model`. If no update named it, the row is against the
//! model the turn was sent on as its setting names it, an alias such as
//! `haiku` that a price table may not know: such a turn is unpriced, a
//! gap CAF knows of. Both count input uncached, and thinking as part of
//! output. What the session has cost so far it says in `usage_update`,
//! just before the answer.
//!
//! It declares **nothing of JetBrains AIR**. Any AIR key switches the
//! adapter into AIR's shapes for the whole connection: reads and searches
//! arrive without their text, subagents' words stop, failures move into
//! `_meta`, and subagents become sessions of their own. Each needs work
//! CAF hasn't done yet (recovering text from raw SDK messages, typed
//! failure records, child sessions), so AIR waits for the features that
//! need it.
//!
//! A newer adapter keeps the Claude profile with its risky features off;
//! feature 1 turns on nothing risky, so that changes nothing yet, and it's
//! stripped the same way. Any other adapter, or one that doesn't say what
//! it is, gets the cautious profile for reading what it sends, and as CAF
//! can't strip it, no session of it is opened: its chats are
//! [lost](crate::chat::Lost::Incompatible) to
//! [`Incompatible::OwnTools`](crate::chat::Incompatible::OwnTools). What
//! was declared at the start stays declared: declarations go out before
//! the adapter says who it is.
//!
//! # The fake and test profiles
//!
//! CAF's fake agent (`Builtin::Fake`, with the `fake` feature) and the
//! made-up agents of CAF's own tests do only what they're told, and have
//! no tools to turn off: nothing is sent to strip them, and CAF's
//! instructions aren't sent. The fake's profile is checked at version
//! 0.0.0, so a fake agent that names itself `caf-fake-agent`, at any
//! version, keeps it: its scripts may say any version they like. One
//! that names itself otherwise falls to the cautious profile, and its
//! chats get no session, as any other adapter's.
//!
//! The fake reads what a turn spent, its account's limits and why a
//! request failed (`data.errorKind`) as the Claude profile does, so a
//! script can send a split by model and name its model in `usage_update`,
//! say its limits, and fail a turn for them; the test agents read only
//! ACP's `usage`, as any agent might send, and fail every turn as
//! `Other`.
//!
//! The fake, and the test agents but one, list their settings as Claude
//! does, as ACP's session config options, with fast mode the option
//! `fast`; the fake declares it takes on/off options. The made-up
//! `models-acp` lists only its models, the older way, as Gemini's adapter
//! did: in the unstable `models` field, set with `session/set_model`.

use agent_client_protocol_schema::v1 as acp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::acp::RpcError;
use crate::chat::{FailureReason, Remedy};
use crate::install::{
    Bundled, Install, LinkFrom, Needs, NpmPackage, Program, Recipe, SignIn, SignedInAs, Status,
    StatusCommand,
};
use crate::limits::{AccountLimits, ExtraUsage, LimitState, LimitWindow, WindowKind};
use crate::time::Wall;
use crate::usage::Tokens;

/// Which profile an agent should have.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum ProfileChoice {
    /// One CAF knows.
    Builtin(Builtin),
    /// The cautious profile, for agents CAF doesn't know: plain ACP,
    /// nothing risky switched on. CAF can't turn off such an agent's own
    /// tools, so it doesn't run one: a chat with it is refused
    /// ([`Refused::OwnTools`](crate::Refused::OwnTools)). It's here for
    /// user profiles (*later*), which will say an agent behaves like one
    /// CAF knows, and so how to strip it.
    Cautious,
}

/// The adapters CAF has profiles for.
///
/// Closed to apps: a new adapter gets a profile by being added to CAF,
/// where it's tested against recordings like the rest. `non_exhaustive`
/// lets CAF add more without breaking anyone's `match`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Builtin {
    /// Claude Code, through `@agentclientprotocol/claude-agent-acp`.
    /// Checked at 0.98.0.
    ClaudeAgentAcp,
    /// A made-up agent installed with npm, that says by itself whether
    /// it's signed in. For tests only, so nothing assumes one agent.
    #[cfg(test)]
    #[cfg_attr(feature = "ts", ts(skip))]
    TestNpm,
    /// A made-up agent downloaded per platform, whose sign-in prints its
    /// link and a code and takes a paste, and that has a status command.
    /// For tests only.
    #[cfg(test)]
    #[cfg_attr(feature = "ts", ts(skip))]
    TestDownload,
    /// A made-up plain ACP adapter, with no tools and no sign-in: for
    /// tests of what any adapter may do. CAF can't install it. For tests
    /// only.
    #[cfg(test)]
    #[cfg_attr(feature = "ts", ts(skip))]
    TestPlain,
    /// A made-up plain ACP adapter that lists its models the older way,
    /// as Gemini's did: the unstable `models` field, set with
    /// `session/set_model`, and no config options. For tests only.
    #[cfg(test)]
    #[cfg_attr(feature = "ts", ts(skip))]
    TestModels,
    /// CAF's fake agent, `caf-fake-agent`, for trying installs and
    /// sign-ins with no real agent: its install always fails at once, as
    /// offline, and it signs in, out and says whether it's signed in
    /// through `caf-fake-agent auth`, as the file `CAF_FAKE_AGENT_AUTH`
    /// names says. Only with the `fake` feature, for tools and their
    /// tests, such as caf-debugger's.
    #[cfg(feature = "fake")]
    #[cfg_attr(feature = "ts", ts(skip))]
    Fake,
}

impl Builtin {
    /// A name for it that never changes, whatever a host calls the agent:
    /// what its install folders, and the file its account's
    /// [limits](crate::limits) are kept in, are named after.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::ClaudeAgentAcp => "claude-agent-acp",
            #[cfg(test)]
            Self::TestNpm => "test-npm",
            #[cfg(test)]
            Self::TestDownload => "test-download",
            #[cfg(test)]
            Self::TestPlain => "test-plain",
            #[cfg(test)]
            Self::TestModels => "test-models",
            #[cfg(feature = "fake")]
            Self::Fake => "caf-fake-agent",
        }
    }

    /// The profile with this [slug](Builtin::slug), if any.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        // Every profile in this build: one added is added here too.
        let every = [
            Self::ClaudeAgentAcp,
            #[cfg(test)]
            Self::TestNpm,
            #[cfg(test)]
            Self::TestDownload,
            #[cfg(test)]
            Self::TestPlain,
            #[cfg(test)]
            Self::TestModels,
            #[cfg(feature = "fake")]
            Self::Fake,
        ];
        every.into_iter().find(|each| each.slug() == slug)
    }

    /// The name its adapter gives in `initialize`.
    fn adapter_name(self) -> &'static str {
        match self {
            Self::ClaudeAgentAcp => CLAUDE_NAME,
            #[cfg(test)]
            Self::TestNpm => "test-npm-acp",
            #[cfg(test)]
            Self::TestDownload => "test-download-acp",
            #[cfg(test)]
            Self::TestPlain => "plain-acp",
            #[cfg(test)]
            Self::TestModels => "models-acp",
            #[cfg(feature = "fake")]
            Self::Fake => FAKE_NAME,
        }
    }

    /// The version of its adapter its profile was checked at.
    fn checked_at(self) -> [u64; 3] {
        match self {
            Self::ClaudeAgentAcp => CLAUDE_CHECKED,
            #[cfg(test)]
            Self::TestNpm => [2, 0, 0],
            #[cfg(test)]
            Self::TestDownload => [1, 4, 0],
            #[cfg(test)]
            Self::TestPlain => [1, 0, 0],
            #[cfg(test)]
            Self::TestModels => [1, 0, 0],
            #[cfg(feature = "fake")]
            Self::Fake => [0, 0, 0],
        }
    }

    /// How to install it and sign in to it.
    pub(crate) fn recipe(self) -> Recipe {
        match self {
            Self::ClaudeAgentAcp => claude_recipe(),
            #[cfg(test)]
            Self::TestNpm => tests::npm_recipe(),
            #[cfg(test)]
            Self::TestDownload => tests::download_recipe(),
            #[cfg(test)]
            Self::TestPlain | Self::TestModels => tests::plain_recipe(),
            #[cfg(feature = "fake")]
            Self::Fake => fake_recipe(),
        }
    }
}

/// What became of a connection's profile once its adapter said who it
/// is.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ProfileReport {
    /// The profile in use.
    pub profile: ProfileChoice,
    /// The adapter's name and version, as it gave them in `initialize`.
    pub adapter: Option<AdapterInfo>,
    /// How far that adapter is checked.
    pub checked: Checked,
}

/// An adapter's name and version, as it gave them.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AdapterInfo {
    /// Its name, such as `@agentclientprotocol/claude-agent-acp`.
    pub name: String,
    /// Its version.
    pub version: String,
}

/// How far CAF has checked the adapter it's talking to.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Checked {
    /// This exact version is checked: every feature its profile lists is
    /// on.
    Yes,
    /// A newer version than any checked: the last checked profile is
    /// used, with its risky features off until this version is checked.
    Newer,
    /// The adapter isn't the one its agent's profile is for, or didn't say
    /// what it is: the cautious profile is used instead.
    Mismatch,
    /// The agent was given the cautious profile.
    Cautious,
}

/// The adapter the Claude profile is for, as it names itself.
const CLAUDE_NAME: &str = "@agentclientprotocol/claude-agent-acp";

/// The version of it the Claude profile was checked against.
const CLAUDE_CHECKED: [u64; 3] = [0, 98, 0];

/// The adapter Claude's recipe runs, inside its install.
const CLAUDE_ADAPTER: &str = "node_modules/.bin/claude-agent-acp";

/// Claude's recipe: claude-agent-acp 0.98.0 from npm, signed in to through
/// the Claude Code it brings.
fn claude_recipe() -> Recipe {
    let adapter = |args: &[&str]| Bundled {
        program: Program::Adapter,
        args: args.iter().map(|&arg| arg.to_owned()).collect(),
        env: Vec::new(),
    };
    let how = [
        ("claude.ai", SignedInAs::Account),
        // A long-lived token from `claude setup-token`: a subscription's.
        ("oauth_token", SignedInAs::Account),
        ("api_key", SignedInAs::ApiKey),
        ("api_key_helper", SignedInAs::ApiKey),
        // Bedrock, Vertex and other clouds.
        ("third_party", SignedInAs::Gateway),
    ];
    Recipe {
        version: "0.98.0".to_owned(),
        install: Install::Npm(NpmPackage {
            manifest: include_str!("profiles/claude-0.98.0/package.json").to_owned(),
            lockfile: include_str!("profiles/claude-0.98.0/package-lock.json").to_owned(),
        }),
        // Measured on Linux x86_64, where npm brings both the glibc and the
        // musl builds of Claude Code.
        size: 626_797_331,
        needs: Needs {
            node: Some(22),
            npm: true,
        },
        adapter: Bundled {
            program: Program::InInstall(CLAUDE_ADAPTER.into()),
            args: Vec::new(),
            env: Vec::new(),
        },
        sign_in: Some(SignIn {
            command: adapter(&["--cli", "auth", "login", "--claudeai"]),
            link: LinkFrom::Browser,
            code_after: None,
            // When the browser is elsewhere, the page shows a code to paste
            // back: Claude Code asks for it on its input.
            paste: true,
            sites: vec!["claude.com".to_owned(), "claude.ai".to_owned()],
        }),
        sign_out: Some(adapter(&["--cli", "auth", "logout"])),
        status: Status {
            pushed: true,
            command: Some(StatusCommand {
                command: adapter(&["--cli", "auth", "status", "--json"]),
                signed_in: "/loggedIn".to_owned(),
                how: Some((
                    "/authMethod".to_owned(),
                    how.into_iter()
                        .map(|(value, how)| (value.to_owned(), how))
                        .collect(),
                )),
            }),
        },
    }
}

/// The fake agent, as it names itself in `initialize`.
#[cfg(feature = "fake")]
const FAKE_NAME: &str = "caf-fake-agent";

/// The fake agent's recipe. Its install is a download, for every
/// platform, from port 1 of this machine, where nothing listens, so it
/// fails at once, as offline, and never installs anything. It signs in,
/// out and says whether it's signed in with its adapter's `auth`
/// commands: the sign-in opens its link with `BROWSER`, shows a code
/// after `Your code: `, and takes a paste, and its link must be to
/// `caf.example`.
#[cfg(feature = "fake")]
fn fake_recipe() -> Recipe {
    use crate::install::{Artifact, Platform, Unpack};

    let adapter = |args: &[&str]| Bundled {
        program: Program::Adapter,
        args: args.iter().map(|&arg| arg.to_owned()).collect(),
        env: Vec::new(),
    };
    let platforms = ["linux", "macos", "windows"]
        .into_iter()
        .flat_map(|os| ["x86_64", "aarch64"].map(|arch| Platform::new(os, arch)));
    let artifacts = platforms
        .map(|platform| {
            Artifact::new(
                platform,
                "https://127.0.0.1:1/caf-fake-agent",
                "0".repeat(64),
                Unpack::No,
            )
        })
        .collect();
    Recipe {
        version: "0.0.0".to_owned(),
        install: Install::Download(artifacts),
        size: 4_000_000,
        needs: Needs::default(),
        adapter: Bundled {
            program: Program::InInstall("caf-fake-agent".into()),
            args: Vec::new(),
            env: Vec::new(),
        },
        sign_in: Some(SignIn {
            command: adapter(&["auth", "login"]),
            link: LinkFrom::Browser,
            code_after: Some("Your code: ".to_owned()),
            paste: true,
            sites: vec!["caf.example".to_owned()],
        }),
        sign_out: Some(adapter(&["auth", "logout"])),
        status: Status {
            pushed: false,
            command: Some(StatusCommand {
                command: adapter(&["auth", "status"]),
                signed_in: "/loggedIn".to_owned(),
                how: Some((
                    "/authMethod".to_owned(),
                    vec![
                        ("claude.ai".to_owned(), SignedInAs::Account),
                        ("api_key".to_owned(), SignedInAs::ApiKey),
                    ],
                )),
            }),
        },
    }
}

/// ACP's error for "the user must sign in first".
pub(crate) const AUTH_REQUIRED: i64 = -32000;

/// ACP's error for something that doesn't exist, such as a session.
pub(crate) const RESOURCE_NOT_FOUND: i64 = -32002;

impl ProfileChoice {
    /// What CAF declares to an adapter with this profile, as ACP's
    /// `clientCapabilities`.
    pub(crate) fn declarations(&self) -> Value {
        let mut declared = json!({
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
        });
        match self {
            Self::Builtin(Builtin::ClaudeAgentAcp) => {
                declared["session"] = json!({ "notices": {}, "configOptions": { "boolean": {} } });
                declared["elicitation"] = json!({ "form": {}, "url": {} });
            }
            #[cfg(test)]
            Self::Builtin(
                Builtin::TestNpm | Builtin::TestDownload | Builtin::TestPlain | Builtin::TestModels,
            ) => {}
            // It offers its settings as its script says, on/off ones too.
            #[cfg(feature = "fake")]
            Self::Builtin(Builtin::Fake) => {
                declared["session"] = json!({ "configOptions": { "boolean": {} } });
            }
            Self::Cautious => {}
        }
        declared
    }

    /// What becomes of this profile once the adapter said who it is, as
    /// `adapter`, or didn't.
    pub(crate) fn check(&self, adapter: Option<AdapterInfo>) -> ProfileReport {
        let checked = match self {
            Self::Cautious => Checked::Cautious,
            Self::Builtin(builtin) => match &adapter {
                Some(info) if info.name == builtin.adapter_name() => match version(&info.version) {
                    Some((version, false)) if version == builtin.checked_at() => Checked::Yes,
                    Some((version, _)) if version > builtin.checked_at() => Checked::Newer,
                    _ => Checked::Mismatch,
                },
                _ => Checked::Mismatch,
            },
        };
        let profile = match checked {
            Checked::Yes | Checked::Newer => self.clone(),
            Checked::Mismatch | Checked::Cautious => Self::Cautious,
        };
        ProfileReport {
            profile,
            adapter,
            checked,
        }
    }

    /// The recipe for installing and signing in to this profile's agent,
    /// if it has one.
    pub(crate) fn recipe(&self) -> Option<Recipe> {
        match self {
            Self::Builtin(builtin) => Some(builtin.recipe()),
            Self::Cautious => None,
        }
    }

    /// Whether CAF can turn off the own tools of an agent with this
    /// profile: every profile but the cautious one says how, or says its
    /// agent has none.
    pub(crate) fn strips(&self) -> bool {
        match self {
            Self::Builtin(_) => true,
            Self::Cautious => false,
        }
    }

    /// What to add to `session/new`, `session/load` and `session/resume`,
    /// as their `_meta`, so the agent has none of its own tools: given
    /// CAF's own instructions to it, and the app's (`instructions`).
    /// `None` if nothing is added: the test and fake agents have no tools
    /// to turn off, and the cautious profile doesn't say how.
    pub(crate) fn stripped(&self, own: &str, instructions: Option<&str>) -> Option<Value> {
        match self {
            Self::Builtin(Builtin::ClaudeAgentAcp) => Some(claude_stripped(own, instructions)),
            // Made-up agents, with no tools.
            #[cfg(test)]
            Self::Builtin(
                Builtin::TestNpm | Builtin::TestDownload | Builtin::TestPlain | Builtin::TestModels,
            ) => None,
            // CAF's fake agent does only what its script says: it has no
            // tools.
            #[cfg(feature = "fake")]
            Self::Builtin(Builtin::Fake) => None,
            Self::Cautious => None,
        }
    }

    /// Whether an adapter with this profile says by itself whether it's
    /// signed in (`_auth/status_update`).
    pub(crate) fn pushes_status(&self) -> bool {
        // Not from the recipe, which holds a whole lockfile: it's asked on
        // every report.
        match self {
            Self::Builtin(Builtin::ClaudeAgentAcp) => true,
            #[cfg(test)]
            Self::Builtin(Builtin::TestNpm) => true,
            #[cfg(test)]
            Self::Builtin(Builtin::TestDownload | Builtin::TestPlain | Builtin::TestModels) => {
                false
            }
            #[cfg(feature = "fake")]
            Self::Builtin(Builtin::Fake) => false,
            Self::Cautious => false,
        }
    }

    /// Why a request failed, as this profile reads `error`: what went
    /// wrong in CAF's terms, what the user can do, and the agent's own
    /// words, uncleaned.
    pub(crate) fn failure(&self, error: &RpcError) -> Said {
        let words = words(error);
        if error.code == AUTH_REQUIRED {
            return Said {
                reason: FailureReason::SignInNeeded,
                remedy: None,
                words,
            };
        }
        let data = data(error);
        let kind = data
            .as_ref()
            .and_then(|data| data.get("errorKind"))
            .and_then(Value::as_str);
        let (reason, remedy) = match (self, kind) {
            (Self::Builtin(Builtin::ClaudeAgentAcp), Some(kind)) => claude_kind(kind),
            #[cfg(feature = "fake")]
            (Self::Builtin(Builtin::Fake), Some(kind)) => claude_kind(kind),
            _ => (FailureReason::Other, Some(Remedy::SendAgain)),
        };
        Said {
            reason,
            remedy,
            words,
        }
    }

    /// Whether `error`, the answer to a request about a session, says the
    /// adapter doesn't have that session: ACP's "resource not found", or,
    /// from Claude's adapter, the text it throws.
    pub(crate) fn session_gone(&self, error: &RpcError) -> bool {
        if error.code == RESOURCE_NOT_FOUND {
            return true;
        }
        match self {
            Self::Builtin(Builtin::ClaudeAgentAcp) => {
                words(error).is_some_and(|words| words.contains("Session not found"))
            }
            #[cfg(test)]
            Self::Builtin(
                Builtin::TestNpm | Builtin::TestDownload | Builtin::TestPlain | Builtin::TestModels,
            ) => false,
            #[cfg(feature = "fake")]
            Self::Builtin(Builtin::Fake) => false,
            Self::Cautious => false,
        }
    }

    /// Whether a line of the adapter's error output says the user must
    /// sign in.
    pub(crate) fn wants_sign_in(&self, line: &str) -> bool {
        match self {
            Self::Builtin(Builtin::ClaudeAgentAcp) => {
                line.contains("/login")
                    || line.contains("Invalid API key")
                    || line.contains("Not logged in")
            }
            #[cfg(test)]
            Self::Builtin(
                Builtin::TestNpm | Builtin::TestDownload | Builtin::TestPlain | Builtin::TestModels,
            ) => false,
            #[cfg(feature = "fake")]
            Self::Builtin(Builtin::Fake) => false,
            Self::Cautious => false,
        }
    }
}

impl ProfileChoice {
    /// How an adapter with this profile lists its settings
    /// ([`settings`](crate::settings)), and which of them CAF supports.
    pub(crate) fn settings(&self) -> SettingsStyle {
        // ACP's session config options, or, from an agent that doesn't
        // list them, the older `models` field: never both.
        let options = SettingsStyle {
            options: true,
            models: true,
            fast: Some("fast"),
        };
        match self {
            // Its fast mode is the option `fast`, in the category
            // `model_config` with `ultracode`.
            Self::Builtin(Builtin::ClaudeAgentAcp) => options,
            #[cfg(test)]
            Self::Builtin(Builtin::TestNpm | Builtin::TestDownload | Builtin::TestPlain) => options,
            #[cfg(test)]
            Self::Builtin(Builtin::TestModels) => SettingsStyle {
                options: false,
                models: true,
                fast: None,
            },
            #[cfg(feature = "fake")]
            Self::Builtin(Builtin::Fake) => options,
            // Never run, as CAF can't strip it.
            Self::Cautious => SettingsStyle {
                options: false,
                models: false,
                fast: None,
            },
        }
    }
}

impl ProfileChoice {
    /// What a turn spent, as an adapter with this profile says in its
    /// answer to the prompt, `result`. Any agent may give ACP's `usage`;
    /// Claude's adapter, and the fake, split it by model too
    /// (`_meta.quota.model_usage`), counting the subagents and compaction
    /// the turn set off, so the split is taken when it has rows. It's
    /// empty on a turn stopped, whose `usage` still counts. A split that
    /// can't be read gives way to `usage` too, and says so: `true`.
    pub(crate) fn spent(&self, result: &Value) -> (Reported, bool) {
        let split = match self {
            Self::Builtin(Builtin::ClaudeAgentAcp) => split_by_model(result),
            #[cfg(test)]
            Self::Builtin(
                Builtin::TestNpm | Builtin::TestDownload | Builtin::TestPlain | Builtin::TestModels,
            ) => Reported::Nothing,
            #[cfg(feature = "fake")]
            Self::Builtin(Builtin::Fake) => split_by_model(result),
            Self::Cautious => Reported::Nothing,
        };
        match split {
            Reported::Nothing => (acp_usage(result), false),
            Reported::Unreadable => (acp_usage(result), true),
            split => (split, false),
        }
    }

    /// The model an adapter with this profile says it's using, in a
    /// `usage_update`, `update`: Claude's adapter, and the fake, name it in
    /// `_meta._claude/model`. Uncleaned.
    pub(crate) fn model_in_usage(&self, update: &Value) -> Option<String> {
        let claude = || {
            (update.pointer("/_meta/_claude~1model"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        match self {
            Self::Builtin(Builtin::ClaudeAgentAcp) => claude(),
            #[cfg(test)]
            Self::Builtin(
                Builtin::TestNpm | Builtin::TestDownload | Builtin::TestPlain | Builtin::TestModels,
            ) => None,
            #[cfg(feature = "fake")]
            Self::Builtin(Builtin::Fake) => claude(),
            Self::Cautious => None,
        }
    }

    /// What an adapter with this profile says of its account's limits in
    /// a `usage_update`, `update`, heard at `now`: Claude's adapter, and
    /// the fake, say it in `_meta._claude/rateLimit` ([`crate::limits`]
    /// says how it's read). `None` if it says nothing that reads.
    pub(crate) fn limits(&self, update: &Value, now: Wall) -> Option<AccountLimits> {
        match self {
            Self::Builtin(Builtin::ClaudeAgentAcp) => claude_limits(update, now),
            #[cfg(test)]
            Self::Builtin(
                Builtin::TestNpm | Builtin::TestDownload | Builtin::TestPlain | Builtin::TestModels,
            ) => None,
            #[cfg(feature = "fake")]
            Self::Builtin(Builtin::Fake) => claude_limits(update, now),
            Self::Cautious => None,
        }
    }
}

/// The earliest reset CAF keeps: 2020-01-01, UTC.
const EARLIEST_RESET: Wall = Wall(1_577_836_800_000);

/// The first reset CAF doesn't keep: 2100-01-01, UTC.
const LATEST_RESET: Wall = Wall(4_102_444_800_000);

/// The longest name of a window CAF keeps, in bytes.
const MOST_WINDOW_NAME: usize = 64;

/// What Claude's adapter says of its account's limits in a `usage_update`,
/// `update`, heard at `now`: `_meta._claude/rateLimit`, the Claude Agent
/// SDK's `rate_limit_info`. `unifiedWindows`' windows, then the limit
/// itself if it's of a kind they don't hold; and whether extra usage is in
/// use. `None` if nothing in it reads.
fn claude_limits(update: &Value, now: Wall) -> Option<AccountLimits> {
    let info = update.pointer("/_meta/_claude~1rateLimit")?;
    let status = |fields: &Value| {
        fields
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let limit = (info.get("rateLimitType").and_then(Value::as_str)).and_then(claude_window_kind);
    let limit_status = status(info);
    let unified = (info.get("unifiedWindows").and_then(Value::as_object))
        .into_iter()
        .flatten()
        .filter_map(|(name, fields)| {
            let kind = claude_window_kind(name)?;
            // The limit's own status, for a window of its kind, wins if
            // it's refused, and stands in for one the window doesn't say.
            let limits_own = limit_status
                .clone()
                .filter(|_| limit.as_ref() == Some(&kind));
            let status = match (status(fields), limits_own) {
                (_, Some(refused)) if refused == "rejected" => Some(refused),
                (Some(own), _) => Some(own),
                (None, limits_own) => limits_own,
            };
            claude_window(kind, fields, status.as_deref(), now)
        });
    let itself = limit
        .clone()
        .and_then(|kind| claude_window(kind, info, limit_status.as_deref(), now));
    let mut windows: Vec<LimitWindow> = Vec::new();
    for window in unified.chain(itself) {
        if windows.iter().all(|kept| kept.kind != window.kind) {
            windows.push(window);
        }
    }
    windows.sort_by(|one, other| one.kind.cmp(&other.kind));
    let extra_usage = (info.get("isUsingOverage").and_then(Value::as_bool))
        .map(|in_use| ExtraUsage { in_use, heard: now });
    if windows.is_empty() && extra_usage.is_none() {
        return None;
    }
    Some(AccountLimits {
        windows,
        extra_usage,
        cleared: None,
    })
}

/// The window Claude names `name`: `None` for `overage`, or a name that
/// isn't plain.
fn claude_window_kind(name: &str) -> Option<WindowKind> {
    let plain = !name.is_empty()
        && name.len() <= MOST_WINDOW_NAME
        && (name.bytes()).all(|byte| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte));
    if !plain {
        return None;
    }
    Some(match name {
        "overage" => return None,
        "five_hour" => WindowKind::FiveHours,
        "seven_day" => WindowKind::Week,
        "seven_day_overage_included" => WindowKind::ExtraWeek,
        _ => match name
            .strip_prefix("seven_day_")
            .filter(|model| !model.is_empty())
        {
            Some(model) => WindowKind::ModelWeek(model.to_ascii_lowercase()),
            None => WindowKind::Other(name.to_owned()),
        },
    })
}

/// A window of Claude's, of `kind`, as `fields` say it, with `status`:
/// its share used (`utilization`, 0 to 1) and its reset (`resetsAt`, in
/// seconds, or in milliseconds if it's above 10^11, which no time in
/// seconds is before the year 5000). `None` if it has no share that
/// reads and isn't refused.
fn claude_window(
    kind: WindowKind,
    fields: &Value,
    status: Option<&str>,
    now: Wall,
) -> Option<LimitWindow> {
    let state = match status {
        Some("allowed_warning") => LimitState::Warning,
        Some("rejected") => LimitState::Refused,
        // Allowed, or a status CAF doesn't know: limits are information
        // only, so the cautious reading is no help.
        _ => LimitState::Fine,
    };
    let share = (fields.get("utilization").and_then(Value::as_f64))
        .filter(|share| share.is_finite())
        .map(|share| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "kept to 0-100 first"
            )]
            let used = (share * 100.0).round().clamp(0.0, 100.0) as u8;
            used
        });
    let used = match state {
        LimitState::Refused => 100,
        _ => share?,
    };
    let resets = (fields.get("resetsAt").and_then(Value::as_f64))
        .filter(|at| at.is_finite())
        .map(|at| {
            if at > 1e11 {
                at.round()
            } else {
                (at * 1000.0).round()
            }
        })
        .filter(|millis| (EARLIEST_RESET.0 as f64..LATEST_RESET.0 as f64).contains(millis))
        .map(|millis| {
            #[expect(clippy::cast_possible_truncation, reason = "kept to 2020-2100 first")]
            let millis = millis as i64;
            Wall(millis)
        });
    Some(LimitWindow {
        kind,
        used,
        resets,
        state,
        heard: now,
    })
}

/// What a turn spent, as its agent said it, read into CAF's terms but not
/// yet checked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reported {
    /// The agent didn't say.
    Nothing,
    /// It said, but not as counts CAF can read.
    Unreadable,
    /// Its tokens, one row per model, never none.
    Rows(Vec<Reckoned>),
}

/// One model's tokens, as an agent said them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Reckoned {
    /// The model, by the agent's id for it: `None` if it didn't say, as
    /// ACP's `usage` doesn't.
    pub(crate) model: Option<String>,
    /// Its tokens.
    pub(crate) tokens: Tokens,
    /// Every token, as the agent added them up.
    pub(crate) total: u64,
}

/// ACP's `usage`, in the answer to a prompt, `result`. Its `inputTokens`
/// are uncached.
fn acp_usage(result: &Value) -> Reported {
    let Some(usage) = result.get("usage").filter(|usage| !usage.is_null()) else {
        return Reported::Nothing;
    };
    let Ok(usage) = serde_json::from_value::<acp::Usage>(usage.clone()) else {
        return Reported::Unreadable;
    };
    Reported::Rows(vec![Reckoned {
        model: None,
        tokens: Tokens {
            input: usage.input_tokens,
            cache_read: usage.cached_read_tokens.unwrap_or(0),
            cache_write: usage.cached_write_tokens.unwrap_or(0),
            output: usage.output_tokens,
            thinking: usage.thought_tokens,
        },
        total: usage.total_tokens,
    }])
}

/// The split by model Claude's adapter gives, in the answer to a prompt,
/// `result`: `_meta.quota.model_usage`. Its `inputTokens` are uncached
/// too. Codex's adapter is said to send the same, but no profile reads
/// it, and what its `inputTokens` count is unchecked. `Nothing` if it
/// isn't there or has no rows.
fn split_by_model(result: &Value) -> Reported {
    /// A row of it.
    #[derive(Deserialize)]
    struct Row {
        model: String,
        token_count: Count,
    }
    /// A row's tokens.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Count {
        total_tokens: u64,
        input_tokens: u64,
        #[serde(default)]
        cached_input_tokens: u64,
        #[serde(default)]
        cached_write_tokens: u64,
        output_tokens: u64,
        #[serde(default)]
        reasoning_output_tokens: Option<u64>,
    }
    let Some(split) = (result.pointer("/_meta/quota/model_usage")).filter(|split| !split.is_null())
    else {
        return Reported::Nothing;
    };
    let Ok(rows) = serde_json::from_value::<Vec<Row>>(split.clone()) else {
        return Reported::Unreadable;
    };
    if rows.is_empty() {
        return Reported::Nothing;
    }
    let rows = rows.into_iter().map(|row| Reckoned {
        model: Some(row.model),
        tokens: Tokens {
            input: row.token_count.input_tokens,
            cache_read: row.token_count.cached_input_tokens,
            cache_write: row.token_count.cached_write_tokens,
            output: row.token_count.output_tokens,
            thinking: row.token_count.reasoning_output_tokens,
        },
        total: row.token_count.total_tokens,
    });
    Reported::Rows(rows.collect())
}

/// How an adapter lists its settings, as its profile says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SettingsStyle {
    /// It lists ACP's session config options (`configOptions`), set with
    /// `session/set_config_option`: the one in the category `model` is
    /// its model, and the one in `thought_level` its effort.
    pub(crate) options: bool,
    /// It lists its models in the older, unstable `models` field, set with
    /// `session/set_model`. Read only if it lists no config options.
    pub(crate) models: bool,
    /// The id of its config option for fast mode, if it has one: ACP has
    /// no category for it.
    pub(crate) fast: Option<&'static str>,
}

/// CAF's own instructions to every agent, `host` being the app's name:
/// they take the place of the agent's built-in ones, which describe tools
/// it no longer has. The app's own come after them.
pub(crate) fn own_instructions(host: &str) -> String {
    format!(
        "You are working inside {host}, the app the user is talking to you in. You have no \
         tools of your own: you can't read or change files, run commands or browse the web, \
         except through tools {host} gives you. Answer in Markdown."
    )
}

/// The `_meta` that runs Claude Code with none of its own tools, with
/// CAF's instructions (`own`) and the app's as its whole system prompt.
/// Checked against claude-agent-acp 0.98.0, which spreads
/// `_meta.claudeCode.options` into the Claude Agent SDK's options as they
/// are, merging `env` into its own and `extraArgs` into its own.
fn claude_stripped(own: &str, instructions: Option<&str>) -> Value {
    let prompt = match instructions.filter(|text| !text.is_empty()) {
        Some(text) => format!("{own}\n\n{text}"),
        None => own.to_owned(),
    };
    json!({ "claudeCode": { "options": {
        // No built-in tools: the SDK passes `--tools ""`.
        "tools": [],
        // None of the user's, project's or local settings files: no
        // CLAUDE.md, skills, subagents, plugins, hooks or permission rules
        // of theirs.
        "settingSources": [],
        // Only the MCP servers the app gives, in `mcpServers`.
        "strictMcpConfig": true,
        "systemPrompt": prompt,
        "promptSuggestions": false,
        // The user's words as they are: no `@path` expansion or slash
        // commands, and no CLAUDE.md or rules files sent with them.
        "verbatimPrompts": true,
        "allowDangerouslySkipPermissions": false,
        // No claude.ai connectors, and no Remote Control: the adapter
        // still offers `/remote-control`, but Claude Code refuses it, so a
        // session is never reachable from claude.ai.
        "settings": { "disableClaudeAiConnectors": true, "disableRemoteControl": true },
        "env": {
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1",
            "CLAUDE_CODE_DISABLE_BUNDLED_SKILLS": "1",
            "CLAUDE_CODE_DISABLE_CLAUDE_MDS": "1",
            "ENABLE_CLAUDEAI_MCP_SERVERS": "false",
        },
        "extraArgs": { "disable-slash-commands": null },
    } } })
}

/// A failure as a profile reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Said {
    /// What went wrong, in CAF's terms.
    pub(crate) reason: FailureReason,
    /// What the user can do.
    pub(crate) remedy: Option<Remedy>,
    /// What the agent said, as it said it: clean it before showing it.
    pub(crate) words: Option<String>,
}

/// Claude Code's SDK's kind of error, as its adapter gives it in
/// `data.errorKind` when a prompt fails.
///
/// The kinds are the Claude Agent SDK's (`SDKAssistantMessageError`) and
/// the adapter's own (`no_result`, `incomplete_tool_call`,
/// `worker_shutdown`, `transport_lost`), grouped as the adapter's
/// `providerFailureCategory` groups them: an unknown kind is a provider
/// error, as there. Sending the message again is offered only where it
/// can help.
fn claude_kind(kind: &str) -> (FailureReason, Option<Remedy>) {
    let again = Some(Remedy::SendAgain);
    match kind {
        "authentication_failed" | "oauth_org_not_allowed" => (FailureReason::SignInNeeded, None),
        "billing_error" | "account_on_hold" => (FailureReason::UsageLimit, None),
        "rate_limit" => (FailureReason::UsageLimit, again),
        // Signing in again doesn't fix these.
        "verification_required" | "cloud_credential_error" => (FailureReason::AccessDenied, None),
        "invalid_request" | "model_not_found" => (FailureReason::BadRequest, None),
        "max_output_tokens" => (FailureReason::TooLong, None),
        _ => (FailureReason::ProviderError, again),
    }
}

/// An error's `data`, as JSON.
fn data(error: &RpcError) -> Option<Value> {
    error
        .data
        .as_ref()
        .and_then(|data| serde_json::from_str(data.get()).ok())
}

/// What the agent said in `error`, uncleaned. The ACP SDK sends an
/// adapter's plain thrown error as a bare "Internal error" with the
/// thrown text in `data.details`; Claude's adapter puts its own text
/// after "Internal error: ".
fn words(error: &RpcError) -> Option<String> {
    let message = error
        .message
        .strip_prefix("Internal error: ")
        .unwrap_or(&error.message)
        .trim();
    let details = data(error).and_then(|data| data.get("details")?.as_str().map(str::to_owned));
    match message {
        "" | "Internal error" => details.filter(|details| !details.trim().is_empty()),
        message => Some(message.to_owned()),
    }
}

/// `text` as a version of three numbers, and whether it's a prerelease
/// (`-rc.1`), which comes before the release of the same numbers. Build
/// metadata (`+…`) is ignored.
pub(crate) fn version(text: &str) -> Option<([u64; 3], bool)> {
    let text = text.split('+').next()?;
    let (core, pre) = match text.split_once('-') {
        Some((core, _)) => (core, true),
        None => (text, false),
    };
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some((version, pre))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::install::{Artifact, EnvValue, Platform, Unpack};

    /// A command line of `program`.
    fn bundled(program: Program, args: &[&str]) -> Bundled {
        Bundled {
            program,
            args: args.iter().map(|&arg| arg.to_owned()).collect(),
            env: Vec::new(),
        }
    }

    /// The test agent installed with npm: it signs in through its adapter,
    /// whose link opens a browser, and says by itself whether it's signed
    /// in. It has no status command, and takes no paste.
    pub(crate) fn npm_recipe() -> Recipe {
        Recipe {
            version: "2.0.0".to_owned(),
            install: Install::Npm(NpmPackage {
                manifest: r#"{"dependencies":{"test-npm-acp":"2.0.0"}}"#.to_owned(),
                lockfile: r#"{"name":"test","lockfileVersion":3,"packages":{}}"#.to_owned(),
            }),
            size: 40_000_000,
            needs: Needs {
                node: Some(22),
                npm: true,
            },
            adapter: Bundled {
                program: Program::InInstall("node_modules/.bin/test-npm-acp".into()),
                args: vec!["--acp".to_owned()],
                env: vec![("TEST_NPM_MODE".to_owned(), EnvValue::Text("acp".to_owned()))],
            },
            sign_in: Some(SignIn {
                command: bundled(Program::Adapter, &["login"]),
                link: LinkFrom::Browser,
                code_after: None,
                paste: false,
                sites: vec!["login.npm.example".to_owned()],
            }),
            // Signing out clears the private folder it's given.
            sign_out: Some(Bundled {
                program: Program::Adapter,
                args: vec!["logout".to_owned()],
                env: vec![("TEST_NPM_HOME".to_owned(), EnvValue::PrivateFolder)],
            }),
            status: Status {
                pushed: true,
                command: None,
            },
        }
    }

    /// The test agent downloaded per platform: it keeps its sign-in in its
    /// private folder, signs in with a program of its install that prints
    /// its link and a code and takes a paste, and has a status command.
    pub(crate) fn download_recipe() -> Recipe {
        let artifact = |arch: &str| Artifact {
            platform: Platform {
                os: "linux".to_owned(),
                arch: arch.to_owned(),
            },
            url: format!("https://dl.example/test-download-1.4.0-{arch}.tar.gz"),
            sha256: "ab".repeat(32),
            unpack: Unpack::TarGz,
        };
        let program = || Program::InInstall("bin/test-download".into());
        let mut adapter = bundled(program(), &["acp"]);
        adapter.env = vec![("TEST_HOME".to_owned(), EnvValue::PrivateFolder)];
        let mut sign_in = bundled(program(), &["auth", "login"]);
        sign_in.env = adapter.env.clone();
        // Through its adapter: run as it's started, with its variables.
        let status = bundled(Program::Adapter, &["auth", "status"]);
        Recipe {
            version: "1.4.0".to_owned(),
            install: Install::Download(vec![artifact("x86_64"), artifact("aarch64")]),
            size: 900_000_000,
            needs: Needs::default(),
            adapter,
            sign_in: Some(SignIn {
                command: sign_in,
                link: LinkFrom::Output("Open this link: ".to_owned()),
                code_after: Some("Your code: ".to_owned()),
                paste: true,
                sites: vec!["accounts.download.example".to_owned()],
            }),
            sign_out: Some(bundled(program(), &["auth", "logout"])),
            status: Status {
                pushed: false,
                command: Some(StatusCommand {
                    command: status,
                    signed_in: "/signedIn".to_owned(),
                    how: Some((
                        "/via".to_owned(),
                        vec![
                            ("key".to_owned(), SignedInAs::ApiKey),
                            ("account".to_owned(), SignedInAs::Account),
                        ],
                    )),
                }),
            },
        }
    }

    /// The plain test agent: a download for no platform, so it can't be
    /// installed, with no sign-in and no status command.
    pub(crate) fn plain_recipe() -> Recipe {
        Recipe {
            version: "1.0.0".to_owned(),
            install: Install::Download(Vec::new()),
            size: 1_000_000,
            needs: Needs::default(),
            adapter: bundled(Program::InInstall("bin/plain-acp".into()), &[]),
            sign_in: None,
            sign_out: None,
            status: Status {
                pushed: false,
                command: None,
            },
        }
    }

    fn claude() -> ProfileChoice {
        ProfileChoice::Builtin(Builtin::ClaudeAgentAcp)
    }

    fn adapter(name: &str, version: &str) -> Option<AdapterInfo> {
        Some(AdapterInfo {
            name: name.to_owned(),
            version: version.to_owned(),
        })
    }

    #[test]
    fn the_claude_profile_is_checked_by_the_adapters_name_and_version() {
        assert_eq!(
            claude().check(adapter(CLAUDE_NAME, "0.98.0")).checked,
            Checked::Yes
        );
        let newer = claude().check(adapter(CLAUDE_NAME, "0.99.1"));
        assert_eq!((newer.checked, newer.profile), (Checked::Newer, claude()));
        for older in ["0.97.9", "0.98", "0.98.0-rc.1", "x", ""] {
            let report = claude().check(adapter(CLAUDE_NAME, older));
            assert_eq!(
                (report.checked, report.profile),
                (Checked::Mismatch, ProfileChoice::Cautious),
                "{older}"
            );
        }
        assert_eq!(
            claude().check(adapter("codex-acp", "0.98.0")).checked,
            Checked::Mismatch
        );
        assert_eq!(claude().check(None).checked, Checked::Mismatch);
        assert_eq!(
            ProfileChoice::Cautious
                .check(adapter(CLAUDE_NAME, "0.98.0"))
                .checked,
            Checked::Cautious
        );
    }

    #[test]
    fn whether_a_profile_pushes_its_status_is_as_its_recipe_says() {
        for builtin in [
            Builtin::ClaudeAgentAcp,
            Builtin::TestNpm,
            Builtin::TestDownload,
            Builtin::TestPlain,
            Builtin::TestModels,
            #[cfg(feature = "fake")]
            Builtin::Fake,
        ] {
            let profile = ProfileChoice::Builtin(builtin);
            assert_eq!(profile.pushes_status(), builtin.recipe().status.pushed);
        }
        assert!(!ProfileChoice::Cautious.pushes_status());
    }

    #[test]
    fn every_profile_but_the_cautious_one_says_how_to_strip_its_agent() {
        for builtin in [
            Builtin::ClaudeAgentAcp,
            Builtin::TestNpm,
            Builtin::TestDownload,
            Builtin::TestPlain,
            Builtin::TestModels,
            #[cfg(feature = "fake")]
            Builtin::Fake,
        ] {
            let profile = ProfileChoice::Builtin(builtin);
            assert!(profile.strips(), "{builtin:?}");
            // Only Claude has tools to turn off.
            assert_eq!(
                profile.stripped("Ours.", Some("Theirs.")).is_some(),
                builtin == Builtin::ClaudeAgentAcp,
                "{builtin:?}"
            );
        }
        assert!(!ProfileChoice::Cautious.strips());
        assert_eq!(ProfileChoice::Cautious.stripped("Ours.", None), None);
        let options = &claude()
            .stripped("Ours.", Some("Theirs."))
            .expect("stripped")["claudeCode"]["options"];
        assert_eq!(options["systemPrompt"], "Ours.\n\nTheirs.");
    }

    #[test]
    fn the_claude_profile_declares_forms_and_notices_but_no_air() {
        let declared = claude().declarations();
        assert_eq!(declared["elicitation"], json!({ "form": {}, "url": {} }));
        assert_eq!(
            declared["session"],
            json!({ "notices": {}, "configOptions": { "boolean": {} } })
        );
        assert_eq!(declared["terminal"], json!(false));
        assert!(declared.get("_meta").is_none(), "no AIR");
        let cautious = ProfileChoice::Cautious.declarations();
        assert!(cautious.get("elicitation").is_none() && cautious.get("session").is_none());
    }

    #[cfg(feature = "fake")]
    #[test]
    fn the_fake_profile_never_installs_and_signs_in_through_its_auth_commands() {
        let fake = ProfileChoice::Builtin(Builtin::Fake);
        let report = fake.check(adapter("caf-fake-agent", "0.0.0"));
        assert_eq!(
            (report.checked, report.profile),
            (Checked::Yes, fake.clone())
        );
        let recipe = fake.recipe().expect("it has one");
        let Install::Download(artifacts) = &recipe.install else {
            panic!("a download, not {:?}", recipe.install);
        };
        // From nowhere, on every platform, so it fails as offline rather
        // than as a platform it isn't for.
        for os in ["linux", "macos", "windows"] {
            for arch in ["x86_64", "aarch64"] {
                assert!(artifacts.iter().any(|artifact| artifact.platform
                    == crate::install::Platform::new(os, arch)
                    && artifact.url == "https://127.0.0.1:1/caf-fake-agent"));
            }
        }
        let sign_in = recipe.sign_in.expect("it signs in");
        assert_eq!(sign_in.command.program, Program::Adapter);
        assert_eq!(sign_in.command.args, ["auth", "login"]);
        assert_eq!(sign_in.sites, ["caf.example"]);
        assert!(sign_in.paste);
        assert_eq!(
            recipe.sign_out.expect("it signs out").args,
            ["auth", "logout"]
        );
        assert!(!fake.pushes_status());
    }

    /// A prompt's answer with ACP's `usage` and Claude's split by model,
    /// `split`.
    fn answered(split: Value) -> Value {
        json!({
            "stopReason": "end_turn",
            "usage": { "inputTokens": 2, "outputTokens": 4, "cachedReadTokens": 10, "totalTokens": 16 },
            "_meta": { "quota": { "model_usage": split } },
        })
    }

    /// A row of what a turn spent.
    fn reckoned(model: Option<&str>, tokens: [u64; 4], thinking: Option<u64>) -> Reckoned {
        let [input, cache_read, cache_write, output] = tokens;
        Reckoned {
            model: model.map(str::to_owned),
            tokens: Tokens {
                input,
                cache_read,
                cache_write,
                output,
                thinking,
            },
            total: input + cache_read + cache_write + output,
        }
    }

    #[test]
    fn claudes_split_by_model_is_read_when_it_has_rows_and_its_usage_otherwise() {
        let split = json!([
            { "model": "claude-sonnet-5-5", "token_count": { "totalTokens": 18, "inputTokens": 3, "cachedInputTokens": 5,
              "cachedWriteTokens": 1, "outputTokens": 9, "reasoningOutputTokens": 2 } },
            // A row that leaves out what it doesn't count, as Codex's
            // adapter may.
            { "model": "gpt-5.5", "token_count": { "totalTokens": 3, "inputTokens": 1, "outputTokens": 2 } },
        ]);
        assert_eq!(
            claude().spent(&answered(split)).0,
            Reported::Rows(vec![
                reckoned(Some("claude-sonnet-5-5"), [3, 5, 1, 9], Some(2)),
                reckoned(Some("gpt-5.5"), [1, 0, 0, 2], None),
            ])
        );
        let usage = Reported::Rows(vec![reckoned(None, [2, 10, 0, 4], None)]);
        assert_eq!(claude().spent(&answered(json!([]))).0, usage);
        assert_eq!(claude().spent(&answered(Value::Null)).0, usage);
        // Any agent's usage is read, but only Claude's split.
        let plain = ProfileChoice::Builtin(Builtin::TestPlain);
        let split = json!([{ "model": "m", "token_count": { "totalTokens": 2, "inputTokens": 1, "outputTokens": 1 } }]);
        assert_eq!(plain.spent(&answered(split)).0, usage);
        assert_eq!(
            claude().spent(&json!({ "stopReason": "end_turn" })).0,
            Reported::Nothing
        );
    }

    #[test]
    fn claude_names_its_model_in_its_usage_updates_and_plain_agents_dont() {
        let update = json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2,
            "_meta": { "_claude/model": "claude-haiku-5-5" } });
        assert_eq!(
            claude().model_in_usage(&update).as_deref(),
            Some("claude-haiku-5-5")
        );
        let plain = ProfileChoice::Builtin(Builtin::TestPlain);
        assert_eq!(plain.model_in_usage(&update), None);
        let unnamed = json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2 });
        assert_eq!(claude().model_in_usage(&unnamed), None);
        #[cfg(feature = "fake")]
        assert_eq!(
            ProfileChoice::Builtin(Builtin::Fake)
                .model_in_usage(&update)
                .as_deref(),
            Some("claude-haiku-5-5")
        );
    }

    #[test]
    fn counts_that_arent_whole_numbers_are_unreadable() {
        let usage = |count: Value| json!({ "usage": { "inputTokens": count, "outputTokens": 1, "totalTokens": 1 } });
        for count in [
            json!(-1),
            json!(1.5),
            json!("1"),
            json!(u64::MAX as f64 * 2.0),
        ] {
            assert_eq!(
                claude().spent(&usage(count.clone())).0,
                Reported::Unreadable,
                "{count}"
            );
        }
        // A split that can't be read gives way to the usage, saying so.
        let split = json!([{ "model": "m", "token_count": { "totalTokens": 1, "inputTokens": -1, "outputTokens": 1 } }]);
        let usage = Reported::Rows(vec![reckoned(None, [2, 10, 0, 4], None)]);
        assert_eq!(claude().spent(&answered(split)), (usage, true));
        let usage = Reported::Rows(vec![reckoned(None, [2, 10, 0, 4], None)]);
        assert_eq!(claude().spent(&answered(json!({}))), (usage, true));
        // Unreadable only if neither can be read.
        let neither = json!({ "usage": { "inputTokens": -1, "outputTokens": 1, "totalTokens": 1 },
            "_meta": { "quota": { "model_usage": {} } } });
        assert_eq!(claude().spent(&neither), (Reported::Unreadable, true));
        assert!(!claude().spent(&answered(json!([]))).1);
    }

    /// A `usage_update` whose `_meta._claude/rateLimit` is `info`.
    fn rate_limited(info: Value) -> Value {
        json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2,
            "_meta": { "_claude/rateLimit": info } })
    }

    /// When the limits of these tests are heard.
    const HEARD: Wall = Wall(1_795_000_000_000);

    /// A window as Claude's are read: heard at [`HEARD`], resetting at
    /// `resets` seconds.
    fn read(kind: WindowKind, used: u8, resets: Option<i64>, state: LimitState) -> LimitWindow {
        LimitWindow {
            kind,
            used,
            resets: resets.map(|seconds| Wall(seconds * 1000)),
            state,
            heard: HEARD,
        }
    }

    /// What Claude's profile reads of `info`.
    fn claude_reads(info: Value) -> Option<AccountLimits> {
        claude().limits(&rate_limited(info), HEARD)
    }

    #[test]
    fn claude_says_its_limits_in_unified_windows_and_the_limit_itself() {
        let info = json!({
            "status": "allowed_warning",
            "rateLimitType": "seven_day_opus",
            "utilization": 0.81,
            "resetsAt": 1_795_400_000_i64,
            "isUsingOverage": false,
            "overageStatus": "rejected",
            "surpassedThreshold": 0.75,
            "unifiedWindows": {
                "five_hour": { "utilization": 0.42, "resetsAt": 1_795_010_000_i64, "status": "allowed" },
                "seven_day": { "utilization": 0.6, "resetsAt": 1_795_300_000_i64 },
                "seven_day_overage_included": { "utilization": 0.12, "resetsAt": 1_795_300_000_i64, "surpassedThreshold": 0.1 },
            },
        });
        let limits = claude_reads(info).expect("they read");
        assert_eq!(
            limits.windows,
            [
                read(
                    WindowKind::FiveHours,
                    42,
                    Some(1_795_010_000),
                    LimitState::Fine
                ),
                read(WindowKind::Week, 60, Some(1_795_300_000), LimitState::Fine),
                read(
                    WindowKind::ModelWeek("opus".into()),
                    81,
                    Some(1_795_400_000),
                    LimitState::Warning
                ),
                read(
                    WindowKind::ExtraWeek,
                    12,
                    Some(1_795_300_000),
                    LimitState::Fine
                ),
            ]
        );
        assert_eq!(
            limits.extra_usage,
            Some(ExtraUsage {
                in_use: false,
                heard: HEARD
            })
        );
        assert_eq!(limits.cleared, None);
    }

    #[test]
    fn a_window_both_hold_is_unified_windows_and_the_limit_fills_the_rest() {
        let info = json!({
            "status": "allowed", "rateLimitType": "five_hour", "utilization": 0.9, "resetsAt": 1_795_300_000_i64,
            "unifiedWindows": { "five_hour": { "utilization": 0.97, "resetsAt": 1_795_010_000_i64, "status": "allowed_warning" } },
        });
        let limits = claude_reads(info).expect("they read");
        assert_eq!(
            limits.windows,
            [read(
                WindowKind::FiveHours,
                97,
                Some(1_795_010_000),
                LimitState::Warning
            )]
        );
        // A unified window that doesn't say its status takes the limit's,
        // if the limit is of its kind, and only then.
        let info = json!({
            "status": "allowed_warning", "rateLimitType": "seven_day", "utilization": 0.8,
            "unifiedWindows": { "five_hour": { "utilization": 0.5 }, "seven_day": { "utilization": 0.81 } },
        });
        assert_eq!(
            claude_reads(info).expect("they read").windows,
            [
                read(WindowKind::FiveHours, 50, None, LimitState::Fine),
                read(WindowKind::Week, 81, None, LimitState::Warning),
            ]
        );
        // The limit refused is never hidden by its unified window, whatever
        // that says.
        for status in [json!("allowed"), json!(null)] {
            let info = json!({
                "status": "rejected", "rateLimitType": "five_hour", "utilization": 1.0,
                "unifiedWindows": { "five_hour": { "utilization": 0.97, "status": status } },
            });
            assert_eq!(
                claude_reads(info).expect("they read").windows,
                [read(WindowKind::FiveHours, 100, None, LimitState::Refused)],
                "{status}"
            );
        }
        // With no unified windows, the limit itself.
        let info = json!({ "status": "allowed", "rateLimitType": "seven_day", "utilization": 0.3, "resetsAt": 1_795_300_000_i64 });
        assert_eq!(
            claude_reads(info).expect("they read").windows,
            [read(
                WindowKind::Week,
                30,
                Some(1_795_300_000),
                LimitState::Fine
            )]
        );
    }

    #[test]
    fn claudes_windows_are_read_by_name() {
        let named = |name: &str| {
            claude_reads(json!({ "unifiedWindows": { name: { "utilization": 0.5 } } }))
                .map(|limits| limits.windows[0].kind.clone())
        };
        assert_eq!(named("five_hour"), Some(WindowKind::FiveHours));
        assert_eq!(named("seven_day"), Some(WindowKind::Week));
        assert_eq!(
            named("seven_day_overage_included"),
            Some(WindowKind::ExtraWeek)
        );
        assert_eq!(
            named("seven_day_sonnet"),
            Some(WindowKind::ModelWeek("sonnet".into()))
        );
        // Models its SDK doesn't list, in lower case.
        assert_eq!(
            named("seven_day_Fable"),
            Some(WindowKind::ModelWeek("fable".into()))
        );
        assert_eq!(
            named("thirty_day"),
            Some(WindowKind::Other("thirty_day".into()))
        );
        assert_eq!(
            named("seven_day_"),
            Some(WindowKind::Other("seven_day_".into()))
        );
        // Overage is never a window, and names that aren't plain aren't
        // kept.
        for name in [
            "overage",
            "",
            "five hour",
            "fünf_stunden",
            "five_hour\n",
            &"x".repeat(65),
        ] {
            assert_eq!(named(name), None, "{name}");
        }
        assert_eq!(
            named(&"x".repeat(64)),
            Some(WindowKind::Other("x".repeat(64)))
        );
    }

    #[test]
    fn claudes_statuses_are_read_and_refused_is_all_used() {
        let state = |status: Value| {
            claude_reads(
                json!({ "rateLimitType": "five_hour", "utilization": 0.5, "status": status }),
            )
            .map(|limits| (limits.windows[0].state, limits.windows[0].used))
        };
        assert_eq!(state(json!("allowed")), Some((LimitState::Fine, 50)));
        assert_eq!(
            state(json!("allowed_warning")),
            Some((LimitState::Warning, 50))
        );
        assert_eq!(state(json!("rejected")), Some((LimitState::Refused, 100)));
        // A status CAF doesn't know, or none, is fine.
        for status in [json!("on_fire"), json!(null), json!(3)] {
            assert_eq!(state(status), Some((LimitState::Fine, 50)));
        }
        // Refused needs no share.
        let refused = json!({ "rateLimitType": "seven_day", "status": "rejected" });
        assert_eq!(
            claude_reads(refused).expect("they read").windows,
            [read(WindowKind::Week, 100, None, LimitState::Refused)]
        );
    }

    #[test]
    fn claudes_shares_are_kept_to_0_to_100_and_rounded() {
        let used = |utilization: Value| {
            claude_reads(
                json!({ "unifiedWindows": { "five_hour": { "utilization": utilization } } }),
            )
            .map(|limits| limits.windows[0].used)
        };
        assert_eq!(used(json!(0)), Some(0));
        assert_eq!(used(json!(0.004)), Some(0));
        assert_eq!(used(json!(0.125)), Some(13), "half up");
        assert_eq!(used(json!(0.996)), Some(100));
        assert_eq!(used(json!(1)), Some(100));
        assert_eq!(used(json!(-0.2)), Some(0));
        assert_eq!(used(json!(1.7)), Some(100));
        assert_eq!(used(json!(1e300)), Some(100));
        // A window with no share that reads is skipped.
        for nonsense in [json!("0.5"), json!(null), json!([0.5]), json!({})] {
            assert_eq!(used(nonsense.clone()), None, "{nonsense}");
        }
        assert_eq!(
            claude_reads(json!({ "unifiedWindows": { "five_hour": {} } })),
            None
        );
    }

    #[test]
    fn claudes_resets_are_kept_only_between_2020_and_2100() {
        let resets = |at: Value| {
            claude_reads(
                json!({ "rateLimitType": "five_hour", "utilization": 0.1, "resetsAt": at }),
            )
            .and_then(|limits| limits.windows[0].resets)
        };
        assert_eq!(
            resets(json!(1_795_010_000_i64)),
            Some(Wall(1_795_010_000_000))
        );
        assert_eq!(
            resets(json!(1_795_010_000.4)),
            Some(Wall(1_795_010_000_400))
        );
        assert_eq!(
            resets(json!(1_577_836_800_i64)),
            Some(Wall(1_577_836_800_000))
        );
        assert_eq!(
            resets(json!(4_102_444_799_i64)),
            Some(Wall(4_102_444_799_000))
        );
        // Above 10^11, milliseconds: no time in seconds is, before the
        // year 5000.
        assert_eq!(
            resets(json!(1_795_010_000_123_i64)),
            Some(Wall(1_795_010_000_123))
        );
        assert_eq!(
            resets(json!(1_577_836_800_000_i64)),
            Some(Wall(1_577_836_800_000))
        );
        let unified = json!({ "unifiedWindows": { "seven_day": { "utilization": 0.1, "resetsAt": 1_795_010_000_000_i64 } } });
        assert_eq!(
            claude_reads(unified).expect("they read").windows[0].resets,
            Some(Wall(1_795_010_000_000))
        );
        for nonsense in [
            json!(4_102_444_800_i64),
            json!(1_577_836_799_i64),
            json!(4_102_444_800_000_i64),
            json!(1_577_836_799_999_i64),
            // At most 10^11 is seconds, so this is past 2100.
            json!(100_000_000_000_i64),
            json!(-1),
            json!(0),
            json!(1e300),
            json!("1795010000"),
            json!(null),
        ] {
            assert_eq!(resets(nonsense.clone()), None, "{nonsense}");
        }
    }

    #[test]
    fn extra_usage_alone_is_news_and_nothing_that_reads_is_none() {
        let info = json!({ "status": "allowed", "rateLimitType": "overage", "utilization": 0.3, "isUsingOverage": true });
        let limits = claude_reads(info).expect("they read");
        assert!(limits.windows.is_empty());
        assert_eq!(
            limits.extra_usage,
            Some(ExtraUsage {
                in_use: true,
                heard: HEARD
            })
        );
        for nothing in [
            json!({}),
            json!(null),
            json!("allowed"),
            json!({ "status": "allowed" }),
            json!({ "isUsingOverage": "yes", "unifiedWindows": [] }),
            json!({ "rateLimitType": 5, "utilization": 0.5 }),
            json!({ "unifiedWindows": "five_hour" }),
        ] {
            assert_eq!(claude_reads(nothing.clone()), None, "{nothing}");
        }
        let unlimited = json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2 });
        assert_eq!(claude().limits(&unlimited, HEARD), None);
    }

    #[test]
    fn only_claude_and_the_fake_say_their_limits() {
        let update = rate_limited(json!({ "rateLimitType": "five_hour", "utilization": 0.5 }));
        assert!(claude().limits(&update, HEARD).is_some());
        for plain in [Builtin::TestPlain, Builtin::TestNpm, Builtin::TestModels] {
            assert_eq!(ProfileChoice::Builtin(plain).limits(&update, HEARD), None);
        }
        assert_eq!(ProfileChoice::Cautious.limits(&update, HEARD), None);
        #[cfg(feature = "fake")]
        assert_eq!(
            ProfileChoice::Builtin(Builtin::Fake).limits(&update, HEARD),
            claude().limits(&update, HEARD)
        );
    }

    #[test]
    fn claude_and_the_fake_read_why_a_request_failed_and_test_agents_dont() {
        let error: RpcError = serde_json::from_value(json!({ "code": -32603,
            "message": "Internal error: API Error: Rate limited", "data": { "errorKind": "rate_limit" } }))
        .expect("an error");
        let claude = claude().failure(&error);
        assert_eq!(claude.reason, FailureReason::UsageLimit);
        assert_eq!(claude.remedy, Some(Remedy::SendAgain));
        let plain = ProfileChoice::Builtin(Builtin::TestPlain).failure(&error);
        assert_eq!(plain.reason, FailureReason::Other);
        #[cfg(feature = "fake")]
        {
            let fake = ProfileChoice::Builtin(Builtin::Fake).failure(&error);
            assert_eq!(fake.reason, FailureReason::UsageLimit);
            assert_eq!(fake.remedy, Some(Remedy::SendAgain));
        }
    }

    #[cfg(feature = "fake")]
    #[test]
    fn the_fake_profile_reads_a_split_by_model_as_claudes_does() {
        let fake = ProfileChoice::Builtin(Builtin::Fake);
        let split = json!([{ "model": "small", "token_count": { "totalTokens": 7, "inputTokens": 3, "outputTokens": 4 } }]);
        assert_eq!(
            fake.spent(&answered(split)).0,
            Reported::Rows(vec![reckoned(Some("small"), [3, 0, 0, 4], None)])
        );
    }
}
