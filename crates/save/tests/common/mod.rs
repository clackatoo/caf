//! What the tests share: an app's types, chats to save, and folders to
//! keep them in.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use caf_core::save::{Entry, Saved};
use caf_core::{AppData, AppItem, ChatId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// An app's own item: the map moved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    pub place: String,
    pub zoom: u8,
}

impl AppItem for Pin {
    fn summary(&self) -> String {
        format!("Moved the map to {}", self.place)
    }
}

/// An app's data about each chat.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Notes {
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl AppData for Notes {
    fn label(&self) -> Option<String> {
        Some(self.title.clone())
    }
}

/// The same app later, whose item is another shape: it reads none of
/// the old ones.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Moved {
    pub to: (f64, f64),
}

impl AppItem for Moved {
    fn summary(&self) -> String {
        "Moved".into()
    }
}

/// The same app later, whose data is another shape.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub path: PathBuf,
}

impl AppData for Project {
    fn label(&self) -> Option<String> {
        None
    }
}

/// `ms` milliseconds into the tests' time.
pub fn wall(ms: i64) -> i64 {
    1_800_000_000_000 + ms
}

/// A window from `start` to `end`.
pub fn ended(start: i64, end: i64) -> Value {
    json!({ "start": wall(start), "end": { "type": "At", "value": wall(end) } })
}

/// An item.
pub fn item(id: u64, kind: Value) -> Value {
    json!({ "id": id, "window": ended(id as i64, id as i64 + 1), "parent": null, "kind": kind })
}

/// A tool call's item.
pub fn tool(id: u64, title: &str, output: &str) -> Value {
    item(
        id,
        json!({ "type": "Tool", "value": {
            "title": title,
            "kind": { "type": "Read" },
            "status": { "type": "Done" },
            "origin": { "type": "Caf", "value": { "type": "Read" } },
            "action": { "type": "Read", "value": { "path": title, "lines": null } },
            "input": { "path": title },
            "input_cut": null,
            "result": [{ "type": "Text", "value": { "text": output, "cut": null } }],
            "locations": [],
            "locations_cut": 0,
            "decision": { "type": "NotAsked" },
            "app": null,
        }}),
    )
}

/// A finished turn the user started.
pub fn turn(id: u64, said: &str, items: Vec<Value>) -> Value {
    json!({
        "id": id,
        "starter": { "type": "User", "value": { "parts": [{ "type": "Text", "value": said }] } },
        "life": { "type": "Finished" },
        "window": ended(id as i64, id as i64 + 100),
        "items": items,
        "imported": false,
    })
}

/// A header for chat `id`.
pub fn header(id: u64) -> Value {
    json!({
        "chat": ChatId::from_number(id),
        "agent": "claude",
        "profile": "claude-agent-acp",
        "cwd": "/work",
        "made": wall(id as i64),
        "changed": wall(1000),
        "label": "Fix the tests",
        "first": "Fix the tests",
        "turns": 1,
        "closed": false,
    })
}

/// A chat saved whole, from its turns.
pub fn saved_with(id: u64, turns: Vec<Value>) -> Value {
    json!({
        "header": header(id),
        "chat": {
            "id": ChatId::from_number(id),
            "agent": "claude",
            "cwd": "/work",
            "state": { "type": "Idle" },
            "turns": turns,
            "usage": { "used": 5000, "size": 200_000, "cost": { "amount": 0.25, "currency": "USD" } },
            "questions": [],
        },
        "session": "s1",
        "data": { "title": "Fix the tests", "tags": ["work"] },
    })
}

/// A small chat with one of most things in it, an app item included.
pub fn small(id: u64) -> Saved<Pin, Notes> {
    let turns = vec![turn(
        1,
        "Fix the tests",
        vec![
            item(2, json!({ "type": "Words", "value": "On it." })),
            item(
                3,
                json!({ "type": "Thinking", "value": "Reading the tests." }),
            ),
            tool(4, "lib.rs", "fn main() {}"),
            item(
                5,
                json!({ "type": "App", "value": { "place": "Kyiv", "zoom": 9 } }),
            ),
            item(
                6,
                json!({ "type": "Unknown", "value": {
                    "what": "plan", "raw": { "entries": [] }, "summary": null,
                }}),
            ),
        ],
    )];
    from(saved_with(id, turns))
}

/// Records of changes to [`small`]'s chat, one of each kind of entry.
pub fn records() -> Vec<Vec<Entry<Pin, Notes>>> {
    let records = json!([
        [
            { "type": "Change", "value": { "type": "TurnAdded", "value":
                turn(10, "And the docs", vec![
                    item(11, json!({ "type": "Words", "value": "Sure" })),
                ]) } },
            { "type": "Change", "value": { "type": "Appended", "value": { "item": 11, "text": ", now." } } },
        ],
        [
            { "type": "Change", "value": { "type": "ItemAdded", "value": {
                "turn": 10,
                "item": item(12, json!({ "type": "App", "value": { "place": "Lviv", "zoom": 4 } })),
            } } },
            { "type": "Change", "value": { "type": "ToolChanged", "value": { "item": 4, "tool": {
                "title": "lib.rs", "kind": { "type": "Read" }, "status": { "type": "Failed" },
                "origin": { "type": "Caf", "value": { "type": "Read" } }, "action": null,
                "input": null, "input_cut": null,
                "result": [{ "type": "Text", "value": { "text": "gone", "cut": null } }],
                "locations": [], "locations_cut": 0, "decision": { "type": "NotAsked" }, "app": null,
            } } } },
            { "type": "Session", "value": "s2" },
        ],
        [
            { "type": "Change", "value": { "type": "State", "value": { "type": "Closed" } } },
            { "type": "Data", "value": { "title": "Docs too", "tags": [] } },
        ],
    ]);
    from(records)
}

/// A value of `T`, from JSON: how the tests make the core's types, which
/// only it can build.
pub fn from<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("the test's JSON is the type's")
}

/// A folder of a test's own, removed when it's done with.
pub struct Folder(PathBuf);

impl Folder {
    pub fn new(name: &str) -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "caf-save-{name}-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
