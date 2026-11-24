//! Serving CAF's tools to a chat: the MCP server `caf` it's given, what
//! the tools driver is told to serve, the tools it may call, and what
//! it's told when its connection to them is lost.

use serde_json::{Value, json};

use super::harness::{Script, host, sent, with};
use super::{CHAT, KEY, PROC, SESSION, claude_init, default_modes};
use crate::core::{OpenChat, Refused};
use crate::event::{Event, RelayChange, ToolEvent};
use crate::ids::ChatId;
use crate::tool::{CafTool, ServedTool, Tools};

/// The MCP server `caf` as it's sent for `chat`.
pub(super) fn caf_server(chat: ChatId) -> Value {
    json!({
        "name": "caf",
        "command": "/opt/app/bin/app",
        "args": ["--relay"],
        "env": [
            { "name": "CAF_RELAY", "value": "/run/user/1000/caf/app-1.sock" },
            { "name": "CAF_RELAY_CHAT", "value": chat.to_string() },
            { "name": "CAF_RELAY_TOKEN", "value": crate::mcp::token(&KEY, chat) },
        ],
    })
}

/// Opens a Claude chat given `tools`, answering its adapter, and returns
/// what opening it gave out, then what opening its session did.
fn open_given(script: &mut Script, tools: &[CafTool]) -> (Vec<Value>, Vec<Value>) {
    let open = OpenChat::new("claude", "/work").with_tools(Tools::caf(tools.iter().copied()));
    let (chat, opened) = script.open(open);
    assert_eq!(chat, Ok(CHAT));
    let out = script.answers(PROC, 1000, &claude_init());
    (opened, out)
}

/// The notes in the chat that CAF's tools were lost.
fn lost_notes(script: &Script) -> usize {
    (script.chat(CHAT)["turns"].as_array().into_iter().flatten())
        .flat_map(|turn| turn["items"].as_array().cloned().unwrap_or_default())
        .filter(|item| item["kind"] == json!({ "type": "Note", "value": { "type": "ToolsLost" } }))
        .count()
}

#[test]
fn a_chat_given_tools_is_refused_until_something_serves_them() {
    let mut script = Script::new(host());
    let open = OpenChat::new("claude", "/work").with_tools(Tools::caf([CafTool::Read]));
    let (chat, out) = script.open(open.clone());
    assert_eq!(chat, Err(Refused::ToolsNotServed));
    assert!(with(&out, "start").is_empty(), "nothing is started");
    // None is fine.
    let (chat, out) = script.open(OpenChat::new("claude", "/work"));
    assert!(chat.is_ok());
    assert!(with(&out, "tool").is_empty(), "nothing to serve");
    script.serving();
    assert!(script.open(open).0.is_ok());
}

#[test]
fn a_chat_given_tools_gets_the_caf_server_and_says_what_its_tools_do() {
    let mut script = Script::new(host());
    assert!(
        script
            .serving()
            .iter()
            .all(|each| each.get("tool").is_none())
    );
    let (opened, out) = open_given(&mut script, &[CafTool::Read, CafTool::Bash]);
    // The tools driver is told what to serve it.
    assert_eq!(
        with(&opened, "tool"),
        [json!({ "tool": { "type": "Serve", "value": {
            "chat": CHAT,
            // TaskStop comes with Bash.
            "tools": [{ "type": "Read" }, { "type": "Bash" }, { "type": "TaskStop" }],
            "app": [],
            "cwd": "/work",
            "instructions": "caf-test's tools to read files (Read), run commands (Bash) and stop background commands (TaskStop), in the folder you're working in.",
        } } })]
    );
    let new = sent(&out, "session/new");
    assert_eq!(new["params"]["mcpServers"], json!([caf_server(CHAT)]));
    let options = &new["params"]["_meta"]["claudeCode"]["options"];
    assert_eq!(
        options["systemPrompt"],
        "You are working inside caf-test, the app the user is talking to you in. caf-test gives \
         you tools to read files (Read), run commands (Bash) and stop background commands \
         (TaskStop), in the folder you're working in, asking the user's leave where it must. You have no other tools of your own: you \
         can't do anything else, such as browse the web, except through other tools caf-test \
         gives you. Answer in Markdown."
    );
    assert_eq!(options["allowedTools"], json!(["mcp__caf"]));
    // The relay's token is the chat's, and no other's.
    let token = new["params"]["mcpServers"][0]["env"][2]["value"]
        .as_str()
        .expect("a token");
    assert!(crate::mcp::token_is(&KEY, CHAT, token));
    assert!(!crate::mcp::token_is(&KEY, ChatId::from_number(1), token));
}

#[test]
fn the_apps_own_servers_come_first_and_none_may_be_named_caf() {
    let mut script = Script::new(host());
    script.serving();
    let mut open = OpenChat::new("claude", "/work").with_tools(Tools::caf([CafTool::Edit]));
    open.mcp_servers = vec![crate::agent::McpServer::Http {
        name: "caf".into(),
        url: "http://localhost:1/mcp".into(),
        headers: Vec::new(),
    }];
    assert_eq!(script.open(open.clone()).0, Err(Refused::ReservedServer));
    open.mcp_servers = vec![crate::agent::McpServer::Http {
        name: "maps".into(),
        url: "http://localhost:1/mcp".into(),
        headers: Vec::new(),
    }];
    script.open(open).0.expect("opened");
    let out = script.answers(PROC, 1000, &claude_init());
    let servers = &sent(&out, "session/new")["params"]["mcpServers"];
    assert_eq!(servers[0]["name"], "maps");
    assert_eq!(servers[1], caf_server(CHAT));
}

#[test]
fn a_relay_that_cant_be_sent_as_it_is_refuses_the_chat() {
    let mut script = Script::new(host());
    let mut serving = super::serving();
    serving.relay.cwd = Some("/opt".into());
    script.event(Event::Serving(serving));
    let open = OpenChat::new("claude", "/work").with_tools(Tools::caf([CafTool::Read]));
    assert_eq!(script.open(open).0, Err(Refused::Unsendable));
}

#[test]
fn a_call_to_a_tool_the_chat_wasnt_given_is_refused() {
    let mut script = Script::new(host());
    script.serving();
    let (_, out) = open_given(&mut script, &[CafTool::Read]);
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    script.answers(
        PROC,
        new,
        &json!({ "modes": default_modes(), "sessionId": SESSION }),
    );
    script.send(CHAT, "go").0.expect("sent");
    let call = |tool, input| ToolEvent::Called {
        chat: CHAT,
        tool: ServedTool::Caf(tool),
        input,
        agent_call: None,
        title: None,
        bad_input: None,
    };
    let out = script.tool(1, call(CafTool::Bash, json!({ "command": "ls" })));
    assert_eq!(
        with(&out, "tool"),
        [
            json!({ "tool": { "type": "Refuse", "value": { "call": 1, "why": { "type": "NotGiven" } } } })
        ]
    );
    let out = script.tool(2, call(CafTool::Read, json!({ "file_path": "/a" })));
    let decided = with(&out, "decide");
    assert_eq!(decided.len(), 1, "a tool it was given is asked about");
    let ticket = decided[0]["decide"].as_u64().expect("a ticket");
    script.policy(ticket, crate::event::PolicyAnswer::Refuse);
}

#[test]
fn a_lost_relay_is_said_once_a_session_while_its_open() {
    let mut script = Script::with_claude_serving();
    let lost = |script: &mut Script, chat| {
        script.event(Event::Relay {
            chat,
            change: RelayChange::Lost,
        })
    };
    script.event(Event::Relay {
        chat: CHAT,
        change: RelayChange::Connected,
    });
    assert_eq!(lost_notes(&script), 0);
    lost(&mut script, CHAT);
    assert_eq!(lost_notes(&script), 1);
    lost(&mut script, CHAT);
    assert_eq!(lost_notes(&script), 1, "once a session");
    // A chat CAF doesn't have says nothing.
    lost(&mut script, ChatId::from_number(4242));
    // Nor one closing, or closed.
    script.close(CHAT).0.expect("closed");
    lost(&mut script, CHAT);
    assert_eq!(lost_notes(&script), 1);
}

#[test]
fn a_chat_with_no_tools_is_never_told_its_tools_were_lost() {
    let mut script = Script::with_claude();
    script.event(Event::Relay {
        chat: CHAT,
        change: RelayChange::Lost,
    });
    assert_eq!(lost_notes(&script), 0);
}

#[test]
fn a_second_word_on_serving_is_ignored() {
    let mut script = Script::new(host());
    script.serving();
    let mut other = super::serving();
    other.key = crate::mcp::Key([9; 32]);
    let out = script.event(Event::Serving(other));
    assert_eq!(with(&out, "report").len(), 1, "{out:#?}");
    let (_, out) = open_given(&mut script, &[CafTool::Read]);
    assert_eq!(
        sent(&out, "session/new")["params"]["mcpServers"],
        json!([caf_server(CHAT)]),
        "the first stands"
    );
}

#[test]
fn neither_the_key_nor_a_token_is_ever_shown() {
    let script = Script::with_claude_serving();
    let key = "07".repeat(32);
    let token = crate::mcp::token(&KEY, CHAT);
    let state = script.core.inspect().to_string();
    assert!(!state.contains(&key), "the key is hidden in the state");
    assert!(!state.contains(&token), "the token is hidden in the state");
    assert_eq!(script.core.inspect()["serving"]["key"], crate::mcp::HIDDEN);
    let run = format!("{:?}", script.core.runs);
    assert!(
        !run.contains(&token),
        "the token is hidden in a run's Debug"
    );
    // Though the agent is sent it.
    assert_eq!(
        script.core.runs[&CHAT].mcp_sent.0,
        [caf_server(CHAT)],
        "the agent is sent the real token"
    );
}

#[test]
fn a_lost_relay_is_said_again_once_the_session_opens_again() {
    let mut script = Script::with_claude_serving();
    let lost = |script: &mut Script| {
        script.event(Event::Relay {
            chat: CHAT,
            change: RelayChange::Lost,
        })
    };
    lost(&mut script);
    assert_eq!(lost_notes(&script), 1);
    // The adapter dropped the session: it's resumed, with the same server.
    let out = script.send(CHAT, "go").1;
    let prompt = sent(&out, "session/prompt")["id"].as_u64().expect("an id");
    let error = json!({ "code": -32002, "message": "Resource not found" });
    let out = script.fails(PROC, prompt, &error);
    let resume = sent(&out, "session/resume");
    assert_eq!(resume["params"]["mcpServers"], json!([caf_server(CHAT)]));
    let id = resume["id"].as_u64().expect("an id");
    script.answers(PROC, id, &json!({ "modes": default_modes() }));
    lost(&mut script);
    assert_eq!(lost_notes(&script), 2, "a new session is told afresh");
    lost(&mut script);
    assert_eq!(lost_notes(&script), 2);
}
