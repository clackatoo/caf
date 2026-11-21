//! How CAF installs agents and signs in to them: each profile's recipe.
//!
//! Everything about one agent is data here, in its profile: how to install
//! it, how big it is, which program to run, how to sign in and out, and how
//! to tell whether it's signed in. Nothing else in CAF is written for one
//! agent, so adding an agent is adding a profile.
//!
//! The core reads a recipe and decides; the setup driver
//! ([`SetupDirective`](crate::directive::SetupDirective)) does the work:
//! downloading, checking, writing, running sign-in commands.
//!
//! # Where installs live
//!
//! Each agent's adapter is installed at the version its profile was checked
//! at, in a folder of its own, named after its profile, never after what a
//! host calls the agent: `<profile>-<version>`, such as
//! `claude-agent-acp-0.98.0`. They live under one folder every
//! CAF program on the machine shares: `$XDG_DATA_HOME/caf/agents/` (by
//! default `~/.local/share/caf/agents/`), so every CAF program that offers
//! an agent shares one install of it. A version stays while any process
//! runs from it, in any CAF program. One older than the version a program
//! pins is removed once that pinned version has started, and only once no
//! process has started from it for 14 days, so two programs pinning
//! different versions don't remove each other's.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Everything CAF knows about installing one agent and signing in to it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Recipe {
    /// The version installed: the one the profile was checked at. It names
    /// the install's folder.
    pub version: String,
    /// How to install it.
    pub install: Install,
    /// About how many bytes it takes once installed, to tell the user
    /// before they agree.
    pub size: u64,
    /// What the machine must have first.
    pub needs: Needs,
    /// The adapter to run, as a file inside the install, with its
    /// arguments and environment.
    pub adapter: Bundled,
    /// How to sign in, if the agent can be signed in to by CAF.
    pub sign_in: Option<SignIn>,
    /// How to sign out.
    pub sign_out: Option<Bundled>,
    /// How to tell whether it's signed in, besides failures.
    pub status: Status,
}

/// How to install an agent.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Install {
    /// An npm package, installed with `npm ci --ignore-scripts` from a
    /// lockfile CAF ships, so every dependency comes at the exact version
    /// and hash in it, and no install script runs.
    Npm(NpmPackage),
    /// A file downloaded and checked, one per platform.
    Download(Vec<Artifact>),
}

/// An npm package to install.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NpmPackage {
    /// The `package.json` to install from: the package at its version, and
    /// nothing else.
    pub manifest: String,
    /// The `package-lock.json` that pins every dependency.
    pub lockfile: String,
}

impl NpmPackage {
    /// A package installed from this `package.json` and
    /// `package-lock.json`.
    #[must_use]
    pub fn new(manifest: impl Into<String>, lockfile: impl Into<String>) -> Self {
        Self {
            manifest: manifest.into(),
            lockfile: lockfile.into(),
        }
    }
}

/// A file to download for one platform.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Artifact {
    /// The platform it's for.
    pub platform: Platform,
    /// Where to get it: `https` only.
    pub url: String,
    /// Its SHA-256, in hex. A file that doesn't match is thrown away.
    pub sha256: String,
    /// How to unpack it.
    pub unpack: Unpack,
}

impl Artifact {
    /// A file for `platform`, downloaded from `url`, checked against
    /// `sha256` (in hex), and unpacked as `unpack` says.
    #[must_use]
    pub fn new(
        platform: Platform,
        url: impl Into<String>,
        sha256: impl Into<String>,
        unpack: Unpack,
    ) -> Self {
        Self {
            platform,
            url: url.into(),
            sha256: sha256.into(),
            unpack,
        }
    }
}

/// An operating system and processor.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Platform {
    /// As Rust names it, such as `linux` or `macos`.
    pub os: String,
    /// As Rust names it, such as `x86_64` or `aarch64`.
    pub arch: String,
}

impl Platform {
    /// The platform `os` and `arch`, as Rust names them.
    #[must_use]
    pub fn new(os: impl Into<String>, arch: impl Into<String>) -> Self {
        Self {
            os: os.into(),
            arch: arch.into(),
        }
    }

    /// The platform this program was built for.
    #[must_use]
    pub fn current() -> Self {
        Self::new(std::env::consts::OS, std::env::consts::ARCH)
    }
}

/// How to unpack a downloaded file.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Unpack {
    /// It's the program itself.
    No,
    /// A zip archive.
    Zip,
    /// A gzipped tar archive.
    TarGz,
}

/// What the machine must have before an agent can be installed.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Needs {
    /// Node, at least this major version, on `PATH`. CAF never installs
    /// Node itself.
    pub node: Option<u32>,
    /// npm, on `PATH`.
    pub npm: bool,
}

/// A program that comes with an agent, with its arguments and
/// environment.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Bundled {
    /// Which program.
    pub program: Program,
    /// Its arguments.
    pub args: Vec<String>,
    /// Variables to set for it.
    pub env: Vec<(String, EnvValue)>,
}

/// Which program a recipe runs.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Program {
    /// The agent's adapter itself, however it's started: CAF's install of
    /// it, or the host's own command. Adapters that pass a command line on
    /// to the agent's own program, as `claude-agent-acp --cli` does, can
    /// sign in this way whoever installed them.
    Adapter,
    /// A file inside CAF's install of the agent, by its path relative to
    /// the install's folder. Only for agents CAF installed.
    InInstall(PathBuf),
}

/// The value of a variable a recipe sets.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum EnvValue {
    /// This text.
    Text(String),
    /// The agent's private folder, made by CAF, readable only by the user:
    /// for an agent that keeps its sign-in in a folder CAF gives it, as
    /// Antigravity does. It's `<profile>-private` beside the installs, so
    /// every CAF program shares it, and it outlives every version.
    PrivateFolder,
}

/// How to sign in to an agent: a command that comes with it (its adapter,
/// or a file in its install), never the user's own copy, and never
/// anything the agent suggests.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SignIn {
    /// The command.
    pub command: Bundled,
    /// Where the link the user must open comes from.
    pub link: LinkFrom,
    /// A code it shows that the user must type on the site, if it shows
    /// one: what comes before it on its line.
    pub code_after: Option<String>,
    /// Whether it may ask the user to paste something back, such as when
    /// their browser is on another machine. What they paste goes to its
    /// standard input, with a newline.
    pub paste: bool,
    /// The sites a link may point to, by host name, such as `claude.ai`.
    /// A link to any other is refused, and the sign-in ends, so a tampered
    /// package can't send the user to a fake sign-in page.
    pub sites: Vec<String>,
}

/// Where a sign-in command's link comes from.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum LinkFrom {
    /// It opens a browser through `BROWSER`, which the setup driver
    /// points at a program of its own that hands the link to CAF instead.
    Browser,
    /// It prints the link on a line of its output, standard output or
    /// error, after this text.
    Output(String),
}

/// How to tell whether an agent is signed in, besides failures.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Status {
    /// The adapter says by itself, in `_auth/status_update`, as Claude's
    /// does when it starts and before each prompt.
    pub pushed: bool,
    /// A command that answers in JSON, of which only the fields named here
    /// are read. The rest, such as an email, never leaves the setup driver.
    pub command: Option<StatusCommand>,
}

/// A command that says whether an agent is signed in.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StatusCommand {
    /// The command.
    pub command: Bundled,
    /// The JSON pointer of a field that's `true` when signed in, such as
    /// `/loggedIn`.
    pub signed_in: String,
    /// The JSON pointer of a field naming how, such as `/authMethod`, and
    /// what each of its values means.
    pub how: Option<(String, Vec<(String, SignedInAs)>)>,
}

/// How an agent is signed in. CAF keeps only this, never who.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum SignedInAs {
    /// With the user's account at the provider, such as a Claude
    /// subscription.
    Account,
    /// With an API key.
    ApiKey,
    /// Through a gateway or another provider's cloud.
    Gateway,
    /// Signed in, but the agent didn't say how.
    Unknown,
}
