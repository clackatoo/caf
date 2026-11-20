//! What the kit's tests of watching and serving share: the fake adapter
//! and its scripts, and ways to wait on a chat.

#![allow(dead_code)] // each test file uses some of it

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use caf_core::Chat;
use caf_core::agent::{AgentSpec, EnvChange, SpawnCommand};
use caf_core::chat::{ItemKind, Turn, TurnLife};
use caf_core::host::Host;
use caf_core::profiles::ProfileChoice;
use caf_driver_tokio::{Caf, ChatHandle, Running, Update, Watch};
use serde_json::{Value, json};

/// Long enough for anything a test waits for, on a loaded machine.
pub const PATIENCE: Duration = Duration::from_secs(60);

/// A folder of its own for each test, under `TMPDIR`.
pub struct Dir(pub PathBuf);

impl Dir {
    pub fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(1000);
        let dir = std::env::temp_dir().join(format!(
            "caf-viewing-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Writes a script of these steps, and returns its path.
    pub fn script(&self, name: &str, steps: &[Value]) -> PathBuf {
        let path = self.path(name);
        let text: String = steps.iter().map(|step| format!("{step}\n")).collect();
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The fake, as agent `fake`, running `script` with its record in `dir`.
pub fn fake(dir: &Dir, script: &Path) -> AgentSpec {
    let mut command = SpawnCommand::new(caf_fake_agent::binary());
    command.env = vec![
        EnvChange::Set(caf_fake_agent::SCRIPT_VAR.into(), script.into()),
        EnvChange::Set(
            caf_fake_agent::RECORD_VAR.into(),
            dir.path("record.jsonl").into(),
        ),
    ];
    AgentSpec::new("fake", command, ProfileChoice::Cautious)
}

/// A host for tests: an adapter that has gone is waited for briefly.
pub fn host() -> Host {
    let mut host = Host::new("caf-test", "1.0.0");
    host.timers.gone = Duration::from_millis(300);
    host.timers.kill_after = Duration::from_millis(500);
    host
}

/// Refuses whatever the script doesn't expect, as an adapter does.
fn fallback() -> Value {
    json!({"on": {"fallback": true, "do": [{"respond": {"error": {"code": -32601, "message": "Method not found"}}}]}})
}

/// Starts as an adapter does, and opens one session, `session`.
pub fn handshake(session: &str) -> Vec<Value> {
    vec![
        fallback(),
        json!({"wait": {"method": "initialize"}}),
        json!({"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}, "agentInfo": {"name": "fake", "version": "1.0.0"}}}}),
        json!({"wait": {"method": "session/new"}}),
        json!({"respond": {"result": {"sessionId": session}}}),
    ]
}

pub fn say(session: &str, text: &str) -> Value {
    json!({"say": {"session": session, "text": text}})
}

pub fn prompt() -> Value {
    json!({"wait": {"method": "session/prompt", "as": "prompt"}})
}

pub fn end_turn() -> Value {
    json!({"respond": {"to": "prompt", "result": {"stopReason": "end_turn"}}})
}

/// The agent asks to run something, offering to allow or reject it.
pub fn ask_to_run() -> Value {
    json!({"request": {"method": "session/request_permission", "as": "ask", "params": {
        "sessionId": "s1",
        "toolCall": {"toolCallId": "t1", "title": "Run rm"},
        "options": [
            {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
            {"optionId": "reject", "name": "Reject", "kind": "reject_once"}
        ]
    }}})
}

/// `steps` after `handshake("s1")`.
pub fn after_handshake(steps: &[Value]) -> Vec<Value> {
    let mut script = handshake("s1");
    script.extend_from_slice(steps);
    script
}

/// A fresh picture of the chat.
pub async fn picture(chat: &ChatHandle) -> Chat {
    match tokio::time::timeout(PATIENCE, chat.watch().next()).await {
        Ok(Some(Update::Picture(picture))) => picture,
        other => panic!("a new watch starts with a picture, not {other:?}"),
    }
}

/// Waits, with `watch`, until `done` is true of the chat, and returns it.
pub async fn until(
    chat: &ChatHandle,
    watch: &mut Watch,
    what: &str,
    done: impl Fn(&Chat) -> bool,
) -> Chat {
    let waited = tokio::time::timeout(PATIENCE, async {
        loop {
            let now = picture(chat).await;
            if done(&now) {
                return now;
            }
            assert!(
                watch.next().await.is_some(),
                "the watch ended waiting for {what}: {now:#?}"
            );
        }
    })
    .await;
    match waited {
        Ok(chat) => chat,
        Err(_) => panic!("gave up waiting for {what}: {:#?}", picture(chat).await),
    }
}

/// The words of a turn, run together.
pub fn words(turn: &Turn) -> String {
    (turn.items.iter())
        .filter_map(|item| match &item.kind {
            ItemKind::Words(words) => Some(words.as_str()),
            _ => None,
        })
        .collect()
}

pub fn finished(chat: &Chat) -> bool {
    chat.turns.last().map(|turn| &turn.life) == Some(&TurnLife::Finished)
}

/// Shuts CAF down and waits for its task to end.
pub async fn shut_down(caf: &Caf, running: Running) {
    let done = tokio::time::timeout(PATIENCE, async {
        caf.shutdown().await;
        running.await;
    })
    .await;
    assert!(done.is_ok(), "shutting down took too long");
}
