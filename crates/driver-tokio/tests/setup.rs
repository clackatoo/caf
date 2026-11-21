//! Installing and signing in through [`Caf`] and [`serve`], against the
//! fake adapter's `auth` commands.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use caf_core::OpenChat;
use caf_core::Refused;
use caf_core::agent::{
    AgentSpec, AgentStatus, EnvChange, InstallState, SignInLink, SignInState, SpawnCommand,
};
use caf_core::chat::ChatState;
use caf_core::ids::AgentName;
use caf_core::install::SignedInAs;
use caf_core::profiles::{Builtin, ProfileChoice};
use caf_core::viewer::{AgentAction, FromViewer, Outcome, RequestNo, ToViewer};
use caf_driver_tokio::viewer::{ViewerRules, serve};
use caf_driver_tokio::{AgentsWatch, Caf, ChatHandle, Config, Error};
use caf_fake_agent::{AUTH_EMAIL, AUTH_SECRET, AUTH_VAR, Auth};
use serde_json::json;
use tokio::sync::mpsc;

use common::*;

/// Where the fake's sign-in sends the user: one of Claude's sites.
const LINK: &str = "https://claude.ai/oauth/authorize?code=true&state=fake-state";

/// The code the user pastes.
const CODE: &str = "pasted-one-time-code";

/// The fake, offered as Claude: so CAF signs in to it as Claude's profile
/// says, through its adapter with `--cli auth …`, which the fake answers
/// as `auth` set by `auth.json`.
fn claude(dir: &Dir, auth: &Auth) -> AgentSpec {
    let settings = dir.path("auth.json");
    std::fs::write(&settings, serde_json::to_string(auth).unwrap()).unwrap();
    let script = dir.script("script.jsonl", &claude_handshake());
    let mut command = SpawnCommand::new(caf_fake_agent::binary());
    command.env = vec![
        EnvChange::Set(caf_fake_agent::SCRIPT_VAR.into(), script.into()),
        EnvChange::Set(AUTH_VAR.into(), settings.into()),
    ];
    AgentSpec::command(
        "claude",
        command,
        ProfileChoice::Builtin(Builtin::ClaudeAgentAcp),
    )
}

/// A handshake as Claude's adapter, at the version its profile was
/// checked at: so CAF lets the host's own command run the profile's
/// sign-in.
fn claude_handshake() -> Vec<serde_json::Value> {
    let mut steps = handshake("s1");
    steps[2] = json!({"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}, "agentInfo": {"name": "@agentclientprotocol/claude-agent-acp", "version": "0.98.0"}}}});
    steps
}

/// Opens a chat with `agent`, and waits until its session is open: so its
/// adapter has said what it is.
async fn checked(caf: &Caf, dir: &Dir, agent: &str) -> ChatHandle {
    let chat = caf.open_chat(OpenChat::new(agent, &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    until(&chat, &mut watch, "the session to open", |chat| {
        matches!(chat.state, ChatState::Open { .. })
    })
    .await;
    chat
}

/// A sign-in that hands its link to `$BROWSER`, then waits for the code.
fn signing_in(dir: &Dir) -> Auth {
    Auth {
        url: Some(LINK.into()),
        browser: true,
        paste: true,
        expect: Some(CODE.into()),
        state: Some(dir.path("signed-in")),
        ..Auth::default()
    }
}

/// A config offering `agents`, with installs under the test's folder.
fn config(dir: &Dir, agents: Vec<AgentSpec>) -> Config {
    let mut config = Config::new(host(), agents);
    config.installs = Some(dir.path("agents"));
    config
}

fn name(agent: &str) -> AgentName {
    AgentName(agent.into())
}

/// Waits, with `watch`, until `done` is true of the agents' statuses, and
/// returns them.
async fn until_agents(
    watch: &mut AgentsWatch,
    what: &str,
    done: impl Fn(&[AgentStatus]) -> bool,
) -> Vec<AgentStatus> {
    let mut seen = Vec::new();
    let found = tokio::time::timeout(PATIENCE, async {
        while let Some(statuses) = watch.next().await {
            if done(&statuses) {
                return Some(statuses);
            }
            seen.push(statuses);
        }
        None
    })
    .await;
    match found {
        Ok(Some(statuses)) => statuses,
        _ => panic!("waiting for {what}, saw {seen:#?}"),
    }
}

fn sign_in_of(statuses: &[AgentStatus]) -> &SignInState {
    &statuses[0].sign_in
}

#[tokio::test]
async fn a_sign_in_runs_through_the_agents_own_adapter() {
    let dir = Dir::new();
    let recording = dir.path("run.jsonl");
    let mut config = config(&dir, vec![claude(&dir, &signing_in(&dir))]);
    config.record = Some(recording.clone());
    let (caf, running) = Caf::start(config);
    let mut agents = caf.watch_agents();
    let first = agents.next().await.expect("the statuses at once");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].name, name("claude"));
    assert_eq!(first[0].install, InstallState::Host, "the host starts it");
    assert_eq!(
        caf.sign_in(&name("claude")).await,
        Err(Error::Refused(Refused::CannotSignIn)),
        "not until its adapter has said it's Claude's"
    );
    let _chat = checked(&caf, &dir, "claude").await;

    caf.sign_in(&name("claude")).await.unwrap();
    let signing = until_agents(&mut agents, "the link", |statuses| {
        matches!(
            sign_in_of(statuses),
            SignInState::SigningIn(SignInLink { url: Some(_), .. })
        )
    })
    .await;
    let SignInState::SigningIn(link) = sign_in_of(&signing) else {
        unreachable!()
    };
    assert_eq!(link.url.as_deref(), Some(LINK));
    assert!(link.paste, "Claude's sign-in takes a pasted code");

    caf.paste(&name("claude"), CODE.into()).await.unwrap();
    until_agents(&mut agents, "signed in with an account", |statuses| {
        *sign_in_of(statuses) == SignInState::SignedIn(SignedInAs::Account)
    })
    .await;

    caf.sign_out(&name("claude")).await.unwrap();
    until_agents(&mut agents, "signed out again", |statuses| {
        *sign_in_of(statuses) == SignInState::SignedOut
    })
    .await;
    shut_down(&caf, running).await;
    assert!(agents.next().await.is_none(), "the watch ends with CAF");

    let text = std::fs::read_to_string(&recording).unwrap();
    let events: Vec<serde_json::Value> = (text.lines().skip(1))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["event"].clone())
        .collect();
    assert_eq!(
        events[0],
        json!({"type": "Host", "value": {"type": "CheckAgents"}}),
        "CAF looks at its agents first"
    );
    assert!(
        events.contains(
            &json!({"type": "Host", "value": {"type": "Paste", "value": {"agent": "claude", "text": null}}})
        ),
        "the paste is recorded, without the code"
    );
    assert!(text.contains(LINK), "the link is recorded, to replay");
    for secret in [CODE, AUTH_EMAIL, AUTH_SECRET] {
        assert!(!text.contains(secret), "{secret} is never recorded");
    }
}

#[tokio::test]
async fn what_cant_be_done_for_an_agent_is_refused() {
    let dir = Dir::new();
    let script = dir.script("plain.jsonl", &handshake("s1"));
    let plain = fake(&dir, &script);
    let (caf, running) = Caf::start(config(&dir, vec![claude(&dir, &Auth::default()), plain]));
    let refused = |result| match result {
        Err(Error::Refused(refused)) => refused,
        other => panic!("refused, not {other:?}"),
    };
    assert_eq!(
        refused(caf.install(&name("claude")).await),
        Refused::NotInstallable
    );
    assert_eq!(
        refused(caf.stop_install(&name("claude")).await),
        Refused::NothingToStop
    );
    assert_eq!(
        refused(caf.paste(&name("claude"), "x".into()).await),
        Refused::NotSigningIn
    );
    assert_eq!(
        refused(caf.stop_sign_in(&name("claude")).await),
        Refused::NotSigningIn
    );
    assert_eq!(
        refused(caf.sign_in(&name("fake")).await),
        Refused::CannotSignIn
    );
    assert_eq!(
        refused(caf.sign_out(&name("fake")).await),
        Refused::CannotSignIn
    );
    // Nor to Claude's, until its adapter has said it's Claude's.
    assert_eq!(
        refused(caf.sign_in(&name("claude")).await),
        Refused::CannotSignIn
    );
    assert_eq!(
        refused(caf.sign_out(&name("claude")).await),
        Refused::CannotSignIn
    );
    assert_eq!(
        refused(caf.install(&name("nobody")).await),
        Refused::NoSuchAgent
    );
    caf.check_agents().await.unwrap();
    shut_down(&caf, running).await;
    assert_eq!(caf.check_agents().await, Err(Error::Gone));
    assert_eq!(caf.sign_in(&name("claude")).await, Err(Error::Gone));
}

#[tokio::test]
async fn an_agent_caf_installs_is_missing_until_it_is() {
    let dir = Dir::new();
    let (caf, running) = Caf::start(config(
        &dir,
        vec![AgentSpec::installed("claude", Builtin::ClaudeAgentAcp)],
    ));
    let mut agents = caf.watch_agents();
    let statuses = until_agents(&mut agents, "the survey", |statuses| {
        !matches!(statuses[0].install, InstallState::Unknown)
    })
    .await;
    let installs: PathBuf = dir.path("agents");
    match &statuses[0].install {
        InstallState::Missing { size } => assert!(*size > 100 << 20, "{size}"),
        // A machine without Node or npm can't install it.
        InstallState::Cannot(_) => {}
        other => panic!("not installed in {}: {other:?}", installs.display()),
    }
    assert!(!installs.exists(), "looking makes nothing");
    shut_down(&caf, running).await;
}

/// A viewer of `caf` with `rules`, past its welcome.
async fn viewer(
    caf: &Caf,
    rules: ViewerRules,
) -> (mpsc::Sender<FromViewer>, mpsc::Receiver<ToViewer>) {
    let (to, from_viewer) = mpsc::channel(16);
    let (to_viewer, mut from) = mpsc::channel(64);
    tokio::spawn(serve(caf.clone(), Arc::new(rules), from_viewer, to_viewer));
    to.send(FromViewer::Hello { min: 1, max: 1 }).await.unwrap();
    assert!(matches!(from.recv().await, Some(ToViewer::Welcome { .. })));
    (to, from)
}

/// The next message that `pick` takes, skipping others.
async fn next_that<T>(
    from: &mut mpsc::Receiver<ToViewer>,
    pick: impl Fn(ToViewer) -> Option<T>,
) -> T {
    tokio::time::timeout(PATIENCE, async {
        loop {
            let message = from.recv().await.expect("the link stays up");
            if let Some(picked) = pick(message) {
                return picked;
            }
        }
    })
    .await
    .expect("in good time")
}

fn done(request: u64) -> impl Fn(ToViewer) -> Option<Outcome> {
    move |message| match message {
        ToViewer::Done {
            request: answered,
            outcome,
        } if answered == RequestNo(request) => Some(outcome),
        _ => None,
    }
}

#[tokio::test]
async fn viewers_hear_every_agents_status_and_act_on_agents_only_if_allowed() {
    let dir = Dir::new();
    let (caf, running) = Caf::start(config(&dir, vec![claude(&dir, &signing_in(&dir))]));

    let (to, mut from) = viewer(&caf, ViewerRules::default()).await;
    let statuses = next_that(&mut from, |message| match message {
        ToViewer::Agents(statuses) => Some(statuses),
        _ => None,
    })
    .await;
    assert_eq!(statuses[0].name, name("claude"), "right after the welcome");
    for (n, action) in [
        AgentAction::SignIn,
        AgentAction::Install,
        AgentAction::SignOut,
    ]
    .into_iter()
    .enumerate()
    {
        let request = 1 + n as u64;
        to.send(FromViewer::Agent {
            request: RequestNo(request),
            agent: name("claude"),
            action,
        })
        .await
        .unwrap();
        assert_eq!(
            next_that(&mut from, done(request)).await,
            Outcome::NotAllowed
        );
    }

    let _chat = checked(&caf, &dir, "claude").await;
    let mut rules = ViewerRules::default();
    rules.agents = true;
    let (to, mut from) = viewer(&caf, rules).await;
    to.send(FromViewer::Agent {
        request: RequestNo(1),
        agent: name("claude"),
        action: AgentAction::SignIn,
    })
    .await
    .unwrap();
    assert_eq!(next_that(&mut from, done(1)).await, Outcome::Done);
    let link = next_that(&mut from, |message| match message {
        ToViewer::Agents(statuses) => match &statuses[0].sign_in {
            SignInState::SigningIn(SignInLink { url: Some(url), .. }) => Some(url.clone()),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(link, LINK, "the link reaches the viewer");
    to.send(FromViewer::Agent {
        request: RequestNo(2),
        agent: name("claude"),
        action: AgentAction::Paste(CODE.into()),
    })
    .await
    .unwrap();
    assert_eq!(next_that(&mut from, done(2)).await, Outcome::Done);
    next_that(&mut from, |message| match message {
        ToViewer::Agents(statuses) => {
            matches!(statuses[0].sign_in, SignInState::SignedIn(_)).then_some(())
        }
        _ => None,
    })
    .await;
    to.send(FromViewer::Agent {
        request: RequestNo(3),
        agent: name("claude"),
        action: AgentAction::Install,
    })
    .await
    .unwrap();
    assert_eq!(
        next_that(&mut from, done(3)).await,
        Outcome::Refused(Refused::NotInstallable),
        "the core's own refusal"
    );
    shut_down(&caf, running).await;
}
