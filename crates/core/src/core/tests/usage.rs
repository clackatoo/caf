//! Usage: what each turn spent, recorded once as it ends, on the turn, in
//! the chat's spend and as a ledger line; the cost the agent reports and
//! the cost estimated; prices given; and a restored chat's spend.

use serde_json::{Value, json};

use super::harness::{Script, agents, assert_has, host, sent, with};
use super::{CHAT, PROC, SESSION, claude_init, plain_init};
use crate::change::Read;
use crate::chat::{Chat, NoAppData};
use crate::core::OpenChat;
use crate::event::{Event, StorageEvent};
use crate::ids::{AgentSessionId, ChangeNo, ChatId, Shelf};
use crate::save::{Header, RestoreChat, Saved};
use crate::time::Wall;
use crate::usage::{ModelPrice, PriceLayer, PriceTable, PricedModel};

/// The agent's answer to a prompt, ending its turn as `stop` says, with
/// ACP's `usage` and Claude's split by model.
fn answer(stop: &str, usage: Value, split: Value) -> Value {
    json!({ "stopReason": stop, "usage": usage, "_meta": { "quota": { "model_usage": split } } })
}

/// ACP's `usage`: `input` uncached, `read` and `written` to the cache,
/// `output`, and the total of them all.
fn usage(input: u64, read: u64, written: u64, output: u64) -> Value {
    json!({
        "inputTokens": input, "cachedReadTokens": read, "cachedWriteTokens": written,
        "outputTokens": output, "totalTokens": input + read + written + output,
    })
}

/// A row of Claude's split by model.
fn row(model: &str, input: u64, read: u64, written: u64, output: u64, thinking: u64) -> Value {
    json!({ "model": model, "token_count": {
        "totalTokens": input + read + written + output, "inputTokens": input,
        "cachedInputTokens": read, "cachedWriteTokens": written,
        "outputTokens": output, "reasoningOutputTokens": thinking,
    } })
}

/// A row of a turn's record, as it reads.
fn tokens(model: &str, input: u64, read: u64, written: u64, output: u64, thinking: Value) -> Value {
    json!({ "model": model, "tokens": {
        "input": input, "cache_read": read, "cache_write": written, "output": output, "thinking": thinking,
    } })
}

/// A `usage_update` saying the session has cost `usd` so far.
fn costing(usd: f64) -> Value {
    json!({ "sessionUpdate": "usage_update", "used": 1000, "size": 200_000, "cost": { "amount": usd, "currency": "USD" } })
}

/// The prices of the tests: Sonnet's, with a fast rate, and Haiku's, by
/// the names LiteLLM gives them.
fn prices() -> PriceTable {
    let mut sonnet = ModelPrice::new(3.0, 15.0);
    sonnet.cache_read = 0.3;
    sonnet.cache_write = 3.75;
    sonnet.fast = Some(2.0);
    PriceTable {
        models: vec![
            PricedModel::new(
                "anthropic/claude-haiku-5-5",
                ModelPrice::new(1.0, 5.0),
                PriceLayer::Bundled,
            ),
            PricedModel::new("anthropic/claude-sonnet-5-5", sonnet, PriceLayer::Own),
        ],
        fetched: None,
    }
}

impl Script {
    /// Sends `text` in `chat`, returning the id of its prompt.
    fn prompt(&mut self, chat: ChatId, text: &str) -> u64 {
        let (sent_ok, out) = self.send(chat, text);
        sent_ok.expect("sent");
        sent(&out, "session/prompt")["id"].as_u64().expect("an id")
    }

    /// The kinds of change `chat` has had, in order.
    fn change_kinds(&self, chat: ChatId) -> Vec<String> {
        let Some(Read::Changes(changes)) = self.core.read(chat, Some(ChangeNo(999))) else {
            panic!("changes")
        };
        (changes.iter())
            .map(|numbered| {
                let change = serde_json::to_value(&numbered.change).expect("changes serialise");
                change["type"].as_str().expect("a kind").to_owned()
            })
            .collect()
    }
}

/// The ledger lines in `out`.
fn ledger(out: &[Value]) -> Vec<Value> {
    (with(out, "usage").into_iter())
        .map(|usage| {
            assert_eq!(usage["usage"]["type"], "Record");
            usage["usage"]["value"].clone()
        })
        .collect()
}

#[test]
fn a_turns_usage_is_recorded_once_as_it_ends_on_the_turn_its_chats_spend_and_the_ledger() {
    let mut script = Script::with_claude();
    script.at(1000);
    let prompt = script.prompt(CHAT, "Fix the tests");
    script.update(PROC, SESSION, &costing(0.25));
    assert_eq!(
        script.chat(CHAT)["turns"][0]["spent"],
        Value::Null,
        "not yet"
    );
    script.at(2000);
    let split = json!([
        row("claude-sonnet-5-5", 10, 2000, 300, 40, 12),
        row("claude-haiku-5-5", 5, 0, 0, 7, 0),
    ]);
    let out = script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(10, 2000, 300, 40), split),
    );
    let spent = json!({
        "models": [
            tokens("claude-sonnet-5-5", 10, 2000, 300, 40, json!(12)),
            tokens("claude-haiku-5-5", 5, 0, 0, 7, json!(0)),
        ],
        "cost": { "amount": 0.25, "currency": "USD" },
        "fast": false,
    });
    let chat = script.chat(CHAT);
    assert_eq!(chat["turns"][0]["spent"], spent);
    assert_eq!(
        chat["spend"],
        json!({
            "tokens": { "input": 15, "cache_read": 2000, "cache_write": 300, "output": 47, "thinking": 12 },
            "reported": 0.25, "estimated": 0.0, "unpriced": 0, "turns": 1,
        })
    );
    // As one ledger line, as the turn ended.
    assert_eq!(
        ledger(&out),
        [json!({
            "at": super::harness::EPOCH + 2000, "app": "caf-test", "chat": CHAT,
            "turn": chat["turns"][0]["id"], "agent": "claude", "profile": "claude-agent-acp",
            "usage": spent,
        })]
    );
    let kinds = script.change_kinds(CHAT);
    let at = |kind: &str| kinds.iter().position(|each| each == kind).expect(kind);
    assert!(
        at("TurnLife") < at("TurnSpent") && at("TurnSpent") < at("Spend"),
        "{kinds:?}"
    );
    // Once: the same answer again, and what the agent says after, record
    // nothing more.
    let out = script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1, 1, 1, 1), json!([])),
    );
    assert!(ledger(&out).is_empty());
    let out = script.update(PROC, SESSION, &costing(0.5));
    assert!(ledger(&out).is_empty());
    assert_eq!(script.chat(CHAT)["turns"][0]["spent"], spent);
    assert_eq!(
        (script.change_kinds(CHAT).iter())
            .filter(|kind| *kind == "TurnSpent")
            .count(),
        1
    );
}

#[test]
fn a_stopped_turn_records_what_it_spent_too_and_with_no_split_its_one_row_is_the_turns_model() {
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("claude", "/work"));
    assert_eq!(chat, Ok(CHAT));
    let out = script.answers(
        PROC,
        sent(&out, "initialize")["id"].as_u64().expect("an id"),
        &claude_init(),
    );
    let options = json!([{ "id": "model", "name": "Model", "category": "model", "type": "select",
        "currentValue": "haiku", "options": [{ "value": "haiku", "name": "Haiku" }] }]);
    script.answers(
        PROC,
        sent(&out, "session/new")["id"].as_u64().expect("an id"),
        &json!({ "sessionId": SESSION, "configOptions": options }),
    );
    let prompt = script.prompt(CHAT, "Tell me a long story");
    script.stop(CHAT).0.expect("stopping");
    // Stopped, Claude's split is empty, but its usage counts.
    let out = script.answers(
        PROC,
        prompt,
        &answer("cancelled", usage(2, 15847, 11185, 0), json!([])),
    );
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{ "life": { "type": "Stopped" }, "spent": {
            "models": [tokens("haiku", 2, 15847, 11185, 0, Value::Null)], "cost": null, "fast": false,
        } }] }),
    );
    assert_eq!(ledger(&out).len(), 1);
}

#[test]
fn a_turn_whose_model_nobody_said_is_put_against_unknown() {
    let mut script = Script::with_claude();
    let prompt = script.prompt(CHAT, "Hello");
    let thinking =
        json!({ "inputTokens": 3, "outputTokens": 9, "thoughtTokens": 4, "totalTokens": 12 });
    script.answers(
        PROC,
        prompt,
        &json!({ "stopReason": "end_turn", "usage": thinking }),
    );
    assert_eq!(
        script.chat(CHAT)["turns"][0]["spent"]["models"],
        json!([tokens("unknown", 3, 0, 0, 9, json!(4))])
    );
}

#[test]
fn an_agent_that_says_nothing_of_what_it_spent_gives_no_record() {
    let mut script = Script::with_claude();
    let prompt = script.prompt(CHAT, "Hello");
    script.update(PROC, SESSION, &costing(0.25));
    let out = script.answers(PROC, prompt, &json!({ "stopReason": "end_turn" }));
    assert!(ledger(&out).is_empty());
    // Nor does a prompt that fails, whatever it cost.
    let prompt = script.prompt(CHAT, "Again");
    script.update(PROC, SESSION, &costing(0.5));
    let out = script.fails(
        PROC,
        prompt,
        &json!({ "code": -32603, "message": "Internal error" }),
    );
    assert!(ledger(&out).is_empty());
    let chat = script.chat(CHAT);
    assert_eq!(chat["turns"][0]["spent"], Value::Null);
    assert_eq!(chat["turns"][1]["spent"], Value::Null);
    assert_eq!(chat["spend"]["turns"], 0);
    assert!(!script.change_kinds(CHAT).iter().any(|kind| kind == "Spend"));
}

#[test]
fn a_plain_agents_split_by_model_isnt_read_its_usage_is() {
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("plain", "/work"));
    let chat = chat.expect("opened");
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let out = script.answers(
        proc,
        sent(&out, "initialize")["id"].as_u64().expect("an id"),
        &plain_init(false),
    );
    script.answers(
        proc,
        sent(&out, "session/new")["id"].as_u64().expect("an id"),
        &json!({ "sessionId": "p1" }),
    );
    let (_, out) = script.send(chat, "Hello");
    let prompt = sent(&out, "session/prompt")["id"].as_u64().expect("an id");
    let split = json!([row("claude-haiku-5-5", 1, 1, 1, 1, 0)]);
    script.answers(proc, prompt, &answer("end_turn", usage(1, 2, 3, 4), split));
    assert_eq!(
        script.chat(chat)["turns"][0]["spent"],
        json!({ "models": [tokens("unknown", 1, 2, 3, 4, Value::Null)], "cost": null, "fast": false })
    );
}

#[test]
fn a_turns_cost_is_how_much_its_sessions_rose_over_it_if_the_agent_said_during_it() {
    let mut script = Script::with_claude();
    let turn = |script: &mut Script, before: Option<f64>, after: &[Value]| {
        if let Some(before) = before {
            script.update(PROC, SESSION, &costing(before));
        }
        let prompt = script.prompt(CHAT, "Go on");
        for update in after {
            script.update(PROC, SESSION, update);
        }
        script.answers(
            PROC,
            prompt,
            &answer("end_turn", usage(1, 0, 0, 1), json!([])),
        );
    };
    let without = json!({ "sessionUpdate": "usage_update", "used": 2000, "size": 200_000 });
    // From nothing.
    turn(&mut script, None, &[costing(0.5)]);
    // An update that doesn't say the cost leaves it as it was.
    turn(
        &mut script,
        None,
        &[without.clone(), costing(0.75), without],
    );
    // Said again before the turn, then up.
    turn(&mut script, Some(0.75), &[costing(1.0)]);
    // Down: the session was replaced.
    turn(&mut script, None, &[costing(0.125)]);
    // Said during the turn, but no higher: it cost nothing.
    turn(&mut script, None, &[costing(0.125)]);
    // Not said during the turn, though it was before: it didn't say.
    turn(&mut script, Some(0.125), &[]);
    let chat = script.chat(CHAT);
    let costs: Vec<Value> = (0..5)
        .map(|at| chat["turns"][at]["spent"]["cost"]["amount"].clone())
        .collect();
    assert_eq!(chat["turns"][5]["spent"]["cost"], Value::Null);
    assert_eq!(
        costs,
        [
            json!(0.5),
            json!(0.25),
            json!(0.25),
            json!(0.125),
            json!(0.0)
        ]
    );
    assert_eq!(chat["spend"]["reported"], json!(1.125));
}

#[test]
fn a_cost_in_another_currency_is_kept_as_reported_but_estimated_in_the_spend() {
    let mut script = Script::with_claude();
    script.event(Event::Prices(prices()));
    let prompt = script.prompt(CHAT, "Hello");
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2, "cost": { "amount": 9.0, "currency": "EUR" } }),
    );
    let split = json!([row("claude-sonnet-5-5", 1_000_000, 0, 0, 0, 0)]);
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1_000_000, 0, 0, 0), split),
    );
    let chat = script.chat(CHAT);
    assert_eq!(
        chat["turns"][0]["spent"]["cost"],
        json!({ "amount": 9.0, "currency": "EUR" })
    );
    assert_has(
        &chat,
        &json!({ "spend": { "reported": 0.0, "estimated": 3.0, "unpriced": 0 } }),
    );
}

#[test]
fn fast_mode_is_as_the_settings_in_effect_said_when_the_turn_was_sent() {
    let fast = |on: bool| {
        json!([
            { "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "sonnet",
              "options": [{ "value": "sonnet", "name": "Sonnet" }] },
            { "id": "fast", "name": "Fast mode", "category": "model_config", "type": "boolean", "currentValue": on },
        ])
    };
    let mut script = Script::new(host());
    let (_, out) = script.open(OpenChat::new("claude", "/work"));
    let out = script.answers(
        PROC,
        sent(&out, "initialize")["id"].as_u64().expect("an id"),
        &claude_init(),
    );
    script.answers(
        PROC,
        sent(&out, "session/new")["id"].as_u64().expect("an id"),
        &json!({ "sessionId": SESSION, "configOptions": fast(true) }),
    );
    script.event(Event::Prices(prices()));
    let prompt = script.prompt(CHAT, "Quickly");
    // Turned off as the turn runs: it was sent fast.
    script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "config_option_update", "configOptions": fast(false) }),
    );
    let one_million = json!([row("claude-sonnet-5-5", 1_000_000, 0, 0, 0, 0)]);
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1_000_000, 0, 0, 0), one_million.clone()),
    );
    let prompt = script.prompt(CHAT, "Slowly");
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1_000_000, 0, 0, 0), one_million),
    );
    let chat = script.chat(CHAT);
    assert_eq!(chat["turns"][0]["spent"]["fast"], true);
    assert_eq!(chat["turns"][1]["spent"]["fast"], false);
    // Priced at the fast rate, then the usual one.
    assert_eq!(chat["spend"]["estimated"], json!(6.0 + 3.0));
}

#[test]
fn a_count_past_2_53_gives_the_turn_no_record_and_says_so() {
    let mut script = Script::with_claude();
    let big = 1_u64 << 53;
    let prompt = script.prompt(CHAT, "Hello");
    let split = json!([row("claude-sonnet-5-5", big, 0, 0, 1, 0)]);
    let out = script.answers(PROC, prompt, &answer("end_turn", usage(1, 0, 0, 1), split));
    assert!(ledger(&out).is_empty());
    assert_eq!(script.chat(CHAT)["turns"][0]["spent"], Value::Null);
    let warned = |out: &[Value]| {
        with(out, "report").iter().any(|report| {
            report["report"] == "Warn"
                && report["message"]
                    .as_str()
                    .is_some_and(|text| text.contains("2^53"))
        })
    };
    assert!(warned(&out), "{out:#?}");
    // So does a turn whose counts are each below it, but not added up.
    let prompt = script.prompt(CHAT, "Again");
    let half = (1_u64 << 52) + 1;
    let split = json!([
        row("claude-sonnet-5-5", half, 0, 0, 0, 0),
        row("claude-haiku-5-5", half, 0, 0, 0, 0)
    ]);
    let out = script.answers(PROC, prompt, &answer("end_turn", usage(1, 0, 0, 1), split));
    assert!(warned(&out), "{out:#?}");
    // And counts that aren't counts at all can't be read.
    let prompt = script.prompt(CHAT, "Once more");
    let bad = json!({ "stopReason": "end_turn", "usage": { "inputTokens": -1, "outputTokens": 1, "totalTokens": 0 } });
    let out = script.answers(PROC, prompt, &bad);
    assert!(
        with(&out, "report")
            .iter()
            .any(|report| report["report"] == "Warn"),
        "{out:#?}"
    );
    let chat = script.chat(CHAT);
    assert!((0..3).all(|at| chat["turns"][at]["spent"].is_null()));
    assert_eq!(chat["spend"]["turns"], 0);
}

#[test]
fn a_chats_spend_adds_up_its_turns_reported_estimated_and_unpriced() {
    let mut script = Script::with_claude();
    script.event(Event::Prices(prices()));
    // Estimated, on a model with a date, and one the table doesn't know.
    let prompt = script.prompt(CHAT, "One");
    let split = json!([
        row("claude-haiku-5-5-20261001", 1_000_000, 0, 0, 200_000, 0),
        row("made-up-1", 70, 0, 0, 7, 0),
    ]);
    script.answers(PROC, prompt, &answer("end_turn", usage(0, 0, 0, 0), split));
    // Reported.
    let prompt = script.prompt(CHAT, "Two");
    script.update(PROC, SESSION, &costing(0.5));
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1, 2, 3, 4), json!([])),
    );
    // With no cost said during it, estimated, though one was said before.
    let prompt = script.prompt(CHAT, "Three");
    let split = json!([row("claude-haiku-5-5", 1_000_000, 0, 0, 0, 0)]);
    script.answers(PROC, prompt, &answer("end_turn", usage(0, 0, 0, 0), split));
    assert_eq!(
        script.chat(CHAT)["spend"],
        json!({
            "tokens": { "input": 2_000_071, "cache_read": 2, "cache_write": 3, "output": 200_011, "thinking": 0 },
            "reported": 0.5, "estimated": 3.0, "unpriced": 77, "turns": 3,
        })
    );
}

#[test]
fn new_prices_work_every_chats_spend_out_again_and_send_it_only_where_it_changed() {
    let mut script = Script::with_claude();
    let (other, out) = script.open(OpenChat::new("claude", "/elsewhere"));
    let other = other.expect("opened");
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    script.answers(PROC, new, &json!({ "sessionId": "sess-2" }));
    // One chat's turn was reported, the other's is estimated.
    let prompt = script.prompt(CHAT, "Reported");
    script.update(PROC, SESSION, &costing(0.5));
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1_000_000, 0, 0, 0), json!([])),
    );
    let prompt = script.prompt(other, "Estimated");
    let split = json!([row("claude-haiku-5-5", 1_000_000, 0, 0, 0, 0)]);
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1_000_000, 0, 0, 0), split),
    );
    assert_eq!(
        script.core.prices(),
        &PriceTable::default(),
        "none until given"
    );
    assert_has(
        &script.chat(other),
        &json!({ "spend": { "estimated": 0.0, "unpriced": 1_000_000 } }),
    );
    let spends = |script: &Script, chat| {
        (script.change_kinds(chat).iter())
            .filter(|kind| *kind == "Spend")
            .count()
    };
    let before = (spends(&script, CHAT), spends(&script, other));
    let out = script.event(Event::Prices(prices()));
    assert_eq!(script.core.prices(), &prices());
    assert_has(
        &script.chat(other),
        &json!({ "spend": { "estimated": 1.0, "unpriced": 0 } }),
    );
    assert_eq!(
        (spends(&script, CHAT), spends(&script, other)),
        (before.0, before.1 + 1)
    );
    assert_eq!(
        with(&out, "changed")
            .iter()
            .map(|changed| changed["changed"].clone())
            .collect::<Vec<_>>(),
        [json!(other)]
    );
    // The same prices again change nothing.
    let out = script.event(Event::Prices(prices()));
    assert!(with(&out, "changed").is_empty());
}

#[test]
fn a_restored_chats_spend_is_worked_out_again_from_its_turns() {
    let mut script = Script::offering(host(), agents());
    script.event(Event::Prices(prices()));
    let (done, out) = script.restore(RestoreChat::new(CHAT, Shelf("work".to_owned())));
    done.expect("restoring");
    let spent = json!({ "models": [tokens("claude-haiku-5-5", 2_000_000, 0, 0, 0, Value::Null)], "cost": null, "fast": false });
    let chat: Chat = serde_json::from_value(json!({
        "id": CHAT, "agent": "claude", "cwd": "/work", "state": { "type": "Idle" },
        "turns": [{
            "id": 7, "starter": { "type": "User", "value": { "parts": [{ "type": "Text", "value": "Hi" }] } },
            "life": { "type": "Finished" },
            "window": { "start": Wall(1), "end": { "type": "At", "value": Wall(2) } },
            "items": [], "imported": false, "model": null, "spent": spent,
        }],
        "usage": null, "questions": [],
        // Not what its turns add up to: never trusted.
        "spend": { "tokens": { "input": 1, "cache_read": 0, "cache_write": 0, "output": 0, "thinking": null },
                   "reported": 99.0, "estimated": 0.0, "unpriced": 0, "turns": 5 },
    }))
    .expect("a chat");
    let saved = Saved {
        header: Header {
            chat: CHAT,
            agent: chat.agent.clone(),
            profile: Some("claude-agent-acp".to_owned()),
            cwd: chat.cwd.clone(),
            made: Wall(0),
            changed: Wall(2),
            label: None,
            first: None,
            turns: 1,
            closed: false,
        },
        chat,
        session: Some(AgentSessionId(SESSION.to_owned())),
        data: NoAppData,
    };
    let ticket = with(&out, "storage")[0]["storage"]["value"]["ticket"]
        .as_u64()
        .expect("a ticket");
    script.storage(ticket, StorageEvent::Loaded(Box::new(saved)));
    assert_eq!(
        script.chat(CHAT)["spend"],
        json!({
            "tokens": { "input": 2_000_000, "cache_read": 0, "cache_write": 0, "output": 0, "thinking": null },
            "reported": 0.0, "estimated": 2.0, "unpriced": 0, "turns": 1,
        })
    );
    assert_eq!(script.change_kinds(CHAT), ["Spend"]);
}

#[test]
fn a_session_opened_again_counts_its_cost_afresh() {
    let mut script = Script::with_claude();
    let prompt = script.prompt(CHAT, "One");
    script.update(PROC, SESSION, &costing(0.5));
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1, 0, 0, 1), json!([])),
    );
    // The agent lost the session: it's resumed, and the message sent again.
    let prompt = script.prompt(CHAT, "Two");
    let out = script.fails(
        PROC,
        prompt,
        &json!({ "code": -32002, "message": "Resource not found" }),
    );
    let resume = sent(&out, "session/resume")["id"].as_u64().expect("an id");
    let out = script.answers(PROC, resume, &json!({}));
    let prompt = sent(&out, "session/prompt")["id"].as_u64().expect("an id");
    // Less than the old session had said, from a session that counts
    // from nothing.
    script.update(PROC, SESSION, &costing(0.75));
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1, 0, 0, 1), json!([])),
    );
    assert_eq!(
        script.chat(CHAT)["turns"][1]["spent"]["cost"],
        json!({ "amount": 0.75, "currency": "USD" })
    );
}

#[test]
fn with_no_split_the_row_is_the_model_claude_said_during_the_turn_else_the_turns() {
    let mut script = Script::new(host());
    let (_, out) = script.open(OpenChat::new("claude", "/work"));
    let out = script.answers(
        PROC,
        sent(&out, "initialize")["id"].as_u64().expect("an id"),
        &claude_init(),
    );
    let options = json!([{ "id": "model", "name": "Model", "category": "model", "type": "select",
        "currentValue": "haiku", "options": [{ "value": "haiku", "name": "Haiku" }] }]);
    script.answers(
        PROC,
        sent(&out, "session/new")["id"].as_u64().expect("an id"),
        &json!({ "sessionId": SESSION, "configOptions": options }),
    );
    let said = |model: &str| json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2, "_meta": { "_claude/model": model } });
    let prompt = script.prompt(CHAT, "One");
    script.update(PROC, SESSION, &said("claude-sonnet-5-5"));
    script.update(PROC, SESSION, &said("claude-haiku-5-5"));
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1, 0, 0, 1), json!([])),
    );
    // Said before this turn, not during it: the turn's model.
    let prompt = script.prompt(CHAT, "Two");
    script.answers(
        PROC,
        prompt,
        &answer("end_turn", usage(1, 0, 0, 1), json!([])),
    );
    // A split names its own models.
    let prompt = script.prompt(CHAT, "Three");
    script.update(PROC, SESSION, &said("claude-haiku-5-5"));
    let split = json!([row("claude-opus-5-5", 1, 0, 0, 1, 0)]);
    script.answers(PROC, prompt, &answer("end_turn", usage(1, 0, 0, 1), split));
    let chat = script.chat(CHAT);
    let model = |at: usize| chat["turns"][at]["spent"]["models"][0]["model"].clone();
    assert_eq!(
        [model(0), model(1), model(2)],
        [
            json!("claude-haiku-5-5"),
            json!("haiku"),
            json!("claude-opus-5-5")
        ]
    );
}
