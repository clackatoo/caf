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
//!
//! # The Claude profile
//!
//! Checked against `@agentclientprotocol/claude-agent-acp` 0.98.0. It
//! declares:
//! - **forms and links** (`elicitation.form`, `elicitation.url`): without
//!   forms, the adapter takes Claude's AskUserQuestion tool away. CAF
//!   declines forms itself while hosts can't show them.
//! - **notices** (`session.notices`): without them, the adapter says its
//!   notices as words, which can arrive while no turn is open and look
//!   like a turn of their own.
//! - **no files and no terminals**: CAF serves neither yet, and refuses
//!   them if asked.
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
//! feature 1 turns on nothing risky, so that changes nothing yet. Any
//! other adapter, or one that doesn't say what it is, gets the cautious
//! profile for reading what it sends. What was declared at the start
//! stays declared: declarations go out before the adapter says who it
//! is.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::acp::RpcError;
use crate::chat::{FailureReason, Remedy};

/// Which profile an agent should have.
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterInfo {
    /// Its name, such as `@agentclientprotocol/claude-agent-acp`.
    pub name: String,
    /// Its version.
    pub version: String,
}

/// How far CAF has checked the adapter it's talking to.
#[non_exhaustive]
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

/// The adapter the Claude profile is for, as it names itself.
const CLAUDE_NAME: &str = "@agentclientprotocol/claude-agent-acp";

/// The version of it the Claude profile was checked against.
const CLAUDE_CHECKED: [u64; 3] = [0, 98, 0];

/// ACP's error for "the user must sign in first".
pub(crate) const AUTH_REQUIRED: i64 = -32000;

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
                declared["session"] = json!({ "notices": {} });
                declared["elicitation"] = json!({ "form": {}, "url": {} });
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
            Self::Builtin(Builtin::ClaudeAgentAcp) => match &adapter {
                Some(info) if info.name == CLAUDE_NAME => match version(&info.version) {
                    Some(version) if version == CLAUDE_CHECKED => Checked::Yes,
                    Some(version) if version > CLAUDE_CHECKED => Checked::Newer,
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

    /// Why a request failed, as this profile reads `error`: what went
    /// wrong in CAF's terms, what the user can do, and the agent's own
    /// words, uncleaned.
    pub(crate) fn failure(&self, error: &RpcError) -> Said {
        let data: Option<Value> = error.data.as_ref().and_then(|data| serde_json::from_str(data.get()).ok());
        let words = error.message.strip_prefix("Internal error: ").unwrap_or(&error.message);
        let words = (!words.trim().is_empty()).then(|| words.to_owned());
        if error.code == AUTH_REQUIRED {
            return Said {
                reason: FailureReason::SignInNeeded,
                remedy: None,
                words,
            };
        }
        let kind = data.as_ref().and_then(|data| data.get("errorKind")).and_then(Value::as_str);
        let reason = match (self, kind) {
            (Self::Builtin(Builtin::ClaudeAgentAcp), Some(kind)) => claude_kind(kind),
            _ => FailureReason::Other,
        };
        let remedy = match reason {
            FailureReason::ProviderError | FailureReason::UsageLimit | FailureReason::Other => Some(Remedy::SendAgain),
            _ => None,
        };
        Said { reason, remedy, words }
    }

    /// Whether a line of the adapter's error output says the user must
    /// sign in.
    pub(crate) fn wants_sign_in(&self, line: &str) -> bool {
        match self {
            Self::Builtin(Builtin::ClaudeAgentAcp) => {
                line.contains("/login") || line.contains("Invalid API key") || line.contains("Not logged in")
            }
            Self::Cautious => false,
        }
    }
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
fn claude_kind(kind: &str) -> FailureReason {
    match kind {
        "authentication_failed" | "oauth_org_not_allowed" => FailureReason::SignInNeeded,
        "billing_error" | "account_on_hold" | "rate_limit" => FailureReason::UsageLimit,
        "overloaded" | "server_error" | "no_result" | "incomplete_tool_call" | "worker_shutdown" | "transport_lost" => {
            FailureReason::ProviderError
        }
        "max_output_tokens" => FailureReason::TooLong,
        _ => FailureReason::Other,
    }
}

/// `text` as a version of three numbers, ignoring anything after a `-`
/// or `+`.
fn version(text: &str) -> Option<[u64; 3]> {
    let core = text.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(claude().check(adapter(CLAUDE_NAME, "0.98.0")).checked, Checked::Yes);
        let newer = claude().check(adapter(CLAUDE_NAME, "0.99.1"));
        assert_eq!((newer.checked, newer.profile), (Checked::Newer, claude()));
        for older in ["0.97.9", "0.98", "x", ""] {
            let report = claude().check(adapter(CLAUDE_NAME, older));
            assert_eq!((report.checked, report.profile), (Checked::Mismatch, ProfileChoice::Cautious), "{older}");
        }
        assert_eq!(claude().check(adapter("codex-acp", "0.98.0")).checked, Checked::Mismatch);
        assert_eq!(claude().check(None).checked, Checked::Mismatch);
        assert_eq!(ProfileChoice::Cautious.check(adapter(CLAUDE_NAME, "0.98.0")).checked, Checked::Cautious);
    }

    #[test]
    fn the_claude_profile_declares_forms_and_notices_but_no_air() {
        let declared = claude().declarations();
        assert_eq!(declared["elicitation"], json!({ "form": {}, "url": {} }));
        assert_eq!(declared["session"], json!({ "notices": {} }));
        assert_eq!(declared["terminal"], json!(false));
        assert!(declared.get("_meta").is_none(), "no AIR");
        let cautious = ProfileChoice::Cautious.declarations();
        assert!(cautious.get("elicitation").is_none() && cautious.get("session").is_none());
    }
}
