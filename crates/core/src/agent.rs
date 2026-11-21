//! The agents a host offers, how to start them, and where each stands:
//! installed or not, signed in or not.
//!
//! A host offers an agent CAF installs itself ([`Launch::Installed`]), or
//! says how to start one of its own ([`Launch::Command`]). Later, CAF also
//! reads the user's own agents and profiles from its shared folder.

use std::ffi::OsString;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::AgentName;
use crate::install::SignedInAs;
use crate::profiles::{Builtin, ProfileChoice};

/// An agent the host offers.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentSpec {
    /// The name chats ask for it by.
    pub name: AgentName,
    /// How to start its adapter.
    pub launch: Launch,
    /// Which profile it should be. The adapter's own name and version
    /// confirm it once it answers `initialize`; a mismatch falls back to
    /// the cautious profile and is reported.
    pub profile: ProfileChoice,
}

impl AgentSpec {
    /// An agent called `name` that CAF installs itself, as its built-in
    /// profile says, and signs in to.
    #[must_use]
    pub fn installed(name: impl Into<String>, builtin: Builtin) -> Self {
        Self {
            name: AgentName(name.into()),
            launch: Launch::Installed,
            profile: ProfileChoice::Builtin(builtin),
        }
    }

    /// An agent called `name`, started by the host's own `command`, with
    /// `profile`. CAF doesn't install it, and signs in to it only if its
    /// profile says how.
    #[must_use]
    pub fn command(name: impl Into<String>, command: SpawnCommand, profile: ProfileChoice) -> Self {
        Self {
            name: AgentName(name.into()),
            launch: Launch::Command(command),
            profile,
        }
    }
}

/// How an agent's adapter is started.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Launch {
    /// From CAF's own install of it, at the version its profile was checked
    /// at. A chat that needs it before it's installed waits
    /// ([`ChatState::WaitingForAgent`]), and the user is asked.
    ///
    /// [`ChatState::WaitingForAgent`]: crate::chat::ChatState::WaitingForAgent
    Installed,
    /// By the host's own command.
    Command(SpawnCommand),
}

/// How to start a program.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SpawnCommand {
    /// The program, by absolute path. Never looked up on `PATH`, and never
    /// run through `npx`, which stays resident and starts three times
    /// slower.
    pub program: PathBuf,
    /// Its arguments.
    #[cfg_attr(feature = "ts", ts(as = "Vec<crate::ts::OsString>"))]
    pub args: Vec<OsString>,
    /// Changes to the environment it inherits. The profile adds its own,
    /// and the driver keeps only `PATH`'s absolute entries, so a project
    /// folder can't put programs on it.
    pub env: Vec<EnvChange>,
    /// The directory to start it in. Usually `None`: each session's folder
    /// travels in ACP, since one process may serve sessions in different
    /// folders.
    pub cwd: Option<PathBuf>,
}

impl SpawnCommand {
    /// Run `program` with no arguments, in the inherited environment.
    /// Set the other fields to change that.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
        }
    }
}

/// One change to a program's environment.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum EnvChange {
    /// Set a variable.
    Set(
        #[cfg_attr(feature = "ts", ts(as = "crate::ts::OsString"))] OsString,
        #[cfg_attr(feature = "ts", ts(as = "crate::ts::OsString"))] OsString,
    ),
    /// Remove a variable.
    Remove(#[cfg_attr(feature = "ts", ts(as = "crate::ts::OsString"))] OsString),
}

/// An MCP server to give the agent's session: the only way ACP lets an
/// app give an agent tools. CAF passes it through untouched: it never
/// wraps it, proxies it or shortens its timeouts.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum McpServer {
    /// A program the agent starts and talks to over its standard input
    /// and output. Every agent supports this.
    Stdio {
        /// The name the agent knows its tools by.
        name: String,
        /// How the agent starts it.
        command: SpawnCommand,
    },
    /// A server the agent reaches over HTTP, such as eyespy's. Only for
    /// agents that support it: an agent that doesn't refuses the chat
    /// rather than quietly running without its tools.
    Http {
        /// The name the agent knows its tools by.
        name: String,
        /// Where it is.
        url: String,
        /// Headers to send with every request.
        headers: Vec<(String, String)>,
    },
}

/// Where an agent stands: installed or not, signed in or not. It's the
/// agent's, not a chat's: every chat with the agent shares one install and
/// one sign-in.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentStatus {
    /// The agent.
    pub name: AgentName,
    /// Whether it's installed.
    pub install: InstallState,
    /// Whether it's signed in.
    pub sign_in: SignInState,
    /// Whether [`Core::sign_in`](crate::Core::sign_in) would be accepted
    /// now: its profile says how to sign in, and that can run now. For an
    /// agent CAF installs, it's installed. For one the host starts with
    /// its own command, a process of it has answered as the adapter its
    /// profile is for, in this run. It's also true while a sign-in is under
    /// way, which `sign_in` accepts as it stands. So it changes as CAF
    /// runs, and the status is told again when it does: offer Sign in only
    /// while it's true.
    pub can_sign_in: bool,
}

/// Whether an agent is installed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum InstallState {
    /// The host starts it with its own command: CAF doesn't install it.
    Host,
    /// CAF hasn't looked yet.
    Unknown,
    /// Not installed. It can be, taking about `size` bytes.
    Missing {
        /// About how many bytes it takes.
        size: u64,
    },
    /// Being installed.
    Installing {
        /// Bytes done so far, as far as the setup driver can tell.
        done: u64,
        /// About how many bytes in all.
        size: u64,
    },
    /// Installed, at this version.
    Installed {
        /// The version.
        version: String,
    },
    /// It can't be installed.
    Cannot(CannotInstall),
}

/// Why an agent can't be installed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum CannotInstall {
    /// The machine has no Node, or one older than it needs.
    Node {
        /// The major version it needs.
        needs: u32,
        /// The one it has, if any.
        has: Option<u32>,
    },
    /// The machine has no npm.
    Npm,
    /// There's no build of it for this machine.
    Platform,
    /// CAF has no recipe for installing it: its profile isn't one CAF
    /// installs, such as the cautious one.
    NoRecipe,
    /// The last try failed, saying this. Installing again tries again.
    Failed(InstallFailure),
}

/// Why an install failed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum InstallFailure {
    /// It couldn't reach the network.
    Offline,
    /// A download didn't match its checksum: it was thrown away.
    Corrupt,
    /// There wasn't room on the disk.
    NoSpace,
    /// The user stopped it.
    Stopped,
    /// Anything else, in the installer's words, cleaned and capped.
    Other(String),
}

/// Whether an agent is signed in.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum SignInState {
    /// CAF can't tell yet: nothing has said.
    Unknown,
    /// Signed out.
    SignedOut,
    /// Signing in: the user has something to do.
    SigningIn(SignInLink),
    /// Signed in.
    SignedIn(SignedInAs),
}

/// What the user must do to finish signing in.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SignInLink {
    /// The link to open, checked against the profile's sites. `None` until
    /// the sign-in command has given it.
    pub url: Option<String>,
    /// A code to type on the site, if the agent shows one.
    pub code: Option<String>,
    /// Whether the user may paste a code back here, such as when their
    /// browser is on another machine.
    pub paste: bool,
}
