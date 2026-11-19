//! What CAF knows about each adapter.
//!
//! A profile holds everything CAF knows about one adapter at one version:
//! - what to declare to it, and what those declarations do to it;
//! - which of its features have been checked;
//! - how to read its oddities;
//! - how to start and configure it;
//! - how its modes compare;
//! - how it describes actions.
//!
//! The core uses profiles to translate each adapter's way of using ACP
//! into CAF's own terms, so nothing past this module knows which agent it
//! is talking to.
//!
//! Profiles are CAF's own. Apps choose one for each agent but never
//! change one. Users will be able to add profiles as data in CAF's shared
//! folder (*later*).

use serde::{Deserialize, Serialize};

/// Which profile an agent should have.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileChoice {
    /// One CAF knows.
    Builtin(Builtin),
    /// The cautious profile, for agents CAF doesn't know: plain ACP,
    /// nothing risky switched on.
    Cautious,
}

/// The adapters CAF has profiles for.
///
/// Closed to apps: a new adapter gets a profile by being added to CAF,
/// where it's tested against recordings like the rest. `non_exhaustive`
/// lets CAF add more without breaking anyone's `match`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Builtin {
    /// Claude Code, through `@agentclientprotocol/claude-agent-acp`.
    /// Checked at 0.98.0.
    ClaudeAgentAcp,
}

/// What became of a connection's profile once its adapter said who it
/// is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileReport {
    /// The profile in use.
    pub profile: ProfileChoice,
    /// The adapter's name and version, as it gave them in `initialize`.
    pub adapter: Option<AdapterInfo>,
    /// How far that adapter is checked.
    pub checked: Checked,
}

/// An adapter's name and version, as it gave them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterInfo {
    /// Its name, such as `@agentclientprotocol/claude-agent-acp`.
    pub name: String,
    /// Its version.
    pub version: String,
}

/// How far CAF has checked the adapter it's talking to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
