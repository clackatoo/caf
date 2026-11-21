//! An agent's settings: what it offers, read from its config options or
//! its older `models` list; choices sent before any message, model first,
//! and never while a turn runs; what can't be applied; what the agent
//! changes by itself; where choices come from; a turn's model; saving
//! them; and learning what an agent offers before any chat.

use serde_json::{Value, json};

use super::harness::{Script, agents, assert_has, host, sent, with, writes};
use super::{CHAT, PROC, SESSION, claude_init, plain_init};
use crate::agent::{AgentSpec, SpawnCommand};
use crate::chat::NoAppData;
use crate::core::{OpenChat, Refused};
use crate::event::{AgentEvent, StorageEvent};
use crate::ids::{ChatId, Shelf};
use crate::profiles::{AUTH_REQUIRED, Builtin, ProfileChoice};
use crate::save::{Header, RestoreChat, Saved};
use crate::settings::{SettingChoice, SettingKind, SettingValue};
use crate::time::Wall;

/// The models Claude offers, as claude-agent-acp 0.98.0 lists them.
fn models() -> Value {
    json!([
        { "value": "default", "name": "Default (recommended)", "description": "Opus 4.7 · Most capable for complex work" },
        { "value": "sonnet", "name": "Sonnet", "description": "Sonnet 4.6 · Best for everyday tasks" },
        { "value": "haiku", "name": "Haiku", "description": "Haiku 4.5 · Fastest for quick answers" }
    ])
}

/// Claude's config options, as claude-agent-acp 0.98.0 lists them to a
/// client that takes on/off options, with `model` current: its effort
/// levels and fast mode only for a model that has them (Haiku has
/// neither), and its mode and ultracode, which CAF leaves alone.
fn claude_options(model: &str, effort: &str, fast: bool) -> Value {
    let mut options = vec![
        json!({ "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "default",
                "options": [{ "value": "default", "name": "Default" }, { "value": "plan", "name": "Plan Mode" }] }),
        json!({ "id": "model", "name": "Model", "description": "AI model to use", "category": "model",
                "type": "select", "currentValue": model, "options": models() }),
    ];
    if model != "haiku" {
        options.push(json!({ "id": "effort", "name": "Effort", "description": "Available effort levels for this model",
            "category": "thought_level", "type": "select", "currentValue": effort,
            "options": [{ "value": "default", "name": "Default" }, { "value": "low", "name": "Low" }, { "value": "high", "name": "High" }] }));
        options.push(json!({ "id": "fast", "name": "Fast mode", "description": "Faster responses on supported models",
            "category": "model_config", "type": "boolean", "currentValue": fast }));
    }
    options.push(json!({ "id": "ultracode", "name": "Ultracode", "category": "model_config", "type": "boolean", "currentValue": false }));
    json!(options)
}

/// What CAF makes of [`claude_options`].
fn claude_offered(model: &str, effort: &str, fast: bool) -> Value {
    let choice = |id: &str, name: &str, description: Value| json!({ "id": id, "name": name, "description": description, "group": null });
    let mut offered = vec![json!({
        "kind": { "type": "Model" },
        "name": "Model",
        "form": { "type": "Choices", "value": [
            choice("default", "Default (recommended)", json!("Opus 4.7 · Most capable for complex work")),
            choice("sonnet", "Sonnet", json!("Sonnet 4.6 · Best for everyday tasks")),
            choice("haiku", "Haiku", json!("Haiku 4.5 · Fastest for quick answers")),
        ] },
        "current": { "type": "Choice", "value": model },
    })];
    if model != "haiku" {
        offered.push(json!({
            "kind": { "type": "Effort" },
            "name": "Effort",
            "form": { "type": "Choices", "value": [
                choice("default", "Default", Value::Null),
                choice("low", "Low", Value::Null),
                choice("high", "High", Value::Null),
            ] },
            "current": { "type": "Choice", "value": effort },
        }));
        offered.push(json!({
            "kind": { "type": "Fast" },
            "name": "Fast mode",
            "form": { "type": "Switch" },
            "current": { "type": "Switch", "value": fast },
        }));
    }
    json!(offered)
}

/// A choice, as JSON.
fn chosen(choice: &SettingChoice) -> Value {
    serde_json::to_value(choice).expect("choices serialise")
}

/// A request's id.
#[track_caller]
fn id_of(message: &Value) -> u64 {
    message["id"].as_u64().expect("an id")
}

/// The one `session/set_config_option` in `out`, with its id.
#[track_caller]
fn set(out: &[Value]) -> (u64, Value) {
    let message = sent(out, "session/set_config_option");
    (id_of(&message), message["params"].clone())
}

/// The prompts written in `out`.
fn prompts(out: &[Value]) -> Vec<Value> {
    (writes(out).into_iter())
        .map(|(_, message)| message)
        .filter(|message| message["method"] == "session/prompt")
        .collect()
}

/// Every note in `chat`, with whose turn it's in: `Caf`, or the starter
/// of the turn it happened in.
fn notes(chat: &Value) -> Vec<(String, Value)> {
    (chat["turns"].as_array().into_iter().flatten())
        .flat_map(|turn| {
            let starter = turn["starter"]["type"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            (turn["items"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter())
            .filter(|item| item["kind"]["type"] == "Note")
            .map(move |item| (starter.clone(), item["kind"]["value"].clone()))
        })
        .collect()
}

/// A note, as JSON.
fn note(kind: &str, choice: &SettingChoice) -> Value {
    json!({ "type": kind, "value": chosen(choice) })
}

/// A `config_option_update` listing `options`.
fn update(options: &Value) -> Value {
    json!({ "sessionUpdate": "config_option_update", "configOptions": options })
}

/// The test agents, with `defaults` as the host's for Claude, and the
/// agent that lists its models the older way, `models`.
fn offering(defaults: Vec<SettingChoice>) -> Vec<AgentSpec> {
    let mut all = agents();
    all[0].settings = defaults;
    all.push(AgentSpec::command(
        "models",
        SpawnCommand::new("/opt/caf/models-acp"),
        ProfileChoice::Builtin(Builtin::TestModels),
    ));
    all.push(AgentSpec::command(
        "unknown",
        SpawnCommand::new("/opt/unknown-acp"),
        ProfileChoice::Cautious,
    ));
    all
}

/// Opening a Claude chat in `/work` that chooses `settings`.
fn choosing(settings: Vec<SettingChoice>) -> OpenChat {
    let mut open = OpenChat::new("claude", "/work");
    open.settings = settings;
    open
}

impl Script {
    /// Opens `open`, a Claude chat, and its session, which offers
    /// `options`, on a core with no chats yet. Returns what its opening
    /// gave out.
    fn open_offering(&mut self, open: OpenChat, options: &Value) -> Vec<Value> {
        let (chat, out) = self.open(open);
        assert_eq!(chat, Ok(CHAT));
        let out = self.answers(PROC, id_of(&sent(&out, "initialize")), &claude_init());
        let new = id_of(&sent(&out, "session/new"));
        self.answers(
            PROC,
            new,
            &json!({ "sessionId": SESSION, "configOptions": options }),
        )
    }
}

#[test]
fn claudes_settings_are_read_from_its_config_options_and_the_rest_left_alone() {
    let mut script = Script::new(host());
    let (_, out) = script.open(OpenChat::new("claude", "/work"));
    // It says it takes on/off options, so fast mode comes as one.
    assert_has(
        &sent(&out, "initialize"),
        &json!({ "params": { "clientCapabilities": { "session": { "configOptions": { "boolean": {} } } } } }),
    );
    assert_eq!(
        script.chat(CHAT)["settings"],
        json!({ "offered": [], "chosen": [] }),
        "nothing is known before its session opens"
    );
    let out = script.answers(PROC, 1000, &claude_init());
    let out = script.answers(
        PROC,
        id_of(&sent(&out, "session/new")),
        &json!({ "sessionId": SESSION, "configOptions": claude_options("default", "default", false) }),
    );
    assert!(writes(&out).is_empty(), "nothing is set: {out:#?}");
    // Its mode and ultracode stay hidden, as the agent set them.
    assert_eq!(
        script.chat(CHAT)["settings"],
        json!({ "offered": claude_offered("default", "default", false), "chosen": [] })
    );
    assert_eq!(
        script.agent_status("claude")["settings"],
        json!({ "type": "Known", "value": claude_offered("default", "default", false) }),
        "every session opened says what the agent offers"
    );
}

#[test]
fn a_chats_choices_are_sent_model_first_before_its_first_message() {
    let mut script = Script::new(host());
    let open = choosing(vec![
        SettingChoice::fast(true),
        SettingChoice::model("sonnet"),
        SettingChoice::effort("high"),
    ]);
    let (chat, out) = script.open(open);
    assert_eq!(chat, Ok(CHAT));
    // Kept in the order they're sent in.
    assert_eq!(
        script.chat(CHAT)["settings"]["chosen"],
        json!([
            chosen(&SettingChoice::model("sonnet")),
            chosen(&SettingChoice::effort("high")),
            chosen(&SettingChoice::fast(true))
        ])
    );
    let (turn, _) = script.send(CHAT, "Hello");
    let turn = turn.expect("sent");
    let out = script.answers(PROC, id_of(&sent(&out, "initialize")), &claude_init());
    let new = id_of(&sent(&out, "session/new"));

    script.at(100);
    let out = script.answers(
        PROC,
        new,
        &json!({ "sessionId": SESSION, "configOptions": claude_options("default", "default", false) }),
    );
    assert!(prompts(&out).is_empty(), "the message waits");
    let (id, params) = set(&out);
    assert_eq!(
        params,
        json!({ "sessionId": SESSION, "configId": "model", "value": "sonnet" })
    );
    // Waiting for the agent's answer.
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 60_100 })]);
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "id": turn.0, "life": { "type": "Waiting" } }] }),
    );

    let out = script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    let (id, params) = set(&out);
    assert_eq!(
        params,
        json!({ "sessionId": SESSION, "configId": "effort", "value": "high" })
    );
    let out = script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "high", false) }),
    );
    let (id, params) = set(&out);
    assert_eq!(
        params,
        json!({ "sessionId": SESSION, "configId": "fast", "type": "boolean", "value": true })
    );
    assert!(prompts(&out).is_empty());

    let out = script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "high", true) }),
    );
    assert_has(
        &json!(prompts(&out)),
        &json!([{ "params": { "prompt": [{ "type": "text", "text": "Hello" }] } }]),
    );
    let whole = script.chat(CHAT);
    assert_eq!(
        whole["settings"]["offered"],
        claude_offered("sonnet", "high", true)
    );
    assert_eq!(notes(&whole), [], "nothing to say");
    assert_eq!(
        whole["turns"][0]["model"],
        json!({ "id": "sonnet", "name": "Sonnet" }),
        "a turn records the model it was sent on"
    );
}

#[test]
fn a_choice_the_agent_doesnt_offer_fails_the_waiting_message_and_falls_back() {
    let mut script = Script::new(host());
    let gone = SettingChoice::model("opus[1m]");
    let (chat, out) = script.open(choosing(vec![gone.clone()]));
    chat.expect("opened");
    let (turn, _) = script.send(CHAT, "Hello");
    let turn = turn.expect("sent");
    let out = script.answers(PROC, id_of(&sent(&out, "initialize")), &claude_init());
    let out = script.answers(
        PROC,
        id_of(&sent(&out, "session/new")),
        &json!({ "sessionId": SESSION, "configOptions": claude_options("default", "default", false) }),
    );
    // Never a partial match: nothing is set, and nothing is sent.
    assert!(writes(&out).is_empty(), "{out:#?}");
    let whole = script.chat(CHAT);
    assert_eq!(
        notes(&whole),
        [("Caf".to_owned(), note("NotOffered", &gone))]
    );
    assert_has(
        &whole,
        &json!({
            "turns": [{ "starter": { "type": "Caf" } }, { "id": turn.0, "life": { "type": "Failed", "value": {
                "reason": { "type": "SettingNotApplied" }, "agent_said": null, "remedy": { "type": "SendAgain" }
            } } }],
            "settings": { "chosen": [chosen(&SettingChoice::model("default"))] }
        }),
    );
    // Sent again, it goes on what the agent uses.
    let (again, out) = script.send(CHAT, "Hello");
    assert_eq!(prompts(&out).len(), 1);
    assert_eq!(
        script.chat(CHAT)["turns"][2]["id"],
        json!(again.expect("sent").0)
    );
    assert_eq!(
        script.chat(CHAT)["turns"][2]["model"],
        json!({ "id": "default", "name": "Default (recommended)" })
    );
}

#[test]
fn a_choice_the_agent_refuses_fails_the_waiting_message_saying_why() {
    let mut script = Script::new(host());
    let out = script.open_offering(
        choosing(vec![SettingChoice::model("sonnet")]),
        &claude_options("default", "default", false),
    );
    let (id, _) = set(&out);
    let (turn, out) = script.send(CHAT, "Hello");
    let turn = turn.expect("sent");
    assert!(writes(&out).is_empty(), "it waits for the setting");
    // As the ACP SDK sends an adapter's plain thrown error.
    let out = script.fails(
        PROC,
        id,
        &json!({ "code": -32603, "message": "Internal error", "data": { "details": "Model sonnet isn't available on your plan" } }),
    );
    assert!(prompts(&out).is_empty());
    let whole = script.chat(CHAT);
    assert_eq!(notes(&whole), [], "it's offered, just refused");
    assert_has(
        &whole,
        &json!({
            "turns": [{ "id": turn.0, "life": { "type": "Failed", "value": {
                "reason": { "type": "SettingNotApplied" },
                "agent_said": "Model sonnet isn't available on your plan",
                "remedy": { "type": "SendAgain" }
            } } }],
            "settings": { "chosen": [chosen(&SettingChoice::model("default"))] }
        }),
    );

    // Refused with no message waiting: the choice just goes back.
    let (done, out) = script.set_setting(CHAT, SettingChoice::model("haiku"));
    done.expect("taken");
    let (id, _) = set(&out);
    script.fails(
        PROC,
        id,
        &json!({ "code": -32602, "message": "Invalid params" }),
    );
    assert_eq!(
        script.chat(CHAT)["settings"]["chosen"],
        json!([chosen(&SettingChoice::model("default"))])
    );
    assert_eq!(script.chat(CHAT)["turns"].as_array().map(Vec::len), Some(1));
}

#[test]
fn the_chats_choice_beats_the_hosts_default_and_a_default_not_offered_is_skipped_quietly() {
    let defaults = vec![
        SettingChoice::model("haiku"),
        SettingChoice::effort("low"),
        // Only the last of a kind counts.
        SettingChoice::model("cheap"),
    ];
    let mut script = Script::offering(host(), offering(defaults));
    let (chat, out) = script.open(choosing(vec![SettingChoice::effort("high")]));
    chat.expect("opened");
    assert_eq!(
        script.chat(CHAT)["settings"]["chosen"],
        json!([
            chosen(&SettingChoice::model("cheap")),
            chosen(&SettingChoice::effort("high"))
        ])
    );
    script.send(CHAT, "Hello").0.expect("sent");
    let out = script.answers(PROC, id_of(&sent(&out, "initialize")), &claude_init());
    let out = script.answers(
        PROC,
        id_of(&sent(&out, "session/new")),
        &json!({ "sessionId": SESSION, "configOptions": claude_options("default", "default", false) }),
    );
    // The host's model isn't offered: dropped, saying nothing.
    let (id, params) = set(&out);
    assert_eq!(params["configId"], "effort");
    assert_eq!(params["value"], "high");
    let out = script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("default", "high", false) }),
    );
    assert_eq!(prompts(&out).len(), 1, "the message goes");
    let whole = script.chat(CHAT);
    assert_eq!(notes(&whole), []);
    assert_has(
        &whole,
        &json!({ "turns": [{ "life": { "type": "Running" } }] }),
    );
    // Dropped from what's chosen, the agent's own applying.
    assert_eq!(
        whole["settings"]["chosen"],
        json!([chosen(&SettingChoice::effort("high"))])
    );

    // A chat that chooses nothing has the host's defaults: viewers see
    // them chosen, waiting, before its session opens.
    let (other, _) = script.open(OpenChat::new("claude", "/work"));
    let other = other.expect("opened");
    assert_eq!(script.chat(other)["settings"]["offered"], json!([]));
    assert_eq!(
        script.chat(other)["settings"]["chosen"],
        json!([
            chosen(&SettingChoice::model("cheap")),
            chosen(&SettingChoice::effort("low"))
        ])
    );
}

#[test]
fn a_change_during_a_turn_waits_for_it_to_end_and_only_the_latest_counts() {
    let mut script = Script::new(host());
    script.open_offering(
        OpenChat::new("claude", "/work"),
        &claude_options("default", "default", false),
    );
    let (first, out) = script.send(CHAT, "One");
    let first = first.expect("sent");
    let prompt = id_of(&prompts(&out)[0]);

    let (done, out) = script.set_setting(CHAT, SettingChoice::model("haiku"));
    done.expect("taken");
    assert!(writes(&out).is_empty(), "held while the turn runs");
    let (done, out) = script.set_setting(CHAT, SettingChoice::model("sonnet"));
    done.expect("taken");
    assert!(writes(&out).is_empty());
    assert_eq!(
        script.chat(CHAT)["settings"]["chosen"],
        json!([chosen(&SettingChoice::model("sonnet"))]),
        "waiting, as it differs from the agent's"
    );
    let (second, out) = script.send(CHAT, "Two");
    let second = second.expect("sent");
    assert!(writes(&out).is_empty());

    let out = script.answers(PROC, prompt, &json!({ "stopReason": "end_turn" }));
    assert!(prompts(&out).is_empty(), "the change goes first");
    let (id, params) = set(&out);
    assert_eq!(params["value"], "sonnet", "only the latest: {out:#?}");
    let out = script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    assert_has(
        &json!(prompts(&out)),
        &json!([{ "params": { "prompt": [{ "type": "text", "text": "Two" }] } }]),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [
            { "id": first.0, "model": { "id": "default", "name": "Default (recommended)" } },
            { "id": second.0, "model": { "id": "sonnet", "name": "Sonnet" } }
        ] }),
    );
}

#[test]
fn with_no_turn_running_a_change_is_sent_at_once_and_one_not_offered_is_refused() {
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("claude", "/work"));
    chat.expect("opened");
    // Before the agent says what it offers, any choice is taken.
    let (done, out_set) = script.set_setting(CHAT, SettingChoice::model("anything"));
    done.expect("taken");
    assert!(writes(&out_set).is_empty(), "no session yet");
    let out = script.answers(PROC, id_of(&sent(&out, "initialize")), &claude_init());
    let out = script.answers(
        PROC,
        id_of(&sent(&out, "session/new")),
        &json!({ "sessionId": SESSION, "configOptions": claude_options("default", "default", false) }),
    );
    // Not offered: said, with no message to fail.
    assert!(writes(&out).is_empty());
    assert_eq!(
        notes(&script.chat(CHAT)),
        [(
            "Caf".to_owned(),
            note("NotOffered", &SettingChoice::model("anything"))
        )]
    );

    // Now it's known, what it doesn't offer is refused.
    for choice in [
        SettingChoice::model("opus"),
        SettingChoice {
            kind: SettingKind::Fast,
            value: SettingValue::Choice("on".into()),
        },
    ] {
        assert_eq!(
            script.set_setting(CHAT, choice).0,
            Err(Refused::NoSuchSetting)
        );
    }
    let (done, out) = script.set_setting(CHAT, SettingChoice::fast(true));
    done.expect("taken");
    let (id, params) = set(&out);
    assert_eq!(
        params,
        json!({ "sessionId": SESSION, "configId": "fast", "type": "boolean", "value": true })
    );
    script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("default", "default", true) }),
    );
    // Haiku has no effort levels.
    let (done, out) = script.set_setting(CHAT, SettingChoice::model("haiku"));
    done.expect("taken");
    let (id, _) = set(&out);
    script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("haiku", "", false) }),
    );
    assert_eq!(
        script.set_setting(CHAT, SettingChoice::effort("low")).0,
        Err(Refused::NoSuchSetting)
    );
    assert_eq!(
        script
            .set_setting(ChatId::from_number(9), SettingChoice::model("haiku"))
            .0,
        Err(Refused::NoSuchChat)
    );
    script.close(CHAT).0.expect("closed");
    assert_eq!(
        script.set_setting(CHAT, SettingChoice::model("sonnet")).0,
        Err(Refused::Closed)
    );
}

#[test]
fn what_the_agent_says_during_a_change_doesnt_move_the_choice() {
    let mut script = Script::new(host());
    script.open_offering(
        OpenChat::new("claude", "/work"),
        &claude_options("default", "default", false),
    );
    let (done, out) = script.set_setting(CHAT, SettingChoice::model("sonnet"));
    done.expect("taken");
    let (id, _) = set(&out);
    // Said before the change, arriving after it was asked for.
    script.update(
        PROC,
        SESSION,
        &update(&claude_options("default", "default", false)),
    );
    let whole = script.chat(CHAT);
    assert_eq!(
        whole["settings"]["chosen"],
        json!([chosen(&SettingChoice::model("sonnet"))])
    );
    assert_eq!(notes(&whole), []);
    // Asking for the same again doesn't ask twice.
    let (done, out) = script.set_setting(CHAT, SettingChoice::model("sonnet"));
    done.expect("taken");
    assert!(writes(&out).is_empty(), "{out:#?}");

    script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    let whole = script.chat(CHAT);
    assert_eq!(
        whole["settings"],
        json!({ "offered": claude_offered("sonnet", "default", false), "chosen": [chosen(&SettingChoice::model("sonnet"))] })
    );
    assert_eq!(notes(&whole), []);
}

#[test]
fn what_the_agent_changes_by_itself_is_followed_and_said_where_it_happened() {
    let mut script = Script::new(host());
    let out = script.open_offering(
        choosing(vec![
            SettingChoice::model("sonnet"),
            SettingChoice::fast(true),
        ]),
        &claude_options("default", "default", false),
    );
    let (id, _) = set(&out);
    let out = script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    let (id, _) = set(&out);
    script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", true) }),
    );
    let (turn, out) = script.send(CHAT, "Write a poem");
    let turn = turn.expect("sent");
    let prompt = id_of(&prompts(&out)[0]);

    // Claude falls back to Haiku on a refusal, which has no fast mode.
    script.at(500);
    let out = script.update(PROC, SESSION, &update(&claude_options("haiku", "", false)));
    assert!(writes(&out).is_empty(), "nothing is sent back");
    script.answers(PROC, prompt, &json!({ "stopReason": "end_turn" }));
    let whole = script.chat(CHAT);
    assert_eq!(
        notes(&whole),
        [
            (
                "User".to_owned(),
                note("AgentChanged", &SettingChoice::model("haiku"))
            ),
            (
                "User".to_owned(),
                note("NotOffered", &SettingChoice::fast(true))
            ),
        ],
        "in the turn it happened in"
    );
    assert_has(
        &whole,
        &json!({
            "turns": [{ "id": turn.0, "model": { "id": "sonnet", "name": "Sonnet" } }],
            "settings": { "chosen": [chosen(&SettingChoice::model("haiku"))] }
        }),
    );

    // Outside any turn, CAF says it in a turn of its own.
    script.update(
        PROC,
        SESSION,
        &update(&claude_options("default", "default", false)),
    );
    let whole = script.chat(CHAT);
    assert_eq!(
        notes(&whole).last(),
        Some(&(
            "Caf".to_owned(),
            note("AgentChanged", &SettingChoice::model("default"))
        ))
    );
    assert_eq!(
        whole["settings"]["chosen"],
        json!([chosen(&SettingChoice::model("default"))])
    );
}

#[test]
fn claude_is_given_the_chats_model_again_whenever_its_session_opens_again() {
    let mut script = Script::new(host());
    let out = script.open_offering(
        choosing(vec![SettingChoice::model("sonnet")]),
        &claude_options("default", "default", false),
    );
    let (id, _) = set(&out);
    script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    let (_, out) = script.send(CHAT, "Hello");
    let prompt = id_of(&prompts(&out)[0]);
    // Claude Code under the adapter died: the session is resumed.
    let out = script.fails(
        PROC,
        prompt,
        &json!({ "code": -32603, "message": "Session not found" }),
    );
    let resume = id_of(&sent(&out, "session/resume"));
    // On resume Claude prefers the user's own settings.model, here its
    // default, to the session's.
    let out = script.answers(
        PROC,
        resume,
        &json!({ "configOptions": claude_options("default", "default", false) }),
    );
    assert!(prompts(&out).is_empty());
    let (id, params) = set(&out);
    assert_eq!(params["value"], "sonnet");
    let out = script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    assert_eq!(prompts(&out).len(), 1, "the message is sent again");
    assert_eq!(notes(&script.chat(CHAT)), []);
}

#[test]
fn an_agent_that_lists_its_models_the_older_way_is_set_with_set_model() {
    let mut script = Script::offering(host(), offering(Vec::new()));
    let mut open = OpenChat::new("models", "/work");
    open.settings = vec![SettingChoice::model("gemini-2.5-flash")];
    let (chat, out) = script.open(open);
    chat.expect("opened");
    script.send(CHAT, "Hello").0.expect("sent");
    let init = json!({ "protocolVersion": 1, "agentCapabilities": {}, "agentInfo": { "name": "models-acp", "version": "1.0.0" } });
    let out = script.answers(PROC, id_of(&sent(&out, "initialize")), &init);
    // Its profile reads only its models, whatever else it lists.
    let out = script.answers(
        PROC,
        id_of(&sent(&out, "session/new")),
        &json!({
            "sessionId": SESSION,
            "models": {
                "availableModels": [
                    { "modelId": "gemini-2.5-pro", "name": "Gemini 2.5 Pro" },
                    { "modelId": "gemini-2.5-flash", "name": "Gemini 2.5 Flash", "description": "Fast and cheap" }
                ],
                "currentModelId": "gemini-2.5-pro"
            },
            "configOptions": claude_options("default", "default", false)
        }),
    );
    let set_model = sent(&out, "session/set_model");
    assert_eq!(
        set_model["params"],
        json!({ "sessionId": SESSION, "modelId": "gemini-2.5-flash" })
    );
    let out = script.answers(PROC, id_of(&set_model), &json!({}));
    assert_eq!(prompts(&out).len(), 1);
    assert_eq!(
        script.chat(CHAT)["settings"],
        json!({
            "offered": [{
                "kind": { "type": "Model" },
                "name": "Model",
                "form": { "type": "Choices", "value": [
                    { "id": "gemini-2.5-pro", "name": "Gemini 2.5 Pro", "description": null, "group": null },
                    { "id": "gemini-2.5-flash", "name": "Gemini 2.5 Flash", "description": "Fast and cheap", "group": null }
                ] },
                "current": { "type": "Choice", "value": "gemini-2.5-flash" }
            }],
            "chosen": [chosen(&SettingChoice::model("gemini-2.5-flash"))]
        })
    );
    assert_eq!(
        script.chat(CHAT)["turns"][0]["model"],
        json!({ "id": "gemini-2.5-flash", "name": "Gemini 2.5 Flash" })
    );
    // It has no effort to set.
    assert_eq!(
        script.set_setting(CHAT, SettingChoice::effort("high")).0,
        Err(Refused::NoSuchSetting)
    );
}

#[test]
fn config_options_are_preferred_to_the_older_list_and_never_merged_with_it() {
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("plain", "/work"));
    chat.expect("opened");
    let out = script.answers(PROC, id_of(&sent(&out, "initialize")), &plain_init(false));
    // As codex's adapter lists them: both ways. Its fast mode is an on/off
    // choice, as to a client that doesn't take on/off options; its effort
    // levels come in groups.
    let options = json!([
        { "id": "m", "name": "Model", "category": "model", "type": "select", "currentValue": "gpt-5",
          "options": [{ "value": "gpt-5", "name": "GPT-5" }] },
        { "id": "reasoning", "name": "Reasoning", "category": "thought_level", "type": "select", "currentValue": "low",
          "options": [{ "group": "g1", "name": "Usual", "options": [{ "value": "low", "name": "Low" }] },
                      { "group": "g2", "name": "Slow", "options": [{ "value": "xhigh", "name": "Extra high" }] }] },
        { "id": "fast", "name": "Fast", "type": "select", "currentValue": "off",
          "options": [{ "value": "on", "name": "On" }, { "value": "off", "name": "Off" }] },
        { "id": "odd", "name": "Odd", "category": "model", "type": "boolean", "currentValue": true }
    ]);
    script.answers(
        PROC,
        id_of(&sent(&out, "session/new")),
        &json!({
            "sessionId": SESSION,
            "configOptions": options,
            "models": { "availableModels": [{ "modelId": "o3", "name": "o3" }], "currentModelId": "o3" }
        }),
    );
    assert_eq!(
        script.chat(CHAT)["settings"]["offered"],
        json!([
            { "kind": { "type": "Model" }, "name": "Model", "current": { "type": "Choice", "value": "gpt-5" },
              "form": { "type": "Choices", "value": [{ "id": "gpt-5", "name": "GPT-5", "description": null, "group": null }] } },
            { "kind": { "type": "Effort" }, "name": "Reasoning", "current": { "type": "Choice", "value": "low" },
              "form": { "type": "Choices", "value": [
                  { "id": "low", "name": "Low", "description": null, "group": "Usual" },
                  { "id": "xhigh", "name": "Extra high", "description": null, "group": "Slow" }
              ] } },
            { "kind": { "type": "Fast" }, "name": "Fast", "form": { "type": "Switch" }, "current": { "type": "Switch", "value": false } }
        ])
    );
    assert_eq!(
        script.set_setting(CHAT, SettingChoice::model("o3")).0,
        Err(Refused::NoSuchSetting),
        "never merged"
    );
    let (done, out) = script.set_setting(CHAT, SettingChoice::fast(true));
    done.expect("taken");
    let (id, params) = set(&out);
    assert_eq!(
        params,
        json!({ "sessionId": SESSION, "configId": "fast", "value": "on" })
    );
    // An answer that doesn't list them: as asked.
    script.answers(PROC, id, &json!({}));
    assert_eq!(
        script.chat(CHAT)["settings"]["offered"][2]["current"],
        json!({ "type": "Switch", "value": true })
    );
}

#[test]
fn an_agent_that_offers_nothing_fails_a_message_waiting_on_a_choice() {
    let mut script = Script::new(host());
    let mut open = OpenChat::new("plain", "/work");
    open.settings = vec![SettingChoice::model("big")];
    let (chat, out) = script.open(open);
    chat.expect("opened");
    let (turn, _) = script.send(CHAT, "Hello");
    let turn = turn.expect("sent");
    let out = script.answers(PROC, id_of(&sent(&out, "initialize")), &plain_init(false));
    let out = script.answers(
        PROC,
        id_of(&sent(&out, "session/new")),
        &json!({ "sessionId": SESSION }),
    );
    assert!(writes(&out).is_empty());
    assert_has(
        &script.chat(CHAT),
        &json!({
            "turns": [{}, { "id": turn.0, "life": { "type": "Failed", "value": { "reason": { "type": "SettingNotApplied" } } }, "model": null }],
            "settings": { "offered": [], "chosen": [] }
        }),
    );
    assert_eq!(
        script.set_setting(CHAT, SettingChoice::model("big")).0,
        Err(Refused::NoSuchSetting),
        "it has said it offers nothing"
    );
}

#[test]
fn a_setting_the_agent_doesnt_answer_in_time_isnt_applied() {
    let mut script = Script::new(host());
    script.open_offering(
        OpenChat::new("claude", "/work"),
        &claude_options("default", "default", false),
    );
    let (done, out) = script.set_setting(CHAT, SettingChoice::model("sonnet"));
    done.expect("taken");
    let (id, _) = set(&out);
    let (turn, _) = script.send(CHAT, "Hello");
    let turn = turn.expect("sent");
    script.at(60_000);
    let out = script.woken();
    assert!(prompts(&out).is_empty());
    assert_has(
        &script.chat(CHAT),
        &json!({
            "turns": [{ "id": turn.0, "life": { "type": "Failed", "value": { "reason": { "type": "SettingNotApplied" } } } }],
            "settings": { "chosen": [chosen(&SettingChoice::model("default"))] }
        }),
    );
    // Its answer, late, changes nothing.
    let before = script.chat(CHAT);
    script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    assert_eq!(script.chat(CHAT), before);
}

#[test]
fn a_change_lost_with_its_adapter_is_sent_again_as_the_session_reopens() {
    let mut script = Script::new(host());
    script.open_offering(
        OpenChat::new("claude", "/work"),
        &claude_options("default", "default", false),
    );
    script
        .set_setting(CHAT, SettingChoice::model("sonnet"))
        .0
        .expect("taken");
    script.send(CHAT, "Hello").0.expect("sent");
    script.agent(PROC, AgentEvent::OutputClosed);
    let out = script.agent(PROC, AgentEvent::Group { empty: true });
    let start = with(&out, "start");
    assert_eq!(start.len(), 1, "started again: {out:#?}");
    let proc = start[0]["start"].as_u64().expect("a process");
    let out = script.answers(proc, id_of(&sent(&out, "initialize")), &claude_init());
    let out = script.answers(
        proc,
        id_of(&sent(&out, "session/resume")),
        &json!({ "configOptions": claude_options("default", "default", false) }),
    );
    let (id, params) = set(&out);
    assert_eq!(params["value"], "sonnet");
    let out = script.answers(
        proc,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    assert_eq!(prompts(&out).len(), 1);
}

#[test]
fn a_saved_chat_keeps_its_choices_and_a_restored_one_sends_them_as_its_session_opens() {
    let mut script = Script::offering(host(), offering(vec![SettingChoice::effort("low")]));
    let mut open = choosing(vec![SettingChoice::model("sonnet")]);
    open.shelf = Some(Shelf("work".to_owned()));
    let out = script.open_offering(open, &claude_options("default", "default", false));
    let (id, _) = set(&out);
    let out = script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    let (id, _) = set(&out);
    script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("sonnet", "low", false) }),
    );
    // Written whole, a second after it changed.
    script.at(1000);
    let out = script.woken();
    let snapshot = with(&out, "storage");
    assert_eq!(snapshot.len(), 1, "{out:#?}");
    assert_eq!(snapshot[0]["storage"]["type"], "Snapshot");
    assert_eq!(
        snapshot[0]["storage"]["value"]["saved"]["chat"]["settings"],
        json!({
            "offered": claude_offered("sonnet", "low", false),
            "chosen": [chosen(&SettingChoice::model("sonnet")), chosen(&SettingChoice::effort("low"))]
        })
    );

    // Restored into a core whose host defaults to Haiku and high effort:
    // what the chat chose stays, and the host's model fills in only what
    // it chose nothing of.
    let defaults = vec![SettingChoice::effort("high"), SettingChoice::fast(true)];
    let mut script = Script::offering(host(), offering(defaults));
    script
        .restore(RestoreChat::new(CHAT, Shelf("work".to_owned())))
        .0
        .expect("restoring");
    let mut chat: crate::chat::Chat =
        serde_json::from_value(snapshot[0]["storage"]["value"]["saved"]["chat"].clone())
            .expect("a chat");
    chat.state = crate::chat::ChatState::Open {
        session: crate::ids::AgentSessionId(SESSION.to_owned()),
    };
    let saved = Saved {
        header: Header {
            chat: CHAT,
            agent: chat.agent.clone(),
            profile: Some("claude-agent-acp".to_owned()),
            cwd: chat.cwd.clone(),
            made: Wall(0),
            changed: Wall(0),
            label: None,
            first: None,
            turns: 0,
            closed: false,
        },
        chat,
        session: Some(crate::ids::AgentSessionId(SESSION.to_owned())),
        data: NoAppData,
    };
    script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    assert_eq!(
        script.chat(CHAT)["settings"]["chosen"],
        json!([
            chosen(&SettingChoice::model("sonnet")),
            chosen(&SettingChoice::effort("low")),
            chosen(&SettingChoice::fast(true))
        ])
    );
    let (_, out) = script.send(CHAT, "Go on");
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let out = script.answers(proc, id_of(&sent(&out, "initialize")), &claude_init());
    let out = script.answers(
        proc,
        id_of(&sent(&out, "session/resume")),
        &json!({ "configOptions": claude_options("default", "default", false) }),
    );
    let (id, params) = set(&out);
    assert_eq!(
        (&params["configId"], &params["value"]),
        (&json!("model"), &json!("sonnet"))
    );
    let out = script.answers(
        proc,
        id,
        &json!({ "configOptions": claude_options("sonnet", "default", false) }),
    );
    let (id, params) = set(&out);
    assert_eq!(
        (&params["configId"], &params["value"]),
        (&json!("effort"), &json!("low"))
    );
    let out = script.answers(
        proc,
        id,
        &json!({ "configOptions": claude_options("sonnet", "low", false) }),
    );
    let (id, params) = set(&out);
    assert_eq!(params["configId"], "fast");
    let out = script.answers(
        proc,
        id,
        &json!({ "configOptions": claude_options("sonnet", "low", true) }),
    );
    assert_eq!(prompts(&out).len(), 1);
}

#[test]
fn learning_an_agents_settings_opens_a_session_reads_it_and_lets_it_go() {
    let mut script = Script::new(host());
    assert_eq!(
        script.agent_status("plain")["settings"],
        json!({ "type": "Unknown" })
    );
    let (done, out) = script.learn_settings("plain");
    done.expect("learning");
    assert_eq!(
        script.agent_status("plain")["settings"],
        json!({ "type": "Learning" })
    );
    // Its connection is 1000, and its process 1001.
    assert_has(&json!(out[0]), &json!({ "start": 1001 }));
    let (again, out_again) = script.learn_settings("plain");
    again.expect("learning still");
    assert!(
        with(&out_again, "start").is_empty() && writes(&out_again).is_empty(),
        "learning once: {out_again:#?}"
    );

    let out = script.answers(1001, id_of(&sent(&out, "initialize")), &plain_init(false));
    let new = sent(&out, "session/new");
    // With no chat, it opens in `/`, with no tools.
    assert_eq!(new["params"], json!({ "cwd": "/", "mcpServers": [] }));
    let options = json!([{ "id": "model", "name": "Model", "category": "model", "type": "select",
                           "currentValue": "a", "options": [{ "value": "a", "name": "A" }] }]);
    let out = script.answers(
        1001,
        id_of(&new),
        &json!({ "sessionId": "learn", "configOptions": options }),
    );
    assert!(prompts(&out).is_empty(), "it costs nothing");
    // Nothing else needs its adapter: stopped, which ends the session.
    assert_eq!(with(&out, "close_input"), [json!({ "close_input": 1001 })]);
    assert_eq!(
        script.agent_status("plain")["settings"],
        json!({ "type": "Known", "value": [{
            "kind": { "type": "Model" }, "name": "Model",
            "form": { "type": "Choices", "value": [{ "id": "a", "name": "A", "description": null, "group": null }] },
            "current": { "type": "Choice", "value": "a" }
        }] })
    );
}

#[test]
fn learning_with_a_running_adapter_uses_it_strips_the_session_and_closes_it() {
    let mut script = Script::with_claude();
    let (done, out) = script.learn_settings("claude");
    done.expect("learning");
    assert!(with(&out, "start").is_empty(), "the chat's adapter is used");
    let new = sent(&out, "session/new");
    assert_eq!(new["params"]["cwd"], "/");
    assert_eq!(
        new["params"]["_meta"]["claudeCode"]["options"]["tools"],
        json!([]),
        "with none of its own tools"
    );
    let out = script.answers(
        PROC,
        id_of(&new),
        &json!({ "sessionId": "learn", "configOptions": claude_options("default", "default", false) }),
    );
    assert_eq!(
        sent(&out, "session/close")["params"],
        json!({ "sessionId": "learn" })
    );
    assert!(with(&out, "close_input").is_empty());
    assert_eq!(
        script.agent_status("claude")["settings"],
        json!({ "type": "Known", "value": claude_offered("default", "default", false) })
    );
    assert_eq!(script.chat(CHAT)["settings"]["offered"], json!([]));
}

#[test]
fn learning_fails_saying_why_or_is_refused() {
    let mut script = Script::offering(host(), offering(Vec::new()));
    assert_eq!(script.learn_settings("nobody").0, Err(Refused::NoSuchAgent));
    assert_eq!(script.learn_settings("unknown").0, Err(Refused::OwnTools));
    // Not known to be installed: it doesn't wait for it, as a chat does.
    let (done, out) = script.learn_settings("npm-agent");
    done.expect("taken");
    assert!(with(&out, "start").is_empty());
    assert_eq!(
        script.agent_status("npm-agent")["settings"],
        json!({ "type": "Failed", "value": "CAF hasn't looked whether the agent is installed yet" })
    );
    assert_eq!(script.state()["conns"], json!({}), "nothing is left");

    // The agent wants its user signed in.
    let (_, out) = script.learn_settings("plain");
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let out = script.answers(proc, id_of(&sent(&out, "initialize")), &plain_init(false));
    script.fails(
        proc,
        id_of(&sent(&out, "session/new")),
        &json!({ "code": AUTH_REQUIRED, "message": "Authentication required" }),
    );
    assert_eq!(
        script.agent_status("plain")["settings"],
        json!({ "type": "Failed", "value": "signing in is needed" })
    );

    // Its adapter can't start.
    let (_, out) = script.learn_settings("models");
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    script.agent(
        proc,
        AgentEvent::StartFailed {
            error: "No such file".to_owned(),
        },
    );
    assert_eq!(
        script.agent_status("models")["settings"],
        json!({ "type": "Failed", "value": "the agent couldn't start: No such file" })
    );
}

#[test]
fn learning_given_up_on_lets_its_adapter_go_and_a_late_answer_changes_nothing() {
    let mut script = Script::new(host());
    let (_, out) = script.learn_settings("plain");
    let out = script.answers(1001, id_of(&sent(&out, "initialize")), &plain_init(false));
    let new = id_of(&sent(&out, "session/new"));
    script.at(30_000);
    let out = script.woken();
    assert_eq!(with(&out, "close_input"), [json!({ "close_input": 1001 })]);
    assert_eq!(
        script.agent_status("plain")["settings"],
        json!({ "type": "Failed", "value": "the agent didn't open a session in time" })
    );
    let out = script.answers(
        1001,
        new,
        &json!({ "sessionId": "late", "configOptions": [] }),
    );
    assert!(prompts(&out).is_empty());
    assert_eq!(
        script.agent_status("plain")["settings"]["type"],
        "Failed",
        "a late answer changes nothing"
    );
}

#[test]
fn a_change_refused_after_a_newer_one_was_chosen_leaves_the_newer_to_be_sent() {
    let mut script = Script::new(host());
    script.open_offering(
        OpenChat::new("claude", "/work"),
        &claude_options("default", "default", false),
    );
    let (_, out) = script.set_setting(CHAT, SettingChoice::model("sonnet"));
    let (id, _) = set(&out);
    let (_, out) = script.set_setting(CHAT, SettingChoice::model("haiku"));
    assert!(writes(&out).is_empty(), "one at a time");
    let (turn, _) = script.send(CHAT, "Hello");
    let turn = turn.expect("sent");
    let out = script.fails(
        PROC,
        id,
        &json!({ "code": -32602, "message": "Invalid params" }),
    );
    // The refusal is of a choice that no longer counts.
    let (id, params) = set(&out);
    assert_eq!(params["value"], "haiku");
    assert_has(
        &script.chat(CHAT),
        &json!({
            "turns": [{ "id": turn.0, "life": { "type": "Waiting" } }],
            "settings": { "chosen": [chosen(&SettingChoice::model("haiku"))] }
        }),
    );
    let out = script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("haiku", "", false) }),
    );
    assert_eq!(prompts(&out).len(), 1);
    assert_eq!(notes(&script.chat(CHAT)), []);
}

#[test]
fn an_answer_to_a_setting_after_its_chat_closed_changes_nothing() {
    let mut script = Script::new(host());
    script.open_offering(
        OpenChat::new("claude", "/work"),
        &claude_options("default", "default", false),
    );
    let (_, out) = script.set_setting(CHAT, SettingChoice::model("sonnet"));
    let (id, _) = set(&out);
    script.close(CHAT).0.expect("closed");
    let before = script.chat(CHAT);
    // Set, but to something else: still nothing.
    script.answers(
        PROC,
        id,
        &json!({ "configOptions": claude_options("haiku", "", false) }),
    );
    assert_eq!(script.chat(CHAT), before);
}

#[test]
fn what_the_agent_changes_in_a_turn_it_started_is_said_in_that_turn() {
    let mut script = Script::new(host());
    script.open_offering(
        choosing(vec![SettingChoice::model("default")]),
        &claude_options("default", "default", false),
    );
    let out = script.answers(
        PROC,
        1002,
        &json!({ "configOptions": claude_options("default", "default", false) }),
    );
    assert!(writes(&out).is_empty());
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "A background task finished." } }));
    script.update(
        PROC,
        SESSION,
        &update(&claude_options("sonnet", "default", false)),
    );
    assert_eq!(
        notes(&script.chat(CHAT)),
        [(
            "Agent".to_owned(),
            note("AgentChanged", &SettingChoice::model("sonnet"))
        )]
    );
}

#[test]
fn learning_from_an_adapter_that_never_starts_says_so() {
    let mut script = Script::new(host());
    script.learn_settings("plain").0.expect("learning");
    script.at(30_000);
    let out = script.woken();
    assert_eq!(with(&out, "signal").len(), 1, "stopped: {out:#?}");
    assert_eq!(
        script.agent_status("plain")["settings"],
        json!({ "type": "Failed", "value": "the agent didn't start in time" })
    );
}

#[test]
fn a_set_answered_without_saying_what_it_is_now_is_taken_as_asked() {
    let mut script = Script::new(host());
    let mut listed = claude_options("default", "default", false);
    // An effort option that doesn't say its current value.
    if let Some(effort) = listed
        .as_array_mut()
        .and_then(|options| options.iter_mut().find(|option| option["id"] == "effort"))
        .and_then(Value::as_object_mut)
    {
        effort.remove("currentValue");
    }
    script.open_offering(OpenChat::new("claude", "/work"), &listed);
    assert_eq!(
        script.chat(CHAT)["settings"]["offered"][1]["current"],
        Value::Null
    );
    let (_, out) = script.set_setting(CHAT, SettingChoice::effort("high"));
    let (id, _) = set(&out);
    // The list again, still not saying.
    script.answers(PROC, id, &json!({ "configOptions": listed }));
    let whole = script.chat(CHAT);
    assert_eq!(
        whole["settings"]["offered"][1]["current"],
        json!({ "type": "Choice", "value": "high" }),
        "not waiting for ever"
    );
    assert_eq!(
        whole["settings"]["chosen"],
        json!([chosen(&SettingChoice::effort("high"))])
    );
    assert_eq!(notes(&whole), []);
}
