//! Every variant of every enum CAF sends a viewer, by name: for tests
//! that must hold one of each, such as the v1 sample and the apply
//! fixtures.
//!
//! Each enum's `match` has no wildcard, so a new variant doesn't compile
//! until it's named here, and then the tests that hold one of each fail
//! until they do. [`Seen`] walks a value and notes every variant in it,
//! however deep.

use crate::settings::{KnownSettings, Setting, SettingForm, SettingKind, SettingValue, Settings};
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::agent::{AgentStatus, CannotInstall, InstallFailure, InstallState, SignInState};
use crate::change::{Change, Mismatch};
use crate::chat::{
    Chat, ChatState, FailureReason, Incompatible, Item, ItemKind, Lost, Note, Part, Remedy,
    Starter, ToolCall, ToolKind, ToolStatus, Turn, TurnLife, UserMessage,
};
use crate::core::{Refused, SessionChoice};
use crate::install::SignedInAs;
use crate::question::{Answer, OptionKind, Question, QuestionKind, SlowChoice};
use crate::time::{End, Window};
use crate::viewer::{AgentAction, FromViewer, Outcome, ToViewer};

/// An enum whose variants are named.
pub(crate) trait Kinds: Serialize {
    /// The enum's name.
    const ENUM: &'static str;
    /// Every variant's name, as serde writes it in `"type"`.
    const ALL: &'static [&'static str];
    /// This value's variant.
    fn kind(&self) -> &'static str;
}

/// Names an enum's variants: `kinds!(Enum { "Name" => Enum::Name, … })`.
macro_rules! kinds {
    ($ty:ident { $($name:literal => $pat:pat),* $(,)? }) => {
        impl Kinds for $ty {
            const ENUM: &'static str = stringify!($ty);
            const ALL: &'static [&'static str] = &[$($name),*];
            fn kind(&self) -> &'static str {
                match self {
                    $($pat => $name,)*
                }
            }
        }
    };
}

kinds!(FromViewer {
    "Hello" => FromViewer::Hello { .. },
    "Watch" => FromViewer::Watch { .. },
    "Unwatch" => FromViewer::Unwatch { .. },
    "Open" => FromViewer::Open { .. },
    "Send" => FromViewer::Send { .. },
    "Stop" => FromViewer::Stop { .. },
    "Answer" => FromViewer::Answer { .. },
    "Close" => FromViewer::Close { .. },
    "Restore" => FromViewer::Restore { .. },
    "Agent" => FromViewer::Agent { .. },
    "Set" => FromViewer::Set { .. },
});

kinds!(AgentAction {
    "Install" => AgentAction::Install,
    "StopInstall" => AgentAction::StopInstall,
    "SignIn" => AgentAction::SignIn,
    "Paste" => AgentAction::Paste(_),
    "StopSignIn" => AgentAction::StopSignIn,
    "SignOut" => AgentAction::SignOut,
    "LearnSettings" => AgentAction::LearnSettings,
});

kinds!(ToViewer {
    "Welcome" => ToViewer::Welcome { .. },
    "NoCommonVersion" => ToViewer::NoCommonVersion { .. },
    "Chats" => ToViewer::Chats(_),
    "Picture" => ToViewer::Picture { .. },
    "Changes" => ToViewer::Changes { .. },
    "Ended" => ToViewer::Ended { .. },
    "Agents" => ToViewer::Agents(_),
    "Saved" => ToViewer::Saved(_),
    "Done" => ToViewer::Done { .. },
});

kinds!(InstallState {
    "Host" => InstallState::Host,
    "Unknown" => InstallState::Unknown,
    "Missing" => InstallState::Missing { .. },
    "Installing" => InstallState::Installing { .. },
    "Installed" => InstallState::Installed { .. },
    "Cannot" => InstallState::Cannot(_),
});

kinds!(CannotInstall {
    "Node" => CannotInstall::Node { .. },
    "Npm" => CannotInstall::Npm,
    "Platform" => CannotInstall::Platform,
    "NoRecipe" => CannotInstall::NoRecipe,
    "Failed" => CannotInstall::Failed(_),
});

kinds!(InstallFailure {
    "Offline" => InstallFailure::Offline,
    "Corrupt" => InstallFailure::Corrupt,
    "NoSpace" => InstallFailure::NoSpace,
    "Stopped" => InstallFailure::Stopped,
    "Other" => InstallFailure::Other(_),
});

kinds!(SignInState {
    "Unknown" => SignInState::Unknown,
    "SignedOut" => SignInState::SignedOut,
    "SigningIn" => SignInState::SigningIn(_),
    "SignedIn" => SignInState::SignedIn(_),
});

kinds!(SignedInAs {
    "Account" => SignedInAs::Account,
    "ApiKey" => SignedInAs::ApiKey,
    "Gateway" => SignedInAs::Gateway,
    "Unknown" => SignedInAs::Unknown,
});

kinds!(Outcome {
    "Opened" => Outcome::Opened(_),
    "Sent" => Outcome::Sent(_),
    "Done" => Outcome::Done,
    "Refused" => Outcome::Refused(_),
    "NotAllowed" => Outcome::NotAllowed,
    "Gone" => Outcome::Gone,
    "CannotRestore" => Outcome::CannotRestore(_),
});

kinds!(Refused {
    "NoSuchChat" => Refused::NoSuchChat,
    "Closed" => Refused::Closed,
    "NoSuchAgent" => Refused::NoSuchAgent,
    "RelativeFolder" => Refused::RelativeFolder,
    "Unsendable" => Refused::Unsendable,
    "QueueFull" => Refused::QueueFull,
    "Unsupported" => Refused::Unsupported,
    "CommandNotAlone" => Refused::CommandNotAlone,
    "NoSuchQuestion" => Refused::NoSuchQuestion,
    "WrongAnswer" => Refused::WrongAnswer,
    "NothingToStop" => Refused::NothingToStop,
    "NotInstallable" => Refused::NotInstallable,
    "AlreadyInstalled" => Refused::AlreadyInstalled,
    "CannotSignIn" => Refused::CannotSignIn,
    "NotSigningIn" => Refused::NotSigningIn,
    "NotSaved" => Refused::NotSaved,
    "Loaded" => Refused::Loaded,
    "BadShelf" => Refused::BadShelf,
    "OwnTools" => Refused::OwnTools,
    "NoSuchSetting" => Refused::NoSuchSetting,
});

kinds!(SessionChoice {
    "New" => SessionChoice::New,
    "Continue" => SessionChoice::Continue(_),
});

kinds!(Answer {
    "Permission" => Answer::Permission(_),
    "SlowToOpen" => Answer::SlowToOpen(_),
});

kinds!(SlowChoice {
    "KeepWaiting" => SlowChoice::KeepWaiting,
    "Retry" => SlowChoice::Retry,
    "Stop" => SlowChoice::Stop,
});

kinds!(Part {
    "Text" => Part::Text(_),
});

kinds!(Change {
    "State" => Change::State(_),
    "TurnAdded" => Change::TurnAdded(_),
    "TurnInserted" => Change::TurnInserted { .. },
    "TurnLife" => Change::TurnLife { .. },
    "ItemAdded" => Change::ItemAdded { .. },
    "Appended" => Change::Appended { .. },
    "ToolChanged" => Change::ToolChanged { .. },
    "ItemEnded" => Change::ItemEnded { .. },
    "Usage" => Change::Usage(_),
    "QuestionAsked" => Change::QuestionAsked(_),
    "QuestionGone" => Change::QuestionGone(_),
    "Settings" => Change::Settings(_),
    "TurnModel" => Change::TurnModel { .. },
});

kinds!(Mismatch {
    "NoTurn" => Mismatch::NoTurn(_),
    "NoItem" => Mismatch::NoItem(_),
    "NotText" => Mismatch::NotText(_),
    "NotTool" => Mismatch::NotTool(_),
    "NoQuestion" => Mismatch::NoQuestion(_),
});

kinds!(ChatState {
    "Opening" => ChatState::Opening,
    "Open" => ChatState::Open { .. },
    "Lost" => ChatState::Lost { .. },
    "Closed" => ChatState::Closed,
    "WaitingForAgent" => ChatState::WaitingForAgent,
    "Idle" => ChatState::Idle,
});

kinds!(Lost {
    "CouldNotStart" => Lost::CouldNotStart { .. },
    "AgentStopped" => Lost::AgentStopped { .. },
    "Restarted" => Lost::Restarted,
    "SignInNeeded" => Lost::SignInNeeded,
    "Refused" => Lost::Refused { .. },
    "Stopped" => Lost::Stopped,
    "Incompatible" => Lost::Incompatible(_),
    "InUse" => Lost::InUse,
    "ShutDown" => Lost::ShutDown,
});

kinds!(Incompatible {
    "Protocol" => Incompatible::Protocol { .. },
    "HttpMcp" => Incompatible::HttpMcp,
    "BadAnswer" => Incompatible::BadAnswer,
    "OwnTools" => Incompatible::OwnTools,
});

kinds!(Starter {
    "User" => Starter::User(_),
    "Agent" => Starter::Agent,
    "OtherChat" => Starter::OtherChat { .. },
    "Notice" => Starter::Notice(_),
    "Caf" => Starter::Caf,
});

kinds!(TurnLife {
    "Waiting" => TurnLife::Waiting,
    "Running" => TurnLife::Running,
    "Stopping" => TurnLife::Stopping,
    "Finished" => TurnLife::Finished,
    "Stopped" => TurnLife::Stopped,
    "Failed" => TurnLife::Failed(_),
    "CutOff" => TurnLife::CutOff,
});

kinds!(FailureReason {
    "TooLong" => FailureReason::TooLong,
    "TooManySteps" => FailureReason::TooManySteps,
    "Refused" => FailureReason::Refused,
    "SignInNeeded" => FailureReason::SignInNeeded,
    "AccessDenied" => FailureReason::AccessDenied,
    "BadRequest" => FailureReason::BadRequest,
    "UsageLimit" => FailureReason::UsageLimit,
    "ProviderError" => FailureReason::ProviderError,
    "AgentStopped" => FailureReason::AgentStopped,
    "DidNotStop" => FailureReason::DidNotStop,
    "BadAnswer" => FailureReason::BadAnswer,
    "Other" => FailureReason::Other,
    "SettingNotApplied" => FailureReason::SettingNotApplied,
});

kinds!(Remedy {
    "SendAgain" => Remedy::SendAgain,
    "SignIn" => Remedy::SignIn,
    "NewChat" => Remedy::NewChat,
});

kinds!(End {
    "Open" => End::Open,
    "At" => End::At(_),
    "Unknown" => End::Unknown,
});

/// `App` isn't named: with no app items of its own, as on the wire CAF
/// pins, a chat can't hold one.
impl Kinds for ItemKind {
    const ENUM: &'static str = "ItemKind";
    const ALL: &'static [&'static str] =
        &["Words", "Thinking", "Tool", "Compacted", "Note", "Unknown"];
    fn kind(&self) -> &'static str {
        match self {
            ItemKind::Words(_) => "Words",
            ItemKind::Thinking(_) => "Thinking",
            ItemKind::Tool(_) => "Tool",
            ItemKind::Compacted(_) => "Compacted",
            ItemKind::Note(_) => "Note",
            ItemKind::App(app) => match *app {},
            ItemKind::Unknown(_) => "Unknown",
        }
    }
}

kinds!(ToolKind {
    "Read" => ToolKind::Read,
    "Edit" => ToolKind::Edit,
    "Delete" => ToolKind::Delete,
    "Move" => ToolKind::Move,
    "Search" => ToolKind::Search,
    "Execute" => ToolKind::Execute,
    "Think" => ToolKind::Think,
    "Fetch" => ToolKind::Fetch,
    "Other" => ToolKind::Other,
});

kinds!(ToolStatus {
    "Pending" => ToolStatus::Pending,
    "Running" => ToolStatus::Running,
    "Done" => ToolStatus::Done,
    "Failed" => ToolStatus::Failed,
});

kinds!(Note {
    "SessionGone" => Note::SessionGone,
    "CannotContinue" => Note::CannotContinue,
    "SessionStuck" => Note::SessionStuck,
    "NotInstalled" => Note::NotInstalled,
    "InstallFailed" => Note::InstallFailed,
    "SignedIn" => Note::SignedIn,
    "AgentChanged" => Note::AgentChanged(_),
    "NotOffered" => Note::NotOffered(_),
});

kinds!(QuestionKind {
    "Permission" => QuestionKind::Permission(_),
    "SlowToOpen" => QuestionKind::SlowToOpen(_),
});

kinds!(OptionKind {
    "AllowOnce" => OptionKind::AllowOnce,
    "AllowAlways" => OptionKind::AllowAlways,
    "RejectOnce" => OptionKind::RejectOnce,
    "RejectAlways" => OptionKind::RejectAlways,
    "Other" => OptionKind::Other,
});

/// The variants seen so far, by enum.
#[derive(Debug, Default)]
pub(crate) struct Seen(BTreeMap<&'static str, BTreeSet<&'static str>>);

impl Seen {
    /// Notes `value`'s variant, once its name is checked against how
    /// serde writes it.
    pub(crate) fn mark<T: Kinds>(&mut self, value: &T) {
        let kind = value.kind();
        let json = serde_json::to_value(value).expect("it writes as JSON");
        assert_eq!(json["type"], kind, "{} is written as {json}", T::ENUM);
        self.0.entry(T::ENUM).or_default().insert(kind);
    }

    /// The variants of `T` not seen, if any, as a message.
    pub(crate) fn missing<T: Kinds>(&self) -> Option<String> {
        let seen = self.0.get(T::ENUM);
        let missing: Vec<_> = (T::ALL.iter())
            .filter(|name| !seen.is_some_and(|seen| seen.contains(*name)))
            .collect();
        (!missing.is_empty()).then(|| format!("{}: {missing:?}", T::ENUM))
    }

    /// The variants of every enum on the viewer wire not seen, as one
    /// message per enum.
    pub(crate) fn missing_on_the_wire(&self) -> Vec<String> {
        [
            self.missing::<FromViewer>(),
            self.missing::<AgentAction>(),
            self.missing::<ToViewer>(),
            self.missing::<Outcome>(),
            self.missing::<Refused>(),
            self.missing::<SessionChoice>(),
            self.missing::<Answer>(),
            self.missing::<SlowChoice>(),
            self.missing::<Part>(),
            self.missing::<Change>(),
            self.missing::<ChatState>(),
            self.missing::<Lost>(),
            self.missing::<Incompatible>(),
            self.missing::<Starter>(),
            self.missing::<TurnLife>(),
            self.missing::<FailureReason>(),
            self.missing::<Remedy>(),
            self.missing::<End>(),
            self.missing::<ItemKind>(),
            self.missing::<ToolKind>(),
            self.missing::<ToolStatus>(),
            self.missing::<Note>(),
            self.missing::<QuestionKind>(),
            self.missing::<OptionKind>(),
            self.missing::<InstallState>(),
            self.missing::<CannotInstall>(),
            self.missing::<InstallFailure>(),
            self.missing::<SignInState>(),
            self.missing::<SignedInAs>(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// A message from a viewer, and everything in it.
    pub(crate) fn message_from(&mut self, message: &FromViewer) {
        self.mark(message);
        match message {
            FromViewer::Open { open, .. } => self.mark(&open.session),
            FromViewer::Send { message, .. } => self.message(message),
            FromViewer::Answer { answer, .. } => {
                self.mark(answer);
                if let Answer::SlowToOpen(choice) = answer {
                    self.mark(choice);
                }
            }
            FromViewer::Agent { action, .. } => self.mark(action),
            _ => {}
        }
    }

    /// A message to a viewer, and everything in it.
    pub(crate) fn message_to(&mut self, message: &ToViewer) {
        self.mark(message);
        match message {
            ToViewer::Picture { chat, .. } => self.chat(chat),
            ToViewer::Changes { changes, .. } => {
                changes.iter().for_each(|each| self.change(&each.change));
            }
            ToViewer::Done { outcome, .. } => {
                self.mark(outcome);
                if let Outcome::Refused(refused) = outcome {
                    self.mark(refused);
                }
            }
            ToViewer::Agents(agents) => agents.iter().for_each(|each| self.agent(each)),
            _ => {}
        }
    }

    /// An agent's status, and everything in it.
    fn agent(&mut self, agent: &AgentStatus) {
        self.mark(&agent.install);
        if let InstallState::Cannot(why) = &agent.install {
            self.mark(why);
            if let CannotInstall::Failed(failure) = why {
                self.mark(failure);
            }
        }
        self.mark(&agent.sign_in);
        if let SignInState::SignedIn(how) = &agent.sign_in {
            self.mark(how);
        }
        self.mark(&agent.settings);
        if let KnownSettings::Known(offered) = &agent.settings {
            offered.iter().for_each(|setting| self.offered(setting));
        }
    }

    /// A change, and everything in it.
    pub(crate) fn change(&mut self, change: &Change) {
        self.mark(change);
        match change {
            Change::State(state) => self.state(state),
            Change::Settings(settings) => self.settings(settings),
            Change::TurnAdded(turn) | Change::TurnInserted { turn, .. } => self.turn(turn),
            Change::TurnLife { life, end, .. } => {
                self.life(life);
                self.mark(end);
            }
            Change::ItemAdded { item, .. } => self.item(item),
            Change::ToolChanged { tool, .. } => self.tool(tool),
            Change::ItemEnded { end, .. } => self.mark(end),
            Change::QuestionAsked(question) => self.question(question),
            Change::Appended { .. }
            | Change::TurnModel { .. }
            | Change::Usage(_)
            | Change::QuestionGone(_) => {}
        }
    }

    /// A chat, and everything in it.
    pub(crate) fn chat(&mut self, chat: &Chat) {
        self.state(&chat.state);
        chat.turns.iter().for_each(|turn| self.turn(turn));
        (chat.questions.iter()).for_each(|question| self.question(question));
        self.settings(&chat.settings);
    }

    /// A chat's settings, and everything in them.
    fn settings(&mut self, settings: &Settings) {
        settings
            .offered
            .iter()
            .for_each(|setting| self.offered(setting));
        for choice in &settings.chosen {
            self.mark(&choice.kind);
            self.mark(&choice.value);
        }
    }

    /// A setting an agent offers.
    fn offered(&mut self, setting: &Setting) {
        self.mark(&setting.kind);
        self.mark(&setting.form);
        if let Some(value) = &setting.current {
            self.mark(value);
        }
    }

    fn state(&mut self, state: &ChatState) {
        self.mark(state);
        if let ChatState::Lost { reason } = state {
            self.mark(reason);
            if let Lost::Incompatible(why) = reason {
                self.mark(why);
            }
        }
    }

    fn turn(&mut self, turn: &Turn) {
        self.mark(&turn.starter);
        match &turn.starter {
            Starter::User(message) | Starter::OtherChat { message, .. } => self.message(message),
            _ => {}
        }
        self.life(&turn.life);
        self.window(&turn.window);
        turn.items.iter().for_each(|item| self.item(item));
    }

    fn life(&mut self, life: &TurnLife) {
        self.mark(life);
        if let TurnLife::Failed(failure) = life {
            self.mark(&failure.reason);
            if let Some(remedy) = &failure.remedy {
                self.mark(remedy);
            }
        }
    }

    fn message(&mut self, message: &UserMessage) {
        message.parts.iter().for_each(|part| self.mark(part));
    }

    fn window(&mut self, window: &Window) {
        self.mark(&window.end);
    }

    fn item(&mut self, item: &Item) {
        self.window(&item.window);
        self.mark(&item.kind);
        match &item.kind {
            ItemKind::Tool(tool) => self.tool(tool),
            ItemKind::Note(note) => self.mark(note),
            _ => {}
        }
    }

    fn tool(&mut self, tool: &ToolCall) {
        self.mark(&tool.kind);
        self.mark(&tool.status);
    }

    fn question(&mut self, question: &Question) {
        self.window(&question.window);
        self.mark(&question.kind);
        if let QuestionKind::Permission(permission) = &question.kind {
            (permission.options.iter()).for_each(|option| self.mark(&option.kind));
        }
    }
}

kinds!(SettingKind {
    "Model" => SettingKind::Model,
    "Effort" => SettingKind::Effort,
    "Fast" => SettingKind::Fast,
});

kinds!(SettingForm {
    "Choices" => SettingForm::Choices(_),
    "Switch" => SettingForm::Switch,
});

kinds!(SettingValue {
    "Choice" => SettingValue::Choice(_),
    "Switch" => SettingValue::Switch(_),
});

kinds!(KnownSettings {
    "Unknown" => KnownSettings::Unknown,
    "Learning" => KnownSettings::Learning,
    "Known" => KnownSettings::Known(_),
    "Failed" => KnownSettings::Failed(_),
});
