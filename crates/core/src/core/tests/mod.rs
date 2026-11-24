//! Tests of the core, as scripts of events (see [`harness`]).

mod harness;

mod basics;
mod chats;
mod continuing;
mod failures;
mod frames;
mod guarantees;
mod holding;
mod jobs;
mod limits;
mod own_tools;
mod patching;
mod permissions;
mod process;
mod reported;
mod saving;
mod served;
mod serving;
mod settings;
mod setup;
mod shared;
mod stop;
mod traces;
mod usage;
mod woken;

use serde_json::{Value, json};

use self::harness::{Script, host, sent};
use crate::core::OpenChat;
use crate::ids::ChatId;

/// The process the first chat's adapter runs as: after the chat (1000)
/// and its connection (1001).
const PROC: u64 = 1002;

/// The first chat.
const CHAT: ChatId = ChatId::from_number(1000);

/// The session the agent opens for the first chat.
const SESSION: &str = "sess-1";

/// What claude-agent-acp 0.98.0 answers `initialize` with, trimmed of
/// what CAF doesn't read.
fn claude_init() -> Value {
    json!({
        "protocolVersion": 1,
        "agentCapabilities": {
            "_meta": { "claudeCode": { "promptQueueing": true }, "authStatus": {} },
            "promptCapabilities": { "image": true, "embeddedContext": true },
            "mcpCapabilities": { "http": true, "sse": true },
            "auth": { "logout": {} },
            "loadSession": true,
            "sessionCapabilities": {
                "additionalDirectories": {}, "close": {}, "delete": {}, "fork": {}, "list": {}, "resume": {}
            }
        },
        "agentInfo": { "name": "@agentclientprotocol/claude-agent-acp", "title": "Claude Agent", "version": "0.98.0" },
        "authMethods": []
    })
}

/// A plain adapter's answer: it can load sessions, but nothing else.
fn plain_init(load: bool) -> Value {
    json!({
        "protocolVersion": 1,
        "agentCapabilities": { "loadSession": load },
        "agentInfo": { "name": "plain-acp", "version": "1.0.0" }
    })
}

/// The modes Claude's adapter says a session is in as it opens it, in
/// `default`, as every session CAF opens is.
fn default_modes() -> Value {
    json!({ "currentModeId": "default", "availableModes": [
        { "id": "default", "name": "Manual" }, { "id": "acceptEdits", "name": "Accept edits" },
        { "id": "plan", "name": "Plan mode" }, { "id": "dontAsk", "name": "Don't ask" },
    ] })
}

/// The key the tests' tools driver serves with.
const KEY: crate::mcp::Key = crate::mcp::Key([7; 32]);

/// How the tests' tools driver serves CAF's tools.
fn serving() -> crate::event::Serving {
    let mut relay = crate::agent::SpawnCommand::new("/opt/app/bin/app");
    relay.args = vec!["--relay".into()];
    crate::event::Serving::new(relay, "/run/user/1000/caf/app-1.sock", KEY)
}

impl Script {
    /// The tools driver says how it serves CAF's tools.
    fn serving(&mut self) -> Vec<Value> {
        self.event(crate::event::Event::Serving(serving()))
    }

    /// A script serving CAF's tools, with a Claude chat in `/work` given
    /// all of them, its session open as [`SESSION`] on [`PROC`].
    fn with_claude_serving() -> Self {
        let mut script = Self::new(host());
        script.serving();
        script.open_claude_as(
            OpenChat::new("claude", "/work")
                .with_tools(crate::tool::Tools::caf(crate::tool::CafTool::ALL)),
        );
        script
    }

    /// A script with a Claude chat in `/work`, its session open as
    /// [`SESSION`] on [`PROC`].
    fn with_claude() -> Self {
        let mut script = Self::new(host());
        script.open_claude();
        script
    }

    /// Opens a Claude chat in `/work` and its session, on a core with no
    /// chats yet.
    fn open_claude(&mut self) {
        self.open_claude_as(OpenChat::new("claude", "/work"));
    }

    /// A script with a Claude chat in `/work` given MCP servers named
    /// `servers`, its session open as [`SESSION`] on [`PROC`].
    fn with_claude_given(servers: &[&str]) -> Self {
        let mut script = Self::new(host());
        let mut open = OpenChat::new("claude", "/work");
        open.mcp_servers = (servers.iter())
            .map(|name| crate::agent::McpServer::Http {
                name: (*name).to_owned(),
                url: "http://localhost:1/mcp".to_owned(),
                headers: Vec::new(),
            })
            .collect();
        script.open_claude_as(open);
        script
    }

    /// Opens `open`, a Claude chat, and its session, on a core with no
    /// chats yet.
    fn open_claude_as(&mut self, open: OpenChat) {
        let (chat, out) = self.open(open);
        assert_eq!(chat, Ok(CHAT));
        let init = sent(&out, "initialize");
        let init_id = init["id"].as_u64().expect("an id");
        let out = self.answers(PROC, init_id, &claude_init());
        let new = sent(&out, "session/new");
        let new_id = new["id"].as_u64().expect("an id");
        self.answers(
            PROC,
            new_id,
            &json!({ "modes": default_modes(), "sessionId": SESSION }),
        );
    }
}
