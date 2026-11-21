//! The samples that pin each version of the saved format:
//! `samples/save/v<VERSION>/`, a chat with one of every kind of item, turn
//! and change in it, an app item and data slot included, saved by that
//! version.
//!
//! - `header`, `snapshot` and `journal` are the format's own bytes, as
//!   [`encode_header`], [`encode_snapshot`] and [`encode_record`] wrote
//!   them;
//! - `files/` is the same chat as [`FileStore`] keeps it, with a second
//!   chat, squeezed;
//! - `shapes.ts` and `variants.txt` are every saved type's shape as that
//!   version had it: its fields and variants.
//!
//! Every version's sample must always read as the values below say, so a
//! change to how any of it is read fails. They aren't compared byte for
//! byte, as zstd may compress the same thing differently from one zstd to
//! the next; what's inside them is, for the version this CAF writes, so
//! a change to what's saved fails until [`VERSION`] goes up. The values
//! only write the files, and only under `CAF_BLESS_SAMPLE`, for the
//! version this CAF writes, which is for a version not yet released.
//!
//! The app's types here are part of the samples: they're the app that
//! saved them, so they never change either.

use std::fs;
use std::path::{Path, PathBuf};

use caf_core::save::{Entry, Header, Saved};
use caf_core::{AppData, AppItem, ChatId, ItemKind, NoAppData, NoAppItems, Shelf};
use caf_save::{
    FileStore, Squeeze, Store, VERSION, decode, decode_header, encode_header, encode_record,
    encode_snapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What a test failing here means.
const CHANGED: &str = "a sample no longer reads as it did: what reads a released version of the \
    saved format has changed. Saves stay readable forever: a change to what's saved needs a new \
    version (caf_save::VERSION), with a sample of its own, and a migration if it's a change of \
    shape. Only while a version is unreleased may `CAF_BLESS_SAMPLE=1 cargo test -p caf-save \
    --test sample` rewrite its sample";

/// The app's own item, as the app that saved the sample had it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Pin {
    place: String,
    zoom: u8,
    #[serde(default)]
    tags: Vec<String>,
}

impl AppItem for Pin {
    fn summary(&self) -> String {
        format!("Moved the map to {}", self.place)
    }
}

/// The app's data, as the app that saved the sample had it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Notes {
    title: String,
    pinned: bool,
    project: Option<PathBuf>,
}

impl AppData for Notes {
    fn label(&self) -> Option<String> {
        Some(self.title.clone())
    }
}

/// Where a version's sample is kept: the one this CAF writes written
/// first under `CAF_BLESS_SAMPLE`, once for every test.
fn folder(version: u32) -> PathBuf {
    static BLESSED: std::sync::Once = std::sync::Once::new();
    let folder = (Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/save"))
        .join(format!("v{version}"));
    if version == VERSION && std::env::var_os("CAF_BLESS_SAMPLE").is_some() {
        BLESSED.call_once(|| bless(&folder));
    }
    folder
}

/// What a version's sample holds, as the values that wrote it.
struct Sample {
    header: Header,
    before: Saved<Pin, Notes>,
    records: Vec<Vec<Entry<Pin, Notes>>>,
    after: Saved<Pin, Notes>,
    squeezed: Saved<Pin, Notes>,
}

/// A version's sample's values.
fn sample(version: u32) -> Sample {
    match version {
        1 => Sample {
            header: from(header(9)),
            before: before(),
            records: records(),
            after: after(),
            squeezed: squeezed(),
        },
        _ => panic!("samples/save/v{version} has no values here: add them, and bless its sample"),
    }
}

/// The chat's id.
fn id() -> ChatId {
    ChatId::from_number(1)
}

/// The shelf it's on in `files/`.
fn shelf() -> Shelf {
    Shelf("work".into())
}

/// `ms` milliseconds into the sample's time.
fn wall(ms: i64) -> i64 {
    1_800_000_000_000 + ms
}

/// A window from `start` to `end`.
fn ended(start: i64, end: i64) -> Value {
    json!({ "start": wall(start), "end": { "type": "At", "value": wall(end) } })
}

/// An item.
fn item(id: u64, parent: Option<u64>, kind: Value) -> Value {
    json!({ "id": id, "window": ended(id as i64, id as i64 + 1), "parent": parent, "kind": kind })
}

/// A tool call.
fn tool(kind: &str, status: &str, app: Value) -> Value {
    json!({
        "title": format!("{kind} lib.rs"),
        "kind": { "type": kind },
        "status": { "type": status },
        "input": { "path": "lib.rs", "lines": [1, 20] },
        "output": "fn main() {}\n",
        "app": app,
    })
}

/// A turn.
fn turn(id: u64, starter: Value, life: Value, items: Vec<Value>) -> Value {
    json!({
        "id": id,
        "starter": starter,
        "life": life,
        "window": ended(id as i64 * 100, id as i64 * 100 + 50),
        "items": items,
        "imported": false,
    })
}

/// A message.
fn said(text: &str) -> Value {
    json!({ "parts": [{ "type": "Text", "value": text }] })
}

/// The chat as its snapshot has it.
fn before() -> Saved<Pin, Notes> {
    let kinds = [
        "Read", "Edit", "Delete", "Move", "Search", "Execute", "Think", "Fetch", "Other",
    ];
    let statuses = ["Pending", "Running", "Done", "Failed"];
    let mut work = vec![
        item(
            10,
            None,
            json!({ "type": "Words", "value": "On it: **two** things." }),
        ),
        item(
            11,
            Some(10),
            json!({ "type": "Thinking", "value": "Reading the tests." }),
        ),
    ];
    work.extend((12..).zip(kinds).map(|(id, kind)| {
        let status = statuses[id as usize % statuses.len()];
        item(
            id,
            None,
            json!({ "type": "Tool", "value": tool(kind, status, Value::Null) }),
        )
    }));
    let pinned = json!({ "place": "Odesa", "zoom": 7, "tags": ["coast"] });
    work.extend([
        item(30, None, json!({ "type": "Tool", "value": tool("Other", "Done", pinned) })),
        item(31, None, json!({ "type": "Compacted", "value": {} })),
        item(32, None, json!({ "type": "App", "value": { "place": "Kyiv", "zoom": 9, "tags": [] } })),
        item(
            33,
            None,
            json!({ "type": "Unknown", "value": {
                "what": "plan",
                "raw": { "sessionUpdate": "plan", "entries": [{ "content": "Fix it", "priority": 1 }] },
                "summary": null,
            }}),
        ),
    ]);
    let notes = [
        "SessionGone",
        "CannotContinue",
        "SessionStuck",
        "NotInstalled",
        "InstallFailed",
        "SignedIn",
    ];
    let notes = (40..).zip(notes).map(|(id, note)| {
        item(
            id,
            None,
            json!({ "type": "Note", "value": { "type": note } }),
        )
    });
    let failed = json!({ "type": "Failed", "value": {
        "reason": { "type": "UsageLimit" },
        "agent_said": "Try again at 5pm",
        "remedy": { "type": "SendAgain" },
    }});
    let finished = json!({ "type": "Finished" });
    let turns = vec![
        json!({
            "id": 1,
            "starter": { "type": "User", "value": said("Earlier") },
            "life": finished,
            "window": { "start": null, "end": { "type": "Unknown" } },
            "items": [{
                "id": 2,
                "window": { "start": null, "end": { "type": "Unknown" } },
                "parent": null,
                "kind": { "type": "Words", "value": "From before." },
            }],
            "imported": true,
        }),
        turn(
            3,
            json!({ "type": "User", "value": said("Fix the tests") }),
            finished.clone(),
            work,
        ),
        turn(
            4,
            json!({ "type": "Caf" }),
            finished.clone(),
            notes.collect(),
        ),
        turn(
            5,
            json!({ "type": "Agent" }),
            json!({ "type": "CutOff" }),
            vec![],
        ),
        turn(
            6,
            json!({ "type": "OtherChat", "value": { "from": ChatId::from_number(2), "message": said("Over to you") } }),
            failed,
            vec![],
        ),
        turn(
            7,
            json!({ "type": "Notice", "value": "The map moved" }),
            json!({ "type": "Stopped" }),
            vec![],
        ),
        turn(
            8,
            json!({ "type": "User", "value": said("And the docs") }),
            json!({ "type": "Running" }),
            vec![json!({
                "id": 80,
                "window": { "start": wall(800), "end": { "type": "Open" } },
                "parent": null,
                "kind": { "type": "Words", "value": "Starting" },
            })],
        ),
    ];
    from(json!({
        "header": header(4),
        "chat": {
            "id": id(),
            "agent": "claude",
            "cwd": "/work",
            "state": { "type": "Open", "value": { "session": "s1" } },
            "turns": turns,
            "usage": { "used": 5000, "size": 200_000, "cost": { "amount": 0.25, "currency": "USD" } },
            "questions": [
                {
                    "id": 90,
                    "window": { "start": wall(810), "end": { "type": "Open" } },
                    "kind": { "type": "Permission", "value": {
                        "tool": 17,
                        "title": "Run cargo test",
                        "options": [
                            { "id": "allow", "label": "Allow", "kind": { "type": "AllowOnce" } },
                            { "id": "always", "label": "Always", "kind": { "type": "AllowAlways" } },
                            { "id": "reject", "label": "Reject", "kind": { "type": "RejectOnce" } },
                            { "id": "never", "label": "Never", "kind": { "type": "RejectAlways" } },
                            { "id": "other", "label": "Other", "kind": { "type": "Other" } },
                        ],
                    }},
                },
                {
                    "id": 91,
                    "window": { "start": wall(820), "end": { "type": "Open" } },
                    "kind": { "type": "SlowToOpen", "value": { "adapter_answered": true } },
                },
            ],
        },
        "session": "s1",
        "data": { "title": "Fix the tests", "pinned": true, "project": "/work/caf" },
    }))
}

/// The chat's header, with `turns` turns.
fn header(turns: u64) -> Value {
    json!({
        "chat": id(),
        "agent": "claude",
        "profile": "claude-agent-acp",
        "cwd": "/work",
        "made": wall(0),
        "changed": wall(2000),
        "label": "Fix the tests",
        "first": "Fix the tests",
        "turns": turns,
        "closed": false,
    })
}

/// What the journal adds, a record at a time: one of every kind of
/// change, and of entry.
fn records() -> Vec<Vec<Entry<Pin, Notes>>> {
    let change = |change: Value| json!({ "type": "Change", "value": change });
    from(json!([
        [
            change(json!({ "type": "Appended", "value": { "item": 80, "text": " on the docs." } })),
            change(json!({ "type": "ItemEnded", "value": { "item": 80, "end": { "type": "At", "value": wall(900) } } })),
            change(json!({ "type": "ItemAdded", "value": {
                "turn": 8,
                "item": item(81, None, json!({ "type": "App", "value": { "place": "Lviv", "zoom": 4, "tags": [] } })),
            }})),
            change(json!({ "type": "ToolChanged", "value": { "item": 12, "tool": tool("Read", "Done", Value::Null) } })),
            change(json!({ "type": "Usage", "value": { "used": 6000, "size": 200_000, "cost": null } })),
            change(json!({ "type": "QuestionGone", "value": 90 })),
            change(json!({ "type": "QuestionAsked", "value": {
                "id": 92,
                "window": { "start": wall(950), "end": { "type": "Open" } },
                "kind": { "type": "Permission", "value": { "tool": null, "title": "Fetch docs.rs", "options": [] } },
            }})),
        ],
        [
            change(json!({ "type": "TurnLife", "value": {
                "turn": 8, "life": { "type": "Finished" }, "end": { "type": "At", "value": wall(1000) },
            }})),
            change(json!({ "type": "TurnAdded", "value": turn(9, json!({ "type": "User", "value": said("Thanks") }), json!({ "type": "Waiting" }), vec![]) })),
            change(json!({ "type": "TurnInserted", "value": {
                "before": 9,
                "turn": turn(10, json!({ "type": "Caf" }), json!({ "type": "Finished" }), vec![
                    item(100, None, json!({ "type": "Note", "value": { "type": "SessionGone" } })),
                ]),
            }})),
            change(json!({ "type": "State", "value": { "type": "Lost", "value": {
                "reason": { "type": "AgentStopped", "value": { "stderr": ["out of memory"] } },
            }}})),
            { "type": "Session", "value": "s2" },
        ],
        [
            { "type": "Data", "value": { "title": "Fix the tests and docs", "pinned": false, "project": null } },
            { "type": "Session", "value": null },
            change(json!({ "type": "State", "value": { "type": "Idle" } })),
        ],
    ]))
}

/// The chat as it reads, its journal applied.
fn after() -> Saved<Pin, Notes> {
    let mut saved = before();
    for entry in records().into_iter().flatten() {
        match entry {
            Entry::Change(change) => saved.chat.apply(&change).expect("it fits"),
            Entry::Session(session) => saved.session = session,
            Entry::Data(data) => saved.data = data,
            _ => unreachable!(),
        }
    }
    saved
}

/// A value of `T`, from JSON.
fn from<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("the sample's JSON is the type's")
}

/// The second chat, in `files/` only, squeezed: the first as it was
/// before its journal, as chat 2.
fn squeezed() -> Saved<Pin, Notes> {
    let mut saved = serde_json::to_value(before()).expect("it's JSON");
    saved["header"]["chat"] = json!(squeezed_id());
    saved["chat"]["id"] = json!(squeezed_id());
    from(saved)
}

/// Its id.
fn squeezed_id() -> ChatId {
    ChatId::from_number(2)
}

/// What a header or snapshot holds, uncompressed.
fn inside(bytes: &[u8]) -> Vec<u8> {
    zstd::stream::decode_all(&bytes[7..]).expect("one frame")
}

/// What each of a journal's records holds, uncompressed.
fn insides(mut journal: &[u8]) -> Vec<Vec<u8>> {
    let mut insides = Vec::new();
    while let Some((length, rest)) = journal.split_first_chunk::<4>() {
        let length = u32::from_le_bytes(*length) as usize;
        insides.push(zstd::stream::decode_all(&rest[..length]).expect("one frame"));
        journal = &rest[length..];
    }
    insides
}

/// The saved types' shapes, as the TypeScript types generated from them
/// say, with nothing but their shapes: every type a saved chat holds,
/// with its fields, and its enums' variants, by name.
fn shapes() -> String {
    let types = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ts/sdk/src/types");
    let mut seen = std::collections::BTreeMap::new();
    let mut waiting = vec![
        "./Saved.ts".to_owned(),
        "./Entry.ts".to_owned(),
        "./Header.ts".to_owned(),
    ];
    while let Some(name) = waiting.pop() {
        if seen.contains_key(&name) {
            continue;
        }
        let text = fs::read_to_string(types.join(&name))
            .unwrap_or_else(|error| panic!("ts/sdk/src/types/{name}: {error}"));
        let mut shape = String::new();
        let mut rest = text.as_str();
        // Doc comments out: they aren't shape.
        while let Some(start) = rest.find("/**") {
            shape.push_str(&rest[..start]);
            let end = rest[start..].find("*/").expect("a comment ends") + start + 2;
            rest = &rest[end..];
        }
        shape.push_str(rest);
        let mut kept = Vec::new();
        for line in shape.lines() {
            let line = line.trim();
            if line.starts_with("import type ") {
                let from = line.split('"').nth(1).expect("where it's from");
                waiting.push(from.to_owned());
            } else if !line.is_empty() && !line.starts_with("//") {
                kept.push(line);
            }
        }
        seen.insert(name, kept.join(" "));
    }
    (seen.values().map(|shape| format!("{shape}\n"))).collect()
}

/// The saved types' fields and variants as serde writes them, with no
/// generated types between: every object in the chats, changes and
/// headers of the newest viewer sample, which caf-core's own tests make
/// hold one of every variant of every enum in them, by where it is, its
/// variant and its fields; and the variants of the two enums that sample
/// can't hold, as serde names them.
fn variants() -> String {
    let viewer = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/viewer");
    let newest = (1..)
        .map(|version| viewer.join(format!("v{version}.json")))
        .take_while(|path| path.exists())
        .last()
        .expect("a viewer sample");
    let sample: Value =
        serde_json::from_str(&fs::read_to_string(&newest).expect("it reads")).expect("JSON");
    let mut seen = std::collections::BTreeSet::new();
    for message in sample["to_viewer"].as_array().expect("messages") {
        let value = &message["value"];
        match message["type"].as_str() {
            Some("Picture") => walk(&value["chat"], "chat", &mut seen),
            Some("Changes") => {
                for change in value["changes"].as_array().expect("changes") {
                    walk(&change["change"], "change", &mut seen);
                }
            }
            Some("Saved") => {
                for saved in value.as_array().expect("saved chats") {
                    walk(&saved["header"], "header", &mut seen);
                }
            }
            _ => {}
        }
    }
    /// The variants serde knows of an enum: as it says a tag isn't one.
    fn named<T: serde::de::DeserializeOwned>() -> String {
        let error = serde_json::from_value::<T>(json!({ "type": "?", "value": null }))
            .err()
            .expect("no such variant")
            .to_string();
        let (_, named) = error.split_once("expected one of ").expect("its variants");
        named.to_owned()
    }
    seen.insert(format!(
        "Entry: {}",
        named::<Entry<NoAppItems, NoAppData>>()
    ));
    seen.insert(format!("ItemKind: {}", named::<ItemKind<NoAppItems>>()));
    (seen.into_iter().map(|line| line + "\n")).collect()
}

/// Notes every object in `value`, at `at`: where it is, its variant if
/// it's one, and its fields. What an agent or tool sent, kept as it came,
/// isn't a saved type's shape, so isn't looked inside.
fn walk(value: &Value, at: &str, seen: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::Object(fields) => {
            let here = match fields.get("type").and_then(Value::as_str) {
                Some(variant) => format!("{at}<{variant}>"),
                None => at.to_owned(),
            };
            let names: Vec<&str> = fields.keys().map(String::as_str).collect();
            seen.insert(format!("{here} {{{}}}", names.join(", ")));
            for (name, inner) in fields {
                if !["type", "raw", "input"].contains(&name.as_str()) {
                    walk(inner, &format!("{here}.{name}"), seen);
                }
            }
        }
        Value::Array(each) => each
            .iter()
            .for_each(|inner| walk(inner, &format!("{at}[]"), seen)),
        _ => {}
    }
}

/// A folder of the test's own, removed when it's done with.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("caf-save-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes the sample of the version this CAF writes.
fn bless(folder: &Path) {
    let values = sample(VERSION);
    let _ = fs::remove_dir_all(folder);
    fs::create_dir_all(folder).expect("the sample's folder");
    let header = encode_header(&values.header);
    let snapshot = encode_snapshot(&values.before, Squeeze::Fast);
    let records: Vec<Vec<u8>> = (values.records.iter())
        .map(|each| encode_record(each))
        .collect();
    fs::write(folder.join("header"), &header).expect("written");
    fs::write(folder.join("snapshot"), &snapshot).expect("written");
    fs::write(folder.join("journal"), records.concat()).expect("written");
    fs::write(folder.join("shapes.ts"), shapes()).expect("written");
    fs::write(folder.join("variants.txt"), variants()).expect("written");
    let files = folder.join("files");
    let mut store = FileStore::new(&files);
    store
        .snapshot(
            &shelf(),
            id(),
            &encode_header(&values.before.header),
            &snapshot,
        )
        .expect("written");
    for record in &records {
        store
            .append(&shelf(), id(), &header, record)
            .expect("written");
    }
    let other = &values.squeezed;
    store
        .snapshot(
            &shelf(),
            squeezed_id(),
            &encode_header(&other.header),
            &encode_snapshot(other, Squeeze::Fast),
        )
        .expect("written");
    store.release(&shelf(), squeezed_id());
    // The first held, so only the second is squeezed.
    FileStore::new(&files)
        .squeeze(std::time::Duration::ZERO)
        .expect("squeezed");
    store.release(&shelf(), id());
    for chat in [id(), squeezed_id()] {
        fs::remove_file(files.join(format!("work/{chat}.lock"))).expect("its lock");
    }
    fs::remove_file(files.join("work/.shelf.lock")).expect("its shelf's lock");
}

/// Reads a version's sample's file.
fn read(version: u32, name: &str) -> Vec<u8> {
    fs::read(folder(version).join(name))
        .unwrap_or_else(|error| panic!("samples/save/v{version}/{name}: {error}"))
}

#[test]
fn every_version_reads_as_its_sample_says() {
    for version in 1..=VERSION {
        let values = sample(version);
        let read = |name| read(version, name);
        let changed = format!("v{version}: {CHANGED}");
        assert_eq!(
            decode_header(&read("header")),
            Ok(values.header.clone()),
            "{changed}"
        );
        let saved = decode::<Pin, Notes>(&read("snapshot"), &read("journal"));
        assert_eq!(saved, Ok(values.after.clone()), "{changed}");

        // As the file store keeps it, read from a copy, as loading locks it.
        let copy = Scratch::new(&format!("sample-v{version}"));
        fs::create_dir_all(copy.0.join("work")).expect("a copy");
        let kept = folder(version).join("files/work");
        for entry in fs::read_dir(&kept).expect("the sample's files") {
            let entry = entry.expect("a file");
            fs::copy(entry.path(), copy.0.join("work").join(entry.file_name())).expect("copied");
        }
        let mut store = FileStore::new(&copy.0);
        let mut listed = store.list(&shelf()).expect("it lists");
        listed.sort();
        let stored = store.load(&shelf(), id()).expect("it loads");
        let other = store.load(&shelf(), squeezed_id()).expect("it loads");
        let flags =
            fs::read(copy.0.join(format!("work/{}.caf", squeezed_id()))).expect("it's there")[8];
        assert_eq!(listed.len(), 2, "{changed}");
        assert_eq!(decode_header(&listed[0].1), Ok(values.header), "{changed}");
        let read = decode(&stored.snapshot, &stored.journal);
        assert_eq!(read, Ok(values.after), "{changed}");
        assert_eq!(flags, 1, "{changed}: the second chat is squeezed");
        let read = decode(&other.snapshot, &other.journal);
        assert_eq!(read, Ok(values.squeezed), "{changed}");
    }
}

/// What a test failing here means.
const SHAPE_CHANGED: &str = "what's saved has changed, but caf_save::VERSION hasn't: every change \
    to a saved type, a field or variant added included, needs a new version, so an older CAF knows \
    it can't read it, with a sample of its own (samples/save/v<VERSION>/). Only while a version is \
    unreleased may `CAF_BLESS_SAMPLE=1 cargo test -p caf-save --test sample` rewrite its sample";

#[test]
fn this_version_writes_what_its_sample_holds() {
    let read = |name| read(VERSION, name);
    let pinned = |name| String::from_utf8(read(name)).expect("text");
    // Every saved type, every field and variant: as the TypeScript types
    // generated from them say, and as serde itself writes them.
    assert!(
        pinned("shapes.ts") == shapes(),
        "shapes.ts: {SHAPE_CHANGED}"
    );
    assert!(
        pinned("variants.txt") == variants(),
        "variants.txt: {SHAPE_CHANGED}"
    );
    // What it writes of the sample's values is what's in the sample.
    let values = sample(VERSION);
    let header = encode_header(&values.header);
    assert!(
        inside(&header) == inside(&read("header")),
        "header: {SHAPE_CHANGED}"
    );
    let snapshot = encode_snapshot(&values.before, Squeeze::Fast);
    assert!(
        inside(&snapshot) == inside(&read("snapshot")),
        "snapshot: {SHAPE_CHANGED}"
    );
    let records: Vec<u8> = (values.records.iter())
        .flat_map(|each| encode_record(each))
        .collect();
    assert!(
        insides(&records) == insides(&read("journal")),
        "journal: {SHAPE_CHANGED}"
    );
}
