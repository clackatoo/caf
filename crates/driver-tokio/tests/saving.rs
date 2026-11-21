//! Saving and restoring chats, end to end: a real `FileStore` in a folder
//! of the test's own, and the fake adapter.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use caf_core::agent::{AgentSpec, McpServer, SpawnCommand};
use caf_core::chat::{AppData, ChatState, NoAppData, NoAppItems, UserMessage};
use caf_core::event::Event;
use caf_core::host::Host;
use caf_core::ids::{ChatId, Shelf};
use caf_core::save::{CannotRestore, Header, RestoreChat, SavedChat, SavedState};
use caf_core::viewer::{FromViewer, Outcome, RequestNo, ToViewer};
use caf_core::{Core, Now, OpenChat, Refused};
use caf_driver_tokio::viewer::{ViewerRules, serve};
use caf_driver_tokio::{Caf, Config, Error, Running, SavedWatch};
use caf_save::{FileStore, Store, StoreError, Stored};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use common::*;

/// The shelf the tests keep chats on.
const SHELF: &str = "work";

fn shelf(name: &str) -> Shelf {
    Shelf(name.to_owned())
}

/// The app's data in these tests: a title, shown as the chat's label,
/// and tags.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Notes {
    title: String,
    tags: Vec<String>,
}

impl AppData for Notes {
    fn label(&self) -> Option<String> {
        (!self.title.is_empty()).then(|| self.title.clone())
    }
}

/// Where the tests' stores keep their chats.
fn root(dir: &Dir) -> std::path::PathBuf {
    dir.path("chats")
}

/// CAF with the fake running `script`, keeping chats in a `FileStore` in
/// `dir`: a new store each time, as a new program would have.
fn start<D: AppData>(dir: &Dir, script: &Path) -> (Caf<NoAppItems, D>, Running) {
    start_with(dir, script, Some(Box::new(FileStore::new(root(dir)))), None)
}

fn start_with<D: AppData>(
    dir: &Dir,
    script: &Path,
    store: Option<Box<dyn Store>>,
    record: Option<std::path::PathBuf>,
) -> (Caf<NoAppItems, D>, Running) {
    let mut config = Config::new(host(), vec![fake(dir, script)]);
    config.store = store;
    config.record = record;
    Caf::start(config)
}

/// Shuts CAF down and waits for its task to end.
async fn stop<D: AppData>(caf: &Caf<NoAppItems, D>, running: Running) {
    let done = tokio::time::timeout(PATIENCE, async {
        caf.shutdown().await;
        running.await;
    })
    .await;
    assert!(done.is_ok(), "shutting down took too long");
}

/// Opening a chat with the fake, kept on `on`.
fn saved_open(dir: &Dir, on: &str) -> OpenChat {
    let mut open = OpenChat::new("fake", &dir.0);
    open.shelf = Some(shelf(on));
    open
}

/// Waits until the saved chats listed are as `done` wants, and gives them.
async fn listed(
    watch: &mut SavedWatch,
    what: &str,
    done: impl Fn(&[SavedChat]) -> bool,
) -> Vec<SavedChat> {
    let mut last = Vec::new();
    let waited = tokio::time::timeout(PATIENCE, async {
        while let Some(saved) = watch.next().await {
            if done(&saved) {
                return Some(saved);
            }
            last = saved;
        }
        None
    })
    .await;
    match waited {
        Ok(Some(saved)) => saved,
        Ok(None) => panic!("CAF ended waiting for {what}"),
        Err(_) => panic!("gave up waiting for {what}: {last:#?}"),
    }
}

/// Restores `chat`, from [`SHELF`], once it's no longer taken: a chat put
/// away is refused as loaded until it's written whole, though it's listed
/// as saved at once.
async fn restore_once_written<D: AppData>(
    caf: &Caf<NoAppItems, D>,
    chat: ChatId,
) -> caf_driver_tokio::ChatHandle<NoAppItems, D> {
    let restored = tokio::time::timeout(PATIENCE, async {
        loop {
            match caf.restore(RestoreChat::new(chat, shelf(SHELF))).await {
                Err(Error::Refused(Refused::Loaded)) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                other => return other,
            }
        }
    })
    .await;
    restored.expect("written in good time").expect("restored")
}

/// Lists `on` again and again, as another program saves to it, until
/// what's listed is as `done` wants, and gives it.
async fn relisted<D: AppData>(
    caf: &Caf<NoAppItems, D>,
    on: &str,
    what: &str,
    done: impl Fn(&[SavedChat]) -> bool,
) -> Vec<SavedChat> {
    let mut watch = caf.watch_saved();
    let mut last = Vec::new();
    let waited = tokio::time::timeout(PATIENCE, async {
        loop {
            caf.list_saved(shelf(on)).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let saved = watch.next().await?;
            if done(&saved) {
                return Some(saved);
            }
            last = saved;
        }
    })
    .await;
    match waited {
        Ok(Some(saved)) => saved,
        Ok(None) => panic!("CAF ended waiting for {what}"),
        Err(_) => panic!("gave up waiting for {what}: {last:#?}"),
    }
}

/// The state `chat` is listed with, if it is.
fn state_of(saved: &[SavedChat], chat: ChatId) -> Option<&SavedState> {
    (saved.iter())
        .find(|each| each.header.chat == chat)
        .map(|each| &each.state)
}

/// The first adapter: opens session `s1` and answers one prompt.
fn first_script(dir: &Dir) -> std::path::PathBuf {
    dir.script(
        "first.jsonl",
        &after_handshake(&[prompt(), say("s1", "Hello there"), end_turn()]),
    )
}

/// An adapter that can resume sessions, and expects to resume `s1`.
fn resuming_script(dir: &Dir) -> std::path::PathBuf {
    dir.script("resuming.jsonl", &[
        json!({"on": {"fallback": true, "do": [{"respond": {"error": {"code": -32601, "message": "Method not found"}}}]}}),
        json!({"wait": {"method": "initialize"}}),
        json!({"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {"sessionCapabilities": {"resume": {}}}, "agentInfo": {"name": "fake", "version": "1.0.0"}}}}),
        json!({"wait": {"method": "session/resume", "expect": {"params": {"sessionId": "s1"}}}}),
        json!({"respond": {"result": {}}}),
        prompt(),
        say("s1", "Again"),
        end_turn(),
    ])
}

/// An adapter that can only load sessions, replaying what it had.
fn loading_script(dir: &Dir) -> std::path::PathBuf {
    dir.script("loading.jsonl", &[
        json!({"on": {"fallback": true, "do": [{"respond": {"error": {"code": -32601, "message": "Method not found"}}}]}}),
        json!({"wait": {"method": "initialize"}}),
        json!({"respond": {"result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": true}, "agentInfo": {"name": "fake", "version": "1.0.0"}}}}),
        json!({"wait": {"method": "session/load", "expect": {"params": {"sessionId": "s1"}}}}),
        json!({"update": {"session": "s1", "update": {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "Hi"}}}}),
        say("s1", "Hello there"),
        json!({"respond": {"result": {}}}),
        prompt(),
        say("s1", "Again"),
        end_turn(),
    ])
}

/// The methods the fakes were sent, in order.
fn methods(dir: &Dir) -> Vec<String> {
    let text = std::fs::read_to_string(dir.path("record.jsonl")).unwrap_or_default();
    (text.lines())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|line| line["received"]["method"].as_str().map(str::to_owned))
        .collect()
}

/// Whether any fake found what it was sent wasn't what its script expected.
fn any_failed(dir: &Dir) -> bool {
    let text = std::fs::read_to_string(dir.path("record.jsonl")).unwrap_or_default();
    text.lines().any(|line| line.contains(r#""failed""#))
}

/// Opens a saved chat, sends "Hi", waits for the answer, and shuts down:
/// the chat's id.
async fn a_chat_saved_by_another_run(dir: &Dir) -> ChatId {
    let (caf, running) = start::<NoAppData>(dir, &first_script(dir));
    let chat = caf.open_chat(saved_open(dir, SHELF)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();
    until(&chat, &mut watch, "the answer", finished).await;
    stop(&caf, running).await;
    chat.id
}

/// Restores `chat` in a new run whose adapter follows `script`, sees its
/// record, and sends to it again: its session is carried on, and what the
/// agent replays isn't added to the record.
async fn a_restored_chat_carries_on(script: fn(&Dir) -> std::path::PathBuf, how: &str) {
    let dir = Dir::new();
    let id = a_chat_saved_by_another_run(&dir).await;
    let (caf, running) = start::<NoAppData>(&dir, &script(&dir));
    let mut saved = caf.watch_saved();
    caf.list_saved(shelf(SHELF)).await.unwrap();
    let list = listed(&mut saved, "the chat saved before", |saved| {
        !saved.is_empty()
    })
    .await;
    assert_eq!(list.len(), 1, "{list:#?}");
    let header = &list[0].header;
    assert_eq!(
        (header.chat, list[0].state.clone()),
        (id, SavedState::Saved)
    );
    assert_eq!((header.first.as_deref(), header.turns), (Some("Hi"), 1));
    assert_eq!(list[0].shelf, shelf(SHELF));

    let chat = caf
        .restore(RestoreChat::new(id, shelf(SHELF)))
        .await
        .unwrap();
    assert_eq!(chat.id, id);
    let restored = picture(&chat).await;
    assert_eq!(restored.state, ChatState::Idle, "restored chats are idle");
    assert_eq!(restored.turns.len(), 1);
    assert_eq!(words(&restored.turns[0]), "Hello there");
    assert!(
        !methods(&dir).contains(&"session/resume".to_owned())
            && !methods(&dir).contains(&"session/load".to_owned()),
        "no agent starts until a message is sent"
    );
    let list = listed(&mut saved, "the chat loaded", |saved| {
        state_of(saved, id) == Some(&SavedState::Loaded)
    })
    .await;
    assert_eq!(list.len(), 1);
    let open = caf.watch_chats().next().await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open[0].id, open[0].shelf.clone()),
        (id, Some(shelf(SHELF)))
    );

    let mut watch = chat.watch();
    chat.send(UserMessage::text("Again")).await.unwrap();
    let now = until(&chat, &mut watch, "the second answer", |chat| {
        chat.turns.len() == 2 && finished(chat)
    })
    .await;
    assert_eq!(
        words(&now.turns[0]),
        "Hello there",
        "{how}: nothing doubled"
    );
    assert_eq!(words(&now.turns[1]), "Again");
    assert!(
        methods(&dir).contains(&how.to_owned()),
        "the session is carried on with {how}: {:?}",
        methods(&dir)
    );
    assert!(!any_failed(&dir), "the fakes got what they expected");
    stop(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_saved_in_one_run_is_restored_in_the_next_and_its_session_resumed() {
    a_restored_chat_carries_on(resuming_script, "session/resume").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restored_session_the_agent_can_only_load_is_loaded_and_its_replay_thrown_away() {
    a_restored_chat_carries_on(loading_script, "session/load").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_put_away_is_let_go_and_comes_back_and_a_deleted_one_is_gone() {
    let dir = Dir::new();
    let (caf, running) = start::<NoAppData>(&dir, &first_script(&dir));
    let mut saved = caf.watch_saved();
    caf.list_saved(shelf(SHELF)).await.unwrap();
    let chat = caf.open_chat(saved_open(&dir, SHELF)).await.unwrap();
    let id = chat.id;
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();
    until(&chat, &mut watch, "the answer", finished).await;
    listed(&mut saved, "the chat, loaded", |saved| {
        state_of(saved, id) == Some(&SavedState::Loaded)
    })
    .await;

    let handle = caf.chat(id);
    handle.put_away().await.unwrap();
    // Its watch ends, as it's gone from memory, and so is it from the
    // open chats.
    let ended =
        tokio::time::timeout(PATIENCE, async { while watch.next().await.is_some() {} }).await;
    assert!(ended.is_ok(), "the watch of a chat put away ends");
    assert!(caf.watch_chats().next().await.unwrap().is_empty());
    let list = listed(&mut saved, "the chat, put away", |saved| {
        state_of(saved, id) == Some(&SavedState::Saved)
    })
    .await;
    assert_eq!(list[0].header.turns, 1);
    assert_eq!(
        caf.chat(id).send(UserMessage::text("Hello?")).await,
        Err(Error::Refused(Refused::NoSuchChat)),
        "it's not loaded"
    );

    // Restored in the same run, it's the same chat.
    let chat = restore_once_written(&caf, id).await;
    assert_eq!(picture(&chat).await.turns.len(), 1);
    assert_eq!(
        caf.restore(RestoreChat::new(id, shelf(SHELF)))
            .await
            .map(|chat| chat.id),
        Err(Error::Refused(Refused::Loaded)),
        "a chat is loaded once"
    );

    // Deleted, it's gone: from memory, the list, and the store.
    caf.delete(id, shelf(SHELF)).await.unwrap();
    listed(&mut saved, "the chat, deleted", |saved| saved.is_empty()).await;
    assert!(caf.watch_chats().next().await.unwrap().is_empty());
    assert_eq!(
        caf.restore(RestoreChat::new(id, shelf(SHELF)))
            .await
            .map(|chat| chat.id),
        Err(Error::CannotRestore(CannotRestore::NotFound))
    );
    let list = caf.watch_saved().next().await.unwrap();
    assert!(list.is_empty(), "a chat that isn't there isn't listed");
    stop(&caf, running).await;
    let files: Vec<_> = std::fs::read_dir(root(&dir).join(SHELF))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&id.to_string()) && !name.ends_with(".lock"))
        .collect();
    assert!(files.is_empty(), "{files:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shelf_moves_with_its_chats_loaded_or_not() {
    let dir = Dir::new();
    let script = dir.script("idle.jsonl", &handshake("s1"));
    let (caf, running) = start::<NoAppData>(&dir, &script);
    let mut saved = caf.watch_saved();
    caf.list_saved(shelf("a")).await.unwrap();
    caf.list_saved(shelf("b")).await.unwrap();
    let chat = caf.open_chat(saved_open(&dir, "a")).await.unwrap();
    // Changed, so it's written.
    chat.set_data(NoAppData).await.unwrap();
    listed(&mut saved, "the chat on a", |saved| {
        saved.len() == 1 && saved[0].shelf == shelf("a")
    })
    .await;
    caf.move_shelf(shelf("a"), shelf("b")).await.unwrap();
    let list = listed(&mut saved, "the chat on b", |saved| {
        saved.len() == 1 && saved[0].shelf == shelf("b")
    })
    .await;
    assert_eq!(list[0].state, SavedState::Loaded);
    let open = caf.watch_chats().next().await.unwrap();
    assert_eq!(open[0].shelf, Some(shelf("b")), "{open:?}");
    let id = chat.id;
    chat.put_away().await.unwrap();
    listed(&mut saved, "the chat put away on b", |saved| {
        state_of(saved, id) == Some(&SavedState::Saved) && saved[0].shelf == shelf("b")
    })
    .await;
    stop(&caf, running).await;
    let mut store = FileStore::new(root(&dir));
    assert!(store.list(&shelf("a")).unwrap().is_empty());
    assert_eq!(store.list(&shelf("b")).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_apps_data_is_saved_with_its_chat_and_its_label_shown() {
    let dir = Dir::new();
    let script = dir.script("idle.jsonl", &handshake("s1"));
    let (caf, running) = start::<Notes>(&dir, &script);
    let mut saved = caf.watch_saved();
    caf.list_saved(shelf(SHELF)).await.unwrap();
    let chat = caf.open_chat(saved_open(&dir, SHELF)).await.unwrap();
    assert_eq!(chat.data().await.unwrap(), Notes::default());
    let notes = Notes {
        title: "Fixing the tests".into(),
        tags: vec!["ci".into()],
    };
    chat.set_data(notes.clone()).await.unwrap();
    assert_eq!(chat.data().await.unwrap(), notes);
    let open = caf.watch_chats().next().await.unwrap();
    assert_eq!(open[0].label.as_deref(), Some("Fixing the tests"));
    listed(&mut saved, "the chat's label", |saved| {
        saved.len() == 1 && saved[0].header.label.as_deref() == Some("Fixing the tests")
    })
    .await;
    assert_eq!(
        caf.chat(ChatId::from_number(4242)).data().await,
        Err(Error::Refused(Refused::NoSuchChat))
    );
    let id = chat.id;
    stop(&caf, running).await;

    let (caf, running) = start::<Notes>(&dir, &script);
    let chat = caf
        .restore(RestoreChat::new(id, shelf(SHELF)))
        .await
        .unwrap();
    assert_eq!(chat.data().await.unwrap(), notes, "its data comes back");
    let open = caf.watch_chats().next().await.unwrap();
    assert_eq!(open[0].label.as_deref(), Some("Fixing the tests"));
    stop(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_open_in_another_caf_is_in_use_until_that_one_lets_it_go() {
    let dir = Dir::new();
    let (first, first_running) = start::<NoAppData>(&dir, &first_script(&dir));
    let chat = first.open_chat(saved_open(&dir, SHELF)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();
    until(&chat, &mut watch, "the answer", finished).await;

    let other = dir.script("other.jsonl", &handshake("s2"));
    let (second, second_running) = start::<NoAppData>(&dir, &other);
    let mut saved = second.watch_saved();
    // Written once its turn ended: listed once that's done.
    let list = relisted(&second, SHELF, "the other's chat", |saved| {
        !saved.is_empty()
    })
    .await;
    assert_eq!(list[0].header.chat, chat.id);
    let restored = second
        .restore(RestoreChat::new(chat.id, shelf(SHELF)))
        .await;
    assert_eq!(
        restored.map(|chat| chat.id),
        Err(Error::CannotRestore(CannotRestore::InUse))
    );
    listed(&mut saved, "the failure", |saved| {
        state_of(saved, chat.id) == Some(&SavedState::Failed(CannotRestore::InUse))
    })
    .await;

    stop(&first, first_running).await;
    let restored = second
        .restore(RestoreChat::new(chat.id, shelf(SHELF)))
        .await;
    let restored = restored.expect("free once the first has shut down");
    assert_eq!(words(&picture(&restored).await.turns[0]), "Hello there");
    stop(&second, second_running).await;
}

/// Keeps a chat on [`SHELF`] as `snapshot`, with a header that reads, in
/// the tests' store.
fn keep_as(dir: &Dir, chat: ChatId, snapshot: &[u8]) {
    let header: Header = serde_json::from_value(json!({
        "chat": chat, "agent": "fake", "profile": null, "cwd": dir.0, "made": 1000,
        "changed": 1000, "label": null, "first": "Hi", "turns": 1, "closed": false,
    }))
    .expect("a header");
    let mut store = FileStore::new(root(dir));
    let head = caf_save::encode_header(&header);
    store
        .snapshot(&shelf(SHELF), chat, &head, snapshot)
        .unwrap();
    store.release(&shelf(SHELF), chat);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_or_damaged_save_isnt_restored_and_is_left_as_it_was() {
    let dir = Dir::new();
    let newer = ChatId::from_number(4001);
    let damaged = ChatId::from_number(4002);
    let mut snapshot = b"CAF".to_vec();
    snapshot.extend_from_slice(&(caf_save::VERSION + 1).to_le_bytes());
    snapshot.extend_from_slice(b"from the future");
    keep_as(&dir, newer, &snapshot);
    let mut broken = b"CAF".to_vec();
    broken.extend_from_slice(&caf_save::VERSION.to_le_bytes());
    broken.extend_from_slice(b"not zstd at all");
    keep_as(&dir, damaged, &broken);
    let before = FileStore::new(root(&dir))
        .load(&shelf(SHELF), newer)
        .unwrap();

    let (caf, running) = start::<NoAppData>(&dir, &first_script(&dir));
    let mut saved = caf.watch_saved();
    caf.list_saved(shelf(SHELF)).await.unwrap();
    listed(&mut saved, "both chats", |saved| saved.len() == 2).await;
    let restored = caf.restore(RestoreChat::new(newer, shelf(SHELF))).await;
    let version = caf_save::VERSION + 1;
    assert_eq!(
        restored.map(|chat| chat.id),
        Err(Error::CannotRestore(CannotRestore::Newer { version }))
    );
    let restored = caf.restore(RestoreChat::new(damaged, shelf(SHELF))).await;
    assert!(
        matches!(
            restored.as_ref().map(|chat| chat.id),
            Err(Error::CannotRestore(CannotRestore::Damaged(_)))
        ),
        "{:?}",
        restored.map(|chat| chat.id)
    );
    let list = listed(&mut saved, "both failures", |saved| {
        saved
            .iter()
            .all(|each| matches!(each.state, SavedState::Failed(_)))
    })
    .await;
    assert_eq!(list.len(), 2);
    assert!(caf.watch_chats().next().await.unwrap().is_empty());
    stop(&caf, running).await;
    let mut store = FileStore::new(root(&dir));
    let after: Stored = store.load(&shelf(SHELF), newer).unwrap();
    assert_eq!(after, before, "a newer save is left untouched");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_store_nothing_that_needs_one_is_done() {
    let dir = Dir::new();
    let script = dir.script("idle.jsonl", &handshake("s1"));
    let (caf, running) = start_with::<NoAppData>(&dir, &script, None, None);
    let not_saved = Err(Error::Refused(Refused::NotSaved));
    assert_eq!(
        caf.open_chat(saved_open(&dir, SHELF))
            .await
            .map(|chat| chat.id),
        not_saved
    );
    assert_eq!(
        caf.list_saved(shelf(SHELF)).await,
        Err(Error::Refused(Refused::NotSaved))
    );
    let id = ChatId::from_number(4242);
    assert_eq!(
        caf.restore(RestoreChat::new(id, shelf(SHELF)))
            .await
            .map(|chat| chat.id),
        not_saved
    );
    assert_eq!(
        caf.delete(id, shelf(SHELF)).await,
        Err(Error::Refused(Refused::NotSaved))
    );
    assert_eq!(
        caf.move_shelf(shelf("a"), shelf("b")).await,
        Err(Error::Refused(Refused::NotSaved))
    );
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    chat.set_data(NoAppData).await.unwrap();
    assert_eq!(chat.data().await, Ok(NoAppData));
    assert_eq!(
        caf.chat(chat.id).put_away().await,
        Err(Error::Refused(Refused::NotSaved))
    );
    assert_eq!(caf.watch_saved().next().await, Some(Vec::new()));
    stop(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutting_down_writes_a_chat_mid_turn_whole_and_leaves_nothing_locked() {
    let dir = Dir::new();
    let script = dir.script(
        "slow.jsonl",
        &after_handshake(&[prompt(), say("s1", "Working on it"), "hang".into()]),
    );
    let (caf, running) = start::<NoAppData>(&dir, &script);
    let chat = caf.open_chat(saved_open(&dir, SHELF)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();
    until(&chat, &mut watch, "the words", |chat| {
        chat.turns
            .first()
            .is_some_and(|turn| words(turn) == "Working on it")
    })
    .await;
    stop(&caf, running).await;
    // Free for another program, and as CAF left it.
    let mut store = FileStore::new(root(&dir));
    let stored = store
        .load(&shelf(SHELF), chat.id)
        .expect("nothing holds it once CAF has shut down");
    let saved: caf_core::save::Saved = caf_save::decode(&stored.snapshot, &stored.journal).unwrap();
    assert_eq!(words(&saved.chat.turns[0]), "Working on it");
    store.release(&shelf(SHELF), chat.id);
}

/// A store that panics on its first write, then keeps chats in files.
struct Panicky {
    files: FileStore,
    panicked: Arc<Mutex<bool>>,
}

impl Store for Panicky {
    fn list(&mut self, shelf: &Shelf) -> Result<Vec<(ChatId, Vec<u8>)>, StoreError> {
        self.files.list(shelf)
    }
    fn load(&mut self, shelf: &Shelf, chat: ChatId) -> Result<Stored, StoreError> {
        self.files.load(shelf, chat)
    }
    fn snapshot(
        &mut self,
        shelf: &Shelf,
        chat: ChatId,
        header: &[u8],
        snapshot: &[u8],
    ) -> Result<(), StoreError> {
        let mut panicked = self.panicked.lock().unwrap();
        if !*panicked {
            *panicked = true;
            drop(panicked);
            panic!("the app's store has a bug");
        }
        drop(panicked);
        self.files.snapshot(shelf, chat, header, snapshot)
    }
    fn append(
        &mut self,
        shelf: &Shelf,
        chat: ChatId,
        header: &[u8],
        record: &[u8],
    ) -> Result<(), StoreError> {
        self.files.append(shelf, chat, header, record)
    }
    fn release(&mut self, shelf: &Shelf, chat: ChatId) {
        self.files.release(shelf, chat);
    }
    fn holds(&self, shelf: &Shelf, chat: ChatId) -> bool {
        self.files.holds(shelf, chat)
    }
    fn delete(&mut self, shelf: &Shelf, chat: ChatId) -> Result<(), StoreError> {
        self.files.delete(shelf, chat)
    }
    fn move_shelf(&mut self, from: &Shelf, to: &Shelf) -> Result<(), StoreError> {
        self.files.move_shelf(from, to)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_store_that_panics_fails_that_write_and_the_chat_is_saved_after() {
    let dir = Dir::new();
    let panicked = Arc::new(Mutex::new(false));
    let store = Panicky {
        files: FileStore::new(root(&dir)),
        panicked: Arc::clone(&panicked),
    };
    let (caf, running) =
        start_with::<NoAppData>(&dir, &first_script(&dir), Some(Box::new(store)), None);
    let chat = caf.open_chat(saved_open(&dir, SHELF)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();
    until(&chat, &mut watch, "the answer", finished).await;
    stop(&caf, running).await;
    assert!(*panicked.lock().unwrap(), "the store panicked");
    let mut store = FileStore::new(root(&dir));
    let stored = store.load(&shelf(SHELF), chat.id).expect("saved after all");
    let saved: caf_core::save::Saved = caf_save::decode(&stored.snapshot, &stored.journal).unwrap();
    assert_eq!(words(&saved.chat.turns[0]), "Hello there");
}

/// A store whose loads wait until the test lets them go.
struct Slow {
    files: FileStore,
    go: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl Store for Slow {
    fn list(&mut self, shelf: &Shelf) -> Result<Vec<(ChatId, Vec<u8>)>, StoreError> {
        self.files.list(shelf)
    }
    fn load(&mut self, shelf: &Shelf, chat: ChatId) -> Result<Stored, StoreError> {
        let _ = (self.go.lock().unwrap()).recv_timeout(PATIENCE);
        self.files.load(shelf, chat)
    }
    fn snapshot(
        &mut self,
        shelf: &Shelf,
        chat: ChatId,
        header: &[u8],
        snapshot: &[u8],
    ) -> Result<(), StoreError> {
        self.files.snapshot(shelf, chat, header, snapshot)
    }
    fn append(
        &mut self,
        shelf: &Shelf,
        chat: ChatId,
        header: &[u8],
        record: &[u8],
    ) -> Result<(), StoreError> {
        self.files.append(shelf, chat, header, record)
    }
    fn release(&mut self, shelf: &Shelf, chat: ChatId) {
        self.files.release(shelf, chat);
    }
    fn delete(&mut self, shelf: &Shelf, chat: ChatId) -> Result<(), StoreError> {
        self.files.delete(shelf, chat)
    }
    fn move_shelf(&mut self, from: &Shelf, to: &Shelf) -> Result<(), StoreError> {
        self.files.move_shelf(from, to)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_store_holds_up_nothing_but_its_own_work() {
    let dir = Dir::new();
    let id = a_chat_saved_by_another_run(&dir).await;
    let (go, wait) = std::sync::mpsc::channel();
    let store = Slow {
        files: FileStore::new(root(&dir)),
        go: Arc::new(Mutex::new(wait)),
    };
    let script = dir.script(
        "second.jsonl",
        &after_handshake(&[prompt(), say("s1", "Quick"), end_turn()]),
    );
    let (caf, running) = start_with::<NoAppData>(&dir, &script, Some(Box::new(store)), None);
    let restoring = tokio::spawn({
        let caf = caf.clone();
        async move {
            caf.restore(RestoreChat::new(id, shelf(SHELF)))
                .await
                .map(|chat| chat.id)
        }
    });
    // While the store reads, other chats go on.
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    chat.send(UserMessage::text("Hi")).await.unwrap();
    until(&chat, &mut watch, "the answer", finished).await;
    assert!(!restoring.is_finished());
    go.send(()).unwrap();
    let restored = tokio::time::timeout(PATIENCE, restoring).await;
    assert_eq!(restored.unwrap().unwrap(), Ok(id));
    stop(&caf, running).await;
}

/// A recording's event.
#[derive(Deserialize)]
#[serde(bound = "")]
struct Step {
    now: Now,
    event: Event<NoAppItems, Notes>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recorded_restore_replays_to_the_same_chat_and_data() {
    let dir = Dir::new();
    let script = dir.script("idle.jsonl", &handshake("s1"));
    let (caf, running) = start::<Notes>(&dir, &script);
    let chat = caf.open_chat(saved_open(&dir, SHELF)).await.unwrap();
    let notes = Notes {
        title: "Before".into(),
        tags: Vec::new(),
    };
    chat.set_data(notes).await.unwrap();
    let id = chat.id;
    stop(&caf, running).await;

    let recording = dir.path("run.jsonl");
    let store = FileStore::new(root(&dir));
    let (caf, running) = start_with::<Notes>(
        &dir,
        &script,
        Some(Box::new(store)),
        Some(recording.clone()),
    );
    let mut restore = RestoreChat::new(id, shelf(SHELF));
    let mut server = SpawnCommand::new("/bin/tools");
    server.env = vec![caf_core::agent::EnvChange::Set(
        "TOKEN".into(),
        "sk-do-not-record".into(),
    )];
    restore.mcp_servers = vec![McpServer::Stdio {
        name: "tools".into(),
        command: server,
    }];
    let chat = caf.restore(restore).await.unwrap();
    let after = Notes {
        title: "After".into(),
        tags: vec!["restored".into()],
    };
    chat.set_data(after.clone()).await.unwrap();
    let live = picture(&chat).await;
    stop(&caf, running).await;

    let text = std::fs::read_to_string(&recording).unwrap();
    assert!(
        !text.contains("sk-do-not-record"),
        "MCP servers' values aren't recorded"
    );
    assert!(text.contains(r#""type":"Restore""#), "{text}");
    assert!(
        text.contains(r#""type":"Loaded""#),
        "the chat restored, whole"
    );
    assert!(text.contains(r#""type":"SetData""#));
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let mut core = Core::<NoAppItems, Notes>::new(
        serde_json::from_value::<Host>(header["host"].clone()).unwrap(),
        serde_json::from_value::<Vec<AgentSpec>>(header["agents"].clone()).unwrap(),
    );
    let mut out = Vec::new();
    for line in lines {
        let step: Step = serde_json::from_str(line).expect("a step reads");
        core.handle(step.now, step.event, &mut out);
    }
    assert_eq!(core.data(id), Some(&after));
    let replayed = core.chat(id).expect("restored in the replay too");
    assert_eq!(replayed.turns, live.turns);
    assert_eq!(replayed.agent, live.agent);
}

/// One viewer of `caf`, said hello to: what it's sent, from the welcome on.
async fn viewer<D: AppData>(
    caf: &Caf<NoAppItems, D>,
    rules: ViewerRules,
) -> (mpsc::Sender<FromViewer>, mpsc::Receiver<ToViewer>) {
    let (to, from_viewer) = mpsc::channel(16);
    let (to_viewer, from) = mpsc::channel(64);
    tokio::spawn(serve(caf.clone(), Arc::new(rules), from_viewer, to_viewer));
    to.send(FromViewer::Hello { min: 1, max: 1 }).await.unwrap();
    (to, from)
}

/// The next message a viewer gets that `want` takes, passing over others.
async fn heard<T>(from: &mut mpsc::Receiver<ToViewer>, want: impl Fn(ToViewer) -> Option<T>) -> T {
    let found = tokio::time::timeout(PATIENCE, async {
        while let Some(message) = from.recv().await {
            if let Some(found) = want(message) {
                return Some(found);
            }
        }
        None
    })
    .await;
    found.ok().flatten().expect("the message, in good time")
}

/// The outcome of request `request`.
async fn outcome(from: &mut mpsc::Receiver<ToViewer>, request: u64) -> Outcome {
    heard(from, |message| match message {
        ToViewer::Done {
            request: done,
            outcome,
        } if done == RequestNo(request) => Some(outcome),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_sees_saved_chats_and_restores_one_as_the_host_allows() {
    let dir = Dir::new();
    let id = a_chat_saved_by_another_run(&dir).await;
    let (caf, running) = start::<NoAppData>(&dir, &resuming_script(&dir));

    // Nothing listed: no Saved after the agents.
    let (to, mut from) = viewer(&caf, ViewerRules::default()).await;
    assert!(matches!(from.recv().await, Some(ToViewer::Welcome { .. })));
    assert!(matches!(from.recv().await, Some(ToViewer::Agents(_))));
    caf.list_saved(shelf(SHELF)).await.unwrap();
    let list = heard(&mut from, |message| match message {
        ToViewer::Saved(saved) if !saved.is_empty() => Some(saved),
        _ => None,
    })
    .await;
    assert_eq!(
        (list[0].header.chat, &list[0].state),
        (id, &SavedState::Saved)
    );
    // The host doesn't let this one restore.
    to.send(FromViewer::Restore {
        request: RequestNo(1),
        chat: id,
        shelf: shelf(SHELF),
    })
    .await
    .unwrap();
    assert_eq!(outcome(&mut from, 1).await, Outcome::NotAllowed);

    // Listed already: Saved comes right after the agents.
    let mut rules = ViewerRules::default();
    rules.restore =
        Box::new(|chat, on| (on.0 == SHELF).then(|| RestoreChat::new(chat, on.clone())));
    let (to, mut from) = viewer(&caf, rules).await;
    assert!(matches!(from.recv().await, Some(ToViewer::Welcome { .. })));
    assert!(matches!(from.recv().await, Some(ToViewer::Agents(_))));
    assert!(
        matches!(from.recv().await, Some(ToViewer::Saved(saved)) if saved.len() == 1),
        "the list, after the agents"
    );
    to.send(FromViewer::Restore {
        request: RequestNo(2),
        chat: id,
        shelf: shelf("elsewhere"),
    })
    .await
    .unwrap();
    assert_eq!(outcome(&mut from, 2).await, Outcome::NotAllowed);
    let missing = ChatId::from_number(4242);
    to.send(FromViewer::Restore {
        request: RequestNo(3),
        chat: missing,
        shelf: shelf(SHELF),
    })
    .await
    .unwrap();
    assert_eq!(
        outcome(&mut from, 3).await,
        Outcome::CannotRestore(CannotRestore::NotFound)
    );
    to.send(FromViewer::Restore {
        request: RequestNo(4),
        chat: id,
        shelf: shelf(SHELF),
    })
    .await
    .unwrap();
    // Answered, with the chat in the list of open chats and loaded in the
    // list of saved ones, in whatever order they come.
    let (mut opened, mut open, mut loaded) = (None, false, false);
    while opened.is_none() || !open || !loaded {
        let message = heard(&mut from, Some).await;
        match message {
            ToViewer::Done {
                request: RequestNo(4),
                outcome,
            } => opened = Some(outcome),
            ToViewer::Chats(chats) => {
                open = (chats.iter()).any(|each| each.id == id && each.shelf == Some(shelf(SHELF)));
            }
            ToViewer::Saved(saved) => {
                loaded = state_of(&saved, id) == Some(&SavedState::Loaded);
            }
            _ => {}
        }
    }
    assert_eq!(opened, Some(Outcome::Opened(id)));
    to.send(FromViewer::Watch {
        chat: id,
        since: None,
    })
    .await
    .unwrap();
    let picture = heard(&mut from, |message| match message {
        ToViewer::Picture { chat, .. } => Some(chat),
        _ => None,
    })
    .await;
    assert_eq!(words(&picture.turns[0]), "Hello there");
    to.send(FromViewer::Restore {
        request: RequestNo(5),
        chat: id,
        shelf: shelf(SHELF),
    })
    .await
    .unwrap();
    assert_eq!(
        outcome(&mut from, 5).await,
        Outcome::Refused(Refused::Loaded)
    );

    // Put away, its watch ends.
    caf.chat(id).put_away().await.unwrap();
    heard(&mut from, |message| match message {
        ToViewer::Ended { chat } => (chat == id).then_some(()),
        _ => None,
    })
    .await;
    stop(&caf, running).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewers_restore_holds_up_nothing_else_on_its_link() {
    let dir = Dir::new();
    let id = a_chat_saved_by_another_run(&dir).await;
    let (go, wait) = std::sync::mpsc::channel();
    let store = Slow {
        files: FileStore::new(root(&dir)),
        go: Arc::new(Mutex::new(wait)),
    };
    let script = dir.script(
        "second.jsonl",
        &after_handshake(&[prompt(), say("s1", "Quick"), end_turn()]),
    );
    let (caf, running) = start_with::<NoAppData>(&dir, &script, Some(Box::new(store)), None);
    let mut rules = ViewerRules::default();
    rules.restore = Box::new(|chat, on| Some(RestoreChat::new(chat, on.clone())));
    let (to, mut from) = viewer(&caf, rules).await;
    to.send(FromViewer::Restore {
        request: RequestNo(1),
        chat: id,
        shelf: shelf(SHELF),
    })
    .await
    .unwrap();
    assert!(matches!(from.recv().await, Some(ToViewer::Welcome { .. })));
    // Meanwhile, a chat the host opens is heard of.
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    heard(&mut from, |message| match message {
        ToViewer::Chats(chats) => chats.iter().any(|each| each.id == chat.id).then_some(()),
        ToViewer::Done {
            request: RequestNo(1),
            ..
        } => {
            panic!("the restore is answered only once the store has read the chat")
        }
        _ => None,
    })
    .await;
    go.send(()).unwrap();
    assert_eq!(outcome(&mut from, 1).await, Outcome::Opened(id));
    stop(&caf, running).await;
}
