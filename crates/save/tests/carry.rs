//! The storage driver's work: each directive done to a store, and how it
//! went.

mod common;

use std::fs;

use caf_core::directive::StorageDirective;
use caf_core::event::StorageEvent;
use caf_core::save::{CannotRestore, Entry, Header, Saved};
use caf_core::{ChatId, Shelf, Ticket};
use caf_save::{FileStore, Store, carry_out, encode_header};
use common::{Folder, Notes, Pin, from, records, small};

type Event = Option<(Ticket, StorageEvent<Pin, Notes>)>;

/// Carries out `directive` on `store`.
fn carry(store: &mut FileStore, directive: StorageDirective<Pin, Notes>) -> Event {
    carry_out(store, directive)
}

/// Saves `saved` whole on `shelf`.
fn snapshot(shelf: &Shelf, saved: Saved<Pin, Notes>) -> StorageDirective<Pin, Notes> {
    StorageDirective::Snapshot {
        ticket: Ticket(1),
        shelf: shelf.clone(),
        saved: Box::new(saved),
    }
}

/// Adds `entries` to chat `id`'s journal.
fn append(shelf: &Shelf, id: u64, entries: Vec<Entry<Pin, Notes>>) -> StorageDirective<Pin, Notes> {
    StorageDirective::Append {
        ticket: Ticket(2),
        shelf: shelf.clone(),
        header: from(common::header(id)),
        entries,
    }
}

/// Loads chat `id`.
fn load(shelf: &Shelf, id: u64) -> StorageDirective<Pin, Notes> {
    StorageDirective::Load {
        ticket: Ticket(3),
        shelf: shelf.clone(),
        chat: ChatId::from_number(id),
    }
}

/// Lets chat `id` go.
fn release(shelf: &Shelf, id: u64) -> StorageDirective<Pin, Notes> {
    StorageDirective::Release {
        shelf: shelf.clone(),
        chat: ChatId::from_number(id),
    }
}

/// How many bytes a write says it took.
fn written(event: Event) -> u64 {
    match event {
        Some((_, StorageEvent::Done { written })) => written,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_chat_written_and_added_to_loads_back() {
    let folder = Folder::new("carry");
    let mut store = FileStore::new(folder.path());
    let work = Shelf("work".into());
    let size = written(carry(&mut store, snapshot(&work, small(1))));
    let caf = folder
        .path()
        .join(format!("work/{}.caf", ChatId::from_number(1)));
    assert_eq!(size, fs::metadata(&caf).expect("it's there").len() - 9);
    let mut expected = small(1);
    for entries in records() {
        for entry in &entries {
            if let Entry::Change(change) = entry {
                expected.chat.apply(change).expect("it fits");
            }
        }
        assert!(written(carry(&mut store, append(&work, 1, entries))) > 4);
    }
    expected.session = Some(caf_core::ids::AgentSessionId("s2".into()));
    expected.data = Notes {
        title: "Docs too".into(),
        tags: vec![],
    };
    assert!(carry(&mut store, release(&work, 1)).is_none());
    match carry(&mut store, load(&work, 1)) {
        Some((Ticket(3), StorageEvent::Loaded(saved))) => assert_eq!(*saved, expected),
        other => panic!("{other:?}"),
    }
    let mut other = FileStore::new(folder.path());
    let not_loaded = |event: Event| match event {
        Some((_, StorageEvent::NotLoaded(why))) => why,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        not_loaded(carry(&mut other, load(&work, 1))),
        CannotRestore::InUse
    );
    assert_eq!(
        not_loaded(carry(&mut other, load(&work, 9))),
        CannotRestore::NotFound
    );
    let failed = carry(&mut other, snapshot(&work, small(1)));
    assert!(matches!(failed, Some((Ticket(1), StorageEvent::Failed(_)))));
}

#[test]
fn a_chat_that_cannot_be_read_is_let_go() {
    let folder = Folder::new("unreadable");
    let mut store = FileStore::new(folder.path());
    let work = Shelf("work".into());
    written(carry(&mut store, snapshot(&work, small(1))));
    carry(&mut store, release(&work, 1));
    let caf = folder
        .path()
        .join(format!("work/{}.caf", ChatId::from_number(1)));
    let mut bytes = fs::read(&caf).expect("it's there");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x40;
    fs::write(&caf, &bytes).expect("damaged");
    let loaded = carry(&mut store, load(&work, 1));
    assert!(matches!(
        loaded,
        Some((_, StorageEvent::NotLoaded(CannotRestore::Damaged(_))))
    ));
    // Not held: another program, maybe a newer one, can have it.
    FileStore::new(folder.path())
        .load(&work, ChatId::from_number(1))
        .expect("it's free");
    assert_eq!(fs::read(&caf).expect("it's there"), bytes, "left as it was");
}

#[test]
fn a_shelf_lists_in_the_order_its_chats_were_made() {
    let folder = Folder::new("list");
    let mut store = FileStore::new(folder.path());
    let work = Shelf("work".into());
    for id in [3, 1, 2, 4] {
        written(carry(&mut store, snapshot(&work, small(id))));
    }
    let head = |id: u64| {
        folder
            .path()
            .join(format!("work/{}.head", ChatId::from_number(id)))
    };
    fs::write(head(2), b"CAF\x01\0\0\0garbage").expect("damaged");
    fs::write(head(4), encode_header(&from::<Header>(common::header(5)))).expect("another's");
    fs::write(folder.path().join("work/notes.txt"), b"not a chat").expect("a stray");
    let Some((Ticket(7), StorageEvent::Listed { chats, unreadable })) = carry(
        &mut store,
        StorageDirective::List {
            ticket: Ticket(7),
            shelf: work,
        },
    ) else {
        panic!("not listed");
    };
    let made: Vec<_> = chats.iter().map(|header| header.chat).collect();
    assert_eq!(made, [1, 3].map(ChatId::from_number));
    let ids: Vec<_> = unreadable.iter().map(|(chat, _)| *chat).collect();
    assert_eq!(ids, [2, 4].map(ChatId::from_number));
    assert!(
        (unreadable.iter()).all(|(_, why)| matches!(why, CannotRestore::Damaged(_))),
        "{unreadable:?}"
    );
    let failed = carry(
        &mut store,
        StorageDirective::List {
            ticket: Ticket(8),
            shelf: Shelf(String::new()),
        },
    );
    assert!(matches!(failed, Some((Ticket(8), StorageEvent::Failed(_)))));
}

#[test]
fn deleting_and_moving_say_they_are_done() {
    let folder = Folder::new("done");
    let mut store = FileStore::new(folder.path());
    let (work, done) = (Shelf("work".into()), Shelf("done".into()));
    written(carry(&mut store, snapshot(&work, small(1))));
    written(carry(&mut store, snapshot(&work, small(2))));
    let moved = carry(
        &mut store,
        StorageDirective::MoveShelf {
            ticket: Ticket(4),
            from: work,
            to: done.clone(),
        },
    );
    assert_eq!(written(moved), 0);
    let deleted = carry(
        &mut store,
        StorageDirective::Delete {
            ticket: Ticket(5),
            shelf: done.clone(),
            chat: ChatId::from_number(1),
        },
    );
    assert_eq!(written(deleted), 0);
    let left: Vec<_> = (store.list(&done).expect("it lists").into_iter())
        .map(|(chat, _)| chat)
        .collect();
    assert_eq!(left, [ChatId::from_number(2)]);
}
