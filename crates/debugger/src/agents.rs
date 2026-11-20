//! The agents the debugger offers: from `agents.toml`, or found.
//!
//! ```toml
//! [[agent]]
//! name = "claude"
//! program = "/home/me/.local/bin/claude-agent-acp"
//! args = []
//! profile = "claude-agent-acp"   # or "cautious"
//! ```
//!
//! `program` is an absolute path, or a bare name looked up on `PATH` as
//! the debugger starts. `args` may be left out.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use caf_core::agent::{AgentSpec, SpawnCommand};
use caf_core::profiles::{Builtin, ProfileChoice};
use serde::Deserialize;

/// The adapter Claude's agent is reached through.
const CLAUDE_ADAPTER: &str = "claude-agent-acp";

/// `agents.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    agent: Vec<Agent>,
}

/// One `[[agent]]`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Agent {
    name: String,
    program: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    profile: Profile,
}

/// The profiles an agent may have.
#[derive(Deserialize)]
enum Profile {
    #[serde(rename = "claude-agent-acp")]
    ClaudeAgentAcp,
    #[serde(rename = "cautious")]
    Cautious,
}

/// Reads `agents.toml`.
///
/// # Errors
///
/// If it can't be read, isn't a list of agents, has two of one name, or
/// names a program that isn't there.
pub fn load(path: &Path) -> Result<Vec<AgentSpec>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    parse(&text, std::env::var_os("PATH").as_deref())
}

/// Reads the text of an `agents.toml`, looking bare program names up on
/// `path`.
fn parse(text: &str, path: Option<&std::ffi::OsStr>) -> Result<Vec<AgentSpec>, String> {
    let file: File = toml::from_str(text).map_err(|error| error.to_string())?;
    let mut names = BTreeSet::new();
    let mut agents = Vec::new();
    for agent in file.agent {
        if agent.name.is_empty() {
            return Err("an agent has no name".into());
        }
        if !names.insert(agent.name.clone()) {
            return Err(format!("two agents are called {:?}", agent.name));
        }
        let program = locate(&agent.program, path)
            .ok_or_else(|| format!("{}: no program {}", agent.name, agent.program.display()))?;
        let mut command = SpawnCommand::new(program);
        command.args = agent.args.into_iter().map(OsString::from).collect();
        let profile = match agent.profile {
            Profile::ClaudeAgentAcp => ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
            Profile::Cautious => ProfileChoice::Cautious,
        };
        agents.push(AgentSpec::command(agent.name, command, profile));
    }
    if agents.is_empty() {
        return Err("it names no agents".into());
    }
    Ok(agents)
}

/// The agents when there's no `agents.toml`: `claude`, if Claude's
/// adapter is on `PATH`, and `fake`, the fake agent following `script`,
/// if it's there to start.
#[must_use]
pub fn found(fake: Option<&Path>, script: &Path) -> Vec<AgentSpec> {
    let mut agents = Vec::new();
    let path = std::env::var_os("PATH");
    if let Some(claude) = locate(Path::new(CLAUDE_ADAPTER), path.as_deref()) {
        agents.push(AgentSpec::command(
            "claude",
            SpawnCommand::new(claude),
            ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
        ));
    }
    if let Some(fake) = fake {
        let mut command = SpawnCommand::new(fake);
        command.args = vec![script.as_os_str().to_owned()];
        agents.push(AgentSpec::command("fake", command, ProfileChoice::Cautious));
    }
    agents
}

/// A program by its absolute path, or a bare name found on `path`.
fn locate(program: &Path, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if program.is_absolute() {
        return program.is_file().then(|| program.to_path_buf());
    }
    // A relative path with folders in it would depend on where the
    // debugger was started.
    if program.components().count() != 1 {
        return None;
    }
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use caf_core::profiles::{Builtin, ProfileChoice};

    use super::parse;

    #[test]
    fn agents_toml_lists_agents_by_path_or_name() {
        let path = std::env::var_os("PATH");
        let agents = parse(
            r#"
            [[agent]]
            name = "sh"
            program = "sh"
            profile = "cautious"

            [[agent]]
            name = "claude"
            program = "/bin/sh"
            args = ["-c", "true"]
            profile = "claude-agent-acp"
            "#,
            path.as_deref(),
        )
        .expect("it reads");
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].name.0, "sh");
        assert!(agents[0].command.program.is_absolute());
        assert_eq!(agents[0].profile, ProfileChoice::Cautious);
        assert_eq!(agents[1].command.args, ["-c", "true"]);
        assert_eq!(
            agents[1].profile,
            ProfileChoice::Builtin(Builtin::ClaudeAgentAcp)
        );
    }

    #[test]
    fn agents_toml_must_be_right() {
        let path = std::env::var_os("PATH");
        let bad = |text: &str| parse(text, path.as_deref()).expect_err(text);
        bad("");
        bad(r#"[[agent]]
            name = "a"
            program = "/bin/sh"
            profile = "codex""#);
        bad(r#"[[agent]]
            name = "a"
            program = "bin/sh"
            profile = "cautious""#);
        bad(r#"[[agent]]
            name = "a"
            program = "/no/such/program"
            profile = "cautious""#);
        bad(r#"[[agent]]
            name = "a"
            program = "/bin/sh"
            profile = "cautious"
            [[agent]]
            name = "a"
            program = "/bin/sh"
            profile = "cautious""#);
        bad(r#"[[agent]]
            name = "a"
            program = "/bin/sh"
            profile = "cautious"
            colour = "red""#);
    }
}
