//! Choosing an agent's settings, end to end, against the fake adapter:
//! choices sent as the session opens, model first, before any message;
//! a change during a turn held until it ends; the older `models` list;
//! the host's defaults; a choice the agent refuses; a change the agent
//! never answers; learning what an agent offers before any chat; and a
//! recording of all that replaying the same.

mod common;

use caf_core::agent::{AgentSpec, EnvChange, SpawnCommand};
use caf_core::chat::{FailureReason, ItemKind, Note, Remedy, TurnLife, UserMessage};
use caf_core::event::Event;
use caf_core::host::Host;
use caf_core::ids::AgentName;
use caf_core::profiles::{Builtin, ProfileChoice};
use caf_core::settings::{KnownSettings, ModelUsed, SettingKind, SettingValue};
use caf_core::{Chat, Core, Now, OpenChat, Refused, SettingChoice};
use caf_driver_tokio::{Caf, Error};
use serde_json::{Value, json};

use common::*;

/// What the agent says `kind` is now.
fn current(chat: &Chat, kind: SettingKind) -> Option<&SettingValue> {
    (chat.settings.offered.iter())
        .find(|setting| setting.kind == kind)
        .and_then(|setting| setting.current.as_ref())
}

/// What was chosen for `kind`.
fn chosen(chat: &Chat, kind: SettingKind) -> Option<&SettingValue> {
    (chat.settings.chosen.iter())
        .find(|choice| choice.kind == kind)
        .map(|choice| &choice.value)
}

fn choice(id: &str) -> SettingValue {
    SettingValue::Choice(id.to_owned())
}

/// Every note in the chat, in order.
fn notes(chat: &Chat) -> Vec<&Note> {
    (chat.turns.iter())
        .flat_map(|turn| &turn.items)
        .filter_map(|item| match &item.kind {
            ItemKind::Note(note) => Some(note),
            _ => None,
        })
        .collect()
}

/// The model a turn records, as the fake names it.
fn model(id: &str, name: &str) -> Option<ModelUsed> {
    serde_json::from_value(json!({ "id": id, "name": name })).unwrap()
}

/// The methods the fake was sent, in order.
fn methods(dir: &Dir) -> Vec<String> {
    (dir.record().iter())
        .filter_map(|line| line["received"]["method"].as_str().map(str::to_owned))
        .collect()
}

/// Whether the fake found anything it was sent wasn't as its script
/// expected.
fn any_failed(dir: &Dir) -> bool {
    dir.record().iter().any(|line| line.get("failed").is_some())
}

/// Lets a script's `wait_file` on `name` go on.
fn touch(dir: &Dir, name: &str) {
    std::fs::write(dir.path(name), "").unwrap();
}

/// A script's `wait_file` on `name`, in `dir`.
fn wait_file(dir: &Dir, name: &str) -> Value {
    json!({"wait_file": {"path": dir.path(name), "timeout": 60_000}})
}

/// `steps` after [`handshake_offering`].
fn offering(steps: &[Value]) -> Vec<Value> {
    let mut script = handshake_offering("s1");
    script.extend_from_slice(steps);
    script
}

/// Opening a chat with the fake that chooses `settings`.
fn choosing(dir: &Dir, settings: Vec<SettingChoice>) -> OpenChat {
    let mut open = OpenChat::new("fake", &dir.0);
    open.settings = settings;
    open
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chats_choices_are_sent_model_first_before_its_first_message() {
    let dir = Dir::new();
    let mut steps = Vec::new();
    steps.extend(set_to(
        "model",
        json!("large"),
        &options("large", "low", false),
    ));
    steps.extend(set_to(
        "effort",
        json!("high"),
        &options("large", "high", false),
    ));
    steps.extend(set_to("fast", json!(true), &options("large", "high", true)));
    steps.extend([prompt(), say("s1", "Hello"), end_turn()]);
    let script = dir.script("script.jsonl", &offering(&steps));
    let (caf, running) = Caf::start(config_for(host(), vec![fake(&dir, &script)]));
    let open = choosing(
        &dir,
        vec![
            SettingChoice::fast(true),
            SettingChoice::effort("high"),
            SettingChoice::model("large"),
        ],
    );
    let chat = caf.open_chat(open).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();

    let done = until(&chat, &mut watch, "the turn to finish", finished).await;
    assert_eq!(words(&done.turns[0]), "Hello");
    assert_eq!(done.turns[0].model, model("large", "Large"));
    assert_eq!(current(&done, SettingKind::Model), Some(&choice("large")));
    assert_eq!(current(&done, SettingKind::Effort), Some(&choice("high")));
    assert_eq!(
        current(&done, SettingKind::Fast),
        Some(&SettingValue::Switch(true))
    );
    assert_eq!(
        done.settings.own,
        [SettingKind::Model, SettingKind::Effort, SettingKind::Fast],
        "the chat's own, in the order they're sent in"
    );
    assert!(notes(&done).is_empty(), "nothing to say: {done:#?}");
    shut_down(&caf, running).await;
    assert_eq!(
        methods(&dir),
        [
            "initialize",
            "session/new",
            "session/set_config_option",
            "session/set_config_option",
            "session/set_config_option",
            "session/prompt"
        ]
    );
    assert!(!any_failed(&dir), "{:#?}", dir.record());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_that_lists_its_models_the_older_way_is_set_with_set_model() {
    let dir = Dir::new();
    let models = json!({
        "availableModels": [
            {"modelId": "fake-pro", "name": "Fake Pro"},
            {"modelId": "fake-flash", "name": "Fake Flash", "description": "Fast and cheap"}
        ],
        "currentModelId": "fake-pro"
    });
    let mut script = handshake("s1");
    // Answering `session/new` with its models.
    script.pop();
    script.extend([
        json!({"respond": {"result": {"sessionId": "s1", "models": models}}}),
        json!({"wait": {"method": "session/set_model", "expect": {"params": {"sessionId": "s1", "modelId": "fake-flash"}}}}),
        json!({"respond": {"result": {}}}),
        prompt(),
        say("s1", "Flash"),
        end_turn(),
    ]);
    let script = dir.script("script.jsonl", &script);
    let (caf, running) = Caf::start(config_for(host(), vec![fake(&dir, &script)]));
    let chat = caf
        .open_chat(choosing(&dir, vec![SettingChoice::model("fake-flash")]))
        .await
        .unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();

    let done = until(&chat, &mut watch, "the turn to finish", finished).await;
    assert_eq!(done.turns[0].model, model("fake-flash", "Fake Flash"));
    assert_eq!(done.settings.offered.len(), 1, "only its models");
    assert_eq!(
        current(&done, SettingKind::Model),
        Some(&choice("fake-flash"))
    );
    // It has no effort to set.
    assert_eq!(
        chat.set_setting(SettingChoice::effort("high")).await,
        Err(Error::Refused(Refused::NoSuchSetting))
    );
    shut_down(&caf, running).await;
    assert!(!any_failed(&dir), "{:#?}", dir.record());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_during_a_turn_is_held_until_it_ends_and_only_the_latest_counts() {
    let dir = Dir::new();
    let mut steps = vec![
        prompt(),
        say("s1", "Working"),
        wait_file(&dir, "go"),
        json!({"note": "the first turn ends"}),
        end_turn(),
    ];
    // Only the latest of each: the effort chosen back is sent all the
    // same, as the chat's own now.
    steps.extend(set_to(
        "model",
        json!("large"),
        &options("large", "low", false),
    ));
    steps.extend(set_to(
        "effort",
        json!("low"),
        &options("large", "low", false),
    ));
    steps.extend([prompt(), say("s1", "Thorough"), end_turn()]);
    let script = dir.script("script.jsonl", &offering(&steps));
    let (caf, running) = Caf::start(config_for(host(), vec![fake(&dir, &script)]));
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("One")).await.unwrap();
    until(&chat, &mut watch, "the first words", |chat| {
        chat.turns
            .first()
            .is_some_and(|turn| words(turn) == "Working")
    })
    .await;

    chat.set_setting(SettingChoice::model("large"))
        .await
        .unwrap();
    chat.set_setting(SettingChoice::effort("high"))
        .await
        .unwrap();
    chat.set_setting(SettingChoice::effort("low"))
        .await
        .unwrap();
    let held = picture(&chat).await;
    assert_eq!(chosen(&held, SettingKind::Model), Some(&choice("large")));
    assert_eq!(
        current(&held, SettingKind::Model),
        Some(&choice("small")),
        "waiting for the turn to end"
    );
    assert_eq!(held.turns[0].life, TurnLife::Running);

    touch(&dir, "go");
    let applied = until(&chat, &mut watch, "the model to change", |chat| {
        finished(chat) && current(chat, SettingKind::Model) == Some(&choice("large"))
    })
    .await;
    assert_eq!(applied.turns[0].model, model("small", "Small"));
    assert_eq!(
        applied.settings.own,
        [SettingKind::Model, SettingKind::Effort]
    );
    chat.send(UserMessage::text("Two")).await.unwrap();
    let done = until(&chat, &mut watch, "the second turn", |chat| {
        chat.turns.len() == 2 && finished(chat)
    })
    .await;
    assert_eq!(words(&done.turns[1]), "Thorough");
    assert_eq!(done.turns[1].model, model("large", "Large"));
    assert!(notes(&done).is_empty(), "{done:#?}");
    shut_down(&caf, running).await;

    // Sent only once the first turn had ended.
    let record = dir.record();
    let ended = (record.iter())
        .position(|line| line["note"] == "the first turn ends")
        .expect("the first turn ended");
    let set = (record.iter())
        .position(|line| line["received"]["method"] == "session/set_config_option")
        .expect("the model was set");
    assert!(ended < set, "{record:#?}");
    assert!(!any_failed(&dir), "{record:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hosts_defaults_are_sent_and_one_the_agent_doesnt_offer_is_dropped_quietly() {
    let dir = Dir::new();
    let mut steps = Vec::new();
    steps.extend(set_to(
        "model",
        json!("large"),
        &options("large", "low", false),
    ));
    steps.extend(set_to("fast", json!(true), &options("large", "low", true)));
    steps.extend([prompt(), say("s1", "Hello"), end_turn()]);
    let mut script = offering(&steps);
    // The session opens once the test has seen the choices waiting.
    script.insert(4, wait_file(&dir, "go"));
    let script = dir.script("script.jsonl", &script);
    let mut agent = fake(&dir, &script);
    agent.settings = vec![SettingChoice::model("large"), SettingChoice::effort("max")];
    let (caf, running) = Caf::start(config_for(host(), vec![agent]));
    let chat = caf
        .open_chat(choosing(&dir, vec![SettingChoice::fast(true)]))
        .await
        .unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();

    // Before the agent has said what it offers, every choice waits.
    let waiting = picture(&chat).await;
    assert!(waiting.settings.offered.is_empty(), "{waiting:#?}");
    assert_eq!(chosen(&waiting, SettingKind::Model), Some(&choice("large")));
    assert_eq!(chosen(&waiting, SettingKind::Effort), Some(&choice("max")));
    assert_eq!(
        chosen(&waiting, SettingKind::Fast),
        Some(&SettingValue::Switch(true))
    );
    touch(&dir, "go");

    let done = until(&chat, &mut watch, "the turn to finish", finished).await;
    assert_eq!(done.turns[0].model, model("large", "Large"));
    assert_eq!(current(&done, SettingKind::Effort), Some(&choice("low")));
    assert_ne!(
        chosen(&done, SettingKind::Effort),
        Some(&choice("max")),
        "the default the agent doesn't offer is gone"
    );
    assert_eq!(
        done.settings.own,
        [SettingKind::Fast],
        "the host's defaults aren't the chat's own"
    );
    assert!(notes(&done).is_empty(), "dropped quietly: {done:#?}");
    shut_down(&caf, running).await;
    assert!(!any_failed(&dir), "{:#?}", dir.record());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_choice_the_agent_refuses_fails_the_waiting_message_with_send_again() {
    let dir = Dir::new();
    let script = dir.script("script.jsonl", &offering(&[
        json!({"wait": {"method": "session/set_config_option", "expect": {"params": {"configId": "model", "value": "large"}}}}),
        wait_file(&dir, "go"),
        // As the ACP SDK sends an adapter's plain thrown error.
        json!({"respond": {"error": {"code": -32603, "message": "Internal error", "data": {"details": "Large isn't on your plan"}}}}),
        prompt(),
        say("s1", "Small it is"),
        end_turn(),
    ]));
    let (caf, running) = Caf::start(config_for(host(), vec![fake(&dir, &script)]));
    let chat = caf
        .open_chat(choosing(&dir, vec![SettingChoice::model("large")]))
        .await
        .unwrap();
    let mut watch = chat.watch();
    let turn = chat.send(UserMessage::text("Hi")).await.unwrap();
    touch(&dir, "go");

    let failed = until(&chat, &mut watch, "the message to fail", |chat| {
        (chat.turns.iter()).any(|each| each.id == turn && matches!(each.life, TurnLife::Failed(_)))
    })
    .await;
    let TurnLife::Failed(failure) = &failed
        .turns
        .iter()
        .find(|each| each.id == turn)
        .unwrap()
        .life
    else {
        unreachable!()
    };
    assert_eq!(failure.reason, FailureReason::SettingNotApplied);
    assert_eq!(failure.remedy, Some(Remedy::SendAgain));
    assert_eq!(
        failure.agent_said.as_deref(),
        Some("Large isn't on your plan")
    );
    assert_eq!(
        notes(&failed),
        [&Note::NotApplied {
            choice: SettingChoice::model("large"),
            name: None
        }]
    );
    assert_eq!(
        chosen(&failed, SettingKind::Model),
        Some(&choice("small")),
        "back to what the agent uses"
    );
    assert!(failed.settings.own.is_empty(), "no longer the chat's own");

    // Sent again, it goes on what the agent uses.
    let again = chat.send(UserMessage::text("Hi")).await.unwrap();
    let done = until(&chat, &mut watch, "the message sent again", finished).await;
    let last = done.turns.last().unwrap();
    assert_eq!((last.id, words(last)), (again, "Small it is".to_owned()));
    assert_eq!(last.model, model("small", "Small"));
    shut_down(&caf, running).await;
    assert!(!any_failed(&dir), "{:#?}", dir.record());
}

/// The fake started by a shell that runs it as it is told: here, a
/// different script the second time. Its profile's status command, which
/// CAF runs as this command with `auth status` after it, runs the fake's
/// own.
fn fake_by_shell(dir: &Dir, shell: &str) -> AgentSpec {
    let mut command = SpawnCommand::new("/bin/sh");
    let shell = format!(r#"if [ "$0" = auth ]; then exec "$FAKE" auth "$@"; fi; {shell}"#);
    command.args = vec!["-c".into(), shell.into()];
    command.env = vec![
        EnvChange::Set("FAKE".into(), caf_fake_agent::binary().into()),
        EnvChange::Set("DIR".into(), dir.0.clone().into()),
    ];
    AgentSpec::command("fake", command, ProfileChoice::Builtin(Builtin::Fake))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_the_agent_never_answers_restarts_its_adapter_and_goes_again() {
    let dir = Dir::new();
    // Takes the change, and never answers it.
    dir.script(
        "first.jsonl",
        &offering(&[json!({"wait": {"method": "session/set_config_option"}})]),
    );
    // The same session opened again, however CAF asks, then the change
    // sent to it first.
    let opened = json!({"respond": {"result": {"sessionId": "s1", "configOptions": options("small", "low", false)}}});
    let mut second = vec![
        json!({"on": {"method": "session/load", "do": [opened]}}),
        json!({"on": {"method": "session/new", "do": [opened]}}),
    ];
    second.extend(handshake("s1").into_iter().take(3));
    second.extend(set_to(
        "model",
        json!("large"),
        &options("large", "low", false),
    ));
    second.extend([prompt(), say("s1", "Back"), end_turn()]);
    dir.script("second.jsonl", &second);
    let shell = r#"if [ -e "$DIR/started" ]; then script=second; else : > "$DIR/started"; script=first; fi
exec "$FAKE" "$DIR/$script.jsonl" --record "$DIR/record.jsonl""#;
    let mut host = host();
    host.timers.request = std::time::Duration::from_secs(2);
    let (caf, running) = Caf::start(config_for(host, vec![fake_by_shell(&dir, shell)]));
    let chat = caf
        .open_chat(choosing(&dir, vec![SettingChoice::model("large")]))
        .await
        .unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();

    let done = until(&chat, &mut watch, "the turn to finish", finished).await;
    assert_eq!(done.turns.len(), 1, "nothing failed: {done:#?}");
    assert_eq!(words(&done.turns[0]), "Back");
    assert_eq!(done.turns[0].model, model("large", "Large"));
    shut_down(&caf, running).await;
    let started = (dir.record().iter())
        .filter(|line| line.get("started").is_some())
        .count();
    assert_eq!(started, 2, "restarted once: {:#?}", dir.record());
    assert!(!any_failed(&dir), "{:#?}", dir.record());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn learning_an_agents_settings_opens_a_session_reads_it_and_lets_it_go() {
    let dir = Dir::new();
    let mut steps = handshake_offering("learn");
    // Answering `session/new` once the test has seen it learning.
    let answer = steps.pop().unwrap();
    steps.pop();
    steps.extend([
        json!({"wait": {"method": "session/new", "expect": {"params": {"cwd": "/", "mcpServers": []}}}}),
        wait_file(&dir, "go"),
        answer,
    ]);
    let script = dir.script("script.jsonl", &steps);
    let (caf, running) = Caf::start(config_for(host(), vec![fake(&dir, &script)]));
    let fake = AgentName("fake".into());
    let mut agents = caf.watch_agents();
    let statuses = agents.next().await.unwrap();
    assert_eq!(statuses[0].settings, KnownSettings::Unknown);

    caf.learn_settings(&fake).await.unwrap();
    let waited = tokio::time::timeout(PATIENCE, async {
        while let Some(statuses) = agents.next().await {
            if statuses[0].settings == KnownSettings::Learning {
                return;
            }
        }
    })
    .await;
    assert!(waited.is_ok(), "it's learning");
    touch(&dir, "go");
    let known = tokio::time::timeout(PATIENCE, async {
        while let Some(statuses) = agents.next().await {
            if let KnownSettings::Known(known) = &statuses[0].settings {
                return known.clone();
            }
        }
        panic!("CAF ended learning")
    })
    .await
    .expect("learned in good time");
    let kinds: Vec<SettingKind> = known.iter().map(|setting| setting.kind).collect();
    assert_eq!(
        kinds,
        [SettingKind::Model, SettingKind::Effort, SettingKind::Fast]
    );
    assert_eq!(known[0].current, Some(choice("small")));

    // Nothing else needs the adapter: it's let go, which ends the session.
    let closed = tokio::time::timeout(PATIENCE, async {
        while !dir.record().iter().any(|line| line["closed"] == "stdin") {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(closed.is_ok(), "{:#?}", dir.record());
    assert_eq!(methods(&dir), ["initialize", "session/new"], "no message");
    shut_down(&caf, running).await;
    assert!(!any_failed(&dir), "{:#?}", dir.record());
}

/// A recording's header.
#[derive(serde::Deserialize)]
struct Header {
    host: Host,
    agents: Vec<AgentSpec>,
}

/// A recording's event.
#[derive(serde::Deserialize)]
struct Step {
    now: Now,
    event: Event,
}

/// Replays a recording into a fresh core.
fn replay(text: &str) -> Core {
    let mut lines = text.lines();
    let header: Header =
        serde_json::from_str(lines.next().expect("a header")).expect("the header reads");
    let mut core = Core::new(header.host, header.agents);
    let mut out = Vec::new();
    // A recording cut short says so, and ends there.
    for line in lines.take_while(|line| !line.starts_with(r#"{"stopped":"#)) {
        let step: Step = serde_json::from_str(line).expect("a step reads");
        core.handle(step.now, step.event, &mut out);
    }
    core
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recorded_run_with_settings_replays_to_the_same_state() {
    let dir = Dir::new();
    let mut steps = Vec::new();
    steps.extend(set_to(
        "model",
        json!("large"),
        &options("large", "low", false),
    ));
    steps.extend(set_to("fast", json!(true), &options("large", "low", true)));
    steps.extend([prompt(), say("s1", "One"), end_turn()]);
    steps.extend(set_to(
        "effort",
        json!("high"),
        &options("large", "high", true),
    ));
    steps.extend([
        prompt(),
        // The agent falls back to another model by itself, which has no
        // fast mode on.
        json!({"update": {"session": "s1", "update": {"sessionUpdate": "config_option_update", "configOptions": options("small", "high", false)}}}),
        say("s1", "Two"),
        end_turn(),
        json!({"wait": {"method": "session/new", "expect": {"params": {"cwd": "/"}}}}),
        json!({"respond": {"result": {"sessionId": "learn", "configOptions": options("small", "low", false)}}}),
    ]);
    let script = dir.script("script.jsonl", &offering(&steps));
    let mut agent = fake(&dir, &script);
    agent.settings = vec![SettingChoice::fast(true)];
    let recording = dir.path("run.jsonl");
    let mut config = config_for(host(), vec![agent]);
    config.record = Some(recording.clone());
    let (caf, running) = Caf::start(config);
    let chat = caf
        .open_chat(choosing(&dir, vec![SettingChoice::model("large")]))
        .await
        .unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("One")).await.unwrap();
    until(&chat, &mut watch, "the first turn", finished).await;
    chat.set_setting(SettingChoice::effort("high"))
        .await
        .unwrap();
    until(&chat, &mut watch, "the effort to change", |chat| {
        current(chat, SettingKind::Effort) == Some(&choice("high"))
    })
    .await;
    chat.send(UserMessage::text("Two")).await.unwrap();
    let live = until(&chat, &mut watch, "the second turn", |chat| {
        chat.turns.len() == 2 && finished(chat)
    })
    .await;
    assert_eq!(
        notes(&live),
        [
            &Note::AgentChanged {
                choice: SettingChoice::model("small"),
                name: None
            },
            &Note::AgentChanged {
                choice: SettingChoice::fast(false),
                name: None
            }
        ]
    );
    let fake = AgentName("fake".into());
    caf.learn_settings(&fake).await.unwrap();
    let mut agents = caf.watch_agents();
    let learned = tokio::time::timeout(PATIENCE, async {
        while let Some(statuses) = agents.next().await {
            if let KnownSettings::Known(known) = &statuses[0].settings
                && known[1].current == Some(choice("low"))
            {
                return statuses;
            }
        }
        panic!("CAF ended learning")
    })
    .await
    .expect("learned in good time");
    shut_down(&caf, running).await;
    let last = picture(&chat).await;

    let text = std::fs::read_to_string(&recording).unwrap();
    let core = replay(&text);
    assert_eq!(core.chat(chat.id), Some(&last));
    assert_eq!(
        core.chat(chat.id).map(|chat| &chat.settings),
        Some(&live.settings)
    );
    assert_eq!(core.agents()[0].settings, learned[0].settings);
    // The same recording replays the same way twice.
    assert_eq!(replay(&text).chat(chat.id), core.chat(chat.id));
    assert!(!any_failed(&dir), "{:#?}", dir.record());
}
