//! Saving chats and restoring them: when a chat is written, and how; what
//! a restore finds and tidies; and putting away, closing, deleting,
//! moving and listing saved chats.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Value, json};

use super::harness::{EPOCH, Script, assert_has, host, sent, with};
use super::{CHAT, PROC, SESSION, claude_init, plain_init};
use crate::Refused;
use crate::chat::{Chat, NoAppData};
use crate::core::OpenChat;
use crate::directive::StorageDirective;
use crate::event::{AgentEvent, Exit, StorageEvent};
use crate::ids::{ChatId, Shelf};
use crate::save::{Entry, Header, RestoreChat, Saved};
use crate::time::Wall;

/// The shelf tests keep chats on.
const SHELF: &str = "work";

/// Opening a Claude chat in `/work`, kept on [`SHELF`].
fn saved_open(agent: &str) -> OpenChat {
    let mut open = OpenChat::new(agent, "/work");
    open.shelf = Some(Shelf(SHELF.to_owned()));
    open
}

/// The storage directives in `out`.
fn storage(out: &[Value]) -> Vec<Value> {
    (with(out, "storage").into_iter())
        .map(|directive| directive["storage"].clone())
        .collect()
}

/// The one storage directive in `out`.
#[track_caller]
fn one_storage(out: &[Value]) -> Value {
    let all = storage(out);
    assert_eq!(all.len(), 1, "one storage directive in {out:#?}");
    all.into_iter().next().unwrap_or_default()
}

/// A directive's ticket.
fn ticket(directive: &Value) -> u64 {
    directive["value"]["ticket"].as_u64().expect("a ticket")
}

/// What the store reports for work done.
fn done(written: u64) -> StorageEvent {
    StorageEvent::Done { written }
}

/// A store in memory, as a storage driver keeps one: each chat's snapshot
/// and journal, by shelf.
#[derive(Default)]
struct Store {
    /// Each chat's snapshot and the journal after it.
    chats: BTreeMap<(Shelf, ChatId), (Saved, Vec<Entry>)>,
    /// The chats locked.
    locked: BTreeSet<ChatId>,
    /// Work given and not done yet, oldest first.
    work: VecDeque<StorageDirective>,
    /// How many snapshots, journal entries and failures it has seen.
    counts: [usize; 3],
}

impl Store {
    /// Takes on the storage work in `out`.
    fn take(&mut self, out: &[Value]) {
        for directive in storage(out) {
            let work = serde_json::from_value(directive).expect("storage work reads back");
            self.work.push_back(work);
        }
    }

    /// Does the oldest work that's answered, and returns its ticket and
    /// answer: or fails it, if it's a write and `fail`.
    fn next(&mut self, fail: bool) -> Option<(u64, StorageEvent)> {
        loop {
            let work = self.work.pop_front()?;
            let (ticket, event) = match work {
                StorageDirective::Snapshot {
                    ticket,
                    shelf,
                    saved,
                } => {
                    if fail {
                        self.counts[2] += 1;
                        return Some((ticket.0, StorageEvent::Failed("disk full".to_owned())));
                    }
                    self.counts[0] += 1;
                    let written = serde_json::to_vec(&saved).expect("JSON").len();
                    self.locked.insert(saved.chat.id);
                    self.chats
                        .insert((shelf, saved.chat.id), (*saved, Vec::new()));
                    (ticket, done(written as u64))
                }
                StorageDirective::Append {
                    ticket,
                    shelf,
                    header,
                    entries,
                } => {
                    if fail {
                        self.counts[2] += 1;
                        return Some((ticket.0, StorageEvent::Failed("disk full".to_owned())));
                    }
                    self.counts[1] += entries.len();
                    let written = serde_json::to_vec(&entries).expect("JSON").len();
                    let (saved, journal) = (self.chats.get_mut(&(shelf, header.chat)))
                        .expect("a journal is added to after its snapshot");
                    assert!(self.locked.contains(&header.chat), "written while locked");
                    saved.header = header;
                    journal.extend(entries);
                    (ticket, done(written as u64))
                }
                StorageDirective::Release { chat, .. } => {
                    assert!(self.locked.remove(&chat), "released only if locked");
                    continue;
                }
                StorageDirective::Load {
                    ticket,
                    shelf,
                    chat,
                } => {
                    let event = match self.read(&shelf, chat) {
                        Some(saved) if !self.locked.contains(&chat) => {
                            self.locked.insert(chat);
                            StorageEvent::Loaded(Box::new(saved))
                        }
                        Some(_) => StorageEvent::NotLoaded(crate::save::CannotRestore::InUse),
                        None => StorageEvent::NotLoaded(crate::save::CannotRestore::NotFound),
                    };
                    (ticket, event)
                }
                StorageDirective::List { ticket, shelf } => {
                    let chats = (self.chats.iter())
                        .filter(|((on, _), _)| *on == shelf)
                        .map(|(_, (saved, _))| saved.header.clone())
                        .collect();
                    let unreadable = Vec::new();
                    (ticket, StorageEvent::Listed { chats, unreadable })
                }
                StorageDirective::Delete {
                    ticket,
                    shelf,
                    chat,
                } => {
                    self.chats.remove(&(shelf, chat));
                    self.locked.remove(&chat);
                    (ticket, done(0))
                }
                StorageDirective::MoveShelf { ticket, from, to } => {
                    let moving: Vec<_> = (self.chats.keys())
                        .filter(|(on, _)| *on == from)
                        .cloned()
                        .collect();
                    for key in moving {
                        let stored = self.chats.remove(&key).expect("there");
                        self.chats.insert((to.clone(), key.1), stored);
                    }
                    (ticket, done(0))
                }
            };
            return Some((ticket.0, event));
        }
    }

    /// A chat as it's read: its snapshot, with its journal applied.
    fn read(&self, shelf: &Shelf, chat: ChatId) -> Option<Saved> {
        let (snapshot, journal) = self.chats.get(&(shelf.clone(), chat))?;
        let mut saved = snapshot.clone();
        for entry in journal {
            apply(&mut saved, entry);
        }
        Some(saved)
    }
}

/// Applies a journal entry to a chat as it's saved.
fn apply(saved: &mut Saved, entry: &Entry) {
    match entry {
        Entry::Change(change) => saved.chat.apply(change).expect("a journal entry fits"),
        Entry::Session(session) => saved.session.clone_from(session),
        Entry::Data(data) => saved.data = *data,
    }
}

impl Script {
    /// Opens a saved chat with `agent` and its session, on a core with no
    /// chats yet: Claude's resumes, a plain agent's loads.
    fn open_saved(&mut self, agent: &str) {
        let (chat, out) = self.open(saved_open(agent));
        assert_eq!(chat, Ok(CHAT));
        let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
        let answer = if agent == "claude" {
            claude_init()
        } else {
            plain_init(true)
        };
        let out = self.answers(PROC, init, &answer);
        let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
        self.answers(PROC, new, &json!({ "sessionId": SESSION }));
    }

    /// Gives the store's answers back until it has nothing more to do,
    /// failing every write if `fail`, and returns what they gave out.
    fn serve(&mut self, store: &mut Store, fail: bool) -> Vec<Value> {
        let mut all = Vec::new();
        while let Some((ticket, event)) = store.next(fail) {
            let out = self.storage(ticket, event);
            store.take(&out);
            all.extend(out);
        }
        all
    }
}

/// A chat as a restore finds it, from its JSON, saved at `changed` ms
/// with `session` to continue.
fn saved(chat: &Value, session: Option<&str>, changed: i64) -> Saved {
    let chat: Chat = serde_json::from_value(chat.clone()).expect("a chat");
    Saved {
        header: Header {
            chat: chat.id,
            agent: chat.agent.clone(),
            profile: Some("claude-agent-acp".to_owned()),
            cwd: chat.cwd.clone(),
            made: Wall(EPOCH - 60_000),
            changed: Wall(EPOCH + changed),
            label: None,
            first: None,
            turns: chat.turns.len() as u64,
            closed: false,
        },
        chat,
        session: session.map(|session| crate::ids::AgentSessionId(session.to_owned())),
        data: NoAppData,
    }
}

/// A chat whose agent was mid-turn when it was saved: a finished turn, a
/// turn running with words still streaming and a tool waiting, a message
/// waiting to be sent, and a question.
fn mid_turn(agent: &str) -> Value {
    let at = |ms: i64| json!(EPOCH - 50_000 + ms);
    let text = |text: &str| json!({ "type": "User", "value": { "parts": [{ "type": "Text", "value": text }] } });
    let tool = |status: &str| json!({ "type": "Tool", "value": { "title": "Run `cargo test`", "kind": { "type": "Execute" }, "status": { "type": status }, "input": null, "output": "", "app": null } });
    json!({
        "id": CHAT, "agent": agent, "cwd": "/work",
        "state": { "type": "Open", "value": { "session": "old" } },
        "usage": null,
        "turns": [
            { "id": 5000, "starter": text("Hello"), "life": { "type": "Finished" }, "imported": false,
              "window": { "start": at(0), "end": { "type": "At", "value": at(10) } },
              "items": [{ "id": 5001, "parent": null, "window": { "start": at(5), "end": { "type": "At", "value": at(5) } }, "kind": { "type": "Words", "value": "Hi." } }] },
            { "id": 5002, "starter": text("Run the tests"), "life": { "type": "Running" }, "imported": false,
              "window": { "start": at(20), "end": { "type": "Open" } },
              "items": [
                  { "id": 5003, "parent": null, "window": { "start": at(21), "end": { "type": "Open" } }, "kind": { "type": "Words", "value": "Running" } },
                  { "id": 5004, "parent": null, "window": { "start": at(22), "end": { "type": "At", "value": at(23) } }, "kind": tool("Done") },
                  { "id": 5005, "parent": null, "window": { "start": at(24), "end": { "type": "Open" } }, "kind": tool("Pending") }
              ] },
            { "id": 5006, "starter": text("and then?"), "life": { "type": "Waiting" }, "imported": false,
              "window": { "start": at(30), "end": { "type": "Open" } }, "items": [] }
        ],
        "questions": [{ "id": 5007, "window": { "start": at(25), "end": { "type": "Open" } },
                        "kind": { "type": "SlowToOpen", "value": { "adapter_answered": true } } }]
    })
}

#[test]
fn a_new_saved_chat_is_written_whole_a_second_after_it_changes() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    let (chat, out) = script.open(saved_open("claude"));
    assert_eq!(chat, Ok(CHAT));
    assert!(storage(&out).is_empty(), "not written at once");
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 1000 })]);
    script.answers(PROC, 1000, &claude_init());
    script.answers(PROC, 1001, &json!({ "sessionId": SESSION }));

    script.at(1000);
    let out = script.woken();
    let snapshot = one_storage(&out);
    assert_has(
        &snapshot,
        &json!({ "type": "Snapshot", "value": { "shelf": SHELF, "saved": {
            "header": {
                "chat": CHAT, "agent": "claude", "profile": "claude-agent-acp", "cwd": "/work",
                "made": EPOCH, "changed": EPOCH, "label": null, "first": null, "turns": 0, "closed": false
            },
            "chat": { "id": CHAT, "state": { "type": "Open", "value": { "session": SESSION } } },
            "session": SESSION,
            "data": null
        } } }),
    );
    store.take(&out);
    script.serve(&mut store, false);

    // Nothing changed: nothing is written.
    script.at(5000);
    assert!(storage(&script.woken()).is_empty());
    assert_eq!(
        store
            .read(&Shelf(SHELF.to_owned()), CHAT)
            .map(|saved| saved.chat),
        script.core.chat(CHAT).cloned()
    );
}

#[test]
fn streamed_words_are_journaled_as_one_entry_and_a_turn_ending_saves_at_once() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(10_000));

    script.at(5000);
    let (turn, out) = script.send(CHAT, "  Fix the tests\nplease");
    let turn = turn.expect("sent");
    assert!(storage(&out).is_empty(), "written a second later");
    let prompt = sent(&out, "session/prompt")["id"].as_u64().expect("an id");
    for (ms, text) in [(5100, "Run"), (5200, "ning "), (5300, "them.")] {
        script.at(ms);
        let chunk = json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } });
        assert!(storage(&script.update(PROC, SESSION, &chunk)).is_empty());
    }
    script.at(6000);
    let out = script.woken();
    let append = one_storage(&out);
    let item = turn.0 + 1;
    assert_has(
        &append,
        &json!({ "type": "Append", "value": {
            "shelf": SHELF,
            "header": { "chat": CHAT, "changed": EPOCH + 5300, "first": "Fix the tests", "turns": 1 },
            "entries": [
                { "type": "Change", "value": { "type": "TurnAdded", "value": { "id": turn.0, "life": { "type": "Waiting" } } } },
                { "type": "Change", "value": { "type": "TurnLife", "value": { "turn": turn.0, "life": { "type": "Running" } } } },
                { "type": "Change", "value": { "type": "ItemAdded", "value": { "turn": turn.0, "item": { "id": item, "kind": { "type": "Words", "value": "Run" } } } } },
                { "type": "Change", "value": { "type": "Appended", "value": { "item": item, "text": "ning them." } } }
            ]
        } }),
    );
    script.storage(ticket(&append), done(10));

    // The turn ending is written as the call ends.
    script.at(6500);
    let out = script.answers(PROC, prompt, &json!({ "stopReason": "end_turn" }));
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Append", "value": { "entries": [
            { "type": "Change", "value": { "type": "ItemEnded", "value": { "item": item } } },
            { "type": "Change", "value": { "type": "TurnLife", "value": { "turn": turn.0, "life": { "type": "Finished" } } } }
        ] } }),
    );
}

#[test]
fn a_journal_that_outgrows_its_snapshot_is_written_whole_next() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(100));

    let write = |script: &mut Script, ms: u64, text: &str| {
        script.at(ms);
        script.send(CHAT, text).0.expect("sent");
        script.at(ms + 1000);
        one_storage(&script.woken())
    };
    let append = write(&mut script, 2000, "one");
    assert_eq!(append["type"], "Append");
    // 100 bytes of journal isn't more than the snapshot's 100.
    script.storage(ticket(&append), done(100));
    let append = write(&mut script, 4000, "two");
    assert_eq!(append["type"], "Append");
    script.storage(ticket(&append), done(1));
    let snapshot = write(&mut script, 6000, "three");
    assert_eq!(snapshot["type"], "Snapshot");
    assert_eq!(
        snapshot["value"]["saved"]["chat"]["turns"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    // The journal starts again from the new snapshot.
    script.storage(ticket(&snapshot), done(1000));
    let append = write(&mut script, 8000, "four");
    assert_eq!(append["type"], "Append");
}

#[test]
fn a_chats_journal_keeps_every_change_however_few_viewers_keep() {
    let mut host = host();
    host.limits.kept_changes = 2;
    let mut script = Script::new(host);
    let mut store = Store::default();
    script.open_saved("claude");
    script.at(1000);
    store.take(&script.woken());
    script.serve(&mut store, false);

    script.at(2000);
    script.send(CHAT, "Run them all").0.expect("sent");
    for tool in 0..20 {
        let call = json!({ "sessionUpdate": "tool_call", "toolCallId": format!("t{tool}"), "title": "Read", "kind": "read", "status": "completed" });
        script.update(PROC, SESSION, &call);
    }
    script.at(3000);
    let out = script.woken();
    let append = one_storage(&out);
    assert_eq!(append["type"], "Append");
    assert!(append["value"]["entries"].as_array().map_or(0, Vec::len) > 20);
    store.take(&out);
    script.serve(&mut store, false);
    assert_eq!(
        store
            .read(&Shelf(SHELF.to_owned()), CHAT)
            .map(|saved| saved.chat),
        script.core.chat(CHAT).cloned()
    );
}

#[test]
fn a_failed_write_is_tried_again_whole_later() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    script.at(1000);
    store.take(&script.woken());
    script.serve(&mut store, false);

    script.at(2000);
    script.send(CHAT, "one").0.expect("sent");
    script.at(3000);
    store.take(&script.woken());
    let out = script.serve(&mut store, true);
    assert_has(
        &json!(with(&out, "report")),
        &json!([{ "report": "Warn", "message": format!("chat {CHAT} couldn't be saved, and is tried again whole in 1 s: disk full") }]),
    );
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 4000 })]);
    // A change meanwhile waits for the retry.
    script.at(3500);
    script.send(CHAT, "two").0.expect("sent");
    // It fails again, and waits twice as long.
    script.at(4000);
    let out = script.woken();
    assert_eq!(one_storage(&out)["type"], "Snapshot");
    store.take(&out);
    let out = script.serve(&mut store, true);
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 6000 })]);
    script.at(6000);
    let out = script.woken();
    assert_eq!(one_storage(&out)["type"], "Snapshot");
    store.take(&out);
    script.serve(&mut store, false);
    assert_eq!(
        store
            .read(&Shelf(SHELF.to_owned()), CHAT)
            .map(|saved| saved.chat),
        script.core.chat(CHAT).cloned()
    );
    // Then on as before.
    script.at(7000);
    script.send(CHAT, "three").0.expect("sent");
    script.at(8000);
    assert_eq!(one_storage(&script.woken())["type"], "Append");
}

#[test]
fn a_restore_finds_what_was_under_way_and_tidies_it() {
    let chat: Chat = serde_json::from_value(mid_turn("claude")).expect("a chat");
    let at = Wall(EPOCH);
    let changes = crate::core::saving::tidy(&chat, at);
    let end = json!({ "type": "At", "value": EPOCH });
    assert_eq!(
        serde_json::to_value(&changes).expect("JSON"),
        json!([
            { "type": "ItemEnded", "value": { "item": 5003, "end": end } },
            { "type": "ToolChanged", "value": { "item": 5005, "tool": { "title": "Run `cargo test`", "kind": { "type": "Execute" }, "status": { "type": "Failed" }, "input": null, "output": "", "app": null } } },
            { "type": "ItemEnded", "value": { "item": 5005, "end": end } },
            { "type": "TurnLife", "value": { "turn": 5002, "life": { "type": "CutOff" }, "end": end } },
            { "type": "TurnLife", "value": { "turn": 5006, "life": { "type": "Stopped" }, "end": end } },
            { "type": "QuestionGone", "value": 5007 },
            { "type": "State", "value": { "type": "Idle" } }
        ])
    );
    // Tidy is tidy: once tidied, nothing more.
    let mut tidied = chat.clone();
    for change in &changes {
        tidied.apply(change).expect("it fits");
    }
    assert!(crate::core::saving::tidy(&tidied, at).is_empty());
    // A turn never ends before it started, whatever the clock said.
    let early = crate::core::saving::tidy(&chat, Wall(0));
    assert!(early.iter().all(|change| !matches!(
        change,
        crate::Change::TurnLife {
            end: crate::time::End::At(Wall(0)),
            ..
        }
    )));
}

#[test]
fn a_restored_chat_is_tidied_and_idle_and_starts_its_agent_only_when_sent_to() {
    let mut script = Script::new(host());
    let mut restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    restore.mcp_servers = vec![crate::agent::McpServer::Http {
        name: "tools".to_owned(),
        url: "http://127.0.0.1:9/mcp".to_owned(),
        headers: Vec::new(),
    }];
    let (done_, out) = script.restore(restore);
    done_.expect("restoring");
    assert_eq!(
        storage(&out),
        [json!({ "type": "Load", "value": { "ticket": 1000, "shelf": SHELF, "chat": CHAT } })]
    );
    assert!(script.core.chat(CHAT).is_none(), "not loaded until read");

    script.at(100);
    let saved = saved(&mid_turn("claude"), Some("old"), -40_000);
    let out = script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    assert!(with(&out, "start").is_empty(), "no agent started");
    // Tidying it is saved, as journal entries: at once, as turns ended.
    let append = one_storage(&out);
    assert_eq!(append["type"], "Append");
    assert_eq!(append["value"]["entries"].as_array().map(Vec::len), Some(7));
    let changed = json!({ "type": "At", "value": EPOCH - 40_000 });
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Idle" },
            "questions": [],
            "turns": [
                { "id": 5000, "life": { "type": "Finished" } },
                { "id": 5002, "life": { "type": "CutOff" }, "window": { "end": changed }, "items": [
                    { "id": 5003, "window": { "end": changed } },
                    { "id": 5004, "kind": { "value": { "status": { "type": "Done" } } } },
                    { "id": 5005, "kind": { "value": { "status": { "type": "Failed" } } } }
                ] },
                { "id": 5006, "life": { "type": "Stopped" } }
            ]
        }),
    );

    // Sending starts its agent, and continues its session, with the
    // host's tools; new ids count on from past the chat's (its
    // connection is 5007, and the write 5008).
    script.at(2000);
    let (turn, out) = script.send(CHAT, "Go on");
    assert_eq!(turn, Ok(crate::TurnId(5009)));
    assert_eq!(with(&out, "start").len(), 1);
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &claude_init());
    assert_has(
        &sent(&out, "session/resume"),
        &json!({ "params": { "sessionId": "old", "cwd": "/work", "mcpServers": [{ "type": "http", "name": "tools" }] } }),
    );
}

#[test]
fn a_restored_chat_the_agent_can_only_load_throws_away_what_it_replays() {
    let mut script = Script::new(host());
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let chat = mid_turn("plain");
    let mut saved = saved(&chat, Some("old"), 0);
    saved.header.profile = None;
    script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    let before = script.chat(CHAT);

    let (turn, out) = script.send(CHAT, "Go on");
    turn.expect("sent");
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &plain_init(true));
    let load = sent(&out, "session/load");
    assert_eq!(load["params"]["sessionId"], "old");
    let id = load["id"].as_u64().expect("an id");
    let chunk = |kind: &str, text: &str| json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } });
    script.update(proc, "old", &chunk("user_message_chunk", "Hello"));
    script.update(proc, "old", &chunk("agent_message_chunk", "Hi."));
    script.update(proc, "old", &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit", "kind": "edit", "status": "completed" }));
    // Nothing replayed acts.
    let out = script.says(proc, &json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/request_permission",
        "params": { "sessionId": "old", "toolCall": { "toolCallId": "t2" }, "options": [{ "optionId": "y", "name": "Yes", "kind": "allow_once" }] }
    }));
    assert!(with(&out, "decide").is_empty(), "no policy is asked");
    let out = script.answers(proc, id, &json!({}));
    sent(&out, "session/prompt");
    let mut turns = script.chat(CHAT)["turns"].clone();
    // Only the message sent was added: nothing of the replay.
    let added = turns
        .as_array_mut()
        .and_then(Vec::pop)
        .expect("the turn sent");
    assert_eq!(added["id"], 5009);
    assert_eq!(turns, before["turns"]);
}

#[test]
fn a_restored_session_the_agent_no_longer_has_opens_new_and_the_chat_says_so() {
    let mut script = Script::new(host());
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let saved = saved(&mid_turn("claude"), Some("old"), 0);
    let out = script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    script.storage(ticket(&one_storage(&out)), done(10_000));
    let out = script.send(CHAT, "Go on").1;
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &claude_init());
    let resume = sent(&out, "session/resume")["id"].as_u64().expect("an id");
    let out = script.fails(
        proc,
        resume,
        &json!({ "code": -32002, "message": "Resource not found: old" }),
    );
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    script.answers(proc, new, &json!({ "sessionId": "fresh" }));
    let chat = script.chat(CHAT);
    let turns = chat["turns"].as_array().expect("turns");
    assert_has(
        &turns[turns.len() - 2],
        &json!({ "starter": { "type": "Caf" }, "items": [{ "kind": { "type": "Note", "value": { "type": "SessionGone" } } }] }),
    );
    assert_eq!(
        chat["state"],
        json!({ "type": "Open", "value": { "session": "fresh" } })
    );
    // Saved with its new session: whole, as the size of the snapshot it
    // was restored from isn't known.
    script.at(1000);
    let snapshot = one_storage(&script.woken());
    assert_has(
        &snapshot,
        &json!({ "type": "Snapshot", "value": { "saved": { "session": "fresh" } } }),
    );
}

#[test]
fn the_journal_hears_when_the_session_to_continue_changes() {
    let mut script = Script::new(host());
    let mut open = saved_open("claude");
    open.session =
        crate::core::SessionChoice::Continue(crate::ids::AgentSessionId("old".to_owned()));
    script.open(open).0.expect("opened");
    let out = script.answers(PROC, 1000, &claude_init());
    let resume = sent(&out, "session/resume")["id"].as_u64().expect("an id");
    script.at(1000);
    let snapshot = one_storage(&script.woken());
    assert_eq!(snapshot["value"]["saved"]["session"], "old");
    script.storage(ticket(&snapshot), done(10_000));
    let out = script.fails(
        PROC,
        resume,
        &json!({ "code": -32002, "message": "Resource not found: old" }),
    );
    let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
    script.answers(PROC, new, &json!({ "sessionId": "fresh" }));
    script.at(2000);
    let append = one_storage(&script.woken());
    let entries = append["value"]["entries"].as_array().expect("entries");
    let sessions: Vec<&Value> = (entries.iter())
        .filter(|entry| entry["type"] == "Session")
        .collect();
    // Gone, then a new one.
    assert_eq!(
        sessions,
        [
            &json!({ "type": "Session", "value": null }),
            &json!({ "type": "Session", "value": "fresh" })
        ]
    );
}

#[test]
fn a_chat_whose_agent_is_gone_comes_back_and_sending_to_it_is_refused() {
    let mut script = Script::new(host());
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let mut saved = saved(&mid_turn("gone"), Some("old"), 0);
    saved.header.profile = Some("long-gone-acp".to_owned());
    let out = script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    assert_eq!(script.chat(CHAT)["agent"], "gone");
    assert_eq!(script.send(CHAT, "hello?").0, Err(Refused::NoSuchAgent));
    // Its header keeps the profile it was saved with.
    let append = one_storage(&out);
    assert_has(
        &append,
        &json!({ "value": { "header": { "agent": "gone", "profile": "long-gone-acp" } } }),
    );
}

#[test]
fn a_chat_whose_agent_two_agents_could_be_isnt_rebound() {
    // Two of the test's agents have Claude's profile.
    let mut script = Script::new(host());
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let saved = saved(&mid_turn("old-claude"), Some("old"), 0);
    script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    assert_eq!(script.chat(CHAT)["agent"], "old-claude");
    assert_eq!(script.send(CHAT, "hello?").0, Err(Refused::NoSuchAgent));
}

#[test]
fn a_chat_whose_agent_was_renamed_comes_back_with_the_one_agent_of_its_profile() {
    let claude = crate::agent::AgentSpec::command(
        "claude-renamed",
        crate::agent::SpawnCommand::new("/opt/caf/claude-agent-acp"),
        crate::profiles::ProfileChoice::Builtin(crate::profiles::Builtin::ClaudeAgentAcp),
    );
    let mut script = Script::offering(host(), vec![claude]);
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let saved = saved(&mid_turn("claude"), Some("old"), 0);
    let out = script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    assert_has(
        &json!(with(&out, "report")),
        &json!([{ "report": "Info", "message": format!("chat {CHAT}: its agent \"claude\" is now called \"claude-renamed\"") }]),
    );
    assert_eq!(script.chat(CHAT)["agent"], "claude-renamed");
    let out = script.send(CHAT, "Go on").1;
    assert_eq!(with(&out, "start").len(), 1);
}

#[test]
fn a_chat_loaded_or_being_restored_isnt_restored_again() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    assert_eq!(script.restore(restore.clone()).0, Err(Refused::Loaded));
    // Put away, it's taken until it's written whole.
    let out = script.put_away(CHAT).1;
    assert_eq!(script.restore(restore.clone()).0, Err(Refused::Loaded));
    store.take(&out);
    script.serve(&mut store, false);
    let out = script.restore(restore.clone()).1;
    assert_eq!(script.restore(restore.clone()).0, Err(Refused::Loaded));
    store.take(&out);
    script.serve(&mut store, false);
    assert_eq!(script.restore(restore).0, Err(Refused::Loaded));
}

#[test]
fn what_cant_be_saved_or_restored_is_refused() {
    let mut script = Script::new(host());
    let long = "x".repeat(257);
    for bad in ["", long.as_str(), "a\nb", "tab\there"] {
        let mut open = OpenChat::new("claude", "/work");
        open.shelf = Some(Shelf(bad.to_owned()));
        assert_eq!(script.open(open).0, Err(Refused::BadShelf), "{bad:?}");
        assert_eq!(script.list_saved(bad).0, Err(Refused::BadShelf));
        let restore = RestoreChat::new(CHAT, Shelf(bad.to_owned()));
        assert_eq!(script.restore(restore).0, Err(Refused::BadShelf));
        assert_eq!(script.delete(CHAT, bad).0, Err(Refused::BadShelf));
        assert_eq!(script.move_shelf(SHELF, bad).0, Err(Refused::BadShelf));
        assert_eq!(script.move_shelf(bad, SHELF).0, Err(Refused::BadShelf));
    }
    // 256 bytes is fine.
    script.list_saved(&"é".repeat(128)).0.expect("listed");
    let mut restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    let mut tool = crate::agent::SpawnCommand::new("/bin/tool");
    tool.cwd = Some("/tmp".into());
    restore.mcp_servers = vec![crate::agent::McpServer::Stdio {
        name: "tool".to_owned(),
        command: tool,
    }];
    assert_eq!(script.restore(restore).0, Err(Refused::Unsendable));
    assert_eq!(script.put_away(CHAT).0, Err(Refused::NoSuchChat));
    let unsaved = (script.open(OpenChat::new("claude", "/work")).0).expect("opened");
    assert_eq!(script.put_away(unsaved).0, Err(Refused::NotSaved));
    assert_eq!(script.delete(unsaved, SHELF).0, Err(Refused::NoSuchChat));
    assert_eq!(
        script.set_data(ChatId::from_number(4242)).0,
        Err(Refused::NoSuchChat)
    );
}

#[test]
fn putting_a_chat_away_cuts_its_turn_off_writes_it_whole_and_lets_it_go() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    store.take(&script.list_saved(SHELF).1);
    script.serve(&mut store, false);
    script.at(1000);
    store.take(&script.woken());
    script.serve(&mut store, false);
    script.send(CHAT, "Run the tests").0.expect("sent");
    script.send(CHAT, "and then?").0.expect("queued");

    script.at(1500);
    let out = script.put_away(CHAT).1;
    // Its session ends with its adapter, which nothing else uses.
    assert_eq!(with(&out, "close_input"), [json!({ "close_input": PROC })]);
    let snapshot = one_storage(&out);
    assert_has(
        &snapshot,
        &json!({ "type": "Snapshot", "value": { "saved": {
            "header": { "turns": 2, "closed": false, "first": "Run the tests" },
            "chat": { "state": { "type": "Idle" }, "turns": [
                { "life": { "type": "CutOff" } }, { "life": { "type": "Stopped" } }
            ] },
            "session": SESSION
        } } }),
    );
    assert!(script.core.chat(CHAT).is_none(), "gone from memory");
    assert!(!script.core.is_idle(), "until it's written and let go");
    // A debugger sees it waiting to be written.
    assert_has(
        &script.state()["saving"]["away"][CHAT.to_string()],
        &json!({ "shelf": SHELF, "writing": ticket(&snapshot) }),
    );
    let saved = script.core.saved(&Shelf(SHELF.to_owned())).expect("listed");
    assert_has(
        &serde_json::to_value(&saved).expect("JSON"),
        &json!([{ "shelf": SHELF, "header": { "chat": CHAT, "turns": 2 }, "state": { "type": "Saved" } }]),
    );
    store.take(&out);
    let out = script.serve(&mut store, false);
    assert!(store.locked.is_empty(), "let go: {out:#?}");

    // Restoring it carries on.
    script.at(2000);
    let out = script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .1;
    store.take(&out);
    let out = script.serve(&mut store, false);
    assert!(storage(&out).is_empty(), "nothing to tidy");
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Idle" }, "turns": [{ "life": { "type": "CutOff" } }, { "life": { "type": "Stopped" } }] }),
    );
    assert_has(
        &serde_json::to_value(script.core.saved(&Shelf(SHELF.to_owned()))).expect("JSON"),
        &json!([{ "state": { "type": "Loaded" } }]),
    );
}

#[test]
fn a_failed_put_away_is_tried_again_and_let_go_once_written() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    let out = script.put_away(CHAT).1;
    store.take(&out);
    let out = script.serve(&mut store, true);
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 1000 })]);
    script.at(1000);
    let out = script.woken();
    assert_eq!(one_storage(&out)["type"], "Snapshot");
    store.take(&out);
    script.serve(&mut store, false);
    assert!(store.locked.is_empty());
    assert_eq!(script.state()["saving"]["away"], json!({}));
}

#[test]
fn closing_a_saved_chat_puts_it_away_closed_and_restoring_it_brings_it_back_idle() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    script.send(CHAT, "Hi").0.expect("sent");
    let out = script.close(CHAT).1;
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Snapshot", "value": { "saved": {
            "header": { "closed": true },
            "chat": { "state": { "type": "Closed" }, "turns": [{ "life": { "type": "Stopped" } }] }
        } } }),
    );
    assert!(script.core.chat(CHAT).is_none(), "put away");
    store.take(&out);
    script.serve(&mut store, false);

    let out = script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .1;
    store.take(&out);
    script.serve(&mut store, false);
    assert_eq!(script.chat(CHAT)["state"], json!({ "type": "Idle" }));
    script.send(CHAT, "Again").0.expect("it can be sent to");
}

#[test]
fn deleting_a_chat_closes_it_lets_it_go_and_removes_it() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    script.at(1000);
    store.take(&script.woken());
    script.serve(&mut store, false);
    store.take(&script.list_saved(SHELF).1);
    script.serve(&mut store, false);
    assert_eq!(
        script
            .core
            .saved(&Shelf(SHELF.to_owned()))
            .map(|list| list.len()),
        Some(1)
    );

    assert_eq!(script.delete(CHAT, "elsewhere").0, Err(Refused::NoSuchChat));
    let out = script.delete(CHAT, SHELF).1;
    assert_eq!(with(&out, "close_input"), [json!({ "close_input": PROC })]);
    assert_eq!(
        storage(&out),
        [
            json!({ "type": "Delete", "value": { "ticket": ticket(&storage(&out)[0]), "shelf": SHELF, "chat": CHAT } })
        ]
    );
    assert!(script.core.chat(CHAT).is_none());
    assert_eq!(
        script.core.saved(&Shelf(SHELF.to_owned())),
        Some(Vec::new())
    );
    store.take(&out);
    script.serve(&mut store, false);
    assert!(store.chats.is_empty());
    // One that isn't loaded is just removed.
    let out = script.delete(ChatId::from_number(4242), SHELF).1;
    assert_eq!(one_storage(&out)["type"], "Delete");
}

#[test]
fn a_chat_deleted_while_it_was_being_restored_is_let_go_when_it_comes() {
    let mut script = Script::new(host());
    let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    script.restore(restore).0.expect("restoring");
    script.delete(CHAT, SHELF).0.expect("deleted");
    let saved = saved(&mid_turn("claude"), Some("old"), 0);
    let out = script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    assert_eq!(
        storage(&out),
        [json!({ "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } })]
    );
    assert!(script.core.chat(CHAT).is_none());
}

#[test]
fn moving_a_shelf_moves_its_loaded_chats_and_its_list() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    script.at(1000);
    store.take(&script.woken());
    script.serve(&mut store, false);
    for shelf in [SHELF, "renamed"] {
        store.take(&script.list_saved(shelf).1);
    }
    script.serve(&mut store, false);

    let out = script.move_shelf(SHELF, "renamed").1;
    assert_has(
        &one_storage(&out),
        &json!({ "type": "MoveShelf", "value": { "from": SHELF, "to": "renamed" } }),
    );
    let renamed = Shelf("renamed".to_owned());
    assert_eq!(
        script.core.saved(&Shelf(SHELF.to_owned())),
        Some(Vec::new())
    );
    assert_has(
        &serde_json::to_value(script.core.saved(&renamed)).expect("JSON"),
        &json!([{ "shelf": "renamed", "header": { "chat": CHAT }, "state": { "type": "Loaded" } }]),
    );
    assert_eq!(script.core.summaries()[0].shelf.as_ref(), Some(&renamed));
    store.take(&out);
    // Once moved, the shelf it went to is read again.
    let out = script.serve(&mut store, false);
    assert_has(
        &json!(storage(&out)),
        &json!([{ "type": "List", "value": { "shelf": "renamed" } }]),
    );
    // Its next write goes to its new shelf.
    script.at(2000);
    script.send(CHAT, "Hi").0.expect("sent");
    script.at(3000);
    assert_eq!(one_storage(&script.woken())["value"]["shelf"], "renamed");
}

#[test]
fn a_listed_shelf_is_kept_up_to_date() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    // Another program's chat is there already.
    let other = ChatId::from_number(9000);
    let mut theirs = saved(&mid_turn("claude"), None, 0);
    theirs.chat.id = other;
    theirs.header.chat = other;
    store
        .chats
        .insert((Shelf(SHELF.to_owned()), other), (theirs, Vec::new()));

    // A chat loaded, but not written yet, is listed, after the older one.
    script.at(1000);
    script.open_saved("claude");
    store.take(&script.list_saved(SHELF).1);
    script.serve(&mut store, false);
    let list = |script: &Script| {
        serde_json::to_value(script.core.saved(&Shelf(SHELF.to_owned()))).expect("JSON")
    };
    assert_has(
        &list(&script),
        &json!([
            { "header": { "chat": other }, "state": { "type": "Saved" } },
            { "header": { "chat": CHAT, "turns": 0 }, "state": { "type": "Loaded" } }
        ]),
    );
    assert_eq!(script.core.saved(&Shelf("never".to_owned())), None);
    // Its header is as it was last written.
    script.send(CHAT, "First things first").0.expect("sent");
    assert_has(&list(&script), &json!([{}, { "header": { "turns": 0 } }]));
    script.at(2000);
    store.take(&script.woken());
    script.serve(&mut store, false);
    assert_has(
        &list(&script),
        &json!([{}, { "header": { "turns": 1, "first": "First things first" } }]),
    );

    // Restoring the other: being restored, then loaded, or failed.
    let out = script
        .restore(RestoreChat::new(other, Shelf(SHELF.to_owned())))
        .1;
    assert_has(
        &list(&script),
        &json!([{ "state": { "type": "Restoring" } }, {}]),
    );
    store.locked.insert(other);
    store.take(&out);
    script.serve(&mut store, false);
    assert_has(
        &list(&script),
        &json!([{ "state": { "type": "Failed", "value": { "type": "InUse" } } }, {}]),
    );
    store.locked.remove(&other);
    store.take(
        &script
            .restore(RestoreChat::new(other, Shelf(SHELF.to_owned())))
            .1,
    );
    script.serve(&mut store, false);
    assert_has(
        &list(&script),
        &json!([{ "state": { "type": "Loaded" } }, {}]),
    );
    // Listed again, loaded chats stay loaded.
    store.take(&script.list_saved(SHELF).1);
    script.serve(&mut store, false);
    assert_has(
        &list(&script),
        &json!([{ "state": { "type": "Loaded" } }, { "state": { "type": "Loaded" } }]),
    );
    // A new one is there at once; one put away, saved; one deleted, gone.
    script.at(5000);
    let (new, _) = script.open(saved_open("claude"));
    let new = new.expect("opened");
    assert_has(
        &list(&script),
        &json!([{}, {}, { "header": { "chat": new }, "state": { "type": "Loaded" } }]),
    );
    store.take(&script.put_away(other).1);
    store.take(&script.delete(new, SHELF).1);
    script.serve(&mut store, false);
    assert_has(
        &list(&script),
        &json!([{ "header": { "chat": other }, "state": { "type": "Saved" } }, { "header": { "chat": CHAT } }]),
    );
}

#[test]
fn shutting_down_writes_every_saved_chat_whole_and_lets_it_go() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    script.at(1000);
    store.take(&script.woken());
    script.serve(&mut store, false);
    script.send(CHAT, "Run the tests").0.expect("sent");
    let out = script.shutdown();
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Snapshot", "value": { "saved": { "chat": {
            "state": { "type": "Lost", "value": { "reason": { "type": "ShutDown" } } },
            "turns": [{ "life": { "type": "CutOff" } }]
        } } } }),
    );
    store.take(&out);
    script.agent(PROC, AgentEvent::Exited(Exit::Code(0)));
    script.agent(PROC, AgentEvent::OutputClosed);
    script.agent(PROC, AgentEvent::Group { empty: true });
    assert!(!script.core.is_idle(), "until it's written");
    script.serve(&mut store, false);
    assert!(store.locked.is_empty(), "let go");
    assert!(script.core.is_idle());
    // It's still loaded: changed again, it's written whole, taking it
    // again.
    script.at(2000);
    script.send(CHAT, "Again").0.expect("sent");
    script.at(3000);
    let out = script.woken();
    let snapshot = one_storage(&out);
    assert_eq!(snapshot["type"], "Snapshot");
}

#[test]
fn a_chats_data_is_saved_and_its_label_shown() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(10_000));
    script.set_data(CHAT).0.expect("set");
    script.at(2000);
    assert_eq!(
        one_storage(&script.woken())["value"]["entries"],
        json!([{ "type": "Data", "value": null }])
    );
}

/// An app's data: a title.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Title(Option<String>);

impl crate::chat::AppData for Title {
    fn label(&self) -> Option<String> {
        self.0.clone()
    }
}

#[test]
fn an_apps_data_is_its_own_and_its_label_is_shown() {
    let mut core = crate::core::Core::<crate::NoAppItems, Title>::starting_ids_at(
        host(),
        super::harness::agents(),
        super::harness::FIRST,
    );
    let now = |ms: i64| crate::time::Now {
        wall: Wall(EPOCH + ms),
        mono: crate::time::Mono(u64::try_from(ms).unwrap_or(0) * 1_000_000),
    };
    let mut out = Vec::new();
    let chat = core
        .open_chat(now(0), saved_open("claude"), &mut out)
        .expect("opened");
    let unsaved = core
        .open_chat(now(0), OpenChat::new("claude", "/work"), &mut out)
        .expect("opened");
    assert_eq!(core.data(chat), Some(&Title(None)));
    let title = Title(Some("Fix the tests".to_owned()));
    core.set_data(now(0), chat, title.clone(), &mut out)
        .expect("set");
    assert_eq!(core.data(chat), Some(&title));
    let summaries = serde_json::to_value(core.summaries()).expect("JSON");
    assert_has(
        &summaries,
        &json!([
            { "id": chat, "label": "Fix the tests", "shelf": SHELF },
            { "id": unsaved, "label": null, "shelf": null }
        ]),
    );
    let mut out = Vec::new();
    core.handle(now(1000), crate::Event::Woken, &mut out);
    let snapshot = out.iter().find_map(|directive| match directive {
        crate::Directive::Storage(StorageDirective::Snapshot { saved, .. }) => Some(saved),
        _ => None,
    });
    let snapshot = snapshot.expect("a snapshot");
    assert_eq!(snapshot.data, title);
    assert_eq!(snapshot.header.label.as_deref(), Some("Fix the tests"));
}

#[test]
fn chat_ids_are_uuids_of_version_7_from_the_seed_and_the_time() {
    let ids = |seed: u64| {
        let mut host = host();
        host.seed = seed;
        let mut core: crate::core::Core = crate::core::Core::new(host, super::harness::agents());
        core.numbered = false;
        let now = crate::time::Now {
            wall: Wall(EPOCH),
            mono: crate::time::Mono(0),
        };
        (0..3)
            .map(|_| {
                let open = OpenChat::new("claude", "/work");
                core.open_chat(now, open, &mut Vec::new()).expect("opened")
            })
            .collect::<Vec<_>>()
    };
    let first = ids(7);
    assert_eq!(first, ids(7), "the same seed gives the same ids");
    assert_ne!(first, ids(8));
    let unique: BTreeSet<_> = first.iter().collect();
    assert_eq!(unique.len(), 3);
    for id in &first {
        assert_eq!(id.0.get_version_num(), 7);
        let (secs, nanos) = id.0.get_timestamp().expect("a time").to_unix();
        assert_eq!(
            i64::try_from(secs).unwrap_or(0) * 1000 + i64::from(nanos / 1_000_000),
            EPOCH
        );
    }
}

/// Dice for scripts made at random: xorshift, from a seed, so each script
/// is the same every run.
struct Dice(u64);

impl Dice {
    /// A number below `sides`.
    fn roll(&mut self, sides: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % sides
    }
}

/// The id of the prompt written in `out`, if one is.
fn prompt_in(out: &[Value]) -> Option<u64> {
    (super::harness::writes(out).into_iter())
        .find(|(_, message)| message["method"] == "session/prompt")
        .and_then(|(_, message)| message["id"].as_u64())
}

/// Drives a saved chat through a script made at random from `seed`,
/// with its store answering late and sometimes failing; then checks the
/// store's copy, its snapshot with its journal applied, is the chat, and
/// that restoring it gives the chat tidied.
fn random_script(seed: u64) -> [usize; 3] {
    let mut dice = Dice(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut host = host();
    host.limits.kept_changes = 3;
    let mut script = Script::new(host);
    script.without_fixtures();
    let mut store = Store::default();
    script.open_saved("claude");
    let words = ["Run", "ning ", "the ", "tests", ".\n", "All ", "pass."];
    let (mut ms, mut prompt, mut tools, mut asked) = (0, None, 0, 0);
    for _ in 0..150 {
        ms += dice.roll(700);
        script.at(ms);
        let chunk = |kind: &str, dice: &mut Dice| {
            let text = words[usize::try_from(dice.roll(7)).unwrap_or(0)];
            json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
        };
        let out = match dice.roll(12) {
            0 | 1 => {
                let text = format!("message {}\nmore", dice.roll(100));
                script.send(CHAT, &text).1
            }
            2..=4 => script.update(PROC, SESSION, &chunk("agent_message_chunk", &mut dice)),
            5 => script.update(PROC, SESSION, &chunk("agent_thought_chunk", &mut dice)),
            6 => {
                tools += 1;
                let call = json!({ "sessionUpdate": "tool_call", "toolCallId": format!("t{tools}"), "title": "Run", "kind": "execute", "status": "pending" });
                script.update(PROC, SESSION, &call)
            }
            7 if tools > 0 => {
                let tool = 1 + dice.roll(tools);
                let update = json!({ "sessionUpdate": "tool_call_update", "toolCallId": format!("t{tool}"), "status": "completed", "rawOutput": "ok" });
                script.update(PROC, SESSION, &update)
            }
            8 if prompt.is_some() && asked < 3 => {
                asked += 1;
                let out = script.says(PROC, &super::stop::permission(900 + asked, "t-ask"));
                let decide = with(&out, "decide");
                let ticket = decide.first().and_then(|each| each["decide"].as_u64());
                let mut all = out.clone();
                if let Some(ticket) = ticket {
                    all.extend(script.policy(ticket, crate::event::PolicyAnswer::AskUser));
                }
                all
            }
            9 if prompt.is_some() => {
                let id = prompt.take().unwrap_or_default();
                let mut out = Vec::new();
                let stop = if dice.roll(3) == 0 {
                    let (stopped, stopping) = script.stop(CHAT);
                    stopped.expect("there's a turn to stop");
                    out.extend(stopping);
                    "cancelled"
                } else {
                    "end_turn"
                };
                out.extend(script.answers(PROC, id, &json!({ "stopReason": stop })));
                out
            }
            10 => script.set_data(CHAT).1,
            _ => script.woken(),
        };
        if let Some(id) = prompt_in(&out) {
            prompt = Some(id);
        }
        store.take(&out);
        // The store answers some of its work, late, and fails some.
        while dice.roll(3) == 0 {
            let Some((ticket, event)) = store.next(dice.roll(8) == 0) else {
                break;
            };
            let out = script.storage(ticket, event);
            if let Some(id) = prompt_in(&out) {
                prompt = Some(id);
            }
            store.take(&out);
        }
    }
    // Everything written, retries and all.
    for _ in 0..4 {
        ms += 61_000;
        script.at(ms);
        let out = script.woken();
        if let Some(id) = prompt_in(&out) {
            prompt = Some(id);
        }
        store.take(&out);
        script.serve(&mut store, false);
    }
    let shelf = Shelf(SHELF.to_owned());
    let stored = store.read(&shelf, CHAT).expect("it's stored");
    let live = script.core.chat(CHAT).cloned().expect("the chat");
    assert_eq!(
        stored.chat, live,
        "seed {seed}: the store's copy is the chat"
    );
    assert_eq!(
        serde_json::to_value(&stored.session).expect("JSON"),
        script.state()["runs"][CHAT.to_string()]["session"],
        "seed {seed}"
    );
    assert_eq!(stored.header.turns, live.turns.len() as u64);

    // Restored, it's the chat, tidied as of when it was last changed.
    let mut tidied = live;
    for change in crate::core::saving::tidy(&tidied.clone(), stored.header.changed) {
        tidied.apply(&change).expect("tidying fits");
    }
    let mut again = Script::new(super::harness::host());
    again.without_fixtures();
    again
        .restore(RestoreChat::new(CHAT, shelf))
        .0
        .expect("restoring");
    again.storage(1000, StorageEvent::Loaded(Box::new(stored)));
    assert_eq!(again.core.chat(CHAT), Some(&tidied), "seed {seed}");

    // The agent's questions are answered as its turn ends.
    if let Some(id) = prompt {
        script.answers(PROC, id, &json!({ "stopReason": "end_turn" }));
    }
    store.counts
}

#[test]
fn a_saved_chat_reads_back_as_it_is_and_restores_tidied() {
    let mut counts = [0; 3];
    for seed in 1..=60 {
        let each = random_script(seed);
        for (all, each) in counts.iter_mut().zip(each) {
            *all += each;
        }
    }
    // Snapshots and journals both, and failures to try again.
    assert!(counts.iter().all(|&count| count > 60), "{counts:?}");
}
