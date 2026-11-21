//! An agent's settings that the user may choose: its model, how hard the
//! model thinks, and fast mode.
//!
//! **The agent says what's available, never CAF.** Each agent lists its
//! settings once a session is open, and the list differs by account,
//! gateway and version. Each agent's profile translates how it lists them
//! (ACP's session config options; an older agent's `models` list; or
//! nothing, for one that can't change them) into this one shape, so an app
//! never needs to know which agent it's talking to. Settings CAF doesn't
//! support yet stay inside the profile, as the agent set them.
//!
//! **The user's choice is kept apart from the agent's report.** A chat
//! holds both ([`Settings`]): what the agent says is current, and what was
//! chosen. They differ while a change waits for the turn running to end,
//! or for the agent's answer.
//!
//! **When a change takes effect:**
//! - a change made while a turn runs is held, and sent when the turn ends,
//!   so it applies from the next message, whichever agent it is; with no
//!   turn running, it's sent at once;
//! - only the latest change of a setting counts;
//! - while a change is asked for, the agent's reports about that setting
//!   don't move the choice: its answer decides. A change the agent leaves
//!   unanswered for [`Timers::request`](crate::host::Timers::request)
//!   means its program is stuck: it's restarted, as for any request so
//!   left, and the session opened again, with every choice sent to it
//!   before the messages waiting go;
//! - when a session opens or reopens, the choices are sent first, and the
//!   messages waiting go once they're answered;
//! - only a choice made for the chat itself that the agent doesn't offer,
//!   or refuses, fails every message waiting, saying why, with
//!   [`Remedy::SendAgain`](crate::chat::Remedy::SendAgain); and the chat
//!   says so whether or not any waits
//!   ([`Note::NotOffered`](crate::chat::Note::NotOffered),
//!   [`Note::NotApplied`](crate::chat::Note::NotApplied)). The host's
//!   default is dropped quietly;
//! - a choice made for the chat itself that can't be applied, or that the
//!   agent stops offering, falls back as a restore would have it: to the
//!   host's default, if the agent offers that, and otherwise to what the
//!   agent uses. It's no longer the chat's own;
//! - **a message never goes with a choice silently replaced**: either it
//!   fails, as when a choice waiting to be sent can't be applied, or the
//!   chat says what changed, as when one applied already stops being
//!   offered ([`Note::NotOffered`](crate::chat::Note::NotOffered)), and
//!   messages go on what's used then;
//! - a choice is checked against what the agent offers when it's made, so
//!   an effort level only the model being changed to offers can be chosen
//!   once that change is answered, not with it;
//! - a change the agent makes by itself, such as a fallback model, is
//!   followed, and the chat says so
//!   ([`Note::AgentChanged`](crate::chat::Note::AgentChanged)).
//!
//! **Where a choice comes from**, strongest first: the chat's own
//! ([`OpenChat::settings`](crate::OpenChat::settings),
//! [`Core::set_setting`](crate::Core::set_setting)); the host's default for
//! the agent ([`AgentSpec::settings`](crate::agent::AgentSpec::settings));
//! the agent's own. A host default the agent doesn't offer is dropped
//! quietly. Which choices are the chat's own is saved with it
//! ([`Settings::own`]): a restored chat takes the host's defaults as they
//! are then for the rest, and leaves what the agent chose by itself to
//! the agent.
//!
//! **What the agent says it uses counts as offered**, even if it isn't in
//! the list it gives, as a model it fell back to may not be.
//!
//! **Choices are matched exactly**, by the agent's own ids, never by a
//! partial name: "opus" never quietly becomes a pricier "Opus (1M
//! context)".

use serde::{Deserialize, Serialize};

/// What a setting is about. A closed list that grows, as CAF supports
/// more of what agents offer (their mode comes with the permission
/// engine).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum SettingKind {
    /// Which model answers.
    Model,
    /// How hard the model thinks: its effort, or thinking level. Which
    /// levels there are depends on the model, and changes with it.
    Effort,
    /// Fast mode: quicker answers from the same model, at a higher price
    /// on some agents (Claude's). Offered only while the model supports
    /// it.
    Fast,
}

/// One setting, as the agent offers it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Setting {
    /// What it's about.
    pub kind: SettingKind,
    /// What the agent calls it, such as "Model".
    pub name: String,
    /// What it can be.
    pub form: SettingForm,
    /// What the agent says it is now, if it says.
    pub current: Option<SettingValue>,
}

/// What a setting can be.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum SettingForm {
    /// One of these, in the order the agent gives them. A choice such as
    /// Claude's `default`, which stands for the model the agent would pick
    /// itself, is passed on as it is: what to make of it is the app's.
    Choices(Vec<Choice>),
    /// On or off.
    Switch,
}

/// One choice of a setting.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Choice {
    /// The agent's own id for it, such as `sonnet`: what's chosen.
    pub id: String,
    /// What the agent calls it, such as "Sonnet".
    pub name: String,
    /// What the agent says of it, if anything.
    pub description: Option<String>,
    /// The group the agent puts it in, if any.
    pub group: Option<String>,
}

/// A setting's value.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum SettingValue {
    /// A choice, by the agent's id.
    Choice(String),
    /// A switch, on or off.
    Switch(bool),
}

/// A value chosen for a setting.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SettingChoice {
    /// The setting.
    pub kind: SettingKind,
    /// Its value.
    pub value: SettingValue,
}

impl SettingChoice {
    /// The model `id`, as the agent names it, such as `sonnet`.
    #[must_use]
    pub fn model(id: impl Into<String>) -> Self {
        Self {
            kind: SettingKind::Model,
            value: SettingValue::Choice(id.into()),
        }
    }

    /// The effort `id`, as the agent names it, such as `high`.
    #[must_use]
    pub fn effort(id: impl Into<String>) -> Self {
        Self {
            kind: SettingKind::Effort,
            value: SettingValue::Choice(id.into()),
        }
    }

    /// Fast mode, on or off.
    #[must_use]
    pub fn fast(on: bool) -> Self {
        Self {
            kind: SettingKind::Fast,
            value: SettingValue::Switch(on),
        }
    }
}

/// A chat's settings: what the agent offers, and what was chosen.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Settings {
    /// What the agent last said it offers, in its order, with what each
    /// is now. Empty before its session first opens, and for an agent
    /// that offers none. Kept while the session is closed, as it was last
    /// said.
    pub offered: Vec<Setting>,
    /// What was chosen, at most one value for each kind. Where it differs
    /// from a setting's `current`, the change is waiting: for the turn
    /// running to end, for the session to open, or for the agent's
    /// answer.
    pub chosen: Vec<SettingChoice>,
    /// The kinds chosen for the chat itself, with
    /// [`OpenChat::settings`](crate::OpenChat::settings) or
    /// [`Core::set_setting`](crate::Core::set_setting), in the order
    /// settings are sent in. Every other choice is the host's default
    /// ([`AgentSpec::settings`](crate::agent::AgentSpec::settings)), or
    /// what the agent changed to by itself, or went back to: one the agent
    /// doesn't offer is dropped quietly, and a restored chat takes the
    /// host's defaults as they are then in their place, or leaves them to
    /// the agent. Missing, as in a chat saved before CAF had settings, it
    /// reads as empty.
    #[serde(default)]
    pub own: Vec<SettingKind>,
}

/// What CAF knows of the settings an agent offers, before any chat:
/// learned from any chat's session with it, or on request
/// ([`Core::learn_settings`](crate::Core::learn_settings)). Not saved: it
/// depends on the account, the version and the user's own settings, so
/// it's learned again each run.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum KnownSettings {
    /// Not known yet.
    #[default]
    Unknown,
    /// Being learned.
    Learning,
    /// Known: what a session with it last offered, as it offered it.
    Known(Vec<Setting>),
    /// Couldn't be learned, saying why in CAF's words.
    Failed(String),
}

/// The model that answered a turn.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ModelUsed {
    /// The agent's id for it, such as `sonnet`.
    pub id: String,
    /// What the agent calls it, such as "Sonnet 4.6", if it says.
    pub name: Option<String>,
}
