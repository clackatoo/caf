//! Tests of the core, as scripts of events (see [`harness`]).

mod harness;

mod basics;
mod process;
mod shared;
mod stop;

use serde_json::{Value, json};

use self::harness::{Script, host, sent};
use crate::core::OpenChat;
use crate::ids::ChatId;

/// The process the first chat's adapter runs as: after the chat (1000)
/// and its connection (1001).
const PROC: u64 = 1002;

/// The first chat.
const CHAT: ChatId = ChatId(1000);

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

impl Script {
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
        let (chat, out) = self.open(OpenChat::new("claude", "/work"));
        assert_eq!(chat, Ok(CHAT));
        let init = sent(&out, "initialize");
        let init_id = init["id"].as_u64().expect("an id");
        let out = self.answers(PROC, init_id, &claude_init());
        let new = sent(&out, "session/new");
        let new_id = new["id"].as_u64().expect("an id");
        self.answers(PROC, new_id, &json!({ "sessionId": SESSION }));
    }

    /// The id of the one `method` request in `out`.
    fn id_of(out: &[Value], method: &str) -> u64 {
        sent(out, method)["id"].as_u64().expect("an id")
    }
}
