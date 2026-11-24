//! The app's own tools, which CAF serves beside its own: which a chat may
//! be given, what it and the tools driver are told of them, and their
//! calls, from the driver's report to the app's item on them.

use serde_json::{Value, json};

use super::harness::{Script, assert_has, host, sent, with};
use super::served::{kind, one_told, the_tool, ticket, told, tools};
use super::serving::caf_server;
use super::{CHAT, PROC, SESSION, claude_init, default_modes};
use crate::core::{OpenChat, Refused};
use crate::event::{PolicyAnswer, ToolEvent};
use crate::tool::{CafTool, Piece, ServedTool, Text, Tools};

/// What the tests' `word_count` says of itself, as the policy is told.
fn word_count_hints() -> Value {
    json!({ "read_only": true, "destructive": false, "open_world": false, "kind": kind("Search") })
}

/// Opens a Claude chat given `tools`, answering its adapter up to its
/// session: what opening it, then its session, gave out.
fn open_given(script: &mut Script, tools: Tools) -> (Vec<Value>, Vec<Value>) {
    let (chat, opened) = script.open(OpenChat::new("claude", "/work").with_tools(tools));
    assert_eq!(chat, Ok(CHAT));
    let out = script.answers(PROC, 1000, &claude_init());
    (opened, out)
}

/// A Claude chat given `tools`, with a turn running.
fn running(tools: Tools) -> Script {
    let mut script = Script::new(host());
    script.serving();
    let (_, out) = open_given(&mut script, tools);
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    script.answers(
        PROC,
        new,
        &json!({ "modes": default_modes(), "sessionId": SESSION }),
    );
    script.send(CHAT, "go").0.expect("sent");
    script
}

/// The agent calls the app's `name` with `input`, the tool titling it
/// `title`, or saying why it doesn't take it.
fn called(name: &str, input: Value, title: Option<&str>, bad_input: Option<&str>) -> ToolEvent {
    ToolEvent::Called {
        chat: CHAT,
        tool: ServedTool::App(name.to_owned()),
        input,
        agent_call: Some("toolu_1".to_owned()),
        title: title.map(str::to_owned),
        bad_input: bad_input.map(str::to_owned),
    }
}

/// A call's end, as the driver reports it, with `item`.
fn finished<A: crate::chat::AppItem>(done: bool, text: &str, item: Option<A>) -> ToolEvent<A> {
    ToolEvent::Finished {
        done,
        pieces: vec![Piece::Text(Text::new(text))],
        folder: None,
        notice: None,
        item,
    }
}

#[test]
fn a_chat_given_an_app_tool_the_app_didnt_register_is_refused() {
    let mut script = Script::new(host());
    let open = |tools: Tools| OpenChat::new("claude", "/work").with_tools(tools);
    // Nothing serves tools yet, so nothing's known of the app's.
    assert_eq!(
        script.open(open(Tools::none().and_app(["nope"]))).0,
        Err(Refused::ToolsNotServed)
    );
    script.serving();
    let (chat, out) = script.open(open(
        Tools::caf([CafTool::Read]).and_app(["word_count", "nope"]),
    ));
    assert_eq!(chat, Err(Refused::UnknownTool("nope".to_owned())));
    assert!(with(&out, "start").is_empty(), "nothing is started");
    // Names are the app's exactly: CAF's own are no app's.
    for name in ["Word_count", "Read", ""] {
        assert_eq!(
            script.open(open(Tools::none().and_app([name]))).0,
            Err(Refused::UnknownTool(name.to_owned()))
        );
    }
    assert!(
        script
            .open(open(Tools::none().and_app(["word_count"])))
            .0
            .is_ok()
    );
}

#[test]
fn a_chat_given_only_app_tools_is_served_them_and_told_of_them() {
    let mut script = Script::new(host());
    script.serving();
    // Each once, in the order given, though the host's list repeats one.
    let mut tools = Tools::none();
    tools.app = vec!["trim_clip".into(), "word_count".into(), "trim_clip".into()];
    let (opened, out) = open_given(&mut script, tools);
    assert_eq!(
        with(&opened, "tool"),
        [json!({ "tool": { "type": "Serve", "value": {
            "chat": CHAT,
            "tools": [],
            "app": ["trim_clip", "word_count"],
            "cwd": "/work",
            "instructions": "caf-test's own tools: trim_clip, word_count.",
        } } })]
    );
    let new = sent(&out, "session/new");
    assert_eq!(new["params"]["mcpServers"], json!([caf_server(CHAT)]));
    assert_eq!(
        new["params"]["_meta"]["claudeCode"]["options"]["systemPrompt"],
        "You are working inside caf-test, the app the user is talking to you in. caf-test gives \
         you caf-test's own tools: trim_clip, word_count, asking the user's leave where it \
         must. You have no other tools of your own: you can't read or change files, run \
         commands or browse the web, except through other tools caf-test gives you. Answer in \
         Markdown."
    );
}

#[test]
fn cafs_tools_and_the_apps_are_named_together() {
    let mut script = Script::new(host());
    script.serving();
    let (opened, out) = open_given(
        &mut script,
        Tools::caf([CafTool::Read]).and_app(["word_count"]),
    );
    assert_eq!(
        one_told(&opened)["value"]["instructions"],
        "caf-test's tools to read files (Read), in the folder you're working in, and caf-test's \
         own tools: word_count."
    );
    assert_eq!(
        sent(&out, "session/new")["params"]["_meta"]["claudeCode"]["options"]["systemPrompt"],
        "You are working inside caf-test, the app the user is talking to you in. caf-test gives \
         you tools to read files (Read), in the folder you're working in, and caf-test's own \
         tools: word_count, asking the user's leave where it must. You have no other tools of \
         your own: you can't do anything else, such as browse the web, except through other \
         tools caf-test gives you. Answer in Markdown."
    );
}

#[test]
fn an_app_call_is_asked_about_with_its_hints_and_run_as_its_tool() {
    let mut script = running(Tools::none().and_app(["word_count"]));
    let input = json!({ "text": "one two" });
    let out = script.tool(
        1,
        called(
            "word_count",
            input.clone(),
            Some("Count words in a note"),
            None,
        ),
    );
    // Not previewed: it doesn't preview.
    assert!(told(&out).is_empty(), "{out:#?}");
    let decided = with(&out, "decide");
    assert_has(
        &json!(decided),
        &json!([{
            "chat": CHAT, "title": "Count words in a note",
            "origin": { "type": "App", "value": "word_count" },
            "action": { "type": "Tool", "value": { "name": "word_count" } },
            "input": input.to_string(),
            "hints": word_count_hints(),
        }]),
    );
    assert_has(
        &the_tool(&script),
        &json!({
            "title": "Count words in a note", "kind": kind("Search"), "status": kind("Pending"),
            "origin": { "type": "App", "value": "word_count" },
            "action": { "type": "Tool", "value": { "name": "word_count" } },
            "input": input, "locations": [], "app": null,
        }),
    );
    let out = script.policy(ticket(&out), PolicyAnswer::Allow);
    // Run as its tool, with what the model wrote, whole.
    assert_eq!(
        one_told(&out),
        json!({ "type": "Run", "value": {
            "call": 1,
            "action": { "type": "Tool", "value": { "name": "word_count" } },
            "input": input,
        } })
    );
    assert_eq!(the_tool(&script)["status"], kind("Running"));
    script.tool(1, finished(true, "2 words", None));
    assert_has(
        &the_tool(&script),
        &json!({ "status": kind("Done"), "result": [{ "type": "Text", "value": { "text": "2 words" } }] }),
    );
}

#[test]
fn an_app_call_with_no_title_has_its_tools() {
    let mut script = running(Tools::none().and_app(["word_count"]));
    let out = script.tool(1, called("word_count", json!({ "text": "a" }), None, None));
    assert_eq!(the_tool(&script)["title"], "Count words");
    script.policy(ticket(&out), PolicyAnswer::Refuse);
    assert_eq!(the_tool(&script)["status"], kind("Refused"));
}

#[test]
fn an_app_tool_that_previews_is_previewed_before_its_asked_about() {
    let mut script = running(Tools::none().and_app(["trim_clip"]));
    let input = json!({ "clip": 3, "to": 10 });
    let out = script.tool(
        1,
        called("trim_clip", input.clone(), Some("Trim clip 3"), None),
    );
    assert!(with(&out, "decide").is_empty(), "not asked yet");
    assert_eq!(
        one_told(&out),
        json!({ "type": "Preview", "value": {
            "call": 1,
            "action": { "type": "Tool", "value": { "name": "trim_clip" } },
            "input": input,
        } })
    );
    let out = script.tool(
        1,
        ToolEvent::Previewed {
            pieces: vec![Piece::Text(Text::new("3s to 10s"))],
        },
    );
    assert_has(
        &json!(with(&out, "decide")),
        &json!([{ "hints": { "read_only": false, "destructive": true, "open_world": false, "kind": kind("Edit") } }]),
    );
    assert_has(
        &the_tool(&script),
        &json!({ "kind": kind("Edit"), "result": [{ "type": "Text", "value": { "text": "3s to 10s" } }] }),
    );
    script.policy(ticket(&out), PolicyAnswer::Allow);
    script.tool(1, finished(true, "Trimmed", None));
}

#[test]
fn an_app_call_given_what_its_tool_doesnt_take_fails_asking_no_one() {
    let mut script = running(Tools::none().and_app(["trim_clip"]));
    let why = "missing field `clip` at line 1 column 2";
    let out = script.tool(1, called("trim_clip", json!({}), None, Some(why)));
    // No preview, no policy: the model is told why.
    assert!(with(&out, "decide").is_empty());
    assert_eq!(
        one_told(&out),
        json!({ "type": "Refuse", "value": { "call": 1, "why": { "type": "BadInput", "value": why } } })
    );
    let tool = the_tool(&script);
    assert_has(
        &tool,
        &json!({
            "title": "Trim a clip", "status": kind("Failed"), "decision": kind("NotAsked"),
            "origin": { "type": "App", "value": "trim_clip" },
            "result": [{ "type": "Text", "value": { "text": why } }],
        }),
    );
    assert_ne!(tool["end"]["type"], "Open", "it ended");
    // Claude's report of it after changes nothing of it.
    script.update(
        PROC,
        SESSION,
        &json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "toolu_1", "status": "completed",
            "_meta": { "claudeCode": { "toolName": "mcp__caf__trim_clip" } },
        }),
    );
    assert_eq!(the_tool(&script)["status"], kind("Failed"));
}

#[test]
fn a_call_to_an_app_tool_the_chat_wasnt_given_is_refused() {
    let mut script = running(Tools::caf([CafTool::Read]).and_app(["word_count"]));
    for (call, name) in [(1, "trim_clip"), (2, "nope")] {
        let out = script.tool(call, called(name, json!({}), Some("x"), None));
        assert_eq!(
            one_told(&out),
            json!({ "type": "Refuse", "value": { "call": call, "why": { "type": "NotGiven" } } })
        );
    }
    assert!(tools(&script).is_empty());
}

#[test]
fn claudes_report_of_an_app_call_joins_it() {
    let mut script = running(Tools::none().and_app(["word_count"]));
    let input = json!({ "text": "a b c" });
    script.update(
        PROC,
        SESSION,
        &json!({
            "sessionUpdate": "tool_call", "toolCallId": "toolu_1", "title": "mcp__caf__word_count",
            "kind": "other", "status": "pending", "rawInput": input,
            "_meta": { "claudeCode": { "toolName": "mcp__caf__word_count" } },
        }),
    );
    // Known for the app's at once, from its server and the chat's tools.
    assert_has(
        &the_tool(&script),
        &json!({ "origin": { "type": "App", "value": "word_count" } }),
    );
    let started = the_tool(&script)["id"].clone();
    let out = script.tool(1, called("word_count", input, Some("Count 3 words"), None));
    let tool = the_tool(&script);
    assert_eq!(tool["id"], started, "one call, one item");
    assert_eq!(tool["title"], "Count 3 words");
    script.policy(ticket(&out), PolicyAnswer::Allow);
    script.tool(1, finished(true, "3", None));
}

#[test]
fn a_tool_of_cafs_server_the_chat_wasnt_given_is_another_servers() {
    let mut script = running(Tools::caf([CafTool::Read]));
    script.update(
        PROC,
        SESSION,
        &json!({
            "sessionUpdate": "tool_call", "toolCallId": "toolu_9", "title": "mcp__caf__word_count",
            "kind": "other", "status": "pending", "rawInput": {},
            "_meta": { "claudeCode": { "toolName": "mcp__caf__word_count" } },
        }),
    );
    assert_has(
        &the_tool(&script),
        &json!({ "origin": { "type": "Server", "value": { "server": "caf", "tool": "word_count" } } }),
    );
}

/// An app's item: what one of its tools made.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Trim {
    /// The clip trimmed.
    clip: u64,
}

impl crate::chat::AppItem for Trim {
    fn summary(&self) -> String {
        format!("Trimmed clip {}", self.clip)
    }
}

#[test]
fn the_apps_item_lands_on_its_tools_call_and_never_on_cafs() {
    let mut script = running(Tools::caf([CafTool::Read]).and_app(["word_count"]));
    let out = script.tool(
        1,
        called("word_count", json!({ "text": "a" }), Some("Count"), None),
    );
    script.policy(ticket(&out), PolicyAnswer::Allow);
    let read = ToolEvent::Called {
        chat: CHAT,
        tool: ServedTool::Caf(CafTool::Read),
        input: json!({ "file_path": "/work/a" }),
        agent_call: Some("toolu_2".to_owned()),
        title: None,
        bad_input: None,
    };
    let out = script.tool(2, read);
    script.policy(ticket(&out), PolicyAnswer::Allow);
    // An app with items of its own finishes both, each with one.
    let (mut core, now) = script.copy_as::<Trim>();
    let mut out = Vec::new();
    let event = |call, item| crate::event::Event::Tool {
        call: crate::ids::CallId(call),
        event: finished(true, "done", Some(item)),
    };
    core.handle(now, event(1, Trim { clip: 3 }), &mut out);
    core.handle(now, event(2, Trim { clip: 4 }), &mut out);
    let warned: Vec<String> = (out.iter())
        .filter_map(|directive| match directive {
            crate::directive::Directive::Report(report)
                if report.level == crate::directive::Level::Warn =>
            {
                Some(report.message.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        warned,
        ["ignored the app's item on call 2, which isn't to one of the app's tools"]
    );
    let chat = serde_json::to_value(core.chat(CHAT).expect("the chat")).expect("JSON");
    let calls: Vec<&Value> = (chat["turns"][0]["items"].as_array().expect("items").iter())
        .filter(|item| item["kind"]["type"] == "Tool")
        .map(|item| &item["kind"]["value"])
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["app"], json!({ "clip": 3 }));
    assert_eq!(calls[0]["status"], kind("Done"));
    assert_eq!(calls[1]["app"], Value::Null, "CAF's own tool's");
    assert_eq!(calls[1]["status"], kind("Done"));
    // This script's calls end too.
    script.tool(1, finished(true, "done", None));
    script.tool(2, finished(true, "done", None));
}

#[test]
fn a_chat_given_only_app_tools_is_told_its_tools_were_lost() {
    let mut script = running(Tools::none().and_app(["word_count"]));
    script.event(crate::event::Event::Relay {
        chat: CHAT,
        change: crate::event::RelayChange::Lost,
    });
    let notes = (script.chat(CHAT)["turns"].as_array().into_iter().flatten())
        .flat_map(|turn| turn["items"].as_array().cloned().unwrap_or_default())
        .filter(|item| item["kind"] == json!({ "type": "Note", "value": { "type": "ToolsLost" } }))
        .count();
    assert_eq!(notes, 1);
}
