//! Installing agents and signing in to them: where each agent stands, the
//! host's commands for it, and what the setup driver reports.
//!
//! Each agent's recipe (see [`install`](crate::install)) says what to do;
//! the setup driver does it. An agent CAF installs starts only once it's
//! installed, and any agent waits while a sign-in to it, started when it
//! was signed out, is under way: until then its chats wait
//! ([`ChatState::WaitingForAgent`]), with their messages.
//!
//! Installs are named after the agent's profile, never after what the host
//! calls it, so agents of one profile share one install, here and in every
//! other CAF program on the machine.
//!
//! Nothing a sign-in or status command prints reaches the core but the
//! link, the code and the fields its recipe names, and of the adapter's
//! own `_auth/status_update` only its `kind` is read: no email is ever
//! kept.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::clean::agent_text;
use super::state::{Job, Machine, Standing, Work};
use super::{Core, Refused, report};
use crate::agent::{
    CannotInstall, EnvChange, InstallFailure, InstallState, Launch, SignInLink, SignInState,
    SpawnCommand,
};
use crate::chat::AppData;
use crate::chat::{AppItem, ChatState, Failure, FailureReason, Note, Remedy, Starter, TurnLife};
use crate::directive::{Directive, Level, SetupDirective};
use crate::event::SetupEvent;
use crate::ids::{AgentName, ChatId, Ticket};
use crate::install::{Bundled, EnvValue, Install, Program, Recipe, SignedInAs};
use crate::profiles::Checked;
use crate::settings::KnownSettings;
use crate::time::{Now, Wall};

/// The longest link kept, in bytes: a longer one is refused.
const MOST_LINK: usize = 4096;

/// The longest code to type kept, in bytes: a longer one is dropped, as
/// cut short it would be the wrong code.
const MOST_CODE: usize = 64;

/// The longest paste passed on, in bytes.
pub(super) const MOST_PASTE: usize = 4096;

/// The longest reason an install failed kept, in bytes.
const MOST_REASON: usize = 300;

/// How long an older version must have gone unused before it's removed:
/// another CAF program may pin it, and use it now and then.
const KEEP_UNUSED: Duration = Duration::from_hours(14 * 24);

impl<A: AppItem, D: AppData> Core<A, D> {
    /// Asks the setup driver what's installed and what the machine has.
    /// Only the answer to the latest survey counts.
    pub(super) fn survey(&mut self, out: &mut Vec<Directive<A, D>>) {
        let ticket = Ticket(self.ids.take());
        self.setup.survey = Some(ticket);
        out.push(Directive::Setup(SetupDirective::Survey { ticket }));
    }

    /// Installs `agent`.
    pub(super) fn install_agent(
        &mut self,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        let spec = self.agents.get(agent).ok_or(Refused::NoSuchAgent)?;
        let recipe = match (&spec.launch, self.recipes.get(agent)) {
            (Launch::Installed, Some(recipe)) => recipe,
            _ => return Err(Refused::NotInstallable),
        };
        let folder = self.folder_of(agent).ok_or(Refused::NotInstallable)?;
        let standing = self.setup.agents.get(agent).ok_or(Refused::NoSuchAgent)?;
        match &standing.install {
            InstallState::Installed { .. } => return Err(Refused::AlreadyInstalled),
            InstallState::Installing { .. } => return Ok(()),
            InstallState::Unknown
            | InstallState::Missing { .. }
            | InstallState::Cannot(CannotInstall::Failed(_)) => {}
            _ => return Err(Refused::NotInstallable),
        }
        let (install, patch, size) = (recipe.install.clone(), recipe.patch.clone(), recipe.size);
        let private = self.private_folder(agent);
        let earlier = self.earlier_level(agent);
        // Another agent of the same profile is installing it already.
        let started = self.install_job(&folder).is_some();
        if let (false, Some(from)) = (started, earlier) {
            // A new patch level of an install that's there: nothing to
            // download.
            self.prepare(agent, &folder, from, out);
        } else if !started {
            // Starting it later needs the folder installs live in.
            if self.setup.machine.is_none() && self.setup.survey.is_none() {
                self.survey(out);
            }
            let ticket = self.job(
                agent,
                Work::Install {
                    folder: folder.clone(),
                    from: None,
                },
            );
            out.push(Directive::Setup(SetupDirective::Install {
                ticket,
                folder: folder.clone(),
                install,
                patch,
                private,
                size,
            }));
        }
        for each in self.sharing(&folder) {
            if !matches!(
                self.install_state(&each),
                Some(InstallState::Installing { .. } | InstallState::Installed { .. })
            ) {
                self.set_install(&each, InstallState::Installing { done: 0, size });
            }
        }
        Ok(())
    }

    /// Stops installing `agent`: chats waiting for it, or for another agent
    /// of its profile, have their waiting messages stopped, and go on
    /// waiting for it.
    pub(super) fn stop_installing(
        &mut self,
        now: Now,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        if !self.agents.contains_key(agent) {
            return Err(Refused::NoSuchAgent);
        }
        let folder = self.folder_of(agent).ok_or(Refused::NothingToStop)?;
        let ticket = self.install_job(&folder).ok_or(Refused::NothingToStop)?;
        self.stop_job(ticket, out);
        let size = self.recipes.get(agent).map_or(0, |recipe| recipe.size);
        for each in self.sharing(&folder) {
            self.set_install(&each, InstallState::Missing { size });
            self.stop_waiting_messages(now, &each, Note::NotInstalled);
        }
        Ok(())
    }

    /// Starts signing in to `agent`.
    pub(super) fn start_sign_in(
        &mut self,
        now: Now,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        if !self.agents.contains_key(agent) {
            return Err(Refused::NoSuchAgent);
        }
        if self.signing_in(agent).is_some() {
            return Ok(());
        }
        let (command, link, code_after, paste) = {
            let sign_in = (self.recipes.get(agent))
                .and_then(|recipe| recipe.sign_in.as_ref())
                .ok_or(Refused::CannotSignIn)?;
            let command = (self.resolve(agent, &sign_in.command)).ok_or(Refused::CannotSignIn)?;
            (
                command,
                sign_in.link.clone(),
                sign_in.code_after.clone(),
                sign_in.paste,
            )
        };
        let before = self.sign_in_state(agent);
        let ticket = self.job(agent, Work::SignIn { before });
        if let Some(job) = self.setup.jobs.get_mut(&ticket) {
            job.deadline = Some(now.mono.after(self.host.timers.sign_in));
        }
        out.push(Directive::Setup(SetupDirective::SignIn {
            ticket,
            command,
            link,
            code_after,
            paste,
        }));
        let signing_in = SignInState::SigningIn(SignInLink {
            url: None,
            code: None,
            paste,
        });
        for each in self.sign_in_group(agent) {
            if let Some(standing) = self.setup.agents.get_mut(&each) {
                standing.sign_in = signing_in.clone();
            }
        }
        Ok(())
    }

    /// Gives `agent`'s sign-in what the user pasted.
    pub(super) fn paste_to(
        &mut self,
        agent: &AgentName,
        text: &str,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        if !self.agents.contains_key(agent) {
            return Err(Refused::NoSuchAgent);
        }
        let ticket = self.signing_in(agent).ok_or(Refused::NotSigningIn)?;
        let pastes = matches!(
            self.sign_in_state(agent),
            SignInState::SigningIn(SignInLink { paste: true, .. })
        );
        let text = text.trim();
        // It goes on one line of the command's input: a line break would
        // be a second answer.
        if !pastes || text.len() > MOST_PASTE || text.chars().any(char::is_control) {
            return Err(Refused::Unsupported);
        }
        out.push(Directive::Setup(SetupDirective::Paste {
            ticket,
            text: text.to_owned(),
        }));
        Ok(())
    }

    /// Stops `agent`'s sign-in: its sign-in state goes back to what it was.
    pub(super) fn stop_signing_in(
        &mut self,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        if !self.agents.contains_key(agent) {
            return Err(Refused::NoSuchAgent);
        }
        let ticket = self.signing_in(agent).ok_or(Refused::NotSigningIn)?;
        self.stop_job(ticket, out);
        Ok(())
    }

    /// Signs out of `agent`, stopping any sign-in to it first.
    pub(super) fn start_sign_out(
        &mut self,
        agent: &AgentName,
        out: &mut Vec<Directive<A, D>>,
    ) -> Result<(), Refused> {
        if !self.agents.contains_key(agent) {
            return Err(Refused::NoSuchAgent);
        }
        let command = (self.recipes.get(agent))
            .and_then(|recipe| recipe.sign_out.as_ref())
            .and_then(|sign_out| self.resolve(agent, sign_out))
            .ok_or(Refused::CannotSignIn)?;
        if let Some(ticket) = self.signing_in(agent) {
            self.stop_job(ticket, out);
        }
        let ticket = self.job(agent, Work::SignOut);
        out.push(Directive::Setup(SetupDirective::Run { ticket, command }));
        Ok(())
    }

    /// Where every agent stands, in the order the host offered them.
    pub(super) fn statuses(&self) -> Vec<crate::agent::AgentStatus> {
        (self.setup.order.iter())
            .filter_map(|name| {
                let standing = self.setup.agents.get(name)?;
                Some(crate::agent::AgentStatus {
                    name: name.clone(),
                    install: standing.install.clone(),
                    sign_in: standing.sign_in.clone(),
                    can_sign_in: self.signing_in(name).is_some() || self.can_sign_in(name),
                    settings: standing.settings.clone(),
                    limits: self.agent_limits(name),
                    own_tools_off: standing.strips.unwrap_or_else(|| {
                        (self.agents.get(name)).is_some_and(|agent| agent.profile.strips())
                    }),
                })
            })
            .collect()
    }

    /// Takes in the setup driver's report on `ticket`.
    pub(super) fn setup_event(
        &mut self,
        now: Now,
        ticket: Ticket,
        event: SetupEvent,
        out: &mut Vec<Directive<A, D>>,
    ) {
        if self.setup.survey == Some(ticket) || matches!(event, SetupEvent::Surveyed { .. }) {
            return self.survey_said(ticket, event, out);
        }
        let Some(job) = self.setup.jobs.get(&ticket) else {
            return report(
                out,
                Level::Debug,
                format!(
                    "ignored a report on setup work {} CAF doesn't know",
                    ticket.0
                ),
            );
        };
        let agent = job.agent.clone();
        let stopped = job.stopped;
        match (job.work.clone(), event) {
            (Work::Install { folder, .. }, SetupEvent::Progress { done }) if !stopped => {
                let done = done.min(super::updates::MOST_EXACT);
                for each in self.sharing(&folder) {
                    if let Some(InstallState::Installing { size, .. }) = self.install_state(&each) {
                        self.set_install(&each, InstallState::Installing { done, size });
                    }
                }
            }
            (Work::Install { folder, .. }, SetupEvent::Installed) => {
                // Even one stopped too late: the folder is there.
                self.setup.jobs.remove(&ticket);
                self.installed(&folder, out);
            }
            (
                Work::Install {
                    folder,
                    from: Some(from),
                },
                event @ (SetupEvent::InstallFailed(_) | SetupEvent::Failed(_)),
            ) => {
                self.setup.jobs.remove(&ticket);
                if !stopped {
                    self.prepare_failed(&folder, &from, &event, out);
                }
            }
            (Work::Install { folder, .. }, SetupEvent::InstallFailed(why)) => {
                self.setup.jobs.remove(&ticket);
                if !stopped {
                    self.install_failed(now, &folder, why, out);
                }
            }
            (Work::Install { folder, .. }, SetupEvent::Failed(why)) => {
                self.setup.jobs.remove(&ticket);
                if !stopped {
                    let why = InstallFailure::Other(agent_text(&why, MOST_REASON));
                    self.install_failed(now, &folder, why, out);
                }
            }
            (Work::Remove { folder }, SetupEvent::Removed { removed }) => {
                self.setup.jobs.remove(&ticket);
                if removed && let Some(machine) = self.setup.machine.as_mut() {
                    machine.installed.remove(&folder);
                    machine.used.remove(&folder);
                }
                let what = if removed {
                    "removed"
                } else {
                    "kept, as it's in use,"
                };
                report(out, Level::Info, format!("{what} an old install, {folder}"));
            }
            (Work::SignIn { .. }, SetupEvent::Link(url)) if !stopped => {
                self.link(ticket, &agent, &url, out);
            }
            (Work::SignIn { .. }, SetupEvent::Code(code)) if !stopped => {
                self.code(&agent, &code, out);
            }
            (Work::SignIn { before }, SetupEvent::Ended { ok }) => {
                self.setup.jobs.remove(&ticket);
                if ok {
                    // Perhaps to another account, even if stopped as it
                    // finished.
                    self.account_changed(now, &agent, out);
                }
                if !stopped && ok {
                    self.signed_in(now, &agent, out);
                } else if ok {
                    // Stopped as it finished: whether it's signed in now
                    // is the status command's to say.
                    self.forget_status(&agent);
                    self.check_status(&agent, out);
                } else if !stopped {
                    report(
                        out,
                        Level::Info,
                        format!("signing in to {} failed", agent.0),
                    );
                    self.set_sign_in(&agent, before);
                }
            }
            (Work::SignIn { before }, SetupEvent::Failed(_)) => {
                self.setup.jobs.remove(&ticket);
                if !stopped {
                    // Never why: it may be what the command printed.
                    report(
                        out,
                        Level::Warn,
                        format!("{}'s sign-in command couldn't run", agent.0),
                    );
                    self.set_sign_in(&agent, before);
                }
            }
            (Work::SignOut, SetupEvent::Ended { ok }) => {
                self.setup.jobs.remove(&ticket);
                // What a status command already running reads may be from
                // before.
                self.forget_status(&agent);
                if ok {
                    self.set_sign_in(&agent, SignInState::SignedOut);
                    self.account_changed(now, &agent, out);
                } else {
                    report(
                        out,
                        Level::Info,
                        format!("signing out of {} failed", agent.0),
                    );
                }
            }
            (Work::Status, SetupEvent::Status(_)) if stopped => {
                self.setup.jobs.remove(&ticket);
            }
            (Work::Status, SetupEvent::Status(how)) => {
                self.setup.jobs.remove(&ticket);
                let state = how.map_or(SignInState::SignedOut, SignInState::SignedIn);
                self.found_signed_in(now, &agent, state, out);
            }
            (
                work @ (Work::Remove { .. } | Work::SignOut | Work::Status),
                SetupEvent::Failed(_),
            ) => {
                self.setup.jobs.remove(&ticket);
                let what = match work {
                    Work::Remove { .. } => "removing an old install of",
                    Work::SignOut => "the sign-out command of",
                    _ => "the status command of",
                };
                report(out, Level::Warn, format!("{what} {} couldn't run", agent.0));
            }
            (Work::Status, SetupEvent::Ended { .. }) => {
                self.setup.jobs.remove(&ticket);
                report(
                    out,
                    Level::Debug,
                    format!("{}'s status command said nothing CAF reads", agent.0),
                );
            }
            (_, event) => report(
                out,
                Level::Debug,
                format!(
                    "ignored a report on setup work {}: {}",
                    ticket.0,
                    setup_event_kind(&event)
                ),
            ),
        }
    }

    /// A survey's report: what it found, if it's the one waited for, or
    /// that it couldn't look.
    fn survey_said(&mut self, ticket: Ticket, event: SetupEvent, out: &mut Vec<Directive<A, D>>) {
        if self.setup.survey != Some(ticket) {
            return report(
                out,
                Level::Debug,
                "ignored a survey CAF no longer waits for".to_owned(),
            );
        }
        match event {
            SetupEvent::Surveyed {
                root,
                installed,
                node,
                npm,
                platform,
                used,
            } => {
                let machine = Machine {
                    root,
                    installed: installed.into_iter().collect(),
                    node,
                    npm,
                    platform,
                    used: used.into_iter().collect(),
                };
                self.surveyed(machine, out);
            }
            SetupEvent::Failed(why) => {
                // Asked again, it may answer.
                self.setup.survey = None;
                let why = agent_text(&why, MOST_REASON);
                report(
                    out,
                    Level::Warn,
                    format!("couldn't look at what's installed: {why}"),
                );
            }
            event => report(
                out,
                Level::Debug,
                format!("ignored a survey's {}", setup_event_kind(&event)),
            ),
        }
    }

    /// A survey answered: each agent CAF installs is installed, can be, or
    /// can't be, and those that have a status command are asked.
    fn surveyed(&mut self, mut machine: Machine, out: &mut Vec<Directive<A, D>>) {
        machine.installed.append(&mut self.setup.fresh);
        self.setup.machine = Some(machine);
        self.setup.survey = None;
        for agent in self.setup.order.clone() {
            let Some(spec) = self.agents.get(&agent) else {
                continue;
            };
            if spec.launch != Launch::Installed
                || matches!(
                    self.install_state(&agent),
                    Some(InstallState::Installing { .. })
                )
            {
                continue;
            }
            let state = self.judge(&agent);
            self.set_install(&agent, state);
        }
        // A new patch level of an install that's there is made from it,
        // with nothing downloaded, and nobody asked.
        for agent in self.setup.order.clone() {
            let Some(folder) = self.folder_of(&agent) else {
                continue;
            };
            let waiting = !matches!(
                self.install_state(&agent),
                Some(InstallState::Installing { .. } | InstallState::Installed { .. })
            );
            if waiting
                && self.install_job(&folder).is_none()
                && let Some(from) = self.earlier_level(&agent)
            {
                self.prepare(&agent, &folder, from, out);
            }
        }
        for agent in self.setup.order.clone() {
            self.check_status(&agent, out);
        }
    }

    /// The complete install `agent`'s folder can be prepared from, with
    /// nothing downloaded, as the last survey found: of the same upstream
    /// version at a lower patch level, the highest, if its recipe patches
    /// it, it isn't installed, and preparing it hasn't failed.
    fn earlier_level(&self, agent: &AgentName) -> Option<String> {
        let spec = self.agents.get(agent)?;
        let (recipe, folder, slug) = (
            self.recipes.get(agent)?,
            self.folder_of(agent)?,
            self.slugs.get(agent)?,
        );
        let level = recipe.patch.as_ref()?.level;
        let machine = self.setup.machine.as_ref()?;
        if spec.launch != Launch::Installed
            || machine.installed.contains(&folder)
            || self.setup.unprepared.contains(&folder)
        {
            return None;
        }
        (machine.installed.iter())
            .filter(|each| is_folder_name(each))
            .filter_map(|each| Some((patch_level(slug, each, &recipe.version)?, each)))
            .filter(|&(earlier, _)| earlier < level)
            .max_by_key(|&(earlier, _)| earlier)
            .map(|(_, each)| each.clone())
    }

    /// Prepares `folder`, `agent`'s, from the earlier patch level `from`:
    /// every agent of its profile is installing it meanwhile.
    fn prepare(
        &mut self,
        agent: &AgentName,
        folder: &str,
        from: String,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some((patch, size)) =
            (self.recipes.get(agent)).and_then(|recipe| Some((recipe.patch.clone()?, recipe.size)))
        else {
            return;
        };
        report(
            out,
            Level::Info,
            format!("preparing {folder} from {from}, with nothing downloaded"),
        );
        let ticket = self.job(
            agent,
            Work::Install {
                folder: folder.to_owned(),
                from: Some(from.clone()),
            },
        );
        out.push(Directive::Setup(SetupDirective::Prepare {
            ticket,
            folder: folder.to_owned(),
            from,
            patch,
        }));
        for each in self.sharing(folder) {
            self.set_install(&each, InstallState::Installing { done: 0, size });
        }
    }

    /// Preparing `folder` from `from` failed: it's never tried again, and
    /// every agent of its profile reads as not installed, to be installed
    /// as any other is, with the user's leave. Messages waiting for it go
    /// on waiting.
    fn prepare_failed(
        &mut self,
        folder: &str,
        from: &str,
        event: &SetupEvent,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let why = match event {
            SetupEvent::InstallFailed(why) => format!("{why:?}"),
            SetupEvent::Failed(why) => why.clone(),
            other => setup_event_kind(other).to_owned(),
        };
        let why = agent_text(&why, MOST_REASON);
        report(
            out,
            Level::Warn,
            format!("preparing {folder} from {from} failed, so it's to be installed: {why}"),
        );
        self.setup.unprepared.insert(folder.to_owned());
        for agent in self.sharing(folder) {
            let size = self.recipes.get(&agent).map_or(0, |recipe| recipe.size);
            self.set_install(&agent, InstallState::Missing { size });
        }
    }

    /// Whether `agent`, which CAF installs, is installed, can be, or can't
    /// be, as the last survey found.
    fn judge(&self, agent: &AgentName) -> InstallState {
        let Some(machine) = &self.setup.machine else {
            return InstallState::Unknown;
        };
        let Some((recipe, folder)) = (self.recipes.get(agent)).zip(self.folder_of(agent)) else {
            return InstallState::Cannot(CannotInstall::NoRecipe);
        };
        if machine.installed.contains(&folder) {
            return InstallState::Installed {
                version: recipe.installed_version(),
            };
        }
        if let Install::Download(artifacts) = &recipe.install
            && !artifacts
                .iter()
                .any(|each| each.platform == machine.platform)
        {
            return InstallState::Cannot(CannotInstall::Platform);
        }
        if let Some(needs) = recipe.needs.node
            && machine.node.is_none_or(|has| has < needs)
        {
            return InstallState::Cannot(CannotInstall::Node {
                needs,
                has: machine.node,
            });
        }
        if recipe.needs.npm && !machine.npm {
            return InstallState::Cannot(CannotInstall::Npm);
        }
        match self.install_state(agent) {
            // Why the last try failed still holds: installing tries again.
            Some(failed @ InstallState::Cannot(CannotInstall::Failed(_))) => failed,
            _ => InstallState::Missing { size: recipe.size },
        }
    }

    /// `folder` is installed: every agent of its profile is, any other
    /// install of it under way is stopped, chats waiting start, and status
    /// commands can run.
    fn installed(&mut self, folder: &str, out: &mut Vec<Directive<A, D>>) {
        if self.setup.survey.is_some() {
            self.setup.fresh.insert(folder.to_owned());
        }
        if let Some(machine) = self.setup.machine.as_mut() {
            machine.installed.insert(folder.to_owned());
        }
        while let Some(ticket) = self.install_job(folder) {
            self.stop_job(ticket, out);
        }
        for agent in self.sharing(folder) {
            let Some(version) = self.recipes.get(&agent).map(Recipe::installed_version) else {
                continue;
            };
            self.set_install(&agent, InstallState::Installed { version });
            self.check_status(&agent, out);
        }
    }

    /// Installing `folder` failed: messages waiting for any agent of its
    /// profile are stopped.
    fn install_failed(
        &mut self,
        now: Now,
        folder: &str,
        why: InstallFailure,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let why = match why {
            InstallFailure::Other(text) => InstallFailure::Other(agent_text(&text, MOST_REASON)),
            InstallFailure::Patch(text) => InstallFailure::Patch(agent_text(&text, MOST_REASON)),
            other => other,
        };
        report(
            out,
            Level::Warn,
            format!("installing {folder} failed: {why:?}"),
        );
        for agent in self.sharing(folder) {
            self.set_install(
                &agent,
                InstallState::Cannot(CannotInstall::Failed(why.clone())),
            );
            self.stop_waiting_messages(now, &agent, Note::InstallFailed);
        }
    }

    /// The sign-in command gave its link: kept if it's to one of the
    /// recipe's sites, and otherwise the sign-in is stopped.
    fn link(
        &mut self,
        ticket: Ticket,
        agent: &AgentName,
        url: &str,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let sites = (self.recipes.get(agent))
            .and_then(|recipe| recipe.sign_in.as_ref())
            .map(|sign_in| sign_in.sites.clone())
            .unwrap_or_default();
        let host = https_host(url);
        let allowed = url.len() <= MOST_LINK
            && !url.chars().any(char::is_control)
            && host
                .as_ref()
                .is_some_and(|host| sites.iter().any(|site| site.eq_ignore_ascii_case(host)));
        if !allowed {
            // Never the whole link: it may carry a one-time secret.
            let host = host.map_or_else(
                || "a link that isn't https".to_owned(),
                |host| agent_text(&host, 100),
            );
            report(
                out,
                Level::Warn,
                format!(
                    "stopped signing in to {}: its link goes to {host}, not a site its profile names",
                    agent.0
                ),
            );
            return self.stop_job(ticket, out);
        }
        self.edit_link(agent, |link| link.url = Some(url.to_owned()));
    }

    /// The sign-in command gave a code to type on the site: kept only as
    /// it came, as one changed would be the wrong code.
    fn code(&mut self, agent: &AgentName, code: &str, out: &mut Vec<Directive<A, D>>) {
        let code = code.trim();
        if code.is_empty() {
            return;
        }
        if code.len() > MOST_CODE || code.chars().any(char::is_control) {
            return report(
                out,
                Level::Warn,
                format!(
                    "{}'s sign-in gave a code CAF can't show as it is: dropped",
                    agent.0
                ),
            );
        }
        self.edit_link(agent, |link| link.code = Some(code.to_owned()));
    }

    /// Changes what the sign-in to `agent`, and every agent sharing its
    /// sign-in, shows.
    fn edit_link(&mut self, agent: &AgentName, edit: impl Fn(&mut SignInLink)) {
        for each in self.sign_in_group(agent) {
            if let Some(standing) = self.setup.agents.get_mut(&each)
                && let SignInState::SigningIn(link) = &mut standing.sign_in
            {
                edit(link);
            }
        }
    }

    /// The user signed in to `agent`: each chat whose last turn failed for
    /// want of it says so, and its status command, if any, says how.
    fn signed_in(&mut self, now: Now, agent: &AgentName, out: &mut Vec<Directive<A, D>>) {
        self.set_sign_in(agent, SignInState::SignedIn(SignedInAs::Unknown));
        let group = self.sign_in_group(agent);
        let chats: Vec<ChatId> = (self.chats.values())
            .filter(|whole| group.contains(&whole.agent) && whole.state != ChatState::Closed)
            .filter(|whole| {
                let mut theirs = whole.turns.iter().rev().filter(|turn| {
                    // A note already said so.
                    turn.starter != Starter::Caf
                        || turn.items.iter().any(|item| {
                            item.kind == crate::chat::ItemKind::Note(Note::SignedIn)
                        })
                });
                theirs.next().is_some_and(|turn| {
                    matches!(&turn.life, TurnLife::Failed(failure) if failure.reason == FailureReason::SignInNeeded)
                })
            })
            .map(|whole| whole.id)
            .collect();
        for chat in chats {
            self.say(now, chat, Note::SignedIn);
        }
        // One already running may have read it before the sign-in ended.
        self.forget_status(agent);
        self.check_status(agent, out);
    }

    /// A sign-in nobody finished in time is stopped.
    pub(super) fn sign_in_overdue(&mut self, ticket: Ticket, out: &mut Vec<Directive<A, D>>) {
        let Some(job) = self.setup.jobs.get_mut(&ticket) else {
            return;
        };
        job.deadline = None;
        if job.stopped {
            return;
        }
        let agent = job.agent.clone();
        report(
            out,
            Level::Warn,
            format!(
                "stopped signing in to {}: nobody finished it in time",
                agent.0
            ),
        );
        self.stop_job(ticket, out);
    }

    /// Lets status commands already running for `agent`'s sign-in go: what
    /// they report is ignored, as it may be from before a sign-in or
    /// sign-out that just ended, and a fresh one may run.
    fn forget_status(&mut self, agent: &AgentName) {
        let group = self.sign_in_group(agent);
        for job in self.setup.jobs.values_mut() {
            if matches!(job.work, Work::Status) && group.contains(&job.agent) {
                job.stopped = true;
            }
        }
    }

    /// Runs `agent`'s status command, if it has one that can run now and
    /// isn't running already.
    fn check_status(&mut self, agent: &AgentName, out: &mut Vec<Directive<A, D>>) {
        let group = self.sign_in_group(agent);
        if (group.iter()).any(|each| {
            self.job_for(each, |work| matches!(work, Work::Status))
                .is_some()
        }) {
            return;
        }
        let Some(status) =
            (self.recipes.get(agent)).and_then(|recipe| recipe.status.command.clone())
        else {
            return;
        };
        let Some(command) = self.resolve(agent, &status.command) else {
            return;
        };
        let ticket = self.job(agent, Work::Status);
        out.push(Directive::Setup(SetupDirective::Status {
            ticket,
            command,
            signed_in: status.signed_in,
            how: status.how,
        }));
    }

    /// The adapter of process `proc` said whether it's signed in, in
    /// `_auth/status_update`. Only `kind` is read, and only from an
    /// adapter whose profile says it reports so.
    pub(super) fn status_pushed(
        &mut self,
        proc: Ticket,
        params: &Value,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let Some(conn) = (self.procs.get(&proc)).and_then(|each| self.conns.get(&each.conn)) else {
            return;
        };
        if !super::procs::profile_of(conn, &self.agents).pushes_status() {
            return;
        }
        let agent = conn.agent.clone();
        let Some(kind) = params.pointer("/authStatus/kind").and_then(Value::as_str) else {
            return;
        };
        let state = match kind {
            "none" => SignInState::SignedOut,
            "account" => SignInState::SignedIn(SignedInAs::Account),
            "api_key" => SignInState::SignedIn(SignedInAs::ApiKey),
            "gateway" | "external" => SignInState::SignedIn(SignedInAs::Gateway),
            _ => {
                return report(
                    out,
                    Level::Debug,
                    format!("{} said it's signed in some way CAF doesn't know", agent.0),
                );
            }
        };
        self.set_sign_in(&agent, state);
    }

    /// A process of `agent`'s answered `initialize`. One of the host's own
    /// command that said it's its profile's adapter may run its recipe's
    /// commands, so the first to say so has its status command run, if its
    /// adapter doesn't say by itself whether it's signed in. One of CAF's
    /// install at the version its profile was checked at, that said it's
    /// that version, lets older versions that nothing has used for a while
    /// be removed.
    pub(super) fn agent_answered(
        &mut self,
        now: Now,
        proc: Ticket,
        agent: &AgentName,
        checked: Checked,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let confirmed = matches!(checked, Checked::Yes | Checked::Newer);
        let first = (self.setup.agents.get_mut(agent)).is_some_and(|standing| {
            let first = confirmed && !standing.checked;
            standing.checked = confirmed;
            first
        });
        // The host's own command can now run its recipe's status command,
        // which until now couldn't: needed only if the adapter doesn't say
        // by itself. (One that does says so as it answers, and asking
        // would take a ticket recordings made before didn't.)
        let hosts =
            (self.agents.get(agent)).is_some_and(|spec| matches!(spec.launch, Launch::Command(_)));
        let pushes = (self.recipes.get(agent)).is_some_and(|recipe| recipe.status.pushed);
        if first && hosts && !pushes {
            self.check_status(agent, out);
        }
        let folder = self.procs.get(&proc).and_then(|each| each.folder.clone());
        let pinned = self.folder_of(agent);
        if checked != Checked::Yes || folder.is_none() || folder != pinned {
            return;
        }
        let (Some(machine), Some(pinned), Some(slug), Some(recipe)) = (
            &self.setup.machine,
            &pinned,
            self.slugs.get(agent),
            self.recipes.get(agent),
        ) else {
            return;
        };
        let (version, level) = (
            recipe.version.as_str(),
            recipe.patch.as_ref().map_or(0, |patch| patch.level),
        );
        let unused_since = now.wall.0 - i64::try_from(KEEP_UNUSED.as_millis()).unwrap_or(i64::MAX);
        let old: Vec<String> = (machine.installed.iter())
            .filter(|each| *each != pinned && is_folder_name(each))
            .filter(|each| older(slug, each, version, level))
            .filter(|each| {
                machine
                    .used
                    .get(*each)
                    .is_some_and(|used| used.0 <= unused_since)
            })
            .filter(|each| {
                !(self.setup.jobs.values())
                    .any(|job| matches!(&job.work, Work::Remove { folder } if folder == *each))
            })
            .cloned()
            .collect();
        for folder in old {
            let ticket = self.job(
                agent,
                Work::Remove {
                    folder: folder.clone(),
                },
            );
            out.push(Directive::Setup(SetupDirective::Remove {
                ticket,
                folder,
                unused_since: Wall(unused_since),
            }));
        }
    }

    /// A failure, as the user sees it: one for want of signing in marks
    /// `chat`'s agent signed out, and offers CAF's own sign-in if it has
    /// one that can run now.
    pub(super) fn sign_in_failure(&mut self, chat: ChatId, failure: &mut Failure) {
        if failure.reason != FailureReason::SignInNeeded {
            return;
        }
        let Some(agent) = self.chats.get(&chat).map(|whole| whole.agent.clone()) else {
            return;
        };
        self.set_sign_in(&agent, SignInState::SignedOut);
        if self.can_sign_in(&agent) {
            failure.remedy = Some(Remedy::SignIn);
        }
    }

    /// Whether [`Core::sign_in`] would start signing in to `agent` now.
    fn can_sign_in(&self, agent: &AgentName) -> bool {
        (self.recipes.get(agent))
            .and_then(|recipe| recipe.sign_in.as_ref())
            .is_some_and(|sign_in| self.resolve(agent, &sign_in.command).is_some())
    }

    /// Whether a process may be started for `agent`: it's the host's own
    /// command, or CAF's install of it is there, and no sign-in to it,
    /// started while it was signed out, is under way.
    pub(super) fn agent_ready(&self, agent: &AgentName) -> bool {
        let Some(spec) = self.agents.get(agent) else {
            return false;
        };
        let started = match spec.launch {
            Launch::Command(_) => true,
            Launch::Installed => self.installed_launch(agent).is_some(),
        };
        let signing_in = (self.signing_in(agent))
            .and_then(|ticket| self.setup.jobs.get(&ticket))
            .is_some_and(|job| {
                matches!(
                    job.work,
                    Work::SignIn {
                        before: SignInState::SignedOut
                    }
                )
            });
        started && !signing_in
    }

    /// How to start `agent` from CAF's install of it, and the install's
    /// folder, if it's installed.
    pub(super) fn installed_launch(&self, agent: &AgentName) -> Option<(SpawnCommand, PathBuf)> {
        if !matches!(
            self.install_state(agent),
            Some(InstallState::Installed { .. })
        ) {
            return None;
        }
        let recipe = self.recipes.get(agent)?;
        let Program::InInstall(path) = &recipe.adapter.program else {
            return None;
        };
        let root = &self.setup.machine.as_ref()?.root;
        let folder = root.join(self.folder_of(agent)?);
        let mut command = SpawnCommand::new(folder.join(inside(path)?));
        command.args = recipe.adapter.args.iter().map(Into::into).collect();
        command.env = self.env(agent, &recipe.adapter.env)?;
        Some((command, folder))
    }

    /// One of `agent`'s recipe's commands, ready to run: `None` if it
    /// can't run now.
    ///
    /// The adapter is run as the agent is started: from CAF's install, or
    /// as the host's own command, with its arguments and variables and the
    /// recipe's after them, once a process of that command has said it's
    /// the adapter the profile is for. A command such as `npx -y <adapter>`
    /// so stays itself. A file of the install needs it installed.
    fn resolve(&self, agent: &AgentName, bundled: &Bundled) -> Option<SpawnCommand> {
        let spec = self.agents.get(agent)?;
        let mut command = match (&bundled.program, &spec.launch) {
            (Program::Adapter, Launch::Installed) => self.installed_launch(agent)?.0,
            (Program::Adapter, Launch::Command(command)) => {
                if !self.setup.agents.get(agent)?.checked {
                    return None;
                }
                command.clone()
            }
            (Program::InInstall(path), Launch::Installed) => {
                let (_, folder) = self.installed_launch(agent)?;
                SpawnCommand::new(folder.join(inside(path)?))
            }
            (Program::InInstall(_), Launch::Command(_)) => return None,
        };
        // The host's own adapter never reads a private folder: nothing
        // gives it one, or makes it.
        if matches!(spec.launch, Launch::Command(_))
            && (bundled.env.iter()).any(|(_, value)| *value == EnvValue::PrivateFolder)
        {
            return None;
        }
        // CAF's install is started with the recipe's own arguments for the
        // adapter, which a command of the recipe replaces; the host's are
        // the host's, and are kept.
        if spec.launch == Launch::Installed {
            command.args.clear();
        }
        command.args.extend(bundled.args.iter().map(Into::into));
        command.env.extend(self.env(agent, &bundled.env)?);
        Some(command)
    }

    /// A recipe's variables for `agent`, with its private folder filled
    /// in: `None` if that can't be named yet.
    fn env(&self, agent: &AgentName, env: &[(String, EnvValue)]) -> Option<Vec<EnvChange>> {
        (env.iter())
            .map(|(name, value)| {
                let value = match value {
                    EnvValue::Text(text) => text.into(),
                    EnvValue::PrivateFolder => {
                        let root = &self.setup.machine.as_ref()?.root;
                        let private = format!("{}-private", self.slugs.get(agent)?);
                        is_folder_name(&private).then(|| root.join(private).into_os_string())?
                    }
                };
                Some(EnvChange::Set(name.into(), value))
            })
            .collect()
    }

    /// Stops `chat`'s waiting for its agent, as the user asked: its
    /// waiting messages, if any, were stopped, so it says why if that's
    /// because the agent isn't installed.
    pub(super) fn stopped_waiting(&mut self, now: Now, chat: ChatId, stopped: bool) {
        let Some(agent) = self.chats.get(&chat).map(|whole| whole.agent.clone()) else {
            return;
        };
        // Not while CAF doesn't know yet.
        let missing = matches!(
            self.install_state(&agent),
            Some(
                InstallState::Missing { .. }
                    | InstallState::Installing { .. }
                    | InstallState::Cannot(_)
            )
        );
        if stopped && missing {
            self.say(now, chat, Note::NotInstalled);
        }
    }

    /// Stops the waiting messages of every chat waiting for `agent`, saying
    /// `note` in each that had some. The chats go on waiting for it.
    fn stop_waiting_messages(&mut self, now: Now, agent: &AgentName, note: Note) {
        let chats: Vec<ChatId> = (self.chats.values())
            .filter(|whole| &whole.agent == agent && whole.state == ChatState::WaitingForAgent)
            .map(|whole| whole.id)
            .collect();
        for chat in chats {
            let waiting = self.waiting_turns(chat);
            for &turn in &waiting {
                self.set_life(now, chat, turn, TurnLife::Stopped);
            }
            if !waiting.is_empty() {
                self.say(now, chat, note.clone());
            }
        }
    }

    /// The state a chat that wants its session takes: waiting for its
    /// agent, if no process of its connection is there and none may start.
    pub(super) fn wanting(&self, chat: ChatId) -> ChatState {
        let Some(run) = self.runs.get(&chat) else {
            return ChatState::Opening;
        };
        let Some(conn) = self.conns.get(&run.conn) else {
            return ChatState::Opening;
        };
        if conn.current.is_none() && !self.agent_ready(&conn.agent) {
            ChatState::WaitingForAgent
        } else {
            ChatState::Opening
        }
    }

    /// Stops every install and sign-in under way, as CAF shuts down.
    pub(super) fn stop_setup(&mut self, out: &mut Vec<Directive<A, D>>) {
        let running: Vec<(Ticket, AgentName, Option<String>)> = (self.setup.jobs.iter())
            .filter(|(_, job)| !job.stopped)
            .filter_map(|(&ticket, job)| match &job.work {
                Work::Install { folder, .. } => {
                    Some((ticket, job.agent.clone(), Some(folder.clone())))
                }
                Work::SignIn { .. } => Some((ticket, job.agent.clone(), None)),
                _ => None,
            })
            .collect();
        for (ticket, agent, folder) in running {
            self.stop_job(ticket, out);
            if let Some(folder) = folder {
                let size = self.recipes.get(&agent).map_or(0, |recipe| recipe.size);
                for each in self.sharing(&folder) {
                    self.set_install(&each, InstallState::Missing { size });
                }
            }
        }
    }

    /// The folder `agent` is installed in: its profile's name and the
    /// version its recipe installs, such as `claude-agent-acp-0.98.0`, or
    /// `claude-agent-acp-0.98.0+caf.1` patched. `None` if CAF doesn't
    /// install it.
    fn folder_of(&self, agent: &AgentName) -> Option<String> {
        let slug = self.slugs.get(agent)?;
        let recipe = self.recipes.get(agent)?;
        let folder = format!("{slug}-{}", recipe.installed_version());
        is_folder_name(&folder).then_some(folder)
    }

    /// `agent`'s private folder, `<profile>-private`, if its recipe gives
    /// it one: any of its commands is given it.
    fn private_folder(&self, agent: &AgentName) -> Option<String> {
        let recipe = self.recipes.get(agent)?;
        let commands = [
            Some(&recipe.adapter),
            recipe.sign_in.as_ref().map(|sign_in| &sign_in.command),
            recipe.sign_out.as_ref(),
            recipe.status.command.as_ref().map(|status| &status.command),
        ];
        let uses = (commands.into_iter().flatten()).any(|bundled| {
            (bundled.env.iter()).any(|(_, value)| *value == EnvValue::PrivateFolder)
        });
        let private = format!("{}-private", self.slugs.get(agent)?);
        (uses && is_folder_name(&private)).then_some(private)
    }

    /// The agents CAF installs into `folder`: every one offered with the
    /// same profile.
    fn sharing(&self, folder: &str) -> Vec<AgentName> {
        (self.setup.order.iter())
            .filter(|agent| {
                self.agents
                    .get(*agent)
                    .is_some_and(|spec| spec.launch == Launch::Installed)
                    && self.folder_of(agent).as_deref() == Some(folder)
            })
            .cloned()
            .collect()
    }

    /// The ticket of the install of `folder` under way and not stopped.
    fn install_job(&self, folder: &str) -> Option<Ticket> {
        (self.setup.jobs.iter())
            .find(|(_, job)| {
                !job.stopped
                    && matches!(&job.work, Work::Install { folder: each, .. } if each == folder)
            })
            .map(|(&ticket, _)| ticket)
    }

    /// Starts tracking work for `agent`, and returns its ticket.
    fn job(&mut self, agent: &AgentName, work: Work) -> Ticket {
        let ticket = Ticket(self.ids.take());
        self.setup.jobs.insert(
            ticket,
            Job {
                agent: agent.clone(),
                work,
                stopped: false,
                deadline: None,
            },
        );
        ticket
    }

    /// The ticket of `agent`'s work of a kind, if some is under way and not
    /// stopped.
    fn job_for(&self, agent: &AgentName, kind: impl Fn(&Work) -> bool) -> Option<Ticket> {
        (self.setup.jobs.iter())
            .find(|(_, job)| &job.agent == agent && !job.stopped && kind(&job.work))
            .map(|(&ticket, _)| ticket)
    }

    /// The ticket of the sign-in under way that's `agent`'s: its own, or
    /// one of an agent sharing its sign-in.
    pub(super) fn signing_in(&self, agent: &AgentName) -> Option<Ticket> {
        (self.sign_in_group(agent).iter())
            .find_map(|each| self.job_for(each, |work| matches!(work, Work::SignIn { .. })))
    }

    /// The agents whose sign-in is `agent`'s. For one CAF installs, every
    /// agent of its profile: they run the same commands, with the same
    /// variables, so they keep their sign-in in the same place. One the
    /// host starts has its own, as its command may set its own place.
    fn sign_in_group(&self, agent: &AgentName) -> Vec<AgentName> {
        let installed =
            (self.agents.get(agent)).is_some_and(|spec| spec.launch == Launch::Installed);
        match self.folder_of(agent).filter(|_| installed) {
            Some(folder) => self.sharing(&folder),
            None => vec![agent.clone()],
        }
    }

    /// Stops work under way. A sign-in's state goes back to what it was.
    fn stop_job(&mut self, ticket: Ticket, out: &mut Vec<Directive<A, D>>) {
        let Some(job) = self.setup.jobs.get_mut(&ticket) else {
            return;
        };
        job.stopped = true;
        job.deadline = None;
        out.push(Directive::Setup(SetupDirective::Stop { ticket }));
        if let Work::SignIn { before } = job.work.clone() {
            let agent = job.agent.clone();
            self.set_sign_in(&agent, before);
        }
    }

    /// `agent`'s install state.
    pub(super) fn install_state(&self, agent: &AgentName) -> Option<InstallState> {
        (self.setup.agents.get(agent)).map(|standing| standing.install.clone())
    }

    /// Sets `agent`'s install state.
    fn set_install(&mut self, agent: &AgentName, state: InstallState) {
        if let Some(standing) = self.setup.agents.get_mut(agent) {
            standing.install = state;
        }
    }

    /// `agent`'s sign-in state.
    fn sign_in_state(&self, agent: &AgentName) -> SignInState {
        (self.setup.agents.get(agent))
            .map_or(SignInState::Unknown, |standing| standing.sign_in.clone())
    }

    /// `agent`'s status command found it signed in as `state` says. Found
    /// signed out once known signed in, it was signed out elsewhere, such
    /// as with `claude /logout`, perhaps to sign in to another account:
    /// its limits are cleared. Not when the adapter says so by itself
    /// (Claude's `_auth/status_update`): it may, as a token fails to
    /// refresh offline, and its limits, which come seldom, would go from
    /// every app. So for Claude, which says so by itself first, a
    /// `claude /logout` outside CAF mostly clears nothing: by the time a
    /// status command runs, it's no longer known signed in.
    fn found_signed_in(
        &mut self,
        now: Now,
        agent: &AgentName,
        state: SignInState,
        out: &mut Vec<Directive<A, D>>,
    ) {
        let was_in = (self.setup.agents.get(agent))
            .is_some_and(|standing| matches!(standing.sign_in, SignInState::SignedIn(_)));
        let out_now = state == SignInState::SignedOut;
        self.set_sign_in(agent, state);
        if was_in && out_now {
            self.account_changed(now, agent, out);
        }
    }

    /// Sets `agent`'s sign-in state, as something reported it. While a
    /// sign-in is under way, it stays signing in, and what it goes back to
    /// if it doesn't end signed in is this.
    fn set_sign_in(&mut self, agent: &AgentName, state: SignInState) {
        if let Some(ticket) = self.signing_in(agent)
            && let Some(Job {
                work: Work::SignIn { before },
                ..
            }) = self.setup.jobs.get_mut(&ticket)
        {
            *before = state;
            return;
        }
        for each in self.sign_in_group(agent) {
            if let Some(standing) = self.setup.agents.get_mut(&each) {
                standing.sign_in = state.clone();
            }
        }
    }
}

/// Where an agent starts, before CAF has heard anything.
pub(super) fn standing(launch: &Launch) -> Standing {
    Standing {
        install: match launch {
            Launch::Command(_) => InstallState::Host,
            _ => InstallState::Unknown,
        },
        sign_in: SignInState::Unknown,
        checked: false,
        strips: None,
        settings: KnownSettings::Unknown,
    }
}

/// Whether `name` names a folder directly under the installs' folder: not
/// hidden (half-built installs are), and with no separator.
fn is_folder_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

/// Whether `folder` is an install of the profile `slug` older than the
/// upstream version `pinned` patched at `level` (0 unpatched): the slug
/// and a version of three numbers, so that `claude-agent-acp-2-1.0.0` is
/// no install of `claude-agent-acp`. A prerelease comes before its
/// release, and of the same upstream version, a lower patch level before
/// a higher one. One patched some way CAF doesn't know is never older
/// than its own upstream version.
fn older(slug: &str, folder: &str, pinned: &str, level: u32) -> bool {
    if let Some(earlier) = patch_level(slug, folder, pinned) {
        return earlier < level;
    }
    let rank = |(numbers, pre): ([u64; 3], bool)| (numbers, !pre);
    let Some(pinned) = crate::profiles::version(pinned) else {
        return false;
    };
    folder
        .strip_prefix(slug)
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(crate::profiles::version)
        .is_some_and(|version| rank(version) < rank(pinned))
}

/// The patch level of `folder`, if it's an install of the profile `slug`
/// at the upstream version `upstream`: 0 unpatched, as
/// `claude-agent-acp-0.98.0`, and `N` as `claude-agent-acp-0.98.0+caf.N`.
/// `None` for any other folder, one of another version or patched some
/// other way included.
fn patch_level(slug: &str, folder: &str, upstream: &str) -> Option<u32> {
    let rest = folder
        .strip_prefix(slug)?
        .strip_prefix('-')?
        .strip_prefix(upstream)?;
    if rest.is_empty() {
        return Some(0);
    }
    let level = rest.strip_prefix("+caf.")?;
    if level.is_empty() || !level.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    level.parse().ok()
}

/// `path` as a path inside an install: `None` if it would leave it.
fn inside(path: &Path) -> Option<&Path> {
    path.components()
        .all(|part| matches!(part, Component::Normal(_)))
        .then_some(path)
}

/// The host of an `https` link, lowercase, without its port: `None` if the
/// link isn't `https`. The host is what follows any user name, so
/// `https://claude.com@evil.example/` is on `evil.example`.
fn https_host(url: &str) -> Option<String> {
    let scheme = url.get(..8)?;
    if !scheme.eq_ignore_ascii_case("https://") {
        return None;
    }
    let rest = &url[8..];
    let authority = rest.split(['/', '?', '#', '\\']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = match host.strip_prefix('[') {
        // An IPv6 address is never a site's name.
        Some(_) => return None,
        None => host.split(':').next()?,
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The kind of a setup report, for the log: never what it carries.
fn setup_event_kind(event: &SetupEvent) -> &'static str {
    match event {
        SetupEvent::Surveyed { .. } => "Surveyed",
        SetupEvent::Progress { .. } => "Progress",
        SetupEvent::Installed => "Installed",
        SetupEvent::InstallFailed(_) => "InstallFailed",
        SetupEvent::Removed { .. } => "Removed",
        SetupEvent::Link(_) => "Link",
        SetupEvent::Code(_) => "Code",
        SetupEvent::Status(_) => "Status",
        SetupEvent::Ended { .. } => "Ended",
        SetupEvent::Failed(_) => "Failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_links_host_is_what_follows_any_user_name_without_its_port() {
        let host = |url: &str| https_host(url);
        assert_eq!(
            host("https://claude.com/cai/oauth/authorize?x=1").as_deref(),
            Some("claude.com")
        );
        assert_eq!(
            host("HTTPS://Claude.COM:443/").as_deref(),
            Some("claude.com")
        );
        assert_eq!(host("https://claude.com.").as_deref(), Some("claude.com"));
        assert_eq!(
            host("https://claude.com@evil.example/").as_deref(),
            Some("evil.example")
        );
        assert_eq!(
            host("https://claude.com.evil.example/").as_deref(),
            Some("claude.com.evil.example")
        );
        assert_eq!(
            host("https://evil.example/claude.com").as_deref(),
            Some("evil.example")
        );
        assert_eq!(
            host("https://evil.example\\@claude.com/").as_deref(),
            Some("evil.example")
        );
        assert_eq!(
            host("https://evil.example?@claude.com").as_deref(),
            Some("evil.example")
        );
        assert_eq!(host("http://claude.com/"), None);
        assert_eq!(host("https://[::1]/"), None);
        assert_eq!(host("https:///"), None);
        assert_eq!(host("claude.com"), None);
    }

    #[test]
    fn an_older_install_is_its_profiles_name_and_an_older_version() {
        let older = |folder: &str| older("claude-agent-acp", folder, "0.98.0", 0);
        assert!(older("claude-agent-acp-0.97.0"));
        assert!(older("claude-agent-acp-0.98.0-rc.1"));
        assert!(!older("claude-agent-acp-0.98.0"));
        assert!(!older("claude-agent-acp-0.99.0"));
        assert!(!older("claude-agent-acp-1.0.0-rc.1"));
        assert!(!older("claude-agent-acp-2-0.1.0"));
        assert!(!older("claude-agent-acp-private"));
        assert!(!older("claude-agent-acpx-0.1.0"));
        assert!(!older("claude-agent-acp"));
        // Patched later, at any level, it's newer.
        assert!(!older("claude-agent-acp-0.98.0+caf.1"));
        assert!(older("claude-agent-acp-0.97.0+caf.3"));
    }

    #[test]
    fn a_lower_patch_level_of_the_same_upstream_version_is_older() {
        let older = |folder: &str| older("claude-agent-acp", folder, "0.98.0", 2);
        assert!(older("claude-agent-acp-0.98.0"));
        assert!(older("claude-agent-acp-0.98.0+caf.1"));
        assert!(older("claude-agent-acp-0.98.0+caf.0"));
        assert!(older("claude-agent-acp-0.97.0+caf.9"));
        assert!(older("claude-agent-acp-0.98.0-rc.1+caf.5"));
        assert!(!older("claude-agent-acp-0.98.0+caf.2"));
        assert!(!older("claude-agent-acp-0.98.0+caf.3"));
        assert!(!older("claude-agent-acp-0.99.0+caf.1"));
        // Patched some way this CAF doesn't know: never.
        assert!(!older("claude-agent-acp-0.98.0+caf.x"));
        assert!(!older("claude-agent-acp-0.98.0+caf."));
        assert!(!older("claude-agent-acp-0.98.0+caf.+1"));
        assert!(!older("claude-agent-acp-0.98.0+caf.1.1"));
        assert!(!older("claude-agent-acp-0.98.0+other.1"));
        assert!(!older("claude-agent-acp-0.98.0.1"));
        assert!(!older("claude-agent-acp-2-0.98.0+caf.1"));
    }

    #[test]
    fn a_path_inside_an_install_never_leaves_it() {
        assert!(inside(Path::new("node_modules/.bin/a")).is_some());
        assert!(inside(Path::new("../a")).is_none());
        assert!(inside(Path::new("/bin/sh")).is_none());
        assert!(inside(Path::new("a/../../b")).is_none());
    }

    #[test]
    fn only_a_plain_name_names_an_install_folder() {
        assert!(is_folder_name("claude-agent-acp-0.98.0"));
        assert!(!is_folder_name(".claude-agent-acp-0.98.0"));
        assert!(!is_folder_name("../x-1.0.0"));
        assert!(!is_folder_name("a/b-1.0.0"));
        assert!(!is_folder_name(""));
    }
}
