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
use crate::settings::SettingChoice;
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
    /// The chats locked, on their shelves.
    locked: BTreeSet<(Shelf, ChatId)>,
    /// What was last done to each chat it has, on its shelf: what its
    /// being let go is checked against.
    last: BTreeMap<(Shelf, ChatId), Last>,
    /// The chats it has that a restore finds damaged: the only ones let go
    /// just as they're loaded.
    damaged: BTreeSet<ChatId>,
    /// Work given and not done yet, oldest first.
    work: VecDeque<StorageDirective>,
    /// How many snapshots, journal entries, failures, loads and releases
    /// it has seen.
    counts: [usize; 5],
}

impl Store {
    /// Takes on the storage work in `out`, checking the core keeps its
    /// side of the contract: no work for a shelf being moved, and no move
    /// while work for either shelf is under way.
    fn take(&mut self, out: &[Value]) {
        for directive in storage(out) {
            let work: StorageDirective =
                serde_json::from_value(directive).expect("storage work reads back");
            let shelves = |work: &StorageDirective| match work {
                StorageDirective::List { shelf, .. }
                | StorageDirective::Load { shelf, .. }
                | StorageDirective::Snapshot { shelf, .. }
                | StorageDirective::Append { shelf, .. }
                | StorageDirective::Delete { shelf, .. }
                | StorageDirective::Release { shelf, .. } => vec![shelf.clone()],
                StorageDirective::MoveShelf { from, to, .. } => vec![from.clone(), to.clone()],
            };
            let moving =
                |work: &StorageDirective| matches!(work, StorageDirective::MoveShelf { .. });
            let mine = shelves(&work);
            let meets =
                |other: &StorageDirective| shelves(other).iter().any(|shelf| mine.contains(shelf));
            for other in self.work.iter().filter(|other| meets(other)) {
                // A move goes once all else for its shelves is answered;
                // a chat let go before it is let go as it's given.
                let ok = if moving(&work) {
                    matches!(other, StorageDirective::Release { .. })
                } else {
                    !moving(other)
                };
                assert!(ok, "{work:?} given while {other:?} is under way");
            }
            self.work.push_back(work);
        }
    }

    /// Does the oldest work that's answered, and returns its ticket and
    /// answer: or fails it, doing nothing, if `fail`.
    fn next(&mut self, fail: bool) -> Option<(u64, StorageEvent)> {
        self.next_any(0, fail)
    }

    /// Does work that's answered, as [`Store::next`], but not always the
    /// oldest: the `pick`th, round, of the work the contract lets it do
    /// now, which is any whose chat has nothing earlier waiting, with
    /// nothing given before it still to let go.
    fn next_any(&mut self, pick: u64, fail: bool) -> Option<(u64, StorageEvent)> {
        let chat_of = |work: &StorageDirective| match work {
            StorageDirective::Load { chat, .. }
            | StorageDirective::Delete { chat, .. }
            | StorageDirective::Release { chat, .. } => Some(*chat),
            StorageDirective::Snapshot { saved, .. } => Some(saved.chat.id),
            StorageDirective::Append { header, .. } => Some(header.chat),
            StorageDirective::List { .. } | StorageDirective::MoveShelf { .. } => None,
        };
        loop {
            let ready: Vec<usize> = (0..self.work.len())
                .filter(|&at| {
                    let chat = chat_of(&self.work[at]);
                    (self.work.iter().take(at)).all(|earlier| {
                        !matches!(earlier, StorageDirective::Release { .. })
                            && (chat.is_none() || chat_of(earlier) != chat)
                    })
                })
                .collect();
            let at = *ready.get(usize::try_from(pick).unwrap_or(0) % ready.len().max(1))?;
            let work = self.work.remove(at)?;
            let ticket = match &work {
                StorageDirective::Release { .. } => None,
                StorageDirective::List { ticket, .. }
                | StorageDirective::Load { ticket, .. }
                | StorageDirective::Snapshot { ticket, .. }
                | StorageDirective::Append { ticket, .. }
                | StorageDirective::Delete { ticket, .. }
                | StorageDirective::MoveShelf { ticket, .. } => Some(ticket.0),
            };
            if let (true, Some(ticket)) = (fail, ticket) {
                if let StorageDirective::Delete { shelf, chat, .. } = &work {
                    self.last.insert((shelf.clone(), *chat), Last::NotDeleted);
                }
                self.counts[2] += 1;
                return Some((ticket, StorageEvent::Failed("disk full".to_owned())));
            }
            let (ticket, event) = match work {
                StorageDirective::Snapshot {
                    ticket,
                    shelf,
                    saved,
                } => {
                    self.counts[0] += 1;
                    // A chat is on one shelf.
                    let id = saved.chat.id;
                    assert!(
                        (self.chats.keys()).all(|(on, chat)| *chat != id || *on == shelf),
                        "chat {id} written to {shelf:?} is on another shelf"
                    );
                    let written = serde_json::to_vec(&saved).expect("JSON").len();
                    self.locked.insert((shelf.clone(), saved.chat.id));
                    self.last
                        .insert((shelf.clone(), saved.chat.id), Last::Whole);
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
                    self.counts[1] += entries.len();
                    let written = serde_json::to_vec(&entries).expect("JSON").len();
                    assert!(
                        self.locked.contains(&(shelf.clone(), header.chat)),
                        "added to while locked, on its shelf"
                    );
                    self.last
                        .insert((shelf.clone(), header.chat), Last::Journal);
                    let (saved, journal) = (self.chats.get_mut(&(shelf, header.chat)))
                        .expect("a journal is added to after its snapshot");
                    saved.header = header;
                    journal.extend(entries);
                    (ticket, done(written as u64))
                }
                StorageDirective::Release { shelf, chat } => {
                    self.counts[4] += 1;
                    // Let go once written whole, or as the store has it if
                    // the core has no use for it: loaded but damaged, or
                    // not deleted.
                    let last = self.last.get(&(shelf.clone(), chat)).copied();
                    let damaged = last == Some(Last::Loaded) && self.damaged.contains(&chat);
                    assert!(
                        matches!(last, Some(Last::Whole | Last::NotDeleted)) || damaged,
                        "chat {chat} let go after {last:?}"
                    );
                    assert!(
                        self.locked.remove(&(shelf.clone(), chat)),
                        "chat {chat} let go only if locked on {shelf:?}"
                    );
                    continue;
                }
                StorageDirective::Load {
                    ticket,
                    shelf,
                    chat,
                } => {
                    self.counts[3] += 1;
                    let locked = self.locked.iter().any(|(_, each)| *each == chat);
                    let event = match self.read(&shelf, chat) {
                        Some(saved) if !locked => {
                            self.locked.insert((shelf.clone(), chat));
                            self.last.insert((shelf, chat), Last::Loaded);
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
                    // From a shelf it isn't on, nothing goes.
                    self.locked.remove(&(shelf.clone(), chat));
                    self.last.remove(&(shelf.clone(), chat));
                    self.chats.remove(&(shelf, chat));
                    (ticket, done(0))
                }
                StorageDirective::MoveShelf { ticket, from, to } => {
                    let moving: Vec<_> = (self.chats.keys())
                        .filter(|(on, _)| *on == from)
                        .cloned()
                        .collect();
                    for key in moving {
                        let stored = self.chats.remove(&key).expect("there");
                        if self.locked.remove(&key) {
                            self.locked.insert((to.clone(), key.1));
                        }
                        if let Some(last) = self.last.remove(&key) {
                            self.last.insert((to.clone(), key.1), last);
                        }
                        self.chats.insert((to.clone(), key.1), stored);
                    }
                    (ticket, done(0))
                }
            };
            return Some((ticket.0, event));
        }
    }

    /// The shelf a chat is on, if it's stored.
    fn shelf_of(&self, chat: ChatId) -> Option<Shelf> {
        (self.chats.keys())
            .find(|(_, each)| *each == chat)
            .map(|(shelf, _)| shelf.clone())
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

/// What a store last did to a chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Last {
    /// Wrote it whole.
    Whole,
    /// Added to its journal.
    Journal,
    /// Loaded it.
    Loaded,
    /// Failed to delete it.
    NotDeleted,
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
        self.open_saved_as(saved_open(agent), agent);
    }

    /// Opens a chat as [`Script::open_saved`] does, CAF's tools served,
    /// given all of them.
    fn open_saved_serving(&mut self, agent: &str) {
        self.serving();
        let open = saved_open(agent).with_tools(crate::tool::Tools::caf(crate::tool::CafTool::ALL));
        self.open_saved_as(open, agent);
    }

    /// Opens `open`, a chat with `agent`, as [`Script::open_saved`] does.
    fn open_saved_as(&mut self, open: OpenChat, agent: &str) {
        let (chat, out) = self.open(open);
        assert_eq!(chat, Ok(CHAT));
        let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
        let answer = if agent == "claude" {
            claude_init()
        } else {
            plain_init(true)
        };
        let out = self.answers(PROC, init, &answer);
        let new = sent(&out, "session/new")["id"].as_u64().expect("an id");
        self.answers(
            PROC,
            new,
            &json!({ "modes": super::default_modes(), "sessionId": SESSION }),
        );
    }

    /// Gives the store's answers back, in any order it may give them, until
    /// it has nothing more to do, failing one in `odds`, and returns what
    /// they gave out.
    fn serve_any(&mut self, store: &mut Store, dice: &mut Dice, odds: u64) -> Vec<Value> {
        let mut all = Vec::new();
        while let Some((ticket, event)) = store.next_any(dice.roll(1000), dice.roll(odds) == 0) {
            let out = self.storage(ticket, event);
            store.take(&out);
            all.extend(out);
        }
        all
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
            archived: None,
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
    let tool = |status: &str| json!({ "type": "Tool", "value": { "title": "Run `cargo test`", "kind": { "type": "Execute" }, "status": { "type": status }, "input": null, "origin": { "type": "NotRecorded" }, "action": null, "input_cut": null, "result": [], "locations": [], "locations_cut": 0, "decision": { "type": "NotRecorded" }, "app": null } });
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
    script.answers(
        PROC,
        1001,
        &json!({ "modes": super::default_modes(), "sessionId": SESSION }),
    );

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
fn a_restore_finds_a_chat_archived_if_its_state_or_when_says_so() {
    let at = Wall(EPOCH);
    let now = Wall(EPOCH + 5000);
    let tidy = |state: Value, archived: Value| {
        let mut chat = mid_turn("claude");
        chat["turns"] = json!([]);
        chat["questions"] = json!([]);
        chat["state"] = state;
        chat["archived"] = archived;
        let chat: Chat = serde_json::from_value(chat).expect("a chat");
        serde_json::to_value(crate::core::saving::tidy(&chat, at, now)).expect("JSON")
    };
    let closed = json!({ "type": "Closed" });
    let idle = json!({ "type": "Idle" });
    // Archived as it should be: left so.
    assert_eq!(tidy(closed.clone(), json!(EPOCH - 1)), json!([]));
    // Closed before CAF kept when: archived as of now.
    assert_eq!(
        tidy(closed.clone(), Value::Null),
        json!([{ "type": "Archived", "value": EPOCH + 5000 }])
    );
    // Archived, but saved in another state: closed, as it's archived.
    assert_eq!(
        tidy(idle.clone(), json!(EPOCH - 1)),
        json!([{ "type": "State", "value": closed }])
    );
    // Neither: idle.
    assert_eq!(tidy(idle, Value::Null), json!([]));
    assert_eq!(
        tidy(json!({ "type": "Opening" }), Value::Null),
        json!([{ "type": "State", "value": { "type": "Idle" } }])
    );
}

#[test]
fn a_restore_finds_what_was_under_way_and_tidies_it() {
    let chat: Chat = serde_json::from_value(mid_turn("claude")).expect("a chat");
    let at = Wall(EPOCH);
    let changes = crate::core::saving::tidy(&chat, at, at);
    let end = json!({ "type": "At", "value": EPOCH });
    assert_eq!(
        serde_json::to_value(&changes).expect("JSON"),
        json!([
            { "type": "ItemEnded", "value": { "item": 5003, "end": end } },
            { "type": "ToolChanged", "value": { "item": 5005, "tool": { "title": "Run `cargo test`", "kind": { "type": "Execute" }, "status": { "type": "Failed" }, "input": null, "origin": { "type": "NotRecorded" }, "action": null, "input_cut": null, "result": [], "locations": [], "locations_cut": 0, "decision": { "type": "NotRecorded" }, "app": null, "stopped_by": null, "job": null } } },
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
    assert!(crate::core::saving::tidy(&tidied, at, at).is_empty());
    // A turn never ends before it started, whatever the clock said.
    let early = crate::core::saving::tidy(&chat, Wall(0), Wall(0));
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
    assert!(with(&out, "report").contains(&json!({
        "report": "Debug",
        "message": format!("chat {CHAT}: what the agent replays of its session is thrown away, as CAF has its record")
    })));
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
    script.answers(
        proc,
        new,
        &json!({ "modes": super::default_modes(), "sessionId": "fresh" }),
    );
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
    script.answers(
        PROC,
        new,
        &json!({ "modes": super::default_modes(), "sessionId": "fresh" }),
    );
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
fn an_archived_chat_whose_agent_is_gone_stays_archived_when_sent_to_and_unarchives_to_no_connection()
 {
    let mut script = Script::new(host());
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let mut chat = mid_turn("gone");
    chat["state"] = json!({ "type": "Closed" });
    chat["archived"] = json!(EPOCH - 10_000);
    let mut saved = saved(&chat, Some("old"), 0);
    saved.header.profile = Some("long-gone-acp".to_owned());
    script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    let archived = json!({ "state": { "type": "Closed" }, "archived": EPOCH - 10_000 });
    assert_has(&script.chat(CHAT), &archived);
    assert_eq!(script.send(CHAT, "hello?").0, Err(Refused::NoSuchAgent));
    assert_has(&script.chat(CHAT), &archived);

    // Unarchived, it's idle, but on no connection: there's no agent to
    // start.
    let (done_, out) = script.unarchive(CHAT);
    done_.expect("unarchived");
    assert!(with(&out, "start").is_empty());
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Idle" }, "archived": null }),
    );
    assert_eq!(script.state()["conns"], json!({}));
    assert_eq!(script.send(CHAT, "hello?").0, Err(Refused::NoSuchAgent));
}

#[test]
fn a_restored_chat_is_given_its_instructions_again_as_its_session_opens() {
    let mut script = Script::new(host());
    let mut restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    restore.instructions = Some("Given again.".to_owned());
    script.restore(restore).0.expect("restoring");
    let saved = saved(&mid_turn("claude"), Some("old"), 0);
    script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    let out = script.send(CHAT, "Go on").1;
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &claude_init());
    let options = &sent(&out, "session/resume")["params"]["_meta"]["claudeCode"]["options"];
    assert_eq!(
        options["systemPrompt"],
        crate::profiles::own_instructions("caf-test", &[], &[]) + "\n\nGiven again."
    );
    assert_eq!(options["tools"], json!([]));
}

#[test]
fn a_restored_chat_is_given_its_tools_again_as_a_new_one_is() {
    use crate::tool::{CafTool, Tools};
    let mut script = Script::new(host());
    let restore =
        RestoreChat::new(CHAT, Shelf(SHELF.to_owned())).with_tools(Tools::caf([CafTool::Write]));
    // Nothing serves them yet.
    assert_eq!(
        script.restore(restore.clone()).0,
        Err(Refused::ToolsNotServed)
    );
    script.serving();
    script.restore(restore).0.expect("restoring");
    let saved = saved(&mid_turn("claude"), Some("old"), 0);
    let out = script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    assert_eq!(
        with(&out, "tool"),
        [json!({ "tool": { "type": "Serve", "value": {
            "chat": CHAT,
            "tools": [{ "type": "Write" }],
            "app": [],
            "cwd": "/work",
            "instructions": "caf-test's tools to write whole files (Write), in the folder you're working in.",
        } } })]
    );
    let out = script.send(CHAT, "Go on").1;
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &claude_init());
    // The same server a new chat gets: the same relay, chat and token.
    let resume = sent(&out, "session/resume");
    assert_eq!(
        resume["params"]["mcpServers"],
        json!([super::serving::caf_server(CHAT)])
    );
    assert_eq!(
        resume["params"]["_meta"]["claudeCode"]["options"]["systemPrompt"],
        crate::profiles::own_instructions("caf-test", &[CafTool::Write], &[])
    );
    // Put away, it's no longer served.
    let id = resume["id"].as_u64().expect("an id");
    script.answers(proc, id, &json!({ "modes": super::default_modes() }));
    let out = script.put_away(CHAT).1;
    assert!(
        with(&out, "tool")
            .contains(&json!({ "tool": { "type": "Unserve", "value": { "chat": CHAT } } })),
        "{out:#?}"
    );
}

#[test]
fn a_restored_chat_has_the_apps_servers_then_cafs_and_none_named_caf() {
    use crate::agent::McpServer;
    use crate::tool::{CafTool, Tools};
    let server = |name: &str| McpServer::Http {
        name: name.to_owned(),
        url: "http://localhost:1/mcp".to_owned(),
        headers: Vec::new(),
    };
    let mut script = Script::new(host());
    script.serving();
    let mut restore =
        RestoreChat::new(CHAT, Shelf(SHELF.to_owned())).with_tools(Tools::caf([CafTool::Read]));
    restore.mcp_servers = vec![server("CAF")];
    assert_eq!(
        script.restore(restore.clone()).0,
        Err(Refused::ReservedServer)
    );
    restore.mcp_servers = vec![server("maps")];
    script.restore(restore.clone()).0.expect("restoring");
    let saved = saved(&mid_turn("claude"), Some("old"), 0);
    script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    // Loaded already, whatever it's given.
    assert_eq!(script.restore(restore).0, Err(Refused::Loaded));
    let out = script.send(CHAT, "Go on").1;
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &claude_init());
    let servers = &sent(&out, "session/resume")["params"]["mcpServers"];
    assert_eq!(servers[0]["name"], "maps");
    assert_eq!(servers[1], super::serving::caf_server(CHAT));
    assert_eq!(servers.as_array().map(Vec::len), Some(2));
}

#[test]
fn restoring_a_loaded_chat_is_refused_as_loaded_whatever_its_tools() {
    use crate::tool::{CafTool, Tools};
    let mut script = Script::new(host());
    let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    script.restore(restore.clone()).0.expect("restoring");
    // Nothing serves tools, and it's loaded: it's the second that's said.
    assert_eq!(
        script
            .restore(restore.with_tools(Tools::caf([CafTool::Read])))
            .0,
        Err(Refused::Loaded)
    );
}

#[test]
fn a_chat_whose_agent_caf_cant_strip_comes_back_and_sending_to_it_is_refused() {
    let mut agents = super::harness::agents();
    agents.push(crate::agent::AgentSpec::command(
        "unknown",
        crate::agent::SpawnCommand::new("/opt/unknown-acp"),
        crate::profiles::ProfileChoice::Cautious,
    ));
    let mut script = Script::offering(host(), agents);
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let mut saved = saved(&mid_turn("unknown"), Some("old"), 0);
    saved.header.profile = None;
    script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    assert_eq!(script.chat(CHAT)["agent"], "unknown");
    let (turn, out) = script.send(CHAT, "hello?");
    assert_eq!(turn, Err(Refused::OwnTools));
    assert!(with(&out, "start").is_empty(), "nothing is started");
    assert_eq!(
        script.chat(CHAT)["state"],
        json!({ "type": "Idle" }),
        "it's as it was restored"
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
    // Written whole at once, so the store has the new name.
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Snapshot", "value": { "saved": {
            "header": { "agent": "claude-renamed" }, "chat": { "agent": "claude-renamed" }
        } } }),
    );
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
fn a_restore_is_under_way_until_its_chat_loads_or_fails_or_is_deleted() {
    let mut script = Script::new(host());
    let shelf = Shelf(SHELF.to_owned());
    let restore = |chat| RestoreChat::new(chat, shelf.clone());
    let load = ticket(&one_storage(&script.restore(restore(CHAT)).1));
    assert!(script.core.restoring(CHAT));
    let stored = saved(&mid_turn("claude"), Some("old"), 0);
    script.storage(load, StorageEvent::Loaded(Box::new(stored)));
    assert!(!script.core.restoring(CHAT) && script.core.chat(CHAT).is_some());
    let other = ChatId::from_number(4242);
    let load = ticket(&one_storage(&script.restore(restore(other)).1));
    assert!(script.core.restoring(other));
    let missing = crate::save::CannotRestore::NotFound;
    script.storage(load, StorageEvent::NotLoaded(missing));
    assert!(!script.core.restoring(other) && script.core.chat(other).is_none());
    // Deleted while its load is under way: over at once, whatever the
    // load then finds.
    script.restore(restore(other)).0.expect("restoring");
    script.delete(other, SHELF).0.expect("deleting");
    assert!(!script.core.restoring(other));
}

#[test]
fn a_chat_put_away_is_there_as_it_was_left_until_its_written() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    let chat = CHAT;
    assert!(script.core.away(chat).is_none(), "loaded, it's not away");
    let out = script.close(chat).1;
    assert!(script.core.chat(chat).is_none());
    let left = script.core.away(chat).expect("as it was left");
    assert_eq!(left.state, crate::chat::ChatState::Closed);
    store.take(&out);
    script.serve(&mut store, false);
    assert!(script.core.away(chat).is_none(), "written, it's gone");
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
fn closing_a_saved_chat_archives_it_and_puts_it_away_and_restoring_it_brings_it_back_archived() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    script.send(CHAT, "Hi").0.expect("sent");
    script.at(500);
    let out = script.close(CHAT).1;
    let archived = json!(EPOCH + 500);
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Snapshot", "value": { "saved": {
            "header": { "closed": true, "archived": archived },
            "chat": { "state": { "type": "Closed" }, "archived": archived, "turns": [{ "life": { "type": "Stopped" } }] }
        } } }),
    );
    assert!(script.core.chat(CHAT).is_none(), "put away");
    store.take(&out);
    script.serve(&mut store, false);
    end_process(&mut script, PROC);

    // Restored, it's archived as it was left, and on no connection.
    script.at(2000);
    let out = script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .1;
    store.take(&out);
    let out = script.serve(&mut store, false);
    assert!(storage(&out).is_empty(), "nothing to tidy");
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Closed" }, "archived": archived }),
    );
    let conns = script.state()["conns"].clone();
    assert!(
        (conns
            .as_object()
            .into_iter()
            .flat_map(|conns| conns.values()))
        .all(|conn| conn["chats"] == json!([])),
        "{conns:#}"
    );
    assert_eq!(
        script.set_setting(CHAT, SettingChoice::model("sonnet")).0,
        Err(Refused::Closed)
    );

    // Sending to it unarchives it, and the message goes as to any other:
    // its agent started, its session continued, the turn and the
    // connection it's put on numbered apart.
    script.at(3000);
    let (turn, out) = script.send(CHAT, "Again");
    let turn = turn.expect("sent");
    let conns = script.state()["conns"].clone();
    assert!(
        conns.get(turn.0.to_string()).is_none(),
        "turn {} isn't a connection's number too: {conns:#}",
        turn.0
    );
    assert_eq!(with(&out, "start").len(), 1);
    let proc = with(&out, "start")[0]["start"].as_u64().expect("a process");
    let init = sent(&out, "initialize")["id"].as_u64().expect("an id");
    let out = script.answers(proc, init, &claude_init());
    assert_eq!(sent(&out, "session/resume")["params"]["sessionId"], SESSION);
    let whole = script.chat(CHAT);
    assert_eq!(whole["archived"], Value::Null);
    assert_ne!(whole["state"], json!({ "type": "Closed" }));
    // Saved so.
    script.at(4000);
    let out = script.woken();
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Append", "value": { "header": { "closed": false, "archived": null } } }),
    );
    store.take(&out);
    script.serve(&mut store, false);
    let saved = store
        .read(&Shelf(SHELF.to_owned()), CHAT)
        .expect("it's saved");
    assert_eq!(saved.chat.archived, None);
}

#[test]
fn unarchiving_a_restored_chat_makes_it_idle_and_saves_it_and_its_agent_starts_when_sent_to() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    script.at(500);
    store.take(&script.close(CHAT).1);
    script.serve(&mut store, false);
    end_process(&mut script, PROC);
    script.at(2000);
    store.take(
        &script
            .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
            .1,
    );
    script.serve(&mut store, false);

    script.at(3000);
    let (done_, out) = script.unarchive(CHAT);
    done_.expect("unarchived");
    assert!(with(&out, "start").is_empty(), "no agent started");
    assert_has(
        &script.chat(CHAT),
        &json!({ "state": { "type": "Idle" }, "archived": null }),
    );
    assert_eq!(
        script
            .change_kinds(CHAT)
            .iter()
            .rev()
            .take(2)
            .collect::<Vec<_>>(),
        ["Archived", "State"],
        "the state changes, then whether it's archived"
    );
    // A chat that isn't archived is left as it is.
    let (done_, out) = script.unarchive(CHAT);
    done_.expect("left alone");
    assert!(out.is_empty(), "{out:#?}");
    assert_eq!(
        script.unarchive(ChatId::from_number(4242)).0,
        Err(Refused::NoSuchChat)
    );
    script.at(4000);
    let out = script.woken();
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Append", "value": { "entries": [
            { "type": "Change", "value": { "type": "State", "value": { "type": "Idle" } } },
            { "type": "Change", "value": { "type": "Archived", "value": null } }
        ] } }),
    );
    store.take(&out);
    script.serve(&mut store, false);

    // Its choices can be made again, and sending starts its agent.
    script
        .set_setting(CHAT, SettingChoice::model("sonnet"))
        .0
        .expect("chosen");
    let (turn, out) = script.send(CHAT, "Again");
    turn.expect("sent");
    assert_eq!(with(&out, "start").len(), 1);
}

#[test]
fn a_chat_saved_closed_before_caf_kept_when_comes_back_archived_as_of_its_restore() {
    let mut script = Script::new(host());
    let (done_, out) = script.restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())));
    done_.expect("restoring");
    let load = ticket(&one_storage(&out));
    script.at(100);
    let mut chat = mid_turn("claude");
    chat["state"] = json!({ "type": "Closed" });
    let mut saved = saved(&chat, Some("old"), -40_000);
    saved.header.closed = true;
    let out = script.storage(load, StorageEvent::Loaded(Box::new(saved)));
    assert!(with(&out, "start").is_empty(), "no agent started");
    assert_has(
        &script.chat(CHAT),
        &json!({
            "state": { "type": "Closed" },
            "archived": EPOCH + 100,
            "turns": [{}, { "life": { "type": "CutOff" } }, { "life": { "type": "Stopped" } }]
        }),
    );
    // Tidying it is saved, being archived as of now with the rest.
    let append = one_storage(&out);
    assert_eq!(append["type"], "Append");
    let entries = append["value"]["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        entries.last(),
        Some(&json!({ "type": "Change", "value": { "type": "Archived", "value": EPOCH + 100 } }))
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry["value"]["type"] == "State"),
        "it stays closed: {entries:#?}"
    );
}

#[test]
fn an_archived_chat_put_away_or_closed_again_stays_archived_as_of_when_it_was() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    script.at(500);
    store.take(&script.close(CHAT).1);
    script.serve(&mut store, false);
    let archived = json!(EPOCH + 500);
    for (at, put_away) in [(2000, true), (4000, false)] {
        script.at(at);
        store.take(
            &script
                .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
                .1,
        );
        script.serve(&mut store, false);
        script.at(at + 1000);
        let (done_, out) = if put_away {
            script.put_away(CHAT)
        } else {
            script.close(CHAT)
        };
        done_.expect("put away");
        assert!(script.core.chat(CHAT).is_none(), "let go");
        assert_has(
            &one_storage(&out),
            &json!({ "type": "Snapshot", "value": { "saved": {
                "header": { "closed": true, "archived": archived },
                "chat": { "state": { "type": "Closed" }, "archived": archived }
            } } }),
        );
        store.take(&out);
        script.serve(&mut store, false);
    }
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
fn a_chat_deleted_while_it_was_being_restored_goes_with_its_lock() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    let shelf = Shelf(SHELF.to_owned());
    let stored = saved(&mid_turn("claude"), Some("old"), 0);
    store
        .chats
        .insert((shelf.clone(), CHAT), (stored, Vec::new()));
    store.take(&script.restore(RestoreChat::new(CHAT, shelf.clone())).1);
    store.take(&script.delete(CHAT, SHELF).1);
    // The store loads it, locking it, then deletes it, lock and all: the
    // store would refuse letting go of what it no longer has.
    let out = script.serve(&mut store, false);
    assert!(storage(&out).is_empty(), "{out:#?}");
    assert!(store.chats.is_empty() && store.locked.is_empty());
    assert!(script.core.chat(CHAT).is_none());
    assert_eq!(script.state()["saving"]["deleted"], json!({}));
}

#[test]
fn a_chat_that_couldnt_be_deleted_while_it_was_being_restored_is_let_go() {
    let mut script = Script::new(host());
    let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    script.restore(restore).0.expect("restoring");
    let delete = ticket(&one_storage(&script.delete(CHAT, SHELF).1));
    let stored = saved(&mid_turn("claude"), Some("old"), 0);
    let out = script.storage(1000, StorageEvent::Loaded(Box::new(stored)));
    assert!(storage(&out).is_empty());
    let out = script.storage(delete, StorageEvent::Failed("read-only".to_owned()));
    assert_eq!(
        storage(&out),
        [json!({ "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } })]
    );
    assert!(script.core.chat(CHAT).is_none());
    // A load that fails is forgotten too.
    let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    let load = ticket(&one_storage(&script.restore(restore).1));
    script.delete(CHAT, SHELF).0.expect("deleted");
    let missing = crate::save::CannotRestore::NotFound;
    script.storage(load, StorageEvent::NotLoaded(missing));
    assert_eq!(script.state()["saving"]["deleted"], json!({}));
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
    // Until the store says it moved, it's where it was.
    let renamed = Shelf("renamed".to_owned());
    assert_eq!(
        script.core.summaries()[0].shelf.as_ref(),
        Some(&Shelf(SHELF.to_owned()))
    );
    store.take(&out);
    // Once moved, both shelves are read again.
    let out = script.serve(&mut store, false);
    assert_has(
        &json!(storage(&out)),
        &json!([
            { "type": "List", "value": { "shelf": SHELF } },
            { "type": "List", "value": { "shelf": "renamed" } }
        ]),
    );
    assert_eq!(script.core.summaries()[0].shelf.as_ref(), Some(&renamed));
    assert_eq!(
        script.core.saved(&Shelf(SHELF.to_owned())),
        Some(Vec::new())
    );
    assert_has(
        &serde_json::to_value(script.core.saved(&renamed)).expect("JSON"),
        &json!([{ "shelf": "renamed", "header": { "chat": CHAT }, "state": { "type": "Loaded" } }]),
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
    store.locked.insert((Shelf(SHELF.to_owned()), other));
    store.take(&out);
    script.serve(&mut store, false);
    assert_has(
        &list(&script),
        &json!([{ "state": { "type": "Failed", "value": { "type": "InUse" } } }, {}]),
    );
    store.locked.remove(&(Shelf(SHELF.to_owned()), other));
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
    // It's still loaded: in use again, calling shutting down off, it's
    // written whole at once, which locks it again.
    script.at(2000);
    let out = script.send(CHAT, "Again").1;
    assert_eq!(one_storage(&out)["type"], "Snapshot");
    store.take(&out);
    script.serve(&mut store, false);
    assert_eq!(store.locked.len(), 1, "locked again");
}

#[test]
fn a_chat_changed_after_its_let_go_as_caf_shuts_down_is_written_whole_and_let_go_again() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved("claude");
    let out = script.shutdown();
    store.take(&out);
    script.agent(PROC, AgentEvent::Exited(Exit::Code(0)));
    script.agent(PROC, AgentEvent::OutputClosed);
    script.agent(PROC, AgentEvent::Group { empty: true });
    let out = script.serve(&mut store, false);
    assert_has(&json!(storage(&out)), &json!([{ "type": "Release" }]));
    assert!(script.core.is_idle());
    // Changed now: written whole at once, and let go again; idle only
    // then.
    let out = script.set_data(CHAT).1;
    assert_eq!(one_storage(&out)["type"], "Snapshot");
    assert!(!script.core.is_idle(), "until it's written");
    store.take(&out);
    let out = script.serve(&mut store, false);
    assert_has(&json!(storage(&out)), &json!([{ "type": "Release" }]));
    assert!(store.locked.is_empty());
    assert!(script.core.is_idle());
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
    // Made in one millisecond, they sort as they were made.
    assert!(first.windows(2).all(|pair| pair[0] < pair[1]), "{first:?}");
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
/// Takes in `out` as the store and the agent would: the store takes its
/// work, unless it has (`taken`), and an adapter started is answered as
/// Claude's, its session resumed. Notes the process and prompt running.
fn follow(
    script: &mut Script,
    store: &mut Store,
    (out, taken): (Vec<Value>, bool),
    proc: &mut u64,
    prompt: &mut Option<u64>,
) {
    let mut outs = vec![(out, taken)];
    while let Some((out, taken)) = outs.pop() {
        if !taken {
            store.take(&out);
        }
        if let Some(id) = prompt_in(&out) {
            *prompt = Some(id);
        }
        if let Some(started) = with(&out, "start").first() {
            *proc = started["start"].as_u64().expect("a process");
        }
        for (to, message) in super::harness::writes(&out) {
            let Some(id) = message["id"].as_u64() else {
                continue;
            };
            let answer = match message["method"].as_str() {
                Some("initialize") => claude_init(),
                Some("session/resume") => json!({}),
                Some("session/new") => json!({ "sessionId": SESSION }),
                _ => continue,
            };
            outs.push((script.answers(to, id, &answer), false));
        }
    }
}

/// Process `proc` ends, as asked.
fn end_process(script: &mut Script, proc: u64) {
    script.agent(proc, AgentEvent::Exited(Exit::Code(0)));
    script.agent(proc, AgentEvent::OutputClosed);
    script.agent(proc, AgentEvent::Group { empty: true });
}

fn random_script(seed: u64) -> [usize; 5] {
    let mut dice = Dice(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut host = host();
    host.limits.kept_changes = 3;
    let mut script = Script::new(host);
    script.without_fixtures();
    let mut store = Store::default();
    // Another program's chat, to list, move and delete.
    let other = ChatId::from_number(9000);
    let mut theirs = saved(&mid_turn("claude"), None, 0);
    theirs.chat.id = other;
    theirs.header.chat = other;
    store
        .chats
        .insert((Shelf(SHELF.to_owned()), other), (theirs, Vec::new()));
    script.open_saved("claude");
    let shelves = [SHELF, "else"];
    let words = ["Run", "ning ", "the ", "tests", ".\n", "All ", "pass."];
    let (mut ms, mut prompt, mut tools, mut asked) = (0, None, 0, 0);
    let mut proc = PROC;
    for _ in 0..150 {
        ms += dice.roll(700);
        script.at(ms);
        // Now and then, put away and restored, or shut down, part-way.
        if dice.roll(25) == 0 {
            let out = script.put_away(CHAT).1;
            prompt = None;
            follow(
                &mut script,
                &mut store,
                (out, false),
                &mut proc,
                &mut prompt,
            );
            // Moved, deleted or listed while its write is under way.
            let out = meddle(&mut script, &mut dice, other);
            follow(
                &mut script,
                &mut store,
                (out, false),
                &mut proc,
                &mut prompt,
            );
            end_process(&mut script, proc);
            settle(
                &mut script,
                &mut store,
                &mut ms,
                &mut dice,
                &mut proc,
                &mut prompt,
            );
            // Restored, though the store may fail to load it, and moved,
            // deleted or listed while it loads.
            while script.core.chat(CHAT).is_none() {
                let shelf = store.shelf_of(CHAT).expect("it's stored");
                let out = script.restore(RestoreChat::new(CHAT, shelf)).1;
                follow(
                    &mut script,
                    &mut store,
                    (out, false),
                    &mut proc,
                    &mut prompt,
                );
                let out = meddle(&mut script, &mut dice, other);
                follow(
                    &mut script,
                    &mut store,
                    (out, false),
                    &mut proc,
                    &mut prompt,
                );
                let served = script.serve_any(&mut store, &mut dice, 4);
                follow(
                    &mut script,
                    &mut store,
                    (served, true),
                    &mut proc,
                    &mut prompt,
                );
            }
            continue;
        }
        if dice.roll(40) == 0 {
            let out = script.shutdown();
            prompt = None;
            follow(
                &mut script,
                &mut store,
                (out, false),
                &mut proc,
                &mut prompt,
            );
            end_process(&mut script, proc);
            settle(
                &mut script,
                &mut store,
                &mut ms,
                &mut dice,
                &mut proc,
                &mut prompt,
            );
            assert!(script.core.is_idle(), "seed {seed}: idle once shut down");
            assert!(store.locked.is_empty(), "seed {seed}: every chat let go");
            // And what it let go is the chat as it is.
            let shelf = store.shelf_of(CHAT).expect("it's stored");
            let stored = store.read(&shelf, CHAT).expect("it's stored");
            assert_eq!(
                Some(&stored.chat),
                script.core.chat(CHAT),
                "seed {seed}: written whole as it was let go"
            );
            continue;
        }
        let chunk = |kind: &str, dice: &mut Dice| {
            let text = words[usize::try_from(dice.roll(7)).unwrap_or(0)];
            json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
        };
        let out = match dice.roll(15) {
            0 | 1 => {
                let text = format!("message {}\nmore", dice.roll(100));
                script.send(CHAT, &text).1
            }
            2..=4 => script.update(proc, SESSION, &chunk("agent_message_chunk", &mut dice)),
            5 => script.update(proc, SESSION, &chunk("agent_thought_chunk", &mut dice)),
            6 => {
                tools += 1;
                let call = json!({ "sessionUpdate": "tool_call", "toolCallId": format!("t{tools}"), "title": "Run", "kind": "execute", "status": "pending" });
                script.update(proc, SESSION, &call)
            }
            7 if tools > 0 => {
                let tool = 1 + dice.roll(tools);
                let update = json!({ "sessionUpdate": "tool_call_update", "toolCallId": format!("t{tool}"), "status": "completed", "rawOutput": "ok" });
                script.update(proc, SESSION, &update)
            }
            8 if prompt.is_some() && asked < 3 => {
                asked += 1;
                let out = script.says(proc, &super::stop::permission(900 + asked, "t-ask"));
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
                out.extend(script.answers(proc, id, &json!({ "stopReason": stop })));
                out
            }
            10 => script.set_data(CHAT).1,
            11 => {
                let shelf = shelves[usize::try_from(dice.roll(2)).unwrap_or(0)];
                script.list_saved(shelf).1
            }
            12 => {
                let from = chat_shelf(&script);
                let to = if from == SHELF { "else" } else { SHELF };
                script.move_shelf(&from, to).1
            }
            13 => {
                let shelf = shelves[usize::try_from(dice.roll(2)).unwrap_or(0)];
                script.delete(other, shelf).1
            }
            _ => script.woken(),
        };
        follow(
            &mut script,
            &mut store,
            (out, false),
            &mut proc,
            &mut prompt,
        );
        // The store answers some of its work, late, and fails some.
        while dice.roll(3) == 0 {
            let Some((ticket, event)) = store.next_any(dice.roll(1000), dice.roll(8) == 0) else {
                break;
            };
            let out = script.storage(ticket, event);
            follow(
                &mut script,
                &mut store,
                (out, false),
                &mut proc,
                &mut prompt,
            );
        }
    }
    // Everything written, retries and all.
    for _ in 0..4 {
        ms += 61_000;
        script.at(ms);
        let out = script.woken();
        follow(
            &mut script,
            &mut store,
            (out, false),
            &mut proc,
            &mut prompt,
        );
        let served = script.serve(&mut store, false);
        follow(
            &mut script,
            &mut store,
            (served, true),
            &mut proc,
            &mut prompt,
        );
    }
    let shelf = store.shelf_of(CHAT).expect("it's stored");
    assert_eq!(
        shelf.0,
        chat_shelf(&script),
        "seed {seed}: kept where it's stored"
    );
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
    // Listed again, each shelf's list is what the store holds.
    for listed in shelves {
        let out = script.list_saved(listed).1;
        follow(
            &mut script,
            &mut store,
            (out, false),
            &mut proc,
            &mut prompt,
        );
        let served = script.serve(&mut store, false);
        follow(
            &mut script,
            &mut store,
            (served, true),
            &mut proc,
            &mut prompt,
        );
        let held: BTreeSet<ChatId> = (store.chats.keys())
            .filter(|(on, _)| on.0 == listed)
            .map(|(_, chat)| *chat)
            .collect();
        let shown: BTreeSet<ChatId> = (script.core.saved(&Shelf(listed.to_owned())).into_iter())
            .flatten()
            .map(|each| each.header.chat)
            .collect();
        assert_eq!(shown, held, "seed {seed}: shelf {listed}");
    }

    // Restored, it's the chat, tidied as of when it was last changed.
    let mut tidied = live;
    for change in crate::core::saving::tidy(
        &tidied.clone(),
        stored.header.changed,
        stored.header.changed,
    ) {
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
        script.answers(proc, id, &json!({ "stopReason": "end_turn" }));
    }
    store.counts
}

/// Moves one of the fuzz's shelves to the other, deletes `other` from
/// one, or lists one, or nothing, as the dice say.
fn meddle(script: &mut Script, dice: &mut Dice, other: ChatId) -> Vec<Value> {
    let shelves = [SHELF, "else"];
    let shelf = shelves[usize::try_from(dice.roll(2)).unwrap_or(0)];
    let elsewhere = if shelf == SHELF { "else" } else { SHELF };
    match dice.roll(4) {
        0 => script.move_shelf(shelf, elsewhere).1,
        1 => script.delete(other, shelf).1,
        2 => script.list_saved(shelf).1,
        _ => Vec::new(),
    }
}

/// The shelf the core keeps [`CHAT`] on, loaded or put away.
fn chat_shelf(script: &Script) -> String {
    let state = script.state();
    let saving = &state["saving"];
    let kept = &saving["kept"][CHAT.to_string()]["shelf"];
    let away = &saving["away"][CHAT.to_string()]["shelf"];
    kept.as_str().or(away.as_str()).expect("a shelf").to_owned()
}

/// Has the store do its work, failing some, until nothing put away or
/// shutting down waits on it, the time passing for writes to be tried
/// again.
fn settle(
    script: &mut Script,
    store: &mut Store,
    ms: &mut u64,
    dice: &mut Dice,
    proc: &mut u64,
    prompt: &mut Option<u64>,
) {
    loop {
        let served = script.serve_any(store, dice, 4);
        follow(script, store, (served, true), proc, prompt);
        if script.core.saving.settled() && store.work.is_empty() {
            return;
        }
        *ms += 61_000;
        script.at(*ms);
        let out = script.woken();
        follow(script, store, (out, false), proc, prompt);
    }
}

#[test]
fn a_saved_chat_reads_back_as_it_is_and_restores_tidied() {
    let mut counts = [0; 5];
    for seed in 1..=60 {
        let each = random_script(seed);
        for (all, each) in counts.iter_mut().zip(each) {
            *all += each;
        }
    }
    // Snapshots and journals both, failures to try again, and chats put
    // away, restored and let go.
    assert!(counts.iter().all(|&count| count > 60), "{counts:?}");
}

#[test]
fn a_host_chooses_how_soon_after_a_change_a_chat_is_saved() {
    let mut host = host();
    host.timers.save = std::time::Duration::from_secs(2);
    let mut script = Script::new(host);
    let (_, out) = script.open(saved_open("claude"));
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 2000 })]);
    script.at(1999);
    assert!(storage(&script.woken()).is_empty());
    script.at(2000);
    assert_eq!(one_storage(&script.woken())["type"], "Snapshot");
}

#[test]
fn chats_a_shelf_cant_read_are_reported_and_left_out() {
    let mut script = Script::new(host());
    script.list_saved(SHELF).0.expect("listing");
    let broken = ChatId::from_number(9000);
    let out = script.storage(
        1000,
        StorageEvent::Listed {
            chats: Vec::new(),
            unreadable: vec![(broken, crate::save::CannotRestore::Newer { version: 9 })],
        },
    );
    assert_eq!(
        with(&out, "report"),
        [
            json!({ "report": "Warn", "message": format!("chat {broken} on shelf \"work\" can't be read: Newer {{ version: 9 }}") })
        ]
    );
    assert_eq!(
        script.core.saved(&Shelf(SHELF.to_owned())),
        Some(Vec::new())
    );
}

#[test]
fn a_shelf_that_couldnt_be_moved_is_read_again() {
    let mut script = Script::new(host());
    for shelf in [SHELF, "renamed"] {
        script.list_saved(shelf).0.expect("listing");
    }
    for ticket in [1000, 1001] {
        let chats = Vec::new();
        let unreadable = Vec::new();
        script.storage(ticket, StorageEvent::Listed { chats, unreadable });
    }
    let out = script.move_shelf(SHELF, "renamed").1;
    let out = script.storage(
        ticket(&one_storage(&out)),
        StorageEvent::Failed("the folder is in use".to_owned()),
    );
    assert_has(
        &json!(storage(&out)),
        &json!([
            { "type": "List", "value": { "shelf": SHELF } },
            { "type": "List", "value": { "shelf": "renamed" } }
        ]),
    );
    assert_eq!(
        with(&out, "report"),
        [
            json!({ "report": "Warn", "message": "shelf \"work\" couldn't be moved to \"renamed\": the folder is in use" })
        ]
    );
}

#[test]
fn a_restored_chats_header_is_its_own_not_the_snapshots() {
    let mut script = Script::new(host());
    let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    script.restore(restore).0.expect("restoring");
    // The snapshot's header, from before its journal: older than the
    // chat it's read with.
    let mut saved = saved(&mid_turn("claude"), Some("old"), -60_000);
    saved.header.turns = 0;
    saved.header.first = None;
    saved.header.label = Some("Old".to_owned());
    saved.header.closed = true;
    let made = saved.header.made;
    let out = script.storage(1000, StorageEvent::Loaded(Box::new(saved)));
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Append", "value": { "header": {
            "made": made.0, "turns": 3, "first": "Hello", "label": null, "closed": false,
            "profile": "claude-agent-acp"
        } } }),
    );
    // What was under way ends when the chat last changed, as it says:
    // its message waiting since 30 ms after its first turn started.
    let latest = json!({ "type": "At", "value": EPOCH - 50_000 + 30 });
    assert_has(
        &script.chat(CHAT),
        &json!({ "turns": [{}, { "life": { "type": "CutOff" }, "window": { "end": latest } }, { "window": { "end": latest } }] }),
    );
}

/// A script with a saved Claude chat written whole (as 10 000 bytes) and
/// a message's changes being added to its journal, not yet done: returns
/// that write's ticket.
fn appending(script: &mut Script) -> u64 {
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(10_000));
    script.at(2000);
    script.send(CHAT, "Run the tests").0.expect("sent");
    script.at(3000);
    let append = one_storage(&script.woken());
    assert_eq!(append["type"], "Append");
    ticket(&append)
}

#[test]
fn shutting_down_with_a_journal_write_under_way_writes_the_chat_whole_before_letting_it_go() {
    let mut script = Script::new(host());
    let append = appending(&mut script);
    let out = script.shutdown();
    assert!(storage(&out).is_empty(), "one write at a time");
    let out = script.storage(append, done(10));
    let snapshot = one_storage(&out);
    assert_eq!(snapshot["type"], "Snapshot");
    let out = script.storage(ticket(&snapshot), done(10_000));
    assert_eq!(
        storage(&out),
        [json!({ "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } })]
    );
}

#[test]
fn shutting_down_writes_a_chat_whose_write_failed_at_once() {
    let mut script = Script::new(host());
    let append = appending(&mut script);
    let out = script.storage(append, StorageEvent::Failed("disk full".to_owned()));
    assert_eq!(with(&out, "wake_at"), [json!({ "wake_at": 4000 })]);
    let out = script.shutdown();
    assert_eq!(one_storage(&out)["type"], "Snapshot", "not after the wait");
}

#[test]
fn putting_away_or_closing_with_a_journal_write_under_way_writes_the_chat_whole_at_once() {
    for close in [false, true] {
        let mut script = Script::new(host());
        let append = appending(&mut script);
        let out = if close {
            script.close(CHAT).1
        } else {
            script.put_away(CHAT).1
        };
        let snapshot = one_storage(&out);
        assert_eq!(snapshot["type"], "Snapshot");
        // The store does them in order: the journal's answer changes
        // nothing, and the chat is let go once it's whole.
        assert!(storage(&script.storage(append, done(10))).is_empty());
        let out = script.storage(ticket(&snapshot), done(10_000));
        assert_eq!(
            storage(&out),
            [json!({ "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } })]
        );
    }
}

#[test]
fn deleting_a_chat_with_a_write_under_way_deletes_it_after() {
    let mut script = Script::new(host());
    let append = appending(&mut script);
    let out = script.delete(CHAT, SHELF).1;
    assert_eq!(one_storage(&out)["type"], "Delete");
    let out = script.storage(append, done(10));
    assert!(storage(&out).is_empty());
    assert!(script.core.chat(CHAT).is_none());
}

#[test]
fn an_answer_to_a_write_no_longer_under_way_changes_nothing() {
    let mut script = Script::new(host());
    let append = appending(&mut script);
    let snapshot = ticket(&one_storage(&script.put_away(CHAT).1));
    // Answered out of order: the chat is let go, and restored.
    script.storage(snapshot, done(10_000));
    let out = script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .1;
    let load = ticket(&one_storage(&out));
    let mut stored = saved(&mid_turn("claude"), Some("old"), 0);
    stored.chat.turns.truncate(1);
    stored.chat.questions.clear();
    let out = script.storage(load, StorageEvent::Loaded(Box::new(stored)));
    let tidy = ticket(&one_storage(&out));
    // The old journal write's answer comes last.
    script.storage(append, StorageEvent::Failed("too late".to_owned()));
    assert_eq!(
        script.state()["saving"]["kept"][CHAT.to_string()]["writing"],
        json!(tidy)
    );
}

#[test]
fn a_chat_saved_by_a_clean_shutdown_is_written_at_once_as_its_restore_makes_it_idle() {
    let mut script = Script::new(host());
    let mut chat = mid_turn("claude");
    chat["state"] = json!({ "type": "Lost", "value": { "reason": { "type": "ShutDown" } } });
    if let Some(turns) = chat["turns"].as_array_mut() {
        turns.truncate(1);
    }
    chat["questions"] = json!([]);
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let out = script.storage(1000, StorageEvent::Loaded(Box::new(saved(&chat, None, 0))));
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Append", "value": { "entries": [
            { "type": "Change", "value": { "type": "State", "value": { "type": "Idle" } } }
        ] } }),
    );
}

#[test]
fn a_damaged_save_isnt_restored_and_is_let_go() {
    /// What's wrong, and the damage.
    type Case = (&'static str, fn(&mut Saved));
    let cases: [Case; 7] = [
        (
            "it holds chat 00000000-0000-0000-0000-00000000162e instead",
            |saved| {
                saved.chat.id = ChatId::from_number(5678);
            },
        ),
        ("a turn or item in it is numbered too near 2^53", |saved| {
            saved.chat.turns[0].id = crate::TurnId((1 << 53) - 1);
        }),
        ("a turn or item in it is numbered too near 2^53", |saved| {
            saved.chat.turns[1].items[0].id = crate::ItemId(u64::MAX);
        }),
        ("a turn or item in it is numbered too near 2^53", |saved| {
            saved.chat.turns[2].id = crate::TurnId((1 << 53) - (1 << 32));
        }),
        ("its folder isn't absolute", |saved| {
            saved.chat.cwd = "work".into();
        }),
        ("what a turn in it spent counts past 2^53", |saved| {
            let mut spent = crate::shape::turn_usage();
            spent.models[0].tokens.thinking = Some(1 << 53);
            saved.chat.turns[0].spent = Some(Box::new(spent));
        }),
        ("what a turn in it spent counts past 2^53", |saved| {
            // Each count below it, but not added up.
            let mut spent = crate::shape::turn_usage();
            spent.models[0].tokens.input = (1 << 52) + 1;
            spent.models[1].tokens.output = (1 << 52) + 1;
            saved.chat.turns[1].spent = Some(Box::new(spent));
        }),
    ];
    for (why, damage) in cases {
        let mut script = Script::new(host());
        store_listed(&mut script);
        script
            .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
            .0
            .expect("restoring");
        let mut stored = saved(&mid_turn("claude"), Some("old"), 0);
        damage(&mut stored);
        let load = 1001;
        let out = script.storage(load, StorageEvent::Loaded(Box::new(stored)));
        assert_eq!(
            storage(&out),
            [json!({ "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } })]
        );
        assert!(script.core.chat(CHAT).is_none());
        assert_has(
            &serde_json::to_value(script.core.saved(&Shelf(SHELF.to_owned()))).expect("JSON"),
            &json!([{ "state": { "type": "Failed", "value": { "type": "Damaged", "value": why } } }]),
        );
        // Nothing went past 2^53.
        assert!(script.core.ids.next < 1 << 53);
    }
}

/// Lists [`SHELF`], holding [`CHAT`] as [`mid_turn`] saved it.
fn store_listed(script: &mut Script) {
    script.list_saved(SHELF).0.expect("listing");
    let header = saved(&mid_turn("claude"), Some("old"), 0).header;
    let chats = vec![header];
    let unreadable = Vec::new();
    script.storage(1000, StorageEvent::Listed { chats, unreadable });
}

#[test]
fn a_restore_the_store_answers_wrongly_fails() {
    let mut script = Script::new(host());
    store_listed(&mut script);
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let out = script.storage(1001, done(0));
    assert!(storage(&out).is_empty());
    assert_has(
        &serde_json::to_value(script.core.saved(&Shelf(SHELF.to_owned()))).expect("JSON"),
        &json!([{ "state": { "type": "Failed", "value": { "type": "Damaged", "value": "the store answered with done" } } }]),
    );
    // It can be tried again.
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring again");
}

#[test]
fn a_chat_put_away_before_it_was_ever_written_is_listed_once_it_is() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    let snapshot = ticket(&one_storage(&script.put_away(CHAT).1));
    script.storage(snapshot, StorageEvent::Failed("disk full".to_owned()));
    // Listed while the store doesn't have it yet.
    let list = ticket(&one_storage(&script.list_saved(SHELF).1));
    let (chats, unreadable) = (Vec::new(), Vec::new());
    script.storage(list, StorageEvent::Listed { chats, unreadable });
    let listed = || json!([{ "header": { "chat": CHAT }, "state": { "type": "Saved" } }]);
    assert_has(
        &serde_json::to_value(script.core.saved(&Shelf(SHELF.to_owned()))).expect("JSON"),
        &listed(),
    );
    script.at(1000);
    let again = ticket(&one_storage(&script.woken()));
    script.storage(again, done(10_000));
    assert_has(
        &serde_json::to_value(script.core.saved(&Shelf(SHELF.to_owned()))).expect("JSON"),
        &listed(),
    );
}

#[cfg(unix)]
#[test]
fn a_save_whose_folder_isnt_utf_8_isnt_restored() {
    use std::os::unix::ffi::OsStrExt as _;
    // No format CAF writes holds one, so this is a store of the app's own,
    // given it in memory: not a script, as a recording can't hold it.
    let mut core: crate::core::Core =
        crate::core::Core::starting_ids_at(host(), super::harness::agents(), super::harness::FIRST);
    let now = crate::time::Now {
        wall: Wall(EPOCH),
        mono: crate::time::Mono(0),
    };
    let mut out = Vec::new();
    let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    core.restore(now, restore, &mut out).expect("restoring");
    let mut stored = saved(&mid_turn("claude"), Some("old"), 0);
    stored.chat.cwd = std::ffi::OsStr::from_bytes(b"/work/\xff").into();
    let mut out = Vec::new();
    let event = StorageEvent::Loaded(Box::new(stored));
    core.handle(
        now,
        crate::Event::Storage {
            ticket: crate::Ticket(1000),
            event,
        },
        &mut out,
    );
    assert!(core.chat(CHAT).is_none());
    let reports: Vec<String> = (out.iter())
        .filter_map(|directive| match directive {
            crate::Directive::Report(report) => Some(report.message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reports,
        [format!(
            "chat {CHAT} couldn't be restored: Damaged(\"its folder isn't UTF-8\")"
        )]
    );
    assert!(out.iter().any(|directive| matches!(
        directive,
        crate::Directive::Storage(StorageDirective::Release { .. })
    )));
}

#[test]
fn shutting_down_writes_an_idle_chat_whole_before_letting_it_go_though_nothing_changed() {
    let mut script = Script::new(host());
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let out = script.storage(
        1000,
        StorageEvent::Loaded(Box::new(saved(&mid_turn("claude"), Some("old"), 0))),
    );
    let append = ticket(&one_storage(&out));
    // Idle, it has nothing to wind down.
    let out = script.shutdown();
    assert!(storage(&out).is_empty());
    let out = script.storage(append, done(10));
    let snapshot = one_storage(&out);
    assert_eq!(
        snapshot["type"], "Snapshot",
        "never let go after a journal write alone"
    );
    let out = script.storage(ticket(&snapshot), done(10_000));
    assert_eq!(
        storage(&out),
        [json!({ "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } })]
    );
}

#[test]
fn a_chat_numbered_up_to_2_to_the_53_less_2_to_the_32_is_restored() {
    let mut script = Script::new(host());
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    let mut stored = saved(&mid_turn("claude"), Some("old"), 0);
    let last = (1 << 53) - (1 << 32) - 1;
    stored.chat.turns[2].id = crate::TurnId(last);
    script.storage(1000, StorageEvent::Loaded(Box::new(stored)));
    assert!(script.core.chat(CHAT).is_some());
    // Counted on from past it, still below 2^53.
    assert!(script.core.ids.next > last && script.core.ids.next < 1 << 53);
}

#[test]
fn a_restore_in_flight_as_caf_shuts_down_is_written_whole_and_let_go() {
    let mut script = Script::new(host());
    let mut store = Store::default();
    let shelf = Shelf(SHELF.to_owned());
    let stored = saved(&mid_turn("claude"), Some("old"), 0);
    store
        .chats
        .insert((shelf.clone(), CHAT), (stored, Vec::new()));
    store.take(&script.restore(RestoreChat::new(CHAT, shelf)).1);
    script.shutdown();
    assert!(!script.core.is_idle(), "it waits for the restore");
    let (load, loaded) = store.next(false).expect("the load");
    let out = script.storage(load, loaded);
    assert_eq!(one_storage(&out)["type"], "Snapshot");
    assert!(!script.core.is_idle(), "it waits for the write");
    store.take(&out);
    script.serve(&mut store, false);
    assert!(store.locked.is_empty(), "let go");
    assert!(script.core.is_idle());
}

#[test]
fn a_move_waits_for_work_on_its_shelves_and_a_failed_one_leaves_its_chats_where_they_were() {
    let mut script = Script::new(host());
    // A chat written, and another put away whose write is under way.
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(10_000));
    let (other, _) = script.open(saved_open("claude"));
    let other = other.expect("opened");
    let away = ticket(&one_storage(&script.put_away(other).1));
    // The move waits for the write.
    assert!(storage(&script.move_shelf(SHELF, "renamed").1).is_empty());
    // Written, the move goes, and letting the chat go waits for it.
    let out = script.storage(away, done(10_000));
    let moving = one_storage(&out);
    assert_eq!(moving["type"], "MoveShelf");
    // Meanwhile, nothing is written for the chat on it.
    script.at(2000);
    script.send(CHAT, "Run the tests").0.expect("sent");
    script.at(3000);
    assert!(storage(&script.woken()).is_empty());
    let out = script.storage(ticket(&moving), StorageEvent::Failed("in use".to_owned()));
    assert_has(
        &json!(storage(&out)),
        &json!([
            { "type": "Release", "value": { "shelf": SHELF, "chat": other } },
            { "type": "Append", "value": { "shelf": SHELF, "header": { "chat": CHAT } } }
        ]),
    );
    assert_eq!(
        script.core.summaries()[0].shelf,
        Some(Shelf(SHELF.to_owned()))
    );
}

#[test]
fn a_loaded_or_put_away_chat_that_couldnt_be_deleted_is_let_go_and_listed_again() {
    for put_away in [false, true] {
        let mut script = Script::new(host());
        script.open_saved("claude");
        script.at(1000);
        let snapshot = one_storage(&script.woken());
        script.storage(ticket(&snapshot), done(10_000));
        let list = ticket(&one_storage(&script.list_saved(SHELF).1));
        let header =
            serde_json::from_value(snapshot["value"]["saved"]["header"].clone()).expect("a header");
        let (chats, unreadable) = (vec![header], Vec::new());
        script.storage(list, StorageEvent::Listed { chats, unreadable });
        let away = put_away.then(|| ticket(&one_storage(&script.put_away(CHAT).1)));
        let delete = (storage(&script.delete(CHAT, SHELF).1).into_iter())
            .find(|each| each["type"] == "Delete")
            .expect("a deletion");
        // The store does the chat's work in order: its last write first.
        if let Some(away) = away {
            assert!(storage(&script.storage(away, done(10_000))).is_empty());
        }
        let out = script.storage(
            ticket(&delete),
            StorageEvent::Failed("read-only".to_owned()),
        );
        assert_has(
            &json!(storage(&out)),
            &json!([
                { "type": "List", "value": { "shelf": SHELF } },
                { "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } }
            ]),
        );
    }
}

#[test]
fn a_list_read_before_a_deletion_or_move_doesnt_bring_back_what_it_moved() {
    let mut script = Script::new(host());
    let header = saved(&mid_turn("claude"), Some("old"), 0).header;
    // Listed, then deleted before the list is answered.
    let list = ticket(&one_storage(&script.list_saved(SHELF).1));
    let delete = ticket(&one_storage(&script.delete(CHAT, SHELF).1));
    let (chats, unreadable) = (vec![header.clone()], Vec::new());
    script.storage(list, StorageEvent::Listed { chats, unreadable });
    assert_eq!(
        script.core.saved(&Shelf(SHELF.to_owned())),
        Some(Vec::new())
    );
    script.storage(delete, done(0));

    // Listed, then moved before the list is answered: the move waits
    // for the list, and once moved, both are read again.
    let list = ticket(&one_storage(&script.list_saved("other").1));
    assert!(storage(&script.move_shelf("other", SHELF).1).is_empty());
    let (chats, unreadable) = (vec![header], Vec::new());
    let out = script.storage(list, StorageEvent::Listed { chats, unreadable });
    let moving = ticket(&one_storage(&out));
    let out = script.storage(moving, done(0));
    assert_has(
        &json!(storage(&out)),
        &json!([
            { "type": "List", "value": { "shelf": "other" } },
            { "type": "List", "value": { "shelf": SHELF } }
        ]),
    );
}

#[test]
fn a_load_the_store_fails_says_so_in_its_words() {
    let mut script = Script::new(host());
    store_listed(&mut script);
    script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .0
        .expect("restoring");
    script.storage(1001, StorageEvent::Failed("I/O error".to_owned()));
    assert_has(
        &serde_json::to_value(script.core.saved(&Shelf(SHELF.to_owned()))).expect("JSON"),
        &json!([{ "state": { "type": "Failed", "value": { "type": "Store", "value": "I/O error" } } }]),
    );
}

#[test]
fn a_chat_written_whole_next_keeps_no_entries_meanwhile() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.send(CHAT, "Run the tests").0.expect("sent");
    let kept = &script.state()["saving"]["kept"][CHAT.to_string()];
    assert_eq!(kept["unsaved"], json!([]));
    assert_eq!(kept["behind"], json!(true));
}

#[test]
fn a_chat_isnt_written_while_its_shelf_is_being_moved() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(10_000));
    let moving = ticket(&one_storage(&script.move_shelf(SHELF, "renamed").1));
    script.at(2000);
    script.send(CHAT, "Run the tests").0.expect("sent");
    script.at(3000);
    assert!(storage(&script.woken()).is_empty(), "it waits for the move");
    // Moved: written where it went, as the call ends.
    let out = script.storage(moving, done(0));
    assert_has(
        &json!(storage(&out)),
        &json!([{ "type": "Append", "value": { "shelf": "renamed" } }]),
    );
}

#[test]
fn moves_of_one_shelf_go_one_at_a_time_and_leave_a_chat_where_the_store_has_it() {
    // Each pair: whether the first move, then the second back, is done.
    for (first, second, ends) in [
        (false, false, SHELF),
        (true, false, "renamed"),
        (false, true, SHELF),
        (true, true, SHELF),
    ] {
        let mut script = Script::new(host());
        script.open_saved("claude");
        script.at(1000);
        let snapshot = ticket(&one_storage(&script.woken()));
        script.storage(snapshot, done(10_000));
        let there = ticket(&one_storage(&script.move_shelf(SHELF, "renamed").1));
        assert!(storage(&script.move_shelf("renamed", SHELF).1).is_empty());
        let answer = |done_: bool| {
            if done_ {
                done(0)
            } else {
                StorageEvent::Failed("in use".to_owned())
            }
        };
        let back = ticket(&one_storage(&script.storage(there, answer(first))));
        script.storage(back, answer(second));
        let kept = &script.state()["saving"]["kept"][CHAT.to_string()];
        assert_eq!(kept["shelf"], ends, "{first} {second}");
    }
}

#[test]
fn a_chat_made_after_the_clock_went_back_still_sorts_after() {
    let mut core: crate::core::Core = crate::core::Core::new(host(), super::harness::agents());
    core.numbered = false;
    let mut open = |ms: i64| {
        let now = crate::time::Now {
            wall: Wall(EPOCH + ms),
            mono: crate::time::Mono(0),
        };
        let chat = OpenChat::new("claude", "/work");
        core.open_chat(now, chat, &mut Vec::new()).expect("opened")
    };
    let first = open(1000);
    let second = open(0);
    let third = open(2000);
    assert!(first < second && second < third);
    assert_eq!(second.0.get_version_num(), 7);
}

#[test]
fn loads_deletions_and_lists_wait_for_a_move_of_their_shelf_and_go_where_it_says() {
    for moved in [true, false] {
        let mut script = Script::new(host());
        let moving = ticket(&one_storage(&script.move_shelf(SHELF, "renamed").1));
        // A restore, a deletion and a list of the shelf being moved wait,
        // as does a list of the one it goes to; another shelf's don't.
        let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
        assert!(storage(&script.restore(restore).1).is_empty());
        let gone = ChatId::from_number(9000);
        assert!(storage(&script.delete(gone, SHELF).1).is_empty());
        assert!(storage(&script.list_saved("renamed").1).is_empty());
        assert_eq!(
            one_storage(&script.list_saved("elsewhere").1)["type"],
            "List"
        );
        assert!(!script.core.is_idle());
        let answer = if moved {
            done(0)
        } else {
            StorageEvent::Failed("in use".to_owned())
        };
        let out = script.storage(moving, answer);
        let went = if moved { "renamed" } else { SHELF };
        assert_has(
            &json!(storage(&out)),
            &json!([
                { "type": "Load", "value": { "shelf": went, "chat": CHAT } },
                { "type": "Delete", "value": { "shelf": went, "chat": gone } },
                { "type": "List", "value": { "shelf": "renamed" } }
            ]),
        );
    }
}

#[test]
fn a_chat_opened_on_a_shelf_being_moved_is_first_written_once_its_moved() {
    let mut script = Script::new(host());
    let moving = ticket(&one_storage(&script.move_shelf(SHELF, "renamed").1));
    script.open(saved_open("claude")).0.expect("opened");
    script.at(1000);
    assert!(storage(&script.woken()).is_empty(), "it waits for the move");
    let out = script.storage(moving, done(0));
    assert_has(
        &one_storage(&out),
        &json!({ "type": "Snapshot", "value": { "shelf": "renamed" } }),
    );
}

#[test]
fn a_tool_call_that_changes_again_is_one_entry() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(10_000));
    script.send(CHAT, "Run them").0.expect("sent");
    let call = json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Run", "kind": "execute", "status": "pending" });
    script.update(PROC, SESSION, &call);
    for output in ["one", "two", "three"] {
        let update = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "in_progress", "rawOutput": output });
        script.update(PROC, SESSION, &update);
    }
    script.at(2000);
    let append = one_storage(&script.woken());
    let changed: Vec<&Value> = (append["value"]["entries"].as_array().into_iter())
        .flatten()
        .filter(|entry| entry["value"]["type"] == "ToolChanged")
        .collect();
    assert_eq!(changed.len(), 1, "{append:#}");
    assert_eq!(
        changed[0]["value"]["value"]["tool"]["result"][0]["value"]["text"],
        "three"
    );
}

#[test]
fn a_chat_deleted_while_restored_and_moved_is_let_go_on_the_shelf_it_moved_to() {
    let mut script = Script::new(host());
    let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    let load = ticket(&one_storage(&script.restore(restore).1));
    // The move waits for the load, and the deletion for the move.
    assert!(storage(&script.move_shelf(SHELF, "renamed").1).is_empty());
    assert!(storage(&script.delete(CHAT, SHELF).1).is_empty());
    let stored = saved(&mid_turn("claude"), Some("old"), 0);
    let out = script.storage(load, StorageEvent::Loaded(Box::new(stored)));
    let moving = ticket(&one_storage(&out));
    let out = script.storage(moving, done(0));
    let delete = one_storage(&out);
    assert_has(
        &delete,
        &json!({ "type": "Delete", "value": { "shelf": "renamed", "chat": CHAT } }),
    );
    assert!(!script.core.is_idle(), "it waits for the deletion");
    let out = script.storage(
        ticket(&delete),
        StorageEvent::Failed("read-only".to_owned()),
    );
    assert_eq!(
        storage(&out),
        [json!({ "type": "Release", "value": { "shelf": "renamed", "chat": CHAT } })]
    );
    assert!(script.core.is_idle());
}

#[test]
fn a_chat_never_written_isnt_let_go_when_it_couldnt_be_deleted() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    let delete = ticket(&one_storage(&script.delete(CHAT, SHELF).1));
    let out = script.storage(delete, StorageEvent::Failed("read-only".to_owned()));
    assert!(storage(&out).is_empty(), "the store never held it");
    // One whose first write was under way is, as that write locked it.
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    let delete = ticket(&one_storage(&script.delete(CHAT, SHELF).1));
    script.storage(snapshot, done(10_000));
    let out = script.storage(delete, StorageEvent::Failed("read-only".to_owned()));
    assert_eq!(
        storage(&out),
        [json!({ "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } })]
    );
}

#[test]
fn a_move_done_lists_its_chats_on_the_shelf_they_went_to_at_once() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    for shelf in [SHELF, "renamed"] {
        let list = ticket(&one_storage(&script.list_saved(shelf).1));
        let (chats, unreadable) = (Vec::new(), Vec::new());
        script.storage(list, StorageEvent::Listed { chats, unreadable });
    }
    let moving = ticket(&one_storage(&script.move_shelf(SHELF, "renamed").1));
    script.storage(moving, done(0));
    let renamed = Shelf("renamed".to_owned());
    assert_eq!(
        script.core.saved(&Shelf(SHELF.to_owned())),
        Some(Vec::new())
    );
    assert_has(
        &serde_json::to_value(script.core.saved(&renamed)).expect("JSON"),
        &json!([{ "shelf": "renamed", "header": { "chat": CHAT } }]),
    );
    assert_eq!(script.core.summaries()[0].shelf, Some(renamed));
}

/// Restores, puts away, closes, deletes, moves, lists and changes saved
/// chats (two of another run's, one damaged, and any opened), opens new
/// ones and shuts down, in an order made at random from `seed`, while the
/// store answers in any order the contract lets it and fails some; then
/// checks that, settled, the store holds locked exactly what the core
/// says it holds for this run, each a chat it has loaded, where the store
/// has it; and that, shut down, it's idle with nothing locked.
fn random_lives(seed: u64) {
    let mut dice = Dice(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut script = Script::new(host());
    script.without_fixtures();
    let mut store = Store::default();
    let mut chats = vec![
        ChatId::from_number(9000),
        ChatId::from_number(9001),
        ChatId::from_number(9002),
    ];
    for &chat in &chats {
        let mut theirs = saved(&mid_turn("claude"), Some("old"), 0);
        theirs.chat.id = chat;
        theirs.header.chat = chat;
        // The third is damaged.
        if chat == ChatId::from_number(9002) {
            theirs.chat.turns[0].id = crate::TurnId(u64::MAX);
            store.damaged.insert(chat);
        }
        let shelf = Shelf(SHELF.to_owned());
        store.chats.insert((shelf, chat), (theirs, Vec::new()));
    }
    let shelves = [SHELF, "else"];
    let mut procs = BTreeSet::new();
    let mut ms = 0;
    for _ in 0..80 {
        ms += dice.roll(3000);
        script.at(ms);
        let chat = chats[usize::try_from(dice.roll(chats.len() as u64)).unwrap_or(0)];
        let shelf = shelves[usize::try_from(dice.roll(2)).unwrap_or(0)];
        let elsewhere = if shelf == SHELF { "else" } else { SHELF };
        let out = match dice.roll(12) {
            0 | 1 => {
                script
                    .restore(RestoreChat::new(chat, Shelf(shelf.to_owned())))
                    .1
            }
            // Sending calls a shutdown off.
            10 => script.send(chat, "Go on").1,
            2 => script.put_away(chat).1,
            3 => script.close(chat).1,
            4 => script.delete(chat, shelf).1,
            5 => script.move_shelf(shelf, elsewhere).1,
            6 => script.list_saved(shelf).1,
            7 => script.set_data(chat).1,
            8 => {
                let mut open = OpenChat::new("claude", "/work");
                open.shelf = Some(Shelf(shelf.to_owned()));
                let (opened, out) = script.open(open);
                chats.push(opened.expect("opened"));
                out
            }
            9 if dice.roll(3) == 0 => {
                // Sometimes the store answers at once, so the chats are
                // let go before anything else changes them.
                let out = script.shutdown();
                store.take(&out);
                if dice.roll(2) == 0 {
                    script.serve_any(&mut store, &mut dice, 6);
                }
                Vec::new()
            }
            _ => script.woken(),
        };
        for started in with(&out, "start") {
            procs.insert(started["start"].as_u64().expect("a process"));
        }
        store.take(&out);
        while dice.roll(2) == 0 {
            let Some((ticket, event)) = store.next_any(dice.roll(1000), dice.roll(6) == 0) else {
                break;
            };
            let out = script.storage(ticket, event);
            store.take(&out);
        }
    }
    let settle = |script: &mut Script, store: &mut Store, dice: &mut Dice, ms: &mut u64| loop {
        script.serve_any(store, dice, u64::MAX);
        let written = !script.core.shutting_down || script.core.saving.all_written();
        if script.core.saving.settled() && written && store.work.is_empty() {
            break;
        }
        *ms += 61_000;
        script.at(*ms);
        let out = script.woken();
        store.take(&out);
    };
    settle(&mut script, &mut store, &mut dice, &mut ms);
    let saving = script.state()["saving"].clone();
    let held: BTreeSet<(Shelf, ChatId)> =
        serde_json::from_value(saving["locks"].clone()).expect("locks");
    assert_eq!(
        held, store.locked,
        "seed {seed}: the core knows what it holds"
    );
    for (shelf, chat) in &held {
        assert_eq!(
            saving["kept"][chat.to_string()]["shelf"],
            json!(shelf.0),
            "seed {seed}: chat {chat} is held, loaded, where it's kept"
        );
    }
    // Not shutting down, each loaded chat written as it is is locked.
    if !script.core.shutting_down {
        for (chat, kept) in saving["kept"].as_object().into_iter().flatten() {
            let written = kept["behind"] == false
                && kept["unsaved"] == json!([])
                && kept["due"].is_null()
                && kept["writing"].is_null();
            if written {
                let chat: ChatId = serde_json::from_value(json!(chat)).expect("a chat id");
                let shelf: Shelf = serde_json::from_value(kept["shelf"].clone()).expect("a shelf");
                assert!(
                    store.locked.contains(&(shelf, chat)),
                    "seed {seed}: chat {chat} is loaded and written, so locked"
                );
            }
        }
    }
    // Shut down, everything is let go: by the shutdown already asked for,
    // if it wasn't called off, which must see to changes since by itself.
    if !script.core.shutting_down {
        let out = script.shutdown();
        store.take(&out);
    }
    for proc in procs {
        script.agent(proc, AgentEvent::Exited(Exit::Code(0)));
        script.agent(proc, AgentEvent::OutputClosed);
        script.agent(proc, AgentEvent::Group { empty: true });
    }
    settle(&mut script, &mut store, &mut dice, &mut ms);
    assert!(script.core.is_idle(), "seed {seed}");
    assert!(store.locked.is_empty(), "seed {seed}: {:?}", store.locked);
}

#[test]
fn saved_chats_restored_put_away_deleted_and_moved_in_any_order_settle_as_they_should() {
    for seed in 1..=200 {
        random_lives(seed);
    }
}

#[test]
fn a_write_answered_with_anything_but_done_or_failed_is_tried_again() {
    let mut script = Script::new(host());
    let append = appending(&mut script);
    let listed = StorageEvent::Listed {
        chats: Vec::new(),
        unreadable: Vec::new(),
    };
    let out = script.storage(append, listed);
    assert!(with(&out, "report").iter().any(|each| {
        each["message"]
            .as_str()
            .is_some_and(|message| message.contains("the store answered with a list"))
    }));
    // Shutting down, it's written whole at once; answered wrongly too,
    // it's tried again until it's done: then it's let go, and the core is
    // idle.
    let snapshot = ticket(&one_storage(&script.shutdown()));
    script.agent(PROC, AgentEvent::Exited(Exit::Code(0)));
    script.agent(PROC, AgentEvent::OutputClosed);
    script.agent(PROC, AgentEvent::Group { empty: true });
    let wrong = StorageEvent::NotLoaded(crate::save::CannotRestore::NotFound);
    script.storage(snapshot, wrong);
    assert!(!script.core.is_idle());
    script.at(10_000);
    let again = ticket(&one_storage(&script.woken()));
    let out = script.storage(again, done(10_000));
    assert_eq!(
        storage(&out),
        [json!({ "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } })]
    );
    assert!(script.core.is_idle());
}

#[test]
fn a_chat_in_use_again_after_shutting_down_is_called_off_isnt_let_go() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(10_000));
    let snapshot = ticket(&one_storage(&script.shutdown()));
    // The host sends a message after all: it's in use.
    script.at(2000);
    script.send(CHAT, "One more thing").0.expect("sent");
    let out = script.storage(snapshot, done(10_000));
    assert!(storage(&out).is_empty(), "not let go: {out:#?}");
    assert_eq!(
        script.state()["saving"]["kept"][CHAT.to_string()]["releasing"],
        false
    );
}

#[test]
fn a_chat_put_away_isnt_deleted_from_another_shelf() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.put_away(CHAT).0.expect("put away");
    assert_eq!(script.delete(CHAT, "elsewhere").0, Err(Refused::NoSuchChat));
}

#[test]
fn a_deletion_answered_with_anything_but_done_or_failed_has_failed() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(10_000));
    let delete = ticket(&one_storage(&script.delete(CHAT, SHELF).1));
    let wrong = StorageEvent::NotLoaded(crate::save::CannotRestore::NotFound);
    let out = script.storage(delete, wrong);
    assert!(with(&out, "report").iter().any(|each| {
        each["message"]
            .as_str()
            .is_some_and(|message| message.contains("couldn't be deleted"))
    }));
    // It's still there, locked by this run, which has no use for it.
    assert_eq!(
        storage(&out),
        [json!({ "type": "Release", "value": { "shelf": SHELF, "chat": CHAT } })]
    );
}

#[test]
fn a_chat_let_go_behind_a_move_and_called_back_is_let_go_then_written_whole_after_it() {
    let mut script = Script::new(host());
    script.open_saved("claude");
    script.at(1000);
    let snapshot = ticket(&one_storage(&script.woken()));
    script.storage(snapshot, done(10_000));
    let last = ticket(&one_storage(&script.shutdown()));
    // A move of its shelf waits for its last write; letting it go waits for
    // the move.
    assert!(storage(&script.move_shelf(SHELF, "renamed").1).is_empty());
    let out = script.storage(last, done(10_000));
    let moving = ticket(&one_storage(&out));
    // Called off: it's to be written whole again, after the move.
    script.at(2000);
    let out = script.send(CHAT, "One more thing").1;
    assert!(storage(&out).is_empty(), "it waits for the move");
    let out = script.storage(moving, done(0));
    let mut work = storage(&out).into_iter().map(|each| each["type"].clone());
    assert_eq!(work.next(), Some(json!("Release")));
    assert_eq!(work.next(), Some(json!("Snapshot")));
    assert_has(
        &json!(storage(&out)),
        &json!([
            { "type": "Release", "value": { "shelf": "renamed", "chat": CHAT } },
            { "type": "Snapshot", "value": { "shelf": "renamed" } }
        ]),
    );
}

/// A command CAF runs for the agent, saved: its output's changes are one
/// entry, and put away while it runs, it's cancelled, and comes back
/// failed, its output kept, ended not saying how.
#[test]
fn a_served_command_is_journaled_squashed_and_comes_back_cut_off() {
    use crate::event::{PolicyAnswer, ToolEvent};
    use crate::tool::{CafTool, ServedTool};

    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved_serving("claude");
    store.take(&script.list_saved(SHELF).1);
    script.serve(&mut store, false);
    script.at(1000);
    store.take(&script.woken());
    script.serve(&mut store, false);
    script.send(CHAT, "Build it").0.expect("sent");
    let out = script.tool(
        1,
        ToolEvent::Called {
            chat: CHAT,
            tool: ServedTool::Caf(CafTool::Bash),
            input: json!({ "command": "cargo build" }),
            agent_call: None,
            title: None,
            bad_input: None,
        },
    );
    let decide = with(&out, "decide")[0]["decide"]
        .as_u64()
        .expect("a ticket");
    script.policy(decide, PolicyAnswer::Allow);
    let lines = |text: &[&str]| text.iter().map(|&line| line.to_owned()).collect();
    script.tool(
        1,
        ToolEvent::Output {
            from: 0,
            lines: lines(&["Compiling", "[=> ]"]),
        },
    );
    script.tool(
        1,
        ToolEvent::Output {
            from: 1,
            lines: lines(&["[==>]"]),
        },
    );
    script.tool(
        1,
        ToolEvent::Output {
            from: 2,
            lines: lines(&["more"]),
        },
    );

    script.at(2000);
    let out = script.woken();
    let append = one_storage(&out);
    let changed: Vec<&Value> = (append["value"]["entries"].as_array().into_iter())
        .flatten()
        .filter(|entry| entry["value"]["type"] == "OutputChanged")
        .collect();
    assert_eq!(
        changed,
        [
            &json!({ "type": "Change", "value": { "type": "OutputChanged", "value": {
            "item": 1006, "from": 0, "lines": ["Compiling", "[==>]", "more"], "dropped": 0
        } } })
        ],
        "{append:#}"
    );
    store.take(&out);
    script.serve(&mut store, false);

    script.at(2500);
    let out = script.put_away(CHAT).1;
    // Its call is cancelled, and it's no longer served, as it's let go.
    assert_eq!(
        with(&out, "tool"),
        [
            json!({ "tool": { "type": "Cancel", "value": { "call": 1 } } }),
            json!({ "tool": { "type": "Unserve", "value": { "chat": CHAT } } }),
        ]
    );
    store.take(&out);
    script.serve(&mut store, false);
    script.at(3000);
    let out = script
        .restore(RestoreChat::new(CHAT, Shelf(SHELF.to_owned())))
        .1;
    store.take(&out);
    script.serve(&mut store, false);
    let tool = &script.chat(CHAT)["turns"][0]["items"][0];
    assert_has(
        tool,
        &json!({ "kind": { "type": "Tool", "value": {
            "title": "cargo build", "status": { "type": "Failed" },
            "result": [{ "type": "Output", "value": {
                "lines": ["Compiling", "[==>]", "more"], "dropped": 0, "ending": { "type": "Unknown" }
            } }],
        } } }),
    );
    // Cut off as its chat was put away, not stopped: no one stopped it.
    assert_eq!(tool["kind"]["value"]["stopped_by"], Value::Null);
    assert_ne!(tool["window"]["end"]["type"], "Open");
}

/// However much a command prints between saves, its journal entry is
/// about as big as the output kept, and reads back to it.
#[test]
fn a_chatty_commands_journal_entry_stays_as_small_as_its_output() {
    use crate::event::{PolicyAnswer, ToolEvent};
    use crate::tool::{CafTool, MOST_OUTPUT, ServedTool};

    let mut script = Script::new(host());
    let mut store = Store::default();
    script.open_saved_serving("claude");
    store.take(&script.list_saved(SHELF).1);
    script.serve(&mut store, false);
    script.at(1000);
    store.take(&script.woken());
    script.serve(&mut store, false);
    script.send(CHAT, "Say yes").0.expect("sent");
    let out = script.tool(
        1,
        ToolEvent::Called {
            chat: CHAT,
            tool: ServedTool::Caf(CafTool::Bash),
            input: json!({ "command": "yes" }),
            agent_call: None,
            title: None,
            bad_input: None,
        },
    );
    let decide = with(&out, "decide")[0]["decide"]
        .as_u64()
        .expect("a ticket");
    script.policy(decide, PolicyAnswer::Allow);
    let line = "y".repeat(1000);
    for batch in 0..200u64 {
        let lines = (0..16).map(|_| line.clone()).collect();
        script.tool(
            1,
            ToolEvent::Output {
                from: batch * 16,
                lines,
            },
        );
    }
    script.at(2000);
    let out = script.woken();
    let append = one_storage(&out);
    let size = append.to_string().len();
    assert!(size < 2 * MOST_OUTPUT + 16 * 1024, "{size} bytes");
    store.take(&out);
    script.serve(&mut store, false);
    let (saved, entries) = store.chats.values().next().expect("saved").clone();
    let mut saved = saved;
    for entry in &entries {
        apply(&mut saved, entry);
    }
    assert_eq!(
        serde_json::to_value(&saved.chat.turns).expect("JSON"),
        script.chat(CHAT)["turns"],
        "the journal reads back to the chat"
    );
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: Vec::new(),
            folder: None,
            notice: None,
            item: None,
        },
    );
}

/// A chat given the app's own tools, saved with a call to one, comes back
/// with the call as it was, and is served the app's tools again only if
/// the host gives them, and the app registered them.
#[test]
fn a_chat_with_an_app_tools_call_is_saved_and_restored_given_them_again() {
    use crate::event::{PolicyAnswer, ToolEvent};
    use crate::tool::{CafTool, Piece, ServedTool, Text, Tools};

    let mut script = Script::new(host());
    let mut store = Store::default();
    script.serving();
    let open = saved_open("claude").with_tools(Tools::caf([CafTool::Read]).and_app(["word_count"]));
    script.open_saved_as(open, "claude");
    store.take(&script.list_saved(SHELF).1);
    script.serve(&mut store, false);
    script.at(1000);
    store.take(&script.woken());
    script.serve(&mut store, false);
    script.send(CHAT, "Count them").0.expect("sent");
    let out = script.tool(
        1,
        ToolEvent::Called {
            chat: CHAT,
            tool: ServedTool::App("word_count".to_owned()),
            input: json!({ "text": "a b" }),
            agent_call: Some("toolu_1".to_owned()),
            title: Some("Count 2 words".to_owned()),
            bad_input: None,
        },
    );
    let decide = with(&out, "decide")[0]["decide"]
        .as_u64()
        .expect("a ticket");
    script.policy(decide, PolicyAnswer::Allow);
    script.tool(
        1,
        ToolEvent::Finished {
            done: true,
            pieces: vec![Piece::Text(Text::new("2"))],
            folder: None,
            notice: None,
            item: None,
        },
    );
    let call = script.chat(CHAT)["turns"][0]["items"][0].clone();
    assert_eq!(
        call["kind"]["value"]["origin"],
        json!({ "type": "App", "value": "word_count" })
    );

    script.at(2000);
    let out = script.put_away(CHAT).1;
    assert!(
        with(&out, "tool")
            .contains(&json!({ "tool": { "type": "Unserve", "value": { "chat": CHAT } } })),
        "{out:#?}"
    );
    store.take(&out);
    script.serve(&mut store, false);
    script.at(3000);
    let restore = RestoreChat::new(CHAT, Shelf(SHELF.to_owned()));
    // One the app didn't register is refused, as at open.
    assert_eq!(
        script
            .restore(restore.clone().with_tools(Tools::none().and_app(["nope"])))
            .0,
        Err(Refused::UnknownTool("nope".to_owned()))
    );
    let (restored, out) = script.restore(restore.with_tools(Tools::none().and_app(["word_count"])));
    restored.expect("restoring");
    store.take(&out);
    let out = script.serve(&mut store, false);
    assert_eq!(
        with(&out, "tool"),
        [json!({ "tool": { "type": "Serve", "value": {
            "chat": CHAT,
            "tools": [{ "type": "TaskStop" }],
            "app": ["word_count"],
            "cwd": "/work",
            "instructions": "caf-test's own tools (word_count), and TaskStop to stop their background tasks.",
        } } })]
    );
    let back = &script.chat(CHAT)["turns"][0]["items"][0];
    assert_eq!(back["kind"], call["kind"], "the call as it was");
    assert_has(
        back,
        &json!({ "kind": { "type": "Tool", "value": {
            "title": "Count 2 words", "status": { "type": "Done" },
            "action": { "type": "Tool", "value": { "name": "word_count" } },
        } } }),
    );
}
