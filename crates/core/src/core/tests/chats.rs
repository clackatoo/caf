//! What a chat holds as the agent works, what the host may send it, and
//! what viewers read of it.

use serde_json::json;

use super::harness::{Script, assert_has, host, includes, sent, with};
use super::{CHAT, PROC, SESSION, claude_init};
use crate::agent::{EnvChange, McpServer, SpawnCommand};
use crate::change::{Change, Read};
use crate::chat::{Part, UserMessage};
use crate::core::{OpenChat, Refused};
use crate::ids::{ChangeNo, ChatId};

fn words(text: &str) -> serde_json::Value {
    json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } })
}

#[test]
fn the_harness_checks_what_it_says_it_does() {
    assert!(includes(&json!({ "a": [1, 2], "b": 3 }), &json!({ "a": [1, 2] })));
    assert!(!includes(&json!({ "a": [1, 2] }), &json!({ "a": [1] })));
    assert!(!includes(&json!({ "a": 1 }), &json!({ "b": 1 })));
}

#[test]
fn words_thinking_and_tools_become_items_in_order() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    script.at(10);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "Let me look" } }));
    script.at(20);
    script.update(PROC, SESSION, &words("Looking."));
    script.at(30);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read lib.rs", "kind": "read", "status": "pending", "rawInput": { "path": "lib.rs" } }));
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "in_progress" }));
    script.at(40);
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed", "rawOutput": "fn main() {}" }));
    script.update(PROC, SESSION, &words("Found it."));
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "image", "data": "aGk=", "mimeType": "image/png" } }));
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "usage_update", "used": 5000, "size": 200_000, "cost": { "amount": 0.25, "currency": "USD" } }));
    let wall = |ms: i64| json!({ "At": 1_800_000_000_000_i64 + ms });
    assert_has(&script.chat(CHAT), &json!({
        "usage": { "used": 5000, "size": 200_000, "cost": { "amount": 0.25, "currency": "USD" } },
        "turns": [{ "items": [
            { "kind": { "Thinking": "Let me look" }, "window": { "end": wall(20) } },
            { "kind": { "Words": "Looking." }, "window": { "end": wall(30) } },
            { "kind": { "Tool": { "title": "Read lib.rs", "kind": "Read", "status": "Done", "input": { "path": "lib.rs" }, "output": "fn main() {}" } }, "window": { "end": wall(40) } },
            { "kind": { "Words": "Found it." } },
            { "kind": { "Unknown": { "what": "agent_message_chunk" } } }
        ] }]
    }));
}

#[test]
fn thinking_isnt_kept_for_a_host_that_doesnt_show_it() {
    let mut host = host();
    host.shows.thinking = false;
    let mut script = Script::new(host);
    script.open_claude();
    script.send(CHAT, "go").0.expect("sent");
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "hmm" } }));
    assert_eq!(script.chat(CHAT)["turns"][0]["items"], json!([]));
}

#[test]
fn what_the_agent_says_outside_a_prompt_is_a_turn_it_started() {
    let mut script = Script::with_claude();
    script.update(PROC, SESSION, &json!({ "sessionUpdate": "notice", "severity": "info", "title": "Task done" }));
    script.update(PROC, SESSION, &words("The build finished."));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{
        "starter": "Agent", "life": "Running",
        "items": [{ "kind": { "Unknown": { "what": "notice", "raw": { "title": "Task done" } } } }, { "kind": { "Words": "The build finished." } }]
    }] }));
    script.send(CHAT, "thanks").0.expect("sent");
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "starter": "Agent", "life": "Finished" }, { "life": "Running" }] }));
}

#[test]
fn an_update_caf_cant_read_is_kept_as_it_came() {
    let mut script = Script::with_claude();
    script.send(CHAT, "go").0.expect("sent");
    let broken = json!({ "sessionUpdate": "tool_call", "toolCallId": 12 });
    script.update(PROC, SESSION, &broken);
    script.update(PROC, SESSION, &json!({ "no": "tag" }));
    assert_has(&script.chat(CHAT), &json!({ "turns": [{ "items": [
        { "kind": { "Unknown": { "what": "tool_call", "raw": broken } } },
        { "kind": { "Unknown": { "what": "session/update", "raw": { "no": "tag" } } } }
    ] }] }));
}

#[test]
fn messages_wait_in_order_up_to_the_hosts_limit() {
    let mut host = host();
    host.limits.queued_messages = 2;
    let mut script = Script::new(host);
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    script.send(CHAT, "one").0.expect("waits");
    script.send(CHAT, "two").0.expect("waits");
    assert_eq!(script.send(CHAT, "three").0, Err(Refused::QueueFull));
    script.answers(PROC, 1000, &claude_init());
    let out = script.answers(PROC, 1001, &json!({ "sessionId": SESSION }));
    assert_has(&sent(&out, "session/prompt"), &json!({ "params": { "prompt": [{ "text": "one" }] } }));
    let out = script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
    assert_has(&sent(&out, "session/prompt"), &json!({ "params": { "prompt": [{ "text": "two" }] } }));
    script.answers(PROC, 1003, &json!({ "stopReason": "end_turn" }));
}

#[test]
fn the_host_is_refused_what_cant_be_done() {
    let mut script = Script::with_claude();
    assert_eq!(script.open(OpenChat::new("nobody", "/work")).0, Err(Refused::NoSuchAgent));
    assert_eq!(script.open(OpenChat::new("claude", "work")).0, Err(Refused::RelativeFolder));
    assert_eq!(script.send(ChatId(4242), "hi").0, Err(Refused::NoSuchChat));
    assert_eq!(script.send_message(CHAT, UserMessage { parts: vec![] }).0, Err(Refused::Unsupported));
    let command = UserMessage {
        parts: vec![Part::Text("/compact".to_owned()), Part::Text("and this".to_owned())],
    };
    assert_eq!(script.send_message(CHAT, command).0, Err(Refused::CommandNotAlone));
    let (_, out) = script.send(CHAT, "/compact");
    assert_eq!(sent(&out, "session/prompt")["params"]["prompt"], json!([{ "type": "text", "text": "/compact" }]));
    script.answers(PROC, 1002, &json!({ "stopReason": "end_turn" }));
    script.close(CHAT).0.expect("closed");
    assert_eq!(script.send(CHAT, "hi").0, Err(Refused::Closed));
}

#[test]
fn mcp_servers_go_with_the_session_unless_the_agent_cant_take_them() {
    let mut script = Script::new(super::harness::host());
    let mut open = OpenChat::new("claude", "/work");
    let mut command = SpawnCommand::new("/opt/tools/serve");
    command.args = vec!["--stdio".into()];
    command.env = vec![EnvChange::Set("TOKEN".into(), "t".into()), EnvChange::Remove("HOME".into())];
    open.mcp_servers = vec![
        McpServer::Stdio { name: "tools".to_owned(), command },
        McpServer::Http { name: "map".to_owned(), url: "http://localhost:9/mcp".to_owned(), headers: vec![("Auth".to_owned(), "x".to_owned())] },
    ];
    script.open(open.clone()).0.expect("opened");
    let out = script.answers(PROC, 1000, &claude_init());
    assert_eq!(sent(&out, "session/new")["params"]["mcpServers"], json!([
        { "name": "tools", "command": "/opt/tools/serve", "args": ["--stdio"], "env": [{ "name": "TOKEN", "value": "t" }] },
        { "type": "http", "name": "map", "url": "http://localhost:9/mcp", "headers": [{ "name": "Auth", "value": "x" }] }
    ]));

    open.agent = crate::ids::AgentName("plain".to_owned());
    let (chat, out) = script.open(open);
    let chat = chat.expect("opened");
    let plain = with(&out, "start")[0]["start"].as_u64().expect("started");
    let out = script.answers(plain, 1000, &super::plain_init(true));
    assert!(with(&out, "write").is_empty());
    assert_has(&script.chat(chat), &json!({ "state": { "Lost": { "reason": { "Incompatible": "HttpMcp" } } } }));
}

#[test]
fn viewers_read_changes_or_the_whole_chat() {
    let mut host = host();
    host.limits.kept_changes = 4;
    let mut script = Script::new(host);
    script.open(OpenChat::new("claude", "/work")).0.expect("opened");
    // No changes yet: the whole chat, as of "before the first".
    let Some(Read::Picture { latest, .. }) = script.core.read(CHAT, None) else { panic!("a picture") };
    assert_eq!(latest, ChangeNo(999));
    script.answers(PROC, 1000, &claude_init());
    let out = script.answers(PROC, 1001, &json!({ "sessionId": SESSION }));
    assert_eq!(with(&out, "changed"), [json!({ "changed": CHAT.0, "latest": 1000 })]);
    let Some(Read::Changes(changes)) = script.core.read(CHAT, Some(ChangeNo(999))) else { panic!("changes") };
    assert!(matches!(changes, [one] if one.no == ChangeNo(1000) && matches!(one.change, Change::State(_))));
    assert!(matches!(script.core.read(CHAT, Some(ChangeNo(1000))), Some(Read::Changes([]))));

    script.send(CHAT, "go").0.expect("sent");
    for n in 0..5 {
        script.update(PROC, SESSION, &words(&format!("{n}")));
    }
    // Kept: the last 4, cut back once 5 are over.
    let Some(Read::Picture { latest, .. }) = script.core.read(CHAT, Some(ChangeNo(999))) else { panic!("too far behind") };
    let Some(Read::Changes(changes)) = script.core.read(CHAT, Some(ChangeNo(latest.0 - 2))) else { panic!("changes") };
    assert_eq!(changes.len(), 2);
    assert!(matches!(script.core.read(CHAT, Some(ChangeNo(latest.0 + 1))), Some(Read::Picture { .. })));
    assert!(script.core.read(ChatId(1), None).is_none());
}

#[test]
fn connections_say_what_each_adapter_is_and_serves() {
    let mut script = Script::with_claude();
    let connections = script.core.connections();
    assert_eq!(connections.len(), 1);
    let one = &connections[0];
    assert_eq!((one.id.0, &one.agent.0, &one.chats), (1001, &"claude".to_owned(), &vec![CHAT]));
    assert_eq!(one.profile.as_ref().map(|report| report.checked), Some(crate::profiles::Checked::Yes));
    script.shutdown();
}
