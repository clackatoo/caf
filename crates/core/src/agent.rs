//! The agents a host offers, and how to start them.
//!
//! In feature 1 the host says how to start each agent's adapter. Later,
//! CAF installs adapters itself and reads the user's own agents and
//! profiles from its shared folder.

use std::ffi::OsString;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::AgentName;
use crate::profiles::ProfileChoice;

/// An agent the host offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    /// The name chats ask for it by.
    pub name: AgentName,
    /// How to start its adapter.
    pub command: SpawnCommand,
    /// Which profile it should be. The adapter's own name and version
    /// confirm it once it answers `initialize`; a mismatch falls back to
    /// the cautious profile and is reported.
    pub profile: ProfileChoice,
}

/// How to start a program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnCommand {
    /// The program, by absolute path. Never looked up on `PATH`, and never
    /// run through `npx`, which stays resident and starts three times
    /// slower.
    pub program: PathBuf,
    /// Its arguments.
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

/// One change to a program's environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvChange {
    /// Set a variable.
    Set(OsString, OsString),
    /// Remove a variable.
    Remove(OsString),
}

/// An MCP server to give the agent's session: the only way ACP lets an
/// app give an agent tools. CAF passes it through untouched: it never
/// wraps it, proxies it or shortens its timeouts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
