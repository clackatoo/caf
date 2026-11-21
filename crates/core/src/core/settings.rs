//! Settings the user may choose, such as the agent's model: reading what
//! the agent offers, sending what's chosen, following what the agent
//! changes by itself, and learning what an agent offers before any chat,
//! by the rules the [`settings`](crate::settings) module sets out.
//!
//! A chat's settings are sent one at a time, the model first, as which
//! effort levels and fast mode there are depends on it, and never while
//! a turn runs. A message waiting goes only once nothing chosen is left
//! to send: [`Core::pump`] asks [`Core::send_settings`] first.
//!
//! How an agent lists its settings is read here, as its profile says
//! ([`SettingsStyle`]): ACP's session config options, or the older
//! `models` field, never both.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{Value, json};

use super::clean::agent_text;
use super::frames::Reply;
use super::procs::profile_of;
use super::state::{Conn, Learn, Open, Purpose, Standing};
use super::{Core, Refused, report};
use crate::change::Change;
use crate::chat::{
    AppData, AppItem, ChatState, Failure, FailureReason, ItemKind, Lost, Note, Remedy, TurnLife,
};
use crate::directive::{Directive, Level};
use crate::host::Grouping;
use crate::ids::{AgentName, AgentSessionId, ChatId, ConnectionId, Ticket};
use crate::profiles::{AUTH_REQUIRED, SettingsStyle, own_instructions};
use crate::settings::{
    Choice, KnownSettings, ModelUsed, Setting, SettingChoice, SettingForm, SettingKind,
    SettingValue, Settings,
};
use crate::time::{Now, Window};

/// The longest name of a setting or choice kept, in bytes.
const MOST_NAME: usize = 200;

/// The longest description of a choice kept, in bytes.
const MOST_DESCRIPTION: usize = 1024;

/// The longest id of a setting or choice kept, in bytes: one longer, or
/// with control characters, is skipped, as it's sent back exactly.
const MOST_ID: usize = 256;

/// The most choices of one setting kept: the rest are skipped.
const MOST_CHOICES: usize = 256;

/// The order a chat's settings are sent in: the model first, as which
/// effort levels and fast mode there are depends on it.
const ORDER: [SettingKind; 3] = [SettingKind::Model, SettingKind::Effort, SettingKind::Fast];

/// Where a chat is with sending what's chosen of its settings, this run.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "inspect", derive(Clone))]
pub(super) struct Choosing {
    /// How its session sets them, as the agent said when it opened.
    pub(super) via: Via,
    /// The kinds chosen and not yet sent to its session.
    pub(super) unsent: BTreeSet<SettingKind>,
    /// The one being set, waiting for the agent's answer.
    pub(super) asked: Option<Asked>,
    /// The agent has said what it offers, in this run.
    pub(super) told: bool,
    /// Setting one found its session gone, and the session was opened
    /// again: until one is set, the next that finds it gone is refused,
    /// so a broken agent can't have it opened over and over.
    pub(super) reopened: bool,
}

/// A setting being set.
#[derive(Clone, Debug, Serialize)]
pub(super) struct Asked {
    /// The process asked.
    pub(super) proc: Ticket,
    /// The request.
    pub(super) id: i64,
    /// What was asked for.
    pub(super) choice: SettingChoice,
}

/// How a session's settings are set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum Via {
    /// They can't be: the agent listed none.
    #[default]
    Nothing,
    /// With `session/set_config_option`, each by its config option.
    Options(Vec<ConfigOption>),
    /// With `session/set_model`: the model alone.
    Models,
}

/// One of ACP's session config options, as CAF sets it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct ConfigOption {
    /// The setting it is.
    pub(super) kind: SettingKind,
    /// The agent's id for it.
    pub(super) id: String,
    /// How it takes a value.
    pub(super) takes: Takes,
}

/// How a config option takes a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub(super) enum Takes {
    /// A choice's id.
    Choice,
    /// `true` or `false`: an option of the type `boolean`.
    Boolean,
    /// `on` or `off`: an on/off option offered as a choice, to a client
    /// that didn't say it takes the type `boolean`.
    OnOff,
}

/// What an agent offers, read from what it said, and how to set it.
#[derive(Debug, Default)]
pub(super) struct Offer {
    /// What it offers, in its order.
    pub(super) settings: Vec<Setting>,
    /// How to set it.
    pub(super) via: Via,
}

impl<A: AppItem, D: AppData> Core<A, D> {
    /// The host's default choices for `agent`, at most one of each kind:
    /// the last given, if more are. One with an id CAF can't send exactly
    /// is skipped.
    fn host_defaults(&self, agent: &AgentName) -> Vec<SettingChoice> {
        let mut settings = Settings::default();
        if let Some(spec) = self.agents.get(agent) {
            for choice in spec.settings.iter().filter(|choice| sendable(choice)) {
                choose(&mut settings, choice.clone());
            }
        }
        settings.chosen
    }

    /// What a new chat with `agent` starts with: the host's defaults for
    /// it, and the chat's own choices (`own`) over them, at most one of
    /// each kind, the last given.
    pub(super) fn first_settings(&self, agent: &AgentName, own: &[SettingChoice]) -> Settings {
        let mut settings = Settings::default();
        for choice in self.host_defaults(agent) {
            choose(&mut settings, choice);
        }
        for choice in own {
            make_own(&mut settings, choice.kind);
            choose(&mut settings, choice.clone());
        }
        settings
    }

    /// A restored chat keeps its own choices; for the rest, it takes the
    /// host's defaults as they are now, or leaves them to the agent. What
    /// the agent chose by itself in an earlier run never outranks the
    /// host.
    pub(super) fn restored_choices(&mut self, chat: ChatId) {
        let Some(agent) = self.chats.get(&chat).map(|whole| whole.agent.clone()) else {
            return;
        };
        let defaults = self.host_defaults(&agent);
        self.change_settings(chat, |settings| {
            let own = settings.own.clone();
            settings.chosen.retain(|choice| own.contains(&choice.kind));
            for choice in defaults
                .into_iter()
                .filter(|choice| !own.contains(&choice.kind))
            {
                choose(settings, choice);
            }
        });
    }

    /// Chooses a setting of `chat`'s agent: sent at once if it can be,
    /// and otherwise as soon as it can.
    pub(super) fn set_setting_as(
        &mut self,
        now: Now,
        chat: ChatId,
        choice: SettingChoice,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        let whole = self.chats.get(&chat).ok_or(Refused::NoSuchChat)?;
        if whole.state == ChatState::Closed {
            return Err(Refused::Closed);
        }
        let run = self.runs.get(&chat).ok_or(Refused::NoSuchChat)?;
        if !sendable(&choice) {
            return Err(Refused::Unsendable);
        }
        // Before the agent has said in this run what it offers, any choice
        // is taken, and checked as its session opens: what a restored chat
        // was offered may have changed.
        if run.choosing.told && !offers(&whole.settings.offered, &choice) {
            return Err(Refused::NoSuchSetting);
        }
        // The same choice, being set already, isn't asked for twice.
        let asking = (run.choosing.asked.as_ref()).is_some_and(|asked| {
            asked.choice == choice && self.pending.contains_key(&(asked.proc, asked.id))
        });
        if let Some(run) = self.runs.get_mut(&chat) {
            if asking {
                run.choosing.unsent.remove(&choice.kind);
            } else {
                run.choosing.unsent.insert(choice.kind);
            }
        }
        self.change_settings(chat, |settings| {
            make_own(settings, choice.kind);
            choose(settings, choice);
        });
        self.pump(now, chat, out);
        Ok(())
    }

    /// `chat`'s session opened, the agent answering `result`: what it
    /// offers is read, and every choice is to be sent, before any message.
    /// Claude's adapter, for one, prefers the user's own model to the
    /// session's on resume.
    pub(super) fn settings_opened(&mut self, chat: ChatId, result: &Value) {
        let style = self.profile_of_chat(chat).settings();
        let offer = offer(style, result);
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        let agent = whole.agent.clone();
        let kinds = whole
            .settings
            .chosen
            .iter()
            .map(|choice| choice.kind)
            .collect();
        if let Some(run) = self.runs.get_mut(&chat) {
            run.choosing.via = offer.via;
            run.choosing.told = true;
            run.choosing.asked = None;
            run.choosing.unsent = kinds;
        }
        if let Some(standing) = self.setup.agents.get_mut(&agent) {
            standing.settings = KnownSettings::Known(offer.settings.clone());
        }
        self.change_settings(chat, |settings| settings.offered = offer.settings);
    }

    /// Sends `chat`'s next setting to send, to its session `session` on
    /// `proc`. Whether nothing is left to send or waiting for an answer, so
    /// a message may go.
    pub(super) fn send_settings(
        &mut self,
        now: Now,
        chat: ChatId,
        proc: Ticket,
        session: &AgentSessionId,
        out: &mut Vec<Directive<A, D>>,
    ) -> bool {
        // One the agent may still answer. One whose request ended, as
        // its process went, no longer counts.
        let asked = (self.runs.get(&chat))
            .and_then(|run| run.choosing.asked.as_ref())
            .map(|asked| (asked.proc, asked.id));
        if let Some(asked) = asked {
            if self.pending.contains_key(&asked) {
                return false;
            }
            if let Some(run) = self.runs.get_mut(&chat) {
                run.choosing.asked = None;
            }
        }
        loop {
            let Some(run) = self.runs.get_mut(&chat) else {
                return false;
            };
            let Some(kind) = ORDER
                .into_iter()
                .find(|kind| run.choosing.unsent.contains(kind))
            else {
                return true;
            };
            run.choosing.unsent.remove(&kind);
            let Some(whole) = self.chats.get(&chat) else {
                return false;
            };
            let Some(value) = chosen(&whole.settings, kind).cloned() else {
                continue;
            };
            let choice = SettingChoice { kind, value };
            // Checked against what the agent offers now: what it offers
            // changes with the model.
            let request = offers(&whole.settings.offered, &choice)
                .then(|| request_for(&run.choosing.via, &choice, session))
                .flatten();
            let Some(request) = request else {
                self.not_applied(now, chat, &choice, None, out);
                continue;
            };
            let wait = self.host.timers.request;
            let purpose = Purpose::Set {
                chat,
                choice: choice.clone(),
                session: session.clone(),
                given_up: false,
            };
            let id = self.request(now, proc, request, purpose, Some(wait), out);
            if let Some(run) = self.runs.get_mut(&chat) {
                match id {
                    Some(id) => run.choosing.asked = Some(Asked { proc, id, choice }),
                    // Its process may not be asked anything: it's sent as
                    // the session opens again.
                    None => {
                        run.choosing.unsent.insert(kind);
                    }
                }
            }
            return false;
        }
    }

    /// `choice` for `chat` can't be applied: the agent doesn't offer it,
    /// or refused it, saying `refused` if it said anything. One of the
    /// chat's own goes back to what the agent uses; the chat says so if
    /// it's no longer offered, and every message waiting fails, as it
    /// would otherwise go on a setting nobody chose. Any other, the host's
    /// default or the agent's own, is dropped quietly.
    fn not_applied(
        &mut self,
        now: Now,
        chat: ChatId,
        choice: &SettingChoice,
        refused: Option<String>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        let offered = offers(&whole.settings.offered, choice);
        let own = whole.settings.own.contains(&choice.kind);
        self.drop_or_fall_back(chat, choice.kind);
        let why = match (&refused, offered) {
            (Some(words), _) => format!("the agent refused it: {words}"),
            (None, true) => "the agent refused it".to_owned(),
            (None, false) => "the agent doesn't offer it".to_owned(),
        };
        if !own {
            return report(
                out,
                Level::Info,
                format!(
                    "chat {}: {choice:?}, not its own, was dropped: {why}",
                    chat.0
                ),
            );
        }
        report(
            out,
            Level::Info,
            format!("chat {}: {choice:?} wasn't applied: {why}", chat.0),
        );
        if !offered {
            self.tell(now, chat, Note::NotOffered(choice.clone()));
        }
        let failure = Failure {
            reason: FailureReason::SettingNotApplied,
            agent_said: refused,
            remedy: Some(Remedy::SendAgain),
        };
        for turn in self.waiting_turns(chat) {
            self.set_life(now, chat, turn, TurnLife::Failed(failure.clone()));
        }
    }

    /// The agent answered request `id` of `proc`, setting `choice` for
    /// `chat`.
    pub(super) fn set_answered(
        &mut self,
        now: Now,
        (proc, id): (Ticket, i64),
        chat: ChatId,
        choice: SettingChoice,
        reply: Reply,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let waited = self.take_asked(chat, (proc, id));
        // A closed chat's record is as it was left.
        let closed = (self.chats.get(&chat)).is_none_or(|whole| whole.state == ChatState::Closed);
        if !waited || closed {
            return report(
                out,
                Level::Debug,
                format!(
                    "chat {}: an answer to a setting no longer waited for",
                    chat.0
                ),
            );
        }
        // A newer choice of it waits to be sent: this one no longer counts.
        let superseded = (self.chats.get(&chat))
            .is_none_or(|whole| chosen(&whole.settings, choice.kind) != Some(&choice.value));
        let before = (self.chats.get(&chat))
            .map(|whole| whole.settings.offered.clone())
            .unwrap_or_default();
        match reply {
            Reply::Result(result) => {
                if let Some(run) = self.runs.get_mut(&chat) {
                    run.choosing.reopened = false;
                }
                let style = self.profile_of_chat(chat).settings();
                let listed = (result.get("configOptions").and_then(Value::as_array))
                    .filter(|_| style.options)
                    .map(|options| options_offer(style, options));
                match listed {
                    // The whole list, each time: it replaces what was.
                    Some(offer) => {
                        if let Some(run) = self.runs.get_mut(&chat) {
                            run.choosing.via = offer.via;
                        }
                        self.change_settings(chat, |settings| {
                            settings.offered = offer.settings;
                            // Listed, but not saying what it is now: as asked.
                            as_asked(settings, &choice, false);
                        });
                    }
                    // An answer that doesn't say: as asked.
                    None => {
                        self.change_settings(chat, |settings| as_asked(settings, &choice, true))
                    }
                }
                // Set, but to something else: the agent's word stands.
                let current = (self.chats.get(&chat))
                    .and_then(|whole| current(&whole.settings, choice.kind).cloned());
                if !superseded && let Some(value) = current.filter(|value| *value != choice.value) {
                    self.agent_changed(
                        now,
                        chat,
                        SettingChoice {
                            kind: choice.kind,
                            value,
                        },
                    );
                }
                self.follow(now, chat, &before);
            }
            // As for a prompt: the session is opened again, resumed where
            // the agent can, and every choice sent to it. Once only, until
            // one is set.
            Reply::Error(error)
                if self.profile_of_chat(chat).session_gone(&error)
                    && self
                        .runs
                        .get(&chat)
                        .is_some_and(|run| !run.choosing.reopened) =>
            {
                report(
                    out,
                    Level::Info,
                    format!(
                        "chat {}: its session was gone as a setting was sent: opening it again",
                        chat.0
                    ),
                );
                if let Some(run) = self.runs.get_mut(&chat) {
                    run.choosing.reopened = true;
                }
                self.end_agent_turn(now, chat);
                return self.want_open(now, chat, out);
            }
            Reply::Error(error) if !superseded => {
                let said = self.profile_of_chat(chat).failure(&error).words;
                let most = self.host.limits.failure_text;
                let words = said
                    .map(|words| agent_text(&words, most))
                    .filter(|words| !words.is_empty());
                self.not_applied(now, chat, &choice, words, out);
            }
            Reply::Unreadable if !superseded => self.not_applied(now, chat, &choice, None, out),
            Reply::Error(_) | Reply::Unreadable => report(
                out,
                Level::Debug,
                format!(
                    "chat {}: the agent refused a setting since chosen again",
                    chat.0
                ),
            ),
        }
        self.pump(now, chat, out);
    }

    /// Stops waiting for `chat`'s setting asked for as request `id` of
    /// `proc`: whether it was waited for.
    fn take_asked(&mut self, chat: ChatId, (proc, id): (Ticket, i64)) -> bool {
        let Some(run) = self.runs.get_mut(&chat) else {
            return false;
        };
        (run.choosing.asked)
            .take_if(|asked| asked.proc == proc && asked.id == id)
            .is_some()
    }

    /// The agent didn't answer request `id` of `proc`, setting `choice`
    /// for `chat`, in time: it isn't applied. The request is kept as given
    /// up, so what a late answer says is still read.
    pub(super) fn set_overdue(
        &mut self,
        now: Now,
        (proc, id): (Ticket, i64),
        chat: ChatId,
        choice: &SettingChoice,
        out: &mut Vec<Directive<A, D>>,
    ) {
        report(
            out,
            Level::Warn,
            format!(
                "chat {}: the agent didn't answer setting {choice:?}",
                chat.0
            ),
        );
        let waited = self.take_asked(chat, (proc, id))
            && (self.chats.get(&chat)).is_some_and(|whole| whole.state != ChatState::Closed);
        let superseded = (self.chats.get(&chat))
            .is_none_or(|whole| chosen(&whole.settings, choice.kind) != Some(&choice.value));
        if waited && !superseded {
            self.not_applied(now, chat, choice, None, out);
        }
        self.pump(now, chat, out);
    }

    /// The agent answered, late, a request setting one of `chat`'s settings
    /// that CAF gave up on: what it lists is read as the agent's word on
    /// what it offers and uses now, as if it said it by itself.
    ///
    /// Only for the session it was asked of: a session opened since may
    /// use something else. If it says the setting is what was asked, and
    /// nothing has been chosen of it since, the choice was applied after
    /// all, and is the chat's own again.
    pub(super) fn late_set_answered(
        &mut self,
        now: Now,
        (chat, choice, session): (ChatId, &SettingChoice, &AgentSessionId),
        reply: &Reply,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let same = (self.runs.get(&chat)).is_some_and(|run| {
            matches!(run.open, Open::Live { .. }) && run.session.as_ref() == Some(session)
        });
        if !same {
            return report(
                out,
                Level::Debug,
                format!(
                    "chat {}: a late answer about a session it no longer has, ignored",
                    chat.0
                ),
            );
        }
        let listed = match reply {
            Reply::Result(result) => result.get("configOptions").and_then(Value::as_array),
            _ => None,
        };
        let style = self.profile_of_chat(chat).settings();
        let applied = listed.is_some_and(|options| {
            let offer = options_offer(style, options);
            let says = (offer.settings.iter())
                .find(|setting| setting.kind == choice.kind)
                .and_then(|setting| setting.current.as_ref());
            says == Some(&choice.value)
        });
        let own =
            (self.chats.get(&chat)).is_some_and(|whole| whole.settings.own.contains(&choice.kind));
        if applied && !own {
            self.change_settings(chat, |settings| {
                make_own(settings, choice.kind);
                choose(settings, choice.clone());
            });
            // Something else of it may be on its way meanwhile, such as the
            // host's default it fell back to: this goes again after it.
            if let Some(run) = self.runs.get_mut(&chat)
                && run
                    .choosing
                    .asked
                    .as_ref()
                    .is_some_and(|asked| asked.choice.kind == choice.kind)
            {
                run.choosing.unsent.insert(choice.kind);
            }
        }
        match listed {
            Some(options) => self.options_updated(now, chat, Some(options), out),
            None => report(
                out,
                Level::Debug,
                format!(
                    "chat {}: the agent answered a setting given up on, saying nothing of it",
                    chat.0
                ),
            ),
        }
    }

    /// `chat`'s choice of `kind` can't be applied, or is no longer offered:
    /// dropped, if it isn't the chat's own. One of its own falls back as a
    /// restore would have it: to the host's default, if the agent offers
    /// that, which is then sent; and otherwise to what the agent says it
    /// uses (see [`fall_back`]). Either way it's no longer the chat's own.
    fn drop_or_fall_back(&mut self, chat: ChatId, kind: SettingKind) {
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        let default = (self.host_defaults(&whole.agent).into_iter())
            .find(|choice| choice.kind == kind)
            .filter(|choice| offers(&whole.settings.offered, choice));
        let mut send = false;
        self.change_settings(chat, |settings| {
            if !settings.own.contains(&kind) {
                settings.chosen.retain(|each| each.kind != kind);
                return;
            }
            settings.own.retain(|each| *each != kind);
            match default {
                Some(choice) => {
                    send = current(settings, kind) != Some(&choice.value);
                    choose(settings, choice);
                }
                None => fall_back(settings, kind),
            }
        });
        if send && let Some(run) = self.runs.get_mut(&chat) {
            run.choosing.unsent.insert(kind);
        }
    }

    /// The agent said what it offers, by itself (`config_option_update`):
    /// the whole list, which replaces what was. What it changed by itself
    /// is followed.
    pub(super) fn options_updated(
        &mut self,
        now: Now,
        chat: ChatId,
        options: Option<&Vec<Value>>,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let style = self.profile_of_chat(chat).settings();
        let (Some(run), Some(whole)) = (self.runs.get(&chat), self.chats.get(&chat)) else {
            return;
        };
        let before = whole.settings.offered.clone();
        // While a session opens, its answer says what it offers.
        if !matches!(run.open, Open::Live { .. }) || whole.state == ChatState::Closed {
            return report(
                out,
                Level::Debug,
                format!(
                    "chat {}: ignored settings said before its session opened",
                    chat.0
                ),
            );
        }
        let Some(options) = options.filter(|_| style.options) else {
            return report(
                out,
                Level::Debug,
                format!("chat {}: ignored settings CAF can't read", chat.0),
            );
        };
        let offer = options_offer(style, options);
        if let Some(run) = self.runs.get_mut(&chat) {
            run.choosing.via = offer.via;
        }
        self.change_settings(chat, |settings| settings.offered = offer.settings);
        self.follow(now, chat, &before);
        // A choice that fell back to the host's default goes to the agent.
        self.pump(now, chat, out);
    }

    /// Follows what the agent changed by itself, or no longer offers, of
    /// `chat`'s choices: each it isn't being sent, or about to be, whose
    /// setting isn't as it was (`before`).
    fn follow(&mut self, now: Now, chat: ChatId, before: &[Setting]) {
        let (Some(whole), Some(run)) = (self.chats.get(&chat), self.runs.get(&chat)) else {
            return;
        };
        let asked = run.choosing.asked.as_ref().map(|asked| asked.choice.kind);
        let settled: Vec<SettingChoice> = (whole.settings.chosen.iter())
            .filter(|choice| {
                Some(choice.kind) != asked && !run.choosing.unsent.contains(&choice.kind)
            })
            .filter(|choice| {
                let was = before.iter().find(|setting| setting.kind == choice.kind);
                let now =
                    (whole.settings.offered.iter()).find(|setting| setting.kind == choice.kind);
                was != now
            })
            .cloned()
            .collect();
        for choice in settled {
            let Some(whole) = self.chats.get(&chat) else {
                return;
            };
            if !offers(&whole.settings.offered, &choice) {
                let own = whole.settings.own.contains(&choice.kind);
                self.drop_or_fall_back(chat, choice.kind);
                if own {
                    self.tell(now, chat, Note::NotOffered(choice));
                }
            } else if let Some(value) = current(&whole.settings, choice.kind)
                .filter(|value| **value != choice.value)
                .cloned()
            {
                self.agent_changed(
                    now,
                    chat,
                    SettingChoice {
                        kind: choice.kind,
                        value,
                    },
                );
            }
        }
    }

    /// The agent changed a setting by itself, to `choice`: the choice
    /// follows it, and the chat says so.
    fn agent_changed(&mut self, now: Now, chat: ChatId, choice: SettingChoice) {
        self.change_settings(chat, |settings| {
            settings.own.retain(|kind| *kind != choice.kind);
            choose(settings, choice.clone());
        });
        self.tell(now, chat, Note::AgentChanged(choice));
    }

    /// Says `note` in `chat`: in the turn open, the agent's or one it
    /// started by itself, as it happened during it; or as a turn of CAF's
    /// own.
    fn tell(&mut self, now: Now, chat: ChatId, note: Note) {
        match self.open_turn(chat) {
            Some(turn) => {
                let window = Window::instant(now);
                self.add_live_item(now, chat, turn, ItemKind::Note(note), window);
            }
            None => self.say(now, chat, note),
        }
    }

    /// Changes `chat`'s settings with `change`, recording it if anything
    /// changed.
    fn change_settings(&mut self, chat: ChatId, change: impl FnOnce(&mut Settings)) {
        let Some(whole) = self.chats.get(&chat) else {
            return;
        };
        let mut settings = whole.settings.clone();
        change(&mut settings);
        if settings != whole.settings {
            self.record(chat, Change::Settings(settings));
        }
    }

    /// The model `chat`'s agent says it's using, if it says.
    pub(super) fn model_used(&self, chat: ChatId) -> Option<ModelUsed> {
        let offered = &self.chats.get(&chat)?.settings.offered;
        let model = offered
            .iter()
            .find(|setting| setting.kind == SettingKind::Model)?;
        let Some(SettingValue::Choice(id)) = &model.current else {
            return None;
        };
        let name = match &model.form {
            SettingForm::Choices(choices) => (choices.iter())
                .find(|choice| choice.id == *id)
                .map(|choice| choice.name.clone()),
            _ => None,
        };
        Some(ModelUsed {
            id: id.clone(),
            name,
        })
    }

    /// Learns what settings `agent` offers, from a session opened only for
    /// that.
    pub(super) fn learn_as(
        &mut self,
        now: Now,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        let spec = self.agents.get(agent).ok_or(Refused::NoSuchAgent)?;
        if !spec.profile.strips() {
            return Err(Refused::OwnTools);
        }
        let learning =
            (self.conns.values()).any(|each| each.agent == *agent && each.learn.is_some());
        let Some(standing) = self.setup.agents.get_mut(agent) else {
            return Err(Refused::NoSuchAgent);
        };
        if learning {
            return Ok(());
        }
        if self.shutting_down {
            set_known(
                standing,
                KnownSettings::Failed("CAF is shutting down".to_owned()),
            );
            return Ok(());
        }
        // What was learned last stays, while it's learned again.
        if !matches!(standing.settings, KnownSettings::Known(_)) {
            standing.settings = KnownSettings::Learning;
        }
        // A running adapter is used, as the host's grouping allows.
        let shared = self.host.grouping != Grouping::PerChat;
        let found = (self.conns.iter())
            .find(|(_, each)| shared && each.only.is_none() && each.agent == *agent)
            .map(|(&conn, _)| conn);
        let conn = match found {
            Some(conn) => conn,
            None => {
                let conn = ConnectionId(self.ids.take());
                self.conns.insert(
                    conn,
                    Conn {
                        agent: agent.clone(),
                        only: None,
                        chats: BTreeSet::new(),
                        current: None,
                        profile: None,
                        stderr: super::clean::Tail::default(),
                        sign_in: false,
                        learn: None,
                    },
                );
                conn
            }
        };
        if let Some(each) = self.conns.get_mut(&conn) {
            each.learn = Some(Learn::Wanted);
        }
        self.advance(now, conn, out);
        Ok(())
    }

    /// Asks `proc`, which has said what it can do, for a session to learn
    /// `conn`'s agent's settings from: in CAF's installs' folder, if known,
    /// and otherwise `/`, with none of the agent's own tools.
    pub(super) fn ask_learn(
        &mut self,
        now: Now,
        conn: ConnectionId,
        proc: Ticket,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(each) = self.conns.get(&conn) else {
            return;
        };
        let profile = profile_of(each, &self.agents);
        if !profile.strips() {
            return self.learn_failed(conn, "CAF can't turn off its own tools");
        }
        let cwd = (self.setup.machine.as_ref())
            .and_then(|machine| machine.root.to_str())
            .filter(|root| std::path::Path::new(root).is_absolute())
            .unwrap_or("/");
        let mut params = json!({ "cwd": cwd, "mcpServers": [] });
        if let Some(meta) = profile.stripped(&own_instructions(&self.host.name), None) {
            params["_meta"] = meta;
        }
        let wait = self.host.timers.session_open;
        let purpose = Purpose::Learn { conn };
        if let Some(id) = self.request(now, proc, ("session/new", params), purpose, Some(wait), out)
            && let Some(each) = self.conns.get_mut(&conn)
        {
            each.learn = Some(Learn::Asked { proc, id });
        }
    }

    /// The agent answered request `id` of `proc`, opening a session to
    /// learn `conn`'s agent's settings from. The session is closed, if the
    /// adapter goes on serving chats; otherwise it's stopped.
    pub(super) fn learned(
        &mut self,
        now: Now,
        (proc, id): (Ticket, i64),
        conn: ConnectionId,
        reply: Reply,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(each) = self.conns.get(&conn) else {
            return;
        };
        let waited = each.learn == Some(Learn::Asked { proc, id });
        let serving = !each.chats.is_empty();
        let agent = each.agent.clone();
        let style = profile_of(each, &self.agents).settings();
        let known = match reply {
            Reply::Result(result) => {
                let session = (result.get("sessionId").and_then(Value::as_str))
                    .filter(|session| super::sessions::keepable(session));
                if let Some(session) = session {
                    let session = AgentSessionId(session.to_owned());
                    if serving {
                        self.close_late(now, proc, session, out);
                    } else if let Some(each) = self.procs.get_mut(&proc) {
                        each.sessions.insert(session.0);
                    }
                }
                KnownSettings::Known(offer(style, &result).settings)
            }
            Reply::Error(error) if error.code == AUTH_REQUIRED => {
                KnownSettings::Failed("signing in is needed".to_owned())
            }
            Reply::Error(error) => KnownSettings::Failed(format!(
                "the agent refused to open a session: {}",
                agent_text(&error.message, self.host.limits.failure_text)
            )),
            Reply::Unreadable => {
                KnownSettings::Failed("the agent's answer couldn't be read".to_owned())
            }
        };
        if !waited {
            return;
        }
        if let Some(standing) = self.setup.agents.get_mut(&agent) {
            set_known(standing, known);
        }
        if let Some(each) = self.conns.get_mut(&conn) {
            each.learn = None;
        }
        self.advance(now, conn, out);
        self.tidy(conn);
    }

    /// Learning `conn`'s agent's settings failed, as `why` says.
    pub(super) fn learn_failed(&mut self, conn: ConnectionId, why: &str) {
        let Some(each) = self.conns.get_mut(&conn) else {
            return;
        };
        if each.learn.take().is_none() {
            return;
        }
        if let Some(standing) = self.setup.agents.get_mut(&each.agent) {
            set_known(standing, KnownSettings::Failed(why.to_owned()));
        }
    }
}

/// Sets what's known of the settings an agent offers to `known`: but a
/// failure to learn them again leaves what was learned before, such as
/// from a chat's session opened meanwhile.
fn set_known(standing: &mut Standing, known: KnownSettings) {
    let keep = matches!(known, KnownSettings::Failed(_))
        && matches!(standing.settings, KnownSettings::Known(_));
    if !keep {
        standing.settings = known;
    }
}

/// Why learning an agent's settings failed, when its adapter was `lost`.
pub(super) fn lost_why(lost: &Lost) -> String {
    match lost {
        Lost::SignInNeeded => "signing in is needed".to_owned(),
        Lost::CouldNotStart { error } => format!("the agent couldn't start: {error}"),
        Lost::Refused { message } => format!("the agent refused: {message}"),
        Lost::Restarted => "the agent was restarted".to_owned(),
        Lost::ShutDown => "CAF shut down".to_owned(),
        Lost::Incompatible(_) => "CAF can't use the agent".to_owned(),
        _ => "the agent stopped".to_owned(),
    }
}

/// What an agent offers, as its answer opening a session (`result`) says,
/// read as its profile's `style` says.
pub(super) fn offer(style: SettingsStyle, result: &Value) -> Offer {
    if style.options
        && let Some(options) = result.get("configOptions").and_then(Value::as_array)
    {
        return options_offer(style, options);
    }
    if style.models
        && let Some(models) = result.get("models").filter(|models| models.is_object())
    {
        return models_offer(models);
    }
    Offer::default()
}

/// What ACP's session config options offer that CAF supports: the first
/// of each kind, read leniently. Others, such as modes, are left as the
/// agent set them.
fn options_offer(style: SettingsStyle, options: &[Value]) -> Offer {
    let mut settings = Vec::new();
    let mut set = Vec::new();
    for option in options {
        let Some(id) = option
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| keepable(id))
        else {
            continue;
        };
        let category = option.get("category").and_then(Value::as_str);
        let kind = match category {
            _ if style.fast == Some(id) => SettingKind::Fast,
            Some("model") => SettingKind::Model,
            Some("thought_level") => SettingKind::Effort,
            _ => continue,
        };
        if settings
            .iter()
            .any(|setting: &Setting| setting.kind == kind)
        {
            continue;
        }
        let name = (option.get("name").and_then(Value::as_str)).map_or_else(
            || agent_text(id, MOST_NAME),
            |name| agent_text(name, MOST_NAME),
        );
        let read = match (kind, option.get("type").and_then(Value::as_str)) {
            (SettingKind::Fast, Some("boolean")) => {
                let current = option.get("currentValue").and_then(Value::as_bool);
                Some((
                    SettingForm::Switch,
                    current.map(SettingValue::Switch),
                    Takes::Boolean,
                ))
            }
            (SettingKind::Fast, Some("select")) => on_off(option),
            (SettingKind::Model | SettingKind::Effort, Some("select")) => {
                let current = (option.get("currentValue").and_then(Value::as_str))
                    .filter(|id| keepable(id))
                    .map(|id| SettingValue::Choice(id.to_owned()));
                let choices = choices(option.get("options"));
                Some((SettingForm::Choices(choices), current, Takes::Choice))
            }
            _ => None,
        };
        let Some((form, current, takes)) = read else {
            continue;
        };
        settings.push(Setting {
            kind,
            name,
            form,
            current,
        });
        set.push(ConfigOption {
            kind,
            id: id.to_owned(),
            takes,
        });
    }
    Offer {
        settings,
        via: Via::Options(set),
    }
}

/// An on/off option offered as a choice of `on` and `off`, to a client
/// that didn't say it takes the type `boolean`, read as a switch.
fn on_off(option: &Value) -> Option<(SettingForm, Option<SettingValue>, Takes)> {
    let mut values: Vec<&str> = (choices(option.get("options")).into_iter())
        .map(|choice| match choice.id.as_str() {
            "on" => "on",
            "off" => "off",
            _ => "",
        })
        .collect();
    values.sort_unstable();
    if values != ["off", "on"] {
        return None;
    }
    let current = match option.get("currentValue").and_then(Value::as_str) {
        Some("on") => Some(SettingValue::Switch(true)),
        Some("off") => Some(SettingValue::Switch(false)),
        _ => None,
    };
    Some((SettingForm::Switch, current, Takes::OnOff))
}

/// A select option's choices, flat or in groups, in the agent's order.
fn choices(options: Option<&Value>) -> Vec<Choice> {
    let mut choices: Vec<Choice> = Vec::new();
    let mut add = |each: &Value, group: Option<&str>| {
        let Some(id) = each
            .get("value")
            .and_then(Value::as_str)
            .filter(|id| keepable(id))
        else {
            return;
        };
        if choices.len() >= MOST_CHOICES || choices.iter().any(|choice| choice.id == id) {
            return;
        }
        let text = |key: &str, most: usize| {
            (each.get(key).and_then(Value::as_str)).map(|text| agent_text(text, most))
        };
        choices.push(Choice {
            id: id.to_owned(),
            name: text("name", MOST_NAME).unwrap_or_else(|| agent_text(id, MOST_NAME)),
            description: text("description", MOST_DESCRIPTION).filter(|text| !text.is_empty()),
            group: group.map(|group| agent_text(group, MOST_NAME)),
        });
    };
    for each in options.and_then(Value::as_array).into_iter().flatten() {
        match each.get("options").and_then(Value::as_array) {
            Some(grouped) => {
                let group = (each.get("name").and_then(Value::as_str))
                    .or_else(|| each.get("group").and_then(Value::as_str));
                for inner in grouped {
                    add(inner, group);
                }
            }
            None => add(each, None),
        }
    }
    choices
}

/// What the older `models` field offers: the agent's models, set with
/// `session/set_model`.
fn models_offer(models: &Value) -> Offer {
    let available = models.get("availableModels").and_then(Value::as_array);
    let mut choices: Vec<Choice> = Vec::new();
    for model in available.into_iter().flatten() {
        let Some(id) = model
            .get("modelId")
            .and_then(Value::as_str)
            .filter(|id| keepable(id))
        else {
            continue;
        };
        if choices.len() >= MOST_CHOICES || choices.iter().any(|choice| choice.id == id) {
            continue;
        }
        let text = |key: &str, most: usize| {
            (model.get(key).and_then(Value::as_str)).map(|text| agent_text(text, most))
        };
        choices.push(Choice {
            id: id.to_owned(),
            name: text("name", MOST_NAME).unwrap_or_else(|| agent_text(id, MOST_NAME)),
            description: text("description", MOST_DESCRIPTION).filter(|text| !text.is_empty()),
            group: None,
        });
    }
    if choices.is_empty() {
        return Offer::default();
    }
    let current = (models.get("currentModelId").and_then(Value::as_str))
        .filter(|id| keepable(id))
        .map(|id| SettingValue::Choice(id.to_owned()));
    Offer {
        settings: vec![Setting {
            kind: SettingKind::Model,
            name: "Model".to_owned(),
            form: SettingForm::Choices(choices),
            current,
        }],
        via: Via::Models,
    }
}

/// The request setting `choice` in `session`, as `via` says, if it can.
fn request_for(
    via: &Via,
    choice: &SettingChoice,
    session: &AgentSessionId,
) -> Option<(&'static str, Value)> {
    match (via, &choice.value) {
        (Via::Options(options), value) => {
            let option = options.iter().find(|option| option.kind == choice.kind)?;
            let mut params = json!({ "sessionId": session.0, "configId": option.id });
            match (option.takes, value) {
                (Takes::Choice, SettingValue::Choice(id)) => params["value"] = json!(id),
                (Takes::Boolean, SettingValue::Switch(on)) => {
                    params["type"] = json!("boolean");
                    params["value"] = json!(on);
                }
                (Takes::OnOff, SettingValue::Switch(on)) => {
                    params["value"] = json!(if *on { "on" } else { "off" });
                }
                _ => return None,
            }
            Some(("session/set_config_option", params))
        }
        (Via::Models, SettingValue::Choice(id)) if choice.kind == SettingKind::Model => Some((
            "session/set_model",
            json!({ "sessionId": session.0, "modelId": id }),
        )),
        _ => None,
    }
}

/// Whether `offered` has `choice`: its setting, with a choice of that id,
/// or a switch for a switch. Matched exactly, by the agent's own ids. What
/// the agent says it uses counts, listed or not, as a model it fell back
/// to may not be.
fn offers(offered: &[Setting], choice: &SettingChoice) -> bool {
    (offered.iter())
        .find(|setting| setting.kind == choice.kind)
        .is_some_and(|setting| match (&setting.form, &choice.value) {
            (SettingForm::Choices(choices), SettingValue::Choice(id)) => {
                choices.iter().any(|each| each.id == *id)
                    || setting.current.as_ref() == Some(&choice.value)
            }
            (SettingForm::Switch, SettingValue::Switch(_)) => true,
            _ => false,
        })
}

/// What's chosen of `kind`, if anything.
fn chosen(settings: &Settings, kind: SettingKind) -> Option<&SettingValue> {
    (settings.chosen.iter())
        .find(|choice| choice.kind == kind)
        .map(|choice| &choice.value)
}

/// What the agent says `kind` is now, if it offers it and says.
fn current(settings: &Settings, kind: SettingKind) -> Option<&SettingValue> {
    (settings.offered.iter())
        .find(|setting| setting.kind == kind)
        .and_then(|setting| setting.current.as_ref())
}

/// Chooses `choice`, in place of any other of its kind: the chosen are
/// kept in the order settings are sent in.
fn choose(settings: &mut Settings, choice: SettingChoice) {
    settings.chosen.retain(|each| each.kind != choice.kind);
    settings.chosen.push(choice);
    settings.chosen.sort_by_key(|each| each.kind);
}

/// Marks `kind` as chosen for the chat itself.
fn make_own(settings: &mut Settings, kind: SettingKind) {
    if !settings.own.contains(&kind) {
        settings.own.push(kind);
        settings.own.sort_unstable();
    }
}

/// Whether CAF can send `choice` exactly: a choice's id is kept to what an
/// agent's would be.
pub(super) fn sendable(choice: &SettingChoice) -> bool {
    match &choice.value {
        SettingValue::Choice(id) => keepable(id),
        _ => true,
    }
}

/// After the agent answered setting `choice` without saying what it is
/// now, it's taken as `choice`, so the change never looks waiting for
/// ever: whatever it was before, if `always`, as the answer listed
/// nothing; only if the list it gave says nothing of it, if not.
fn as_asked(settings: &mut Settings, choice: &SettingChoice, always: bool) {
    let setting = (settings.offered.iter_mut()).find(|setting| setting.kind == choice.kind);
    if let Some(setting) = setting
        && (always || setting.current.is_none())
    {
        setting.current = Some(choice.value.clone());
    }
}

/// The choice of `kind` goes back to what the agent says it is, or goes,
/// if it doesn't say.
fn fall_back(settings: &mut Settings, kind: SettingKind) {
    match current(settings, kind).cloned() {
        Some(value) => choose(settings, SettingChoice { kind, value }),
        None => settings.chosen.retain(|each| each.kind != kind),
    }
}

/// Whether CAF keeps `id`, an agent's id for a setting or a choice: it's
/// sent back exactly, so one too long, or with control characters, is
/// skipped rather than changed.
fn keepable(id: &str) -> bool {
    !id.is_empty() && id.len() <= MOST_ID && !id.chars().any(char::is_control)
}
