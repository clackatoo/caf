//! Account limits: what the fake adapter says of them, on every agent's
//! status, and kept beside the ledger across restarts and between apps.
//! Every ledger is in the test's own folder.

mod common;

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use caf_core::OpenChat;
use caf_core::chat::{NoAppItems, UserMessage};
use caf_core::event::Event;
use caf_core::limits::{AccountLimits, ProfileLimits, WindowKind};
use caf_core::profiles::Builtin;
use caf_core::time::Wall;
use caf_driver_tokio::{AgentsWatch, Caf, Config, Running, Step, Steps};
use serde_json::{Value, json};

use common::*;

/// The wall clock now.
fn now() -> Wall {
    let since = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    Wall(i64::try_from(since.as_millis()).unwrap())
}

/// The adapter says the limit of `kind`, Claude's name for it, is
/// `utilization` used, resetting in four hours.
fn limit(kind: &str, utilization: f64) -> Value {
    let resets = now().0 / 1000 + 4 * 3600;
    json!({"update": {"session": "s1", "update": {
        "sessionUpdate": "usage_update",
        "used": 1000,
        "size": 200_000,
        "_meta": {"_claude/rateLimit": {
            "status": "allowed",
            "rateLimitType": kind,
            "utilization": utilization,
            "resetsAt": resets,
        }},
    }}})
}

/// A fake whose one turn says the limit of `kind` is `utilization` used.
fn saying(dir: &Dir, kind: &str, utilization: f64) -> Config {
    let script = dir.script(
        &format!("{kind}.jsonl"),
        &after_handshake(&[
            prompt(),
            say("s1", "Done"),
            limit(kind, utilization),
            end_turn(),
        ]),
    );
    config_with_ledger(dir, host(), vec![fake(dir, &script)])
}

/// Takes a turn with the fake, so it says what its script does.
async fn take_a_turn(caf: &Caf, dir: &Dir) {
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Go")).await.unwrap();
    until(&chat, &mut watch, "the turn to finish", finished).await;
}

/// Waits until the fake's account limits are as `done` says, and gives
/// them.
async fn until_limits(
    agents: &mut AgentsWatch,
    what: &str,
    done: impl Fn(&AccountLimits) -> bool,
) -> AccountLimits {
    let waited = tokio::time::timeout(PATIENCE, async {
        while let Some(statuses) = agents.next().await {
            if done(&statuses[0].limits) {
                return statuses[0].limits.clone();
            }
        }
        panic!("CAF ended waiting for {what}")
    })
    .await;
    waited.unwrap_or_else(|_| panic!("gave up waiting for {what}"))
}

/// How much of the window of `kind` is used, if there is one.
fn used(limits: &AccountLimits, kind: &WindowKind) -> Option<u8> {
    limits.window(kind).map(|window| window.used)
}

/// The fake's limits kept beside the ledger in `dir`.
fn kept(dir: &Dir) -> Option<AccountLimits> {
    let (kept, errors) = caf_usage::limits::load(&dir.path("usage"), now());
    assert!(errors.is_empty(), "{errors:?}");
    (kept.into_iter())
        .find(|kept| kept.profile == Builtin::Fake)
        .map(|kept| kept.limits)
}

/// Every account limits the core is given, as steps show them.
#[derive(Clone, Default)]
struct Given(Arc<Mutex<Vec<Vec<ProfileLimits>>>>);

impl Steps<NoAppItems> for Given {
    fn step(&mut self, step: Step<'_, NoAppItems>) {
        if let Event::Limits(limits) = step.event {
            self.0.lock().unwrap().push(limits.clone());
        }
    }
}

/// Shuts down every CAF given.
async fn shut_all(running: Vec<(Caf, Running)>) {
    for (caf, running) in running {
        shut_down(&caf, running).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn limits_the_agent_says_are_on_its_status_and_kept_across_a_restart() {
    let dir = Dir::new();
    let (caf, running) = Caf::start(saying(&dir, "five_hour", 0.42));
    let mut agents = caf.watch_agents();
    assert_eq!(
        agents.next().await.unwrap()[0].limits,
        AccountLimits::default()
    );
    take_a_turn(&caf, &dir).await;
    let heard = until_limits(&mut agents, "the five hours", |limits| {
        used(limits, &WindowKind::FiveHours) == Some(42)
    })
    .await;
    shut_down(&caf, running).await;
    assert_eq!(kept(&dir), Some(heard.clone()), "kept beside the ledger");

    // Started again, CAF has them before the agent says anything.
    let given = Given::default();
    let (caf, running) = Caf::start_watching(saying(&dir, "five_hour", 0.42), given.clone());
    let mut agents = caf.watch_agents();
    let restored = until_limits(&mut agents, "the limits kept", |limits| {
        !limits.windows.is_empty()
    })
    .await;
    assert_eq!(restored, heard);
    let given = given.0.lock().unwrap().clone();
    assert_eq!(given, [vec![ProfileLimits::new(Builtin::Fake, heard)]]);
    shut_down(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn limits_one_app_heard_show_in_another_after_its_next_save() {
    let dir = Dir::new();
    let (first, first_running) = Caf::start(saying(&dir, "five_hour", 0.42));
    let (second, second_running) = Caf::start(saying(&dir, "seven_day", 0.1));
    let mut first_agents = first.watch_agents();
    let mut second_agents = second.watch_agents();

    take_a_turn(&first, &dir).await;
    until_limits(&mut first_agents, "the five hours", |limits| {
        used(limits, &WindowKind::FiveHours) == Some(42)
    })
    .await;
    // The first has saved them by the time the second hears its own.
    let started = std::time::Instant::now();
    while kept(&dir).is_none() {
        assert!(started.elapsed() < PATIENCE, "kept");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    take_a_turn(&second, &dir).await;
    let both = until_limits(&mut second_agents, "both windows", |limits| {
        used(limits, &WindowKind::FiveHours) == Some(42)
            && used(limits, &WindowKind::Week) == Some(10)
    })
    .await;
    assert!(
        both.window(&WindowKind::FiveHours).is_some(),
        "the first's news, from its save"
    );
    // Kept as both: neither undid the other's.
    let started = std::time::Instant::now();
    while kept(&dir).as_ref() != Some(&both) {
        assert!(started.elapsed() < PATIENCE, "both kept: {:?}", kept(&dir));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    shut_all(vec![(first, first_running), (second, second_running)]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_ledger_limits_are_on_the_status_and_not_kept() {
    let dir = Dir::new();
    let mut config = saying(&dir, "five_hour", 0.42);
    config.usage = None;
    let (caf, running) = Caf::start(config);
    let mut agents = caf.watch_agents();
    take_a_turn(&caf, &dir).await;
    until_limits(&mut agents, "the five hours", |limits| {
        used(limits, &WindowKind::FiveHours) == Some(42)
    })
    .await;
    shut_down(&caf, running).await;
    assert!(!dir.path("usage").exists(), "nothing kept");
}
