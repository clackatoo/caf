//! Every agent runs with none of its own tools: what's sent to strip it
//! as each session opens, with CAF's instructions and the app's, and the
//! agents CAF can't strip, which it refuses.

use serde_json::{Value, json};

use super::harness::{Script, agents, host, sent, with, writes};
use super::{CHAT, PROC, SESSION, claude_init};
use crate::agent::McpServer;
use crate::agent::{AgentSpec, SpawnCommand};
use crate::core::sessions::MOST_INSTRUCTIONS;
use crate::core::{OpenChat, Refused, SessionChoice};
use crate::ids::AgentSessionId;
use crate::ids::Shelf;
use crate::profiles::ProfileChoice;
use crate::save::RestoreChat;

/// CAF's own instructions, to an agent of the tests' host.
const OWN: &str = "You are working inside caf-test, the app the user is talking to you in. You \
    have no tools of your own: you can't read or change files, run commands or browse the web, \
    except through tools caf-test gives you. Answer in Markdown.";

/// What strips Claude, with `prompt` as its whole system prompt.
fn stripped(prompt: &str) -> Value {
    json!({ "claudeCode": { "options": {
        "tools": [],
        "settingSources": [],
        "strictMcpConfig": true,
        "allowedTools": ["mcp__caf"],
        "systemPrompt": prompt,
        "promptSuggestions": false,
        "verbatimPrompts": true,
        "allowDangerouslySkipPermissions": false,
        "settings": { "disableClaudeAiConnectors": true, "disableRemoteControl": true },
        "env": {
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1",
            "CLAUDE_CODE_DISABLE_BUNDLED_SKILLS": "1",
            "CLAUDE_CODE_DISABLE_CLAUDE_MDS": "1",
            "ENABLE_CLAUDEAI_MCP_SERVERS": "false",
        },
        "extraArgs": { "disable-slash-commands": null },
    } } })
}

/// The tests' agents, and `unknown`, which has the cautious profile.
fn with_unknown() -> Vec<AgentSpec> {
    let mut all = agents();
    all.push(AgentSpec::command(
        "unknown",
        SpawnCommand::new("/opt/unknown-acp"),
        ProfileChoice::Cautious,
    ));
    all
}

/// Opening a Claude chat in `/work` that continues session `old`, telling
/// it `instructions`.
fn continuing(instructions: &str) -> OpenChat {
    let mut open = OpenChat::new("claude", "/work");
    open.session = SessionChoice::Continue(AgentSessionId("old".to_owned()));
    open.instructions = Some(instructions.to_owned());
    open
}

#[test]
fn a_new_session_has_none_of_claudes_own_tools_and_caf_and_the_apps_instructions() {
    let mut script = Script::new(host());
    let mut open = OpenChat::new("claude", "/work");
    open.instructions = Some("The project is a map editor.\nKeep answers short.".to_owned());
    script.open(open).0.expect("opened");
    let out = script.answers(PROC, 1000, &claude_init());
    assert_eq!(
        sent(&out, "session/new"),
        json!({
            "jsonrpc": "2.0",
            "id": 1001,
            "method": "session/new",
            "params": {
                "cwd": "/work",
                "mcpServers": [],
                "_meta": stripped(&format!("{OWN}\n\nThe project is a map editor.\nKeep answers short.")),
            },
        })
    );
}

#[test]
fn with_no_instructions_from_the_app_claude_has_only_cafs() {
    for instructions in [None, Some(String::new())] {
        let mut script = Script::new(host());
        let mut open = OpenChat::new("claude", "/work");
        open.instructions = instructions;
        script.open(open).0.expect("opened");
        let out = script.answers(PROC, 1000, &claude_init());
        assert_eq!(sent(&out, "session/new")["params"]["_meta"], stripped(OWN));
    }
}

#[test]
fn a_resumed_session_is_stripped_too() {
    let mut script = Script::new(host());
    script.open(continuing("Notes.")).0.expect("opened");
    let out = script.answers(PROC, 1000, &claude_init());
    assert_eq!(
        sent(&out, "session/resume")["params"],
        json!({
            "cwd": "/work",
            "mcpServers": [],
            "sessionId": "old",
            "_meta": stripped(&format!("{OWN}\n\nNotes.")),
        })
    );
}

#[test]
fn a_loaded_session_is_stripped_too() {
    let mut script = Script::new(host());
    script.open(continuing("Notes.")).0.expect("opened");
    // An adapter that can load sessions, but not resume them.
    let mut init = claude_init();
    init["agentCapabilities"]["sessionCapabilities"]
        .as_object_mut()
        .expect("an object")
        .remove("resume");
    let out = script.answers(PROC, 1000, &init);
    assert_eq!(
        sent(&out, "session/load")["params"],
        json!({
            "cwd": "/work",
            "mcpServers": [],
            "sessionId": "old",
            "_meta": stripped(&format!("{OWN}\n\nNotes.")),
        })
    );
}

#[test]
fn a_session_opened_again_is_stripped_again_with_the_same_instructions() {
    let mut script = Script::new(host());
    let mut open = OpenChat::new("claude", "/work");
    open.instructions = Some("Notes.".to_owned());
    script.open(open).0.expect("opened");
    script.answers(PROC, 1000, &claude_init());
    script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": SESSION }),
    );
    // The adapter dropped the session: the prompt resumes it.
    script.send(CHAT, "go").0.expect("sent");
    let out = script.fails(
        PROC,
        1002,
        &json!({ "code": -32002, "message": "Resource not found" }),
    );
    assert_eq!(
        sent(&out, "session/resume")["params"]["_meta"],
        stripped(&format!("{OWN}\n\nNotes."))
    );
}

#[test]
fn an_agent_with_no_tools_is_sent_nothing_to_strip_it() {
    let mut script = Script::new(host());
    let mut open = OpenChat::new("plain", "/work");
    open.instructions = Some("Notes.".to_owned());
    script.open(open).0.expect("opened");
    let out = script.answers(PROC, 1000, &super::plain_init(false));
    assert_eq!(
        sent(&out, "session/new")["params"],
        json!({ "cwd": "/work", "mcpServers": [] })
    );
}

#[test]
fn an_agent_caf_cant_strip_is_refused_and_says_so_in_its_status() {
    let mut script = Script::offering(host(), with_unknown());
    let (chat, out) = script.open(OpenChat::new("unknown", "/work"));
    assert_eq!(chat, Err(Refused::OwnTools));
    assert!(
        with(&out, "start").is_empty() && writes(&out).is_empty(),
        "nothing is started: {out:#?}"
    );
    assert_eq!(script.state()["chats"], json!([]), "no chat is made");
    assert_eq!(
        script.agent_status("unknown"),
        json!({ "name": "unknown", "install": { "type": "Host" }, "sign_in": { "type": "Unknown" }, "can_sign_in": false, "own_tools_off": false, "settings": { "type": "Unknown" }, "limits": { "windows": [], "extra_usage": null, "cleared": null } })
    );
    assert_eq!(script.agent_status("claude")["own_tools_off"], true);
    assert_eq!(script.agent_status("plain")["own_tools_off"], true);
}

#[test]
fn instructions_longer_than_64_kib_are_refused() {
    let mut script = Script::new(host());
    let mut open = OpenChat::new("claude", "/work");
    open.instructions = Some("x".repeat(MOST_INSTRUCTIONS + 1));
    let (chat, out) = script.open(open.clone());
    assert_eq!(chat, Err(Refused::Unsendable));
    assert!(with(&out, "start").is_empty(), "nothing is started");
    let mut restore = RestoreChat::new(CHAT, crate::ids::Shelf("work".to_owned()));
    restore.instructions = open.instructions.clone();
    let (restored, out) = script.restore(restore);
    assert_eq!(restored, Err(Refused::Unsendable));
    assert!(with(&out, "storage").is_empty(), "nothing is read");

    // 64 KiB, counted in bytes, are fine.
    open.instructions = Some("é".repeat(MOST_INSTRUCTIONS / 2));
    script.open(open).0.expect("opened");
    let out = script.answers(PROC, 1000, &claude_init());
    let prompt =
        &sent(&out, "session/new")["params"]["_meta"]["claudeCode"]["options"]["systemPrompt"];
    assert!(prompt.as_str().is_some_and(|prompt| prompt.ends_with("éé")));
}

#[test]
fn an_agents_status_follows_the_adapter_that_last_answered() {
    let mut per_chat = host();
    per_chat.grouping = crate::host::Grouping::PerChat;
    let mut script = Script::new(per_chat);
    script
        .open(OpenChat::new("claude", "/work"))
        .0
        .expect("opened");
    let mut other = claude_init();
    other["agentInfo"]["name"] = json!("someone-elses-acp");
    script.answers(PROC, 1000, &other);
    assert_eq!(script.agent_status("claude")["own_tools_off"], false);
    // Another chat's adapter is Claude's, as its profile is for.
    let (_, out) = script.open(OpenChat::new("claude", "/work"));
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &claude_init());
    sent(&out, "session/new");
    assert_eq!(script.agent_status("claude")["own_tools_off"], true);
}

#[test]
fn an_mcp_server_named_caf_is_refused_as_cafs_own() {
    let server = |name: &str| McpServer::Http {
        name: name.to_owned(),
        url: "http://localhost:1/mcp".to_owned(),
        headers: Vec::new(),
    };
    // Claude Code writes `caf.x` as `caf_x`, so its tools as `mcp__caf__…`.
    for name in [
        "caf",
        "CAF",
        "caf__x",
        "Caf__",
        "caf.",
        "caf._x",
        "caf. tools",
    ] {
        let mut script = Script::new(host());
        let mut open = OpenChat::new("claude", "/work");
        open.mcp_servers = vec![server("eyespy"), server(name)];
        let (chat, out) = script.open(open);
        assert_eq!(chat, Err(Refused::ReservedServer), "{name}");
        assert!(writes(&out).is_empty() && with(&out, "start").is_empty());
        assert_eq!(script.state()["chats"], json!([]), "no chat is made");
        let mut restore = RestoreChat::new(CHAT, Shelf("work".to_owned()));
        restore.mcp_servers = vec![server(name)];
        let (restored, out) = script.restore(restore);
        assert_eq!(restored, Err(Refused::ReservedServer), "{name}");
        assert!(out.is_empty(), "nothing is read: {out:#?}");
    }
    // A name with `caf` in it is the app's own.
    for name in ["caffeine", "caf_tools", "my caf", "caf\u{e9}x"] {
        let mut script = Script::new(host());
        let mut open = OpenChat::new("claude", "/work");
        open.mcp_servers = vec![server(name)];
        assert_eq!(script.open(open).0, Ok(CHAT), "{name}");
    }
}

#[test]
fn mcp_servers_the_agent_would_name_alike_are_refused() {
    let server = |name: &str| McpServer::Http {
        name: name.to_owned(),
        url: "http://localhost:1/mcp".to_owned(),
        headers: Vec::new(),
    };
    // Claude Code writes both as `my_tools`: their tools couldn't be told
    // apart.
    for names in [
        ["my tools", "my.tools"],
        ["eyes", "eyes"],
        ["a\u{e9}", "a."],
    ] {
        let mut script = Script::new(host());
        let mut open = OpenChat::new("claude", "/work");
        open.mcp_servers = names.iter().map(|name| server(name)).collect();
        let (chat, out) = script.open(open);
        assert_eq!(chat, Err(Refused::SameServerName), "{names:?}");
        assert!(writes(&out).is_empty() && with(&out, "start").is_empty());
        let mut restore = RestoreChat::new(CHAT, Shelf("work".to_owned()));
        restore.mcp_servers = names.iter().map(|name| server(name)).collect();
        assert_eq!(
            script.restore(restore).0,
            Err(Refused::SameServerName),
            "{names:?}"
        );
    }
    // Names only in another case are written apart.
    let mut script = Script::new(host());
    let mut open = OpenChat::new("claude", "/work");
    open.mcp_servers = vec![server("Tools"), server("tools"), server("my-tools")];
    assert_eq!(script.open(open).0, Ok(CHAT));
}
