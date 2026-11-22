//! Account limits: heard from the agent, kept per profile on every
//! agent's status, forgotten as their windows reset, cleared as the
//! account is signed out of or in to, and saved, but never what the usage
//! driver gave.

use serde_json::{Value, json};

use super::harness::{EPOCH, Script, assert_has, host, sent, with};
use super::{CHAT, PROC, SESSION};
use crate::core::OpenChat;
use crate::event::{Event, SetupEvent};
use crate::ids::ChatId;
use crate::limits::{AccountLimits, ProfileLimits};
use crate::profiles::Builtin;
use crate::time::Wall;

/// The second chat, opened after the first's session.
const OTHER: ChatId = ChatId::from_number(1003);

/// The second chat's session.
const OTHER_SESSION: &str = "sess-2";

/// The wall clock `ms` into a script, in seconds, as Claude says resets.
fn seconds(ms: i64) -> i64 {
    (EPOCH + ms) / 1000
}

/// A `usage_update` whose `_meta._claude/rateLimit` is `info`.
fn limited(info: Value) -> Value {
    json!({ "sessionUpdate": "usage_update", "used": 1000, "size": 200_000,
        "_meta": { "_claude/rateLimit": info } })
}

/// A report of Claude's five hours, `used` of 1, and week, each resetting
/// as they say, in milliseconds into the script.
fn five_and_week(five: f64, five_resets: i64, week: f64, week_resets: i64) -> Value {
    limited(json!({
        "status": "allowed", "rateLimitType": "five_hour", "utilization": five,
        "resetsAt": seconds(five_resets), "isUsingOverage": false,
        "unifiedWindows": {
            "five_hour": { "utilization": five, "resetsAt": seconds(five_resets), "status": "allowed" },
            "seven_day": { "utilization": week, "resetsAt": seconds(week_resets), "status": "allowed" },
        },
    }))
}

/// A window as a status shows it, heard `heard` milliseconds into the
/// script.
fn window(kind: &str, used: u8, resets: Option<i64>, state: &str, heard: i64) -> Value {
    json!({
        "kind": { "type": kind }, "used": used,
        "resets": resets.map(|ms| seconds(ms) * 1000),
        "state": { "type": state }, "heard": EPOCH + heard,
    })
}

/// The limits each [`SaveLimits`](crate::directive::UsageDirective::SaveLimits)
/// in `out` keeps, with its profile.
fn saved(out: &[Value]) -> Vec<Value> {
    (with(out, "usage").into_iter())
        .filter(|usage| usage["usage"]["type"] == "SaveLimits")
        .map(|usage| usage["usage"]["value"].clone())
        .collect()
}

/// Limits as the usage driver gives them: `json`, as a `ProfileLimits`
/// of Claude's.
fn given(limits: &Value) -> Event {
    let limits: AccountLimits = serde_json::from_value(limits.clone()).expect("limits read");
    Event::Limits(vec![ProfileLimits {
        profile: Builtin::ClaudeAgentAcp,
        limits,
    }])
}

impl Script {
    /// The limits `agent`'s status shows.
    fn limits(&self, agent: &str) -> Value {
        self.agent_status(agent)["limits"].clone()
    }

    /// Opens a second Claude chat on the first's adapter, its session
    /// [`OTHER_SESSION`].
    fn open_other(&mut self) {
        let (other, out) = self.open(OpenChat::new("claude", "/elsewhere"));
        assert_eq!(other, Ok(OTHER));
        let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
        self.answers(PROC, new, &json!({ "sessionId": OTHER_SESSION }));
    }
}

#[test]
fn what_claude_says_of_its_limits_is_on_every_status_of_its_profile_and_saved() {
    let mut script = Script::with_claude();
    script.at(1000);
    let out = script.update(
        PROC,
        SESSION,
        &five_and_week(0.42, 3_600_000, 0.6, 86_400_000),
    );
    let limits = json!({
        "windows": [
            window("FiveHours", 42, Some(3_600_000), "Fine", 1000),
            window("Week", 60, Some(86_400_000), "Fine", 1000),
        ],
        "extra_usage": { "in_use": false, "heard": EPOCH + 1000 },
        "cleared": null,
    });
    assert_eq!(script.limits("claude"), limits);
    // Every agent of the profile shares them, as they share an account.
    assert_eq!(script.limits("claude-npx"), limits);
    assert_eq!(
        script.limits("plain"),
        json!({ "windows": [], "extra_usage": null, "cleared": null })
    );
    assert_eq!(
        saved(&out),
        [json!({ "profile": { "type": "ClaudeAgentAcp" }, "limits": limits })]
    );
    assert_eq!(script.state()["limits"]["kept"]["claude-agent-acp"], limits);
}

#[test]
fn limits_are_heard_even_from_an_update_the_rest_of_which_doesnt_read() {
    let mut script = Script::with_claude();
    let info = json!({ "rateLimitType": "five_hour", "utilization": 0.5 });
    let out = script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "usage_update", "_meta": { "_claude/rateLimit": info } }),
    );
    assert_eq!(saved(&out).len(), 1);
    assert_eq!(script.limits("claude")["windows"][0]["used"], 50);
    // An update of any other kind says nothing of them, nor does one
    // that names no window.
    for update in [
        json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" },
            "_meta": { "_claude/rateLimit": { "rateLimitType": "seven_day", "utilization": 0.5 } } }),
        limited(json!({ "status": "allowed" })),
        json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2 }),
    ] {
        let out = script.update(PROC, SESSION, &update);
        assert!(saved(&out).is_empty(), "{update}");
    }
    assert_eq!(
        script.limits("claude")["windows"].as_array().map(Vec::len),
        Some(1)
    );
}

#[test]
fn an_agent_whose_profile_says_nothing_of_limits_has_none() {
    let mut script = Script::new(host());
    let (chat, out) = script.open(OpenChat::new("plain", "/work"));
    chat.expect("opened");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(PROC, init, &super::plain_init(false));
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    script.answers(PROC, new, &json!({ "sessionId": SESSION }));
    let out = script.update(
        PROC,
        SESSION,
        &limited(json!({ "rateLimitType": "five_hour", "utilization": 0.5 })),
    );
    assert!(saved(&out).is_empty());
    assert_eq!(script.limits("plain")["windows"], json!([]));
    assert_eq!(script.limits("claude")["windows"], json!([]));
}

#[test]
fn the_latest_news_wins_window_by_window_from_any_session() {
    let mut script = Script::with_claude();
    script.open_other();
    script.at(1000);
    script.update(
        PROC,
        SESSION,
        &five_and_week(0.1, 3_600_000, 0.3, 86_400_000),
    );
    // The other session says only its five hours: the week stays.
    script.at(2000);
    let info = json!({ "unifiedWindows": { "five_hour": { "utilization": 0.15, "resetsAt": seconds(3_600_000) } } });
    let out = script.update(PROC, OTHER_SESSION, &limited(info));
    assert_eq!(
        script.limits("claude")["windows"],
        json!([
            window("FiveHours", 15, Some(3_600_000), "Fine", 2000),
            window("Week", 30, Some(86_400_000), "Fine", 1000),
        ])
    );
    assert_eq!(saved(&out)[0]["limits"], script.limits("claude"));
    // A model's own week joins them, in its place.
    script.at(3000);
    let info = json!({ "status": "allowed_warning", "rateLimitType": "seven_day_opus", "utilization": 0.81 });
    script.update(PROC, SESSION, &limited(info));
    let kinds: Vec<Value> = (script.limits("claude")["windows"].as_array())
        .expect("windows")
        .iter()
        .map(|window| window["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        [
            json!({ "type": "FiveHours" }),
            json!({ "type": "Week" }),
            json!({ "type": "ModelWeek", "value": "opus" })
        ]
    );
}

#[test]
fn what_the_usage_driver_gives_is_merged_the_latest_winning_and_never_saved_back() {
    let mut script = Script::with_claude();
    script.at(5000);
    script.update(
        PROC,
        SESSION,
        &five_and_week(0.2, 3_600_000, 0.3, 86_400_000),
    );
    let before = script.limits("claude");
    // Older news of another app's changes nothing.
    let older = json!({
        "windows": [window("FiveHours", 90, Some(3_600_000), "Warning", 4000)],
        "extra_usage": { "in_use": true, "heard": EPOCH + 4000 }, "cleared": null,
    });
    let out = script.event(given(&older));
    assert_eq!(script.limits("claude"), before);
    assert!(saved(&out).is_empty());
    // Later news wins, window by window, and isn't saved back.
    script.at(7000);
    let later = json!({
        "windows": [
            window("FiveHours", 35, Some(3_600_000), "Fine", 6000),
            window("ExtraWeek", 12, Some(86_400_000), "Fine", 6000),
        ],
        "extra_usage": null, "cleared": null,
    });
    let out = script.event(given(&later));
    assert!(saved(&out).is_empty());
    assert_eq!(
        script.limits("claude")["windows"],
        json!([
            window("FiveHours", 35, Some(3_600_000), "Fine", 6000),
            window("Week", 30, Some(86_400_000), "Fine", 5000),
            window("ExtraWeek", 12, Some(86_400_000), "Fine", 6000),
        ])
    );
    assert_eq!(script.limits("claude")["extra_usage"]["in_use"], false);
}

#[test]
fn limits_given_before_any_agent_says_are_on_the_statuses_from_the_start() {
    let mut script = Script::new(host());
    let kept = json!({
        "windows": [window("Week", 64, Some(86_400_000), "Fine", -60_000)],
        "extra_usage": null, "cleared": null,
    });
    let out = script.event(given(&kept));
    assert!(saved(&out).is_empty());
    assert_eq!(script.limits("claude"), kept);
    assert_eq!(script.limits("claude-npx"), kept);
    // And the core wakes for its reset.
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 86_400_000 })]);
}

#[test]
fn no_window_outlives_its_reset() {
    let mut script = Script::with_claude();
    script.at(1000);
    let report = limited(json!({
        "rateLimitType": "thirty_day", "utilization": 0.05,
        "unifiedWindows": {
            "five_hour": { "utilization": 0.4, "resetsAt": seconds(3_600_000) },
            "seven_day": { "utilization": 0.3, "resetsAt": seconds(86_400_000) },
        },
    }));
    let out = script.update(PROC, SESSION, &report);
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 3_600_000 })]);
    // Not yet.
    script.at(3_599_999);
    script.woken();
    assert_eq!(
        script.limits("claude")["windows"].as_array().map(Vec::len),
        Some(3)
    );
    script.at(3_600_000);
    let out = script.woken();
    assert_eq!(
        script.limits("claude")["windows"],
        json!([
            window("Week", 30, Some(86_400_000), "Fine", 1000),
            json!({ "kind": { "type": "Other", "value": "thirty_day" }, "used": 5, "resets": null, "state": { "type": "Fine" }, "heard": EPOCH + 1000 }),
        ])
    );
    // Forgetting isn't news to save: every app forgets it alike.
    assert!(saved(&out).is_empty());
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 86_400_000 })]);
    // Woken late, by anything: the one with no reset stays.
    script.at(90_000_000);
    let out = script.update(
        PROC,
        SESSION,
        &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } }),
    );
    assert!(saved(&out).is_empty());
    assert_eq!(
        script.limits("claude")["windows"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(script.state()["limits"]["wake"], Value::Null);
}

#[test]
fn a_window_already_reset_is_no_news_from_the_agent_or_the_usage_driver() {
    let mut script = Script::with_claude();
    script.at(10_000);
    let info = json!({ "unifiedWindows": { "five_hour": { "utilization": 0.4, "resetsAt": seconds(5000) } } });
    let out = script.update(PROC, SESSION, &limited(info));
    assert!(saved(&out).is_empty());
    assert_eq!(script.limits("claude")["windows"], json!([]));
    let stale = json!({
        "windows": [window("FiveHours", 40, Some(5000), "Fine", 1000)],
        "extra_usage": null, "cleared": null,
    });
    script.event(given(&stale));
    assert_eq!(script.limits("claude")["windows"], json!([]));
}

#[test]
fn signing_out_clears_the_profiles_limits_and_saves_the_clearing() {
    let mut script = Script::with_claude();
    script.at(1000);
    script.update(
        PROC,
        SESSION,
        &five_and_week(0.2, 3_600_000, 0.3, 86_400_000),
    );
    // A sign-out that fails clears nothing.
    script.at(2000);
    let run = ticket(&script.sign_out("claude").1, "run");
    let out = script.setup(run, SetupEvent::Ended { ok: false });
    assert!(saved(&out).is_empty());
    assert_eq!(
        script.limits("claude")["windows"].as_array().map(Vec::len),
        Some(2)
    );
    let run = ticket(&script.sign_out("claude").1, "run");
    let out = script.setup(run, SetupEvent::Ended { ok: true });
    let cleared = json!({ "windows": [], "extra_usage": null, "cleared": EPOCH + 2000 });
    assert_eq!(script.limits("claude"), cleared);
    assert_eq!(script.limits("claude-npx"), cleared);
    assert_eq!(
        saved(&out),
        [json!({ "profile": { "type": "ClaudeAgentAcp" }, "limits": cleared })]
    );
    assert_eq!(script.state()["limits"]["wake"], Value::Null);
    // A saved copy from before never brings them back.
    let before = json!({
        "windows": [window("FiveHours", 20, Some(3_600_000), "Fine", 1000)],
        "extra_usage": { "in_use": true, "heard": EPOCH + 1000 }, "cleared": null,
    });
    script.event(given(&before));
    assert_eq!(script.limits("claude"), cleared);
}

#[test]
fn a_sign_in_finishing_clears_the_profiles_limits_as_the_account_may_be_another() {
    let mut script = Script::with_claude();
    script.at(1000);
    script.update(
        PROC,
        SESSION,
        &five_and_week(0.2, 3_600_000, 0.3, 86_400_000),
    );
    // One that fails clears nothing.
    let sign_in = ticket(&script.sign_in("claude").1, "sign_in");
    let out = script.setup(sign_in, SetupEvent::Ended { ok: false });
    assert!(saved(&out).is_empty());
    assert_eq!(
        script.limits("claude")["windows"].as_array().map(Vec::len),
        Some(2)
    );
    script.at(2000);
    let sign_in = ticket(&script.sign_in("claude").1, "sign_in");
    let out = script.setup(sign_in, SetupEvent::Ended { ok: true });
    let cleared = json!({ "windows": [], "extra_usage": null, "cleared": EPOCH + 2000 });
    assert_eq!(script.limits("claude"), cleared);
    assert_eq!(saved(&out)[0]["limits"], cleared);
    // News heard after is kept.
    script.at(3000);
    script.update(
        PROC,
        SESSION,
        &five_and_week(0.01, 3_600_000, 0.02, 86_400_000),
    );
    assert_eq!(script.limits("claude")["windows"][0]["used"], 1);
    assert_eq!(script.limits("claude")["cleared"], EPOCH + 2000);
    // Stopped as it finished, it may still have signed in to another.
    script.at(4000);
    let sign_in = ticket(&script.sign_in("claude").1, "sign_in");
    script.stop_sign_in("claude").0.expect("stopped");
    script.setup(sign_in, SetupEvent::Ended { ok: true });
    assert_eq!(script.limits("claude")["cleared"], EPOCH + 4000);
    assert_eq!(script.limits("claude")["windows"], json!([]));
}

#[test]
fn refused_is_always_all_used() {
    let mut script = Script::with_claude();
    script.at(1000);
    let info = json!({ "status": "rejected", "rateLimitType": "five_hour", "utilization": 0.93, "resetsAt": seconds(3_600_000) });
    script.update(PROC, SESSION, &limited(info));
    assert_eq!(
        script.limits("claude")["windows"],
        json!([window("FiveHours", 100, Some(3_600_000), "Refused", 1000)])
    );
    // Whatever another app wrote.
    script.at(2000);
    let written = json!({
        "windows": [window("Week", 40, Some(86_400_000), "Refused", 2000)],
        "extra_usage": null, "cleared": null,
    });
    script.event(given(&written));
    assert_eq!(script.limits("claude")["windows"][1]["used"], 100);
}

#[test]
fn limits_never_stop_a_message_being_sent() {
    let mut script = Script::with_claude();
    let info = json!({ "status": "rejected", "rateLimitType": "five_hour", "resetsAt": seconds(3_600_000),
        "unifiedWindows": { "seven_day": { "utilization": 1.0, "status": "rejected" } } });
    script.update(PROC, SESSION, &limited(info));
    assert_eq!(
        script.limits("claude")["windows"][0]["state"]["type"],
        "Refused"
    );
    let (sent_ok, out) = script.send(CHAT, "go anyway");
    sent_ok.expect("sent");
    assert_has(
        &sent(&out, "session/prompt"),
        &json!({ "params": { "sessionId": SESSION, "prompt": [{ "text": "go anyway" }] } }),
    );
}

#[test]
fn limits_given_for_a_profile_no_agent_has_are_kept_but_shown_on_none() {
    let mut script = Script::new(host());
    let kept = AccountLimits {
        cleared: Some(Wall(EPOCH)),
        ..AccountLimits::default()
    };
    script.event(Event::Limits(vec![ProfileLimits {
        profile: Builtin::TestModels,
        limits: kept,
    }]));
    assert_eq!(
        script.state()["limits"]["kept"]["test-models"]["cleared"],
        EPOCH
    );
    for status in script.agents().as_array().expect("statuses") {
        assert_eq!(status["limits"]["cleared"], Value::Null);
    }
}

/// The ticket of the first `key` directive in `out`.
fn ticket(out: &[Value], key: &str) -> u64 {
    with(out, key)[0][key].as_u64().expect("a ticket")
}

#[test]
fn woken_before_the_wall_clock_says_a_reset_has_passed_the_core_waits_again() {
    use crate::core::Core;
    use crate::directive::{Directive, TimerDirective};
    use crate::time::{Mono, Now};

    let mut core: Core = Core::starting_ids_at(host(), super::harness::agents(), 1000);
    let minute = 60_000_000_000;
    // The wake-up asked for, if one was.
    let wake_in = |out: &[Directive]| {
        (out.iter()).find_map(|directive| match directive {
            Directive::Timer(TimerDirective::WakeAt(at)) => *at,
            _ => None,
        })
    };
    let kept = json!({
        "windows": [window("FiveHours", 40, Some(3_600_000), "Fine", 0)],
        "extra_usage": null, "cleared": null,
    });
    let mut out = Vec::new();
    let start = Now {
        wall: Wall(EPOCH),
        mono: Mono(0),
    };
    core.handle(start, given(&kept), &mut out);
    assert_eq!(wake_in(&out), Some(Mono(60 * minute)));
    // The steady clock says an hour has gone, the wall clock a minute
    // less: the window stays, and the core waits that minute.
    let mut out = Vec::new();
    let early = Now {
        wall: Wall(EPOCH + 59 * 60_000),
        mono: Mono(60 * minute),
    };
    core.handle(early, Event::Woken, &mut out);
    assert_eq!(core.agents()[0].limits.windows.len(), 1);
    assert_eq!(wake_in(&out), Some(Mono(61 * minute)));
    let mut out = Vec::new();
    let later = Now {
        wall: Wall(EPOCH + 60 * 60_000),
        mono: Mono(61 * minute),
    };
    core.handle(later, Event::Woken, &mut out);
    assert!(core.agents()[0].limits.windows.is_empty());
    assert_eq!(wake_in(&out), None);
    assert_eq!(core.inspect()["wake"], Value::Null);
}

#[test]
fn history_a_session_replays_says_nothing_of_limits() {
    use crate::core::SessionChoice;
    use crate::ids::AgentSessionId;

    let mut script = Script::new(host());
    let mut open = OpenChat::new("claude", "/work");
    open.session = SessionChoice::Continue(AgentSessionId("old".into()));
    let (chat, out) = script.open(open);
    chat.expect("opened");
    // A Claude that can load sessions, but not resume them, so its
    // history is replayed.
    let mut init = super::claude_init();
    init["agentCapabilities"]["sessionCapabilities"] = json!({});
    let init_id = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(PROC, init_id, &init);
    let load = sent(&out, "session/load")["id"].as_u64().expect("an id");
    script.at(1000);
    let report = five_and_week(0.2, 3_600_000, 0.3, 86_400_000);
    let out = script.update(PROC, "old", &report);
    assert!(saved(&out).is_empty());
    assert_eq!(script.limits("claude")["windows"], json!([]));
    // Once it's loaded, what it says is heard.
    script.answers(PROC, load, &json!({}));
    script.at(2000);
    let out = script.update(PROC, "old", &report);
    assert_eq!(saved(&out).len(), 1);
    assert_eq!(
        script.limits("claude")["windows"].as_array().map(Vec::len),
        Some(2)
    );
}
