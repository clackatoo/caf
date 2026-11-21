//! A crash at any step of a move or a deletion, as the store takes the
//! steps: here, so the steps are the store's own.

use std::fs;
use std::path::Path;

use caf_core::directive::StorageDirective;
use caf_core::event::StorageEvent;
use caf_core::save::{CannotRestore, Header, Saved};
use caf_core::{ChatId, Shelf, Ticket};

use super::{DELETES, MOVE_LINKS, MOVE_REMOVES, SHELF_LOCK};
use crate::common::{self, Folder, Notes, Pin, from, records, small};
use crate::{FileStore, Squeeze, Store, carry_out, decode, encode_header, encode_record};

/// Saves chat `id` on `on` whole, then each record, and lets it go.
fn save(root: &Path, on: &Shelf, id: u64) {
    let mut store = FileStore::new(root);
    let chat = ChatId::from_number(id);
    let head = encode_header(&from::<Header>(common::header(id)));
    let snapshot = crate::encode_snapshot(&small(id), Squeeze::Fast);
    store
        .snapshot(on, chat, &head, &snapshot)
        .expect("it's written");
    for entries in records() {
        store
            .append(on, chat, &head, &encode_record(&entries))
            .expect("it's added");
    }
}

/// Chat `id` as it reads after [`save`].
fn saved(id: u64) -> Saved<Pin, Notes> {
    let journal: Vec<u8> = records()
        .iter()
        .flat_map(|each| encode_record(each))
        .collect();
    decode(&crate::encode_snapshot(&small(id), Squeeze::Fast), &journal).expect("it reads")
}

/// Whether chat `id` reads from `on` whole, or not at all, but never with
/// less than it had.
fn whole_or_not_at_all(root: &Path, on: &Shelf, id: u64) -> bool {
    let loaded = carry_out::<Pin, Notes>(
        &mut FileStore::new(root),
        StorageDirective::Load {
            ticket: Ticket(1),
            shelf: on.clone(),
            chat: ChatId::from_number(id),
        },
    );
    match loaded {
        Some((_, StorageEvent::Loaded(read))) => *read == saved(id),
        Some((_, StorageEvent::NotLoaded(CannotRestore::NotFound | CannotRestore::Damaged(_)))) => {
            true
        }
        other => panic!("{other:?}"),
    }
}

/// Whether chat `id` is listed on `on`.
fn listed(root: &Path, on: &Shelf, id: u64) -> bool {
    let listed = FileStore::new(root).list(on).expect("it lists");
    listed
        .iter()
        .any(|(chat, _)| *chat == ChatId::from_number(id))
}

/// A chat's file's name.
fn name(id: u64, extension: &str) -> String {
    format!("{}.{extension}", ChatId::from_number(id))
}

#[test]
fn a_move_cut_off_at_any_step_loses_nothing() {
    let (work, done) = (Shelf("work".into()), Shelf("done".into()));
    let steps: Vec<(bool, &str)> = (MOVE_LINKS.iter().map(|each| (true, *each)))
        .chain(MOVE_REMOVES.iter().map(|each| (false, *each)))
        .collect();
    for cut in 0..=steps.len() {
        let folder = Folder::new("moving");
        let root = folder.path();
        save(root, &work, 1);
        save(root, &done, 2);
        for (link, extension) in &steps[..cut] {
            let (here, there) = (
                root.join("work").join(name(1, extension)),
                root.join("done").join(name(1, extension)),
            );
            if *link {
                fs::hard_link(&here, &there).expect("linked");
            } else {
                fs::remove_file(&here).expect("removed");
            }
        }
        for on in [&work, &done] {
            assert!(whole_or_not_at_all(root, on, 1), "cut at {cut}");
        }
        assert!(
            listed(root, &work, 1) || listed(root, &done, 1),
            "cut at {cut}: it's listed somewhere"
        );
        FileStore::new(root)
            .move_shelf(&work, &done)
            .unwrap_or_else(|error| panic!("cut at {cut}, moving again: {error:?}"));
        let stored = FileStore::new(root)
            .load(&done, ChatId::from_number(1))
            .expect("it's moved");
        assert_eq!(decode(&stored.snapshot, &stored.journal), Ok(saved(1)));
        assert!(!root.join("work").exists(), "cut at {cut}");
    }
}

#[test]
fn a_deletion_cut_off_at_any_step_is_seen_and_finished() {
    let work = Shelf("work".into());
    for cut in 0..=DELETES.len() {
        let folder = Folder::new("deleting");
        let root = folder.path();
        save(root, &work, 1);
        for extension in &DELETES[..cut] {
            fs::remove_file(root.join("work").join(name(1, extension))).expect("removed");
        }
        assert!(whole_or_not_at_all(root, &work, 1), "cut at {cut}");
        let anything = ["caf", "journal", "head"]
            .iter()
            .any(|extension| root.join("work").join(name(1, extension)).exists());
        assert_eq!(listed(root, &work, 1), anything, "cut at {cut}");
        FileStore::new(root)
            .delete(&work, ChatId::from_number(1))
            .expect("it's deleted");
        let left: Vec<_> = (fs::read_dir(root.join("work")).expect("the shelf"))
            .map(|entry| entry.expect("a file").file_name())
            .collect();
        assert_eq!(left, [SHELF_LOCK], "cut at {cut}");
    }
}

/// Links all of chat 1's files from `work` to `done`, as a move cut off
/// once it had, and writes it again whole on `written`, a snapshot of
/// what's there now, by a store that then holds it, or lets it go.
fn half_moved_then_written(root: &Path, written: &Shelf, let_go: bool) -> FileStore {
    let (work, done) = (Shelf("work".into()), Shelf("done".into()));
    save(root, &work, 1);
    save(root, &done, 2);
    for extension in MOVE_LINKS {
        let (here, there) = (
            root.join("work").join(name(1, extension)),
            root.join("done").join(name(1, extension)),
        );
        fs::hard_link(&here, &there).expect("linked");
    }
    let mut store = FileStore::new(root);
    let chat = ChatId::from_number(1);
    store.load(written, chat).expect("it loads");
    let head = encode_header(&from::<Header>(common::header(1)));
    let snapshot = crate::encode_snapshot(&saved(1), Squeeze::Fast);
    store
        .snapshot(written, chat, &head, &snapshot)
        .expect("it's written");
    if let_go {
        store.release(written, chat);
    }
    store
}

#[test]
fn a_move_cut_off_and_written_since_is_finished_with_what_was_written() {
    let (work, done) = (Shelf("work".into()), Shelf("done".into()));
    let chat = ChatId::from_number(1);
    for (written, let_go) in [(&done, false), (&done, true), (&work, true)] {
        let folder = Folder::new("half-moved");
        let root = folder.path();
        let mut store = half_moved_then_written(root, written, let_go);
        store
            .move_shelf(&work, &done)
            .unwrap_or_else(|error| panic!("on {written:?}: {error:?}"));
        assert!(!root.join("work").exists(), "one copy, on {written:?}");
        store.release(&done, chat);
        let stored = FileStore::new(root).load(&done, chat).expect("it's moved");
        // What was written: the chat whole, in a snapshot of the next
        // generation, with an empty journal.
        assert!(stored.journal.is_empty(), "on {written:?}");
        assert_eq!(decode(&stored.snapshot, &stored.journal), Ok(saved(1)));
        let caf = fs::read(root.join("done").join(name(1, "caf"))).expect("it's there");
        assert_eq!(caf[..8], 2_u64.to_le_bytes(), "on {written:?}");
    }
}

#[test]
fn a_new_file_made_exclusive_is_never_another_s() {
    let folder = Folder::new("exclusive");
    fs::create_dir_all(folder.path()).expect("a folder");
    let dir = rustix::fs::open(
        folder.path(),
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .expect("it opens");
    fs::write(folder.path().join("taken.new"), b"another's").expect("written");
    let made = super::write_new(&dir, "taken.new", &[b"mine"], None, true);
    assert_eq!(
        made.map_err(|error| error.kind()),
        Err(std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(
        fs::read(folder.path().join("taken.new")).expect("there"),
        b"another's"
    );
    super::write_new(&dir, "free.new", &[b"mine"], None, true).expect("made");
    assert_eq!(
        fs::read(folder.path().join("free.new")).expect("there"),
        b"mine"
    );
}
