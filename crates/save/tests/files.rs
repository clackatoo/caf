//! The file store: where it keeps chats, its locks, and what a crash can
//! leave.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use caf_core::directive::StorageDirective;
use caf_core::event::StorageEvent;
use caf_core::save::{CannotRestore, Header, Saved};
use caf_core::{ChatId, Shelf, Ticket};
use caf_save::{
    FileStore, Squeeze, Store, StoreError, VERSION, carry_out, decode, encode_header,
    encode_record, encode_snapshot,
};
use common::{Folder, Notes, Pin, from, records, small};
use sha2::{Digest, Sha256};

/// A shelf.
fn shelf(name: &str) -> Shelf {
    Shelf(name.into())
}

/// Chat `id`'s header, written.
fn head(id: u64) -> Vec<u8> {
    encode_header(&from::<Header>(common::header(id)))
}

/// Chat `id`'s snapshot.
fn snap(id: u64) -> Vec<u8> {
    encode_snapshot(&small(id), Squeeze::Fast)
}

/// Each of [`records`], written.
fn written_records() -> Vec<Vec<u8>> {
    records().iter().map(|each| encode_record(each)).collect()
}

/// A chat's file.
fn file(root: &Path, folder: &str, id: u64, extension: &str) -> PathBuf {
    root.join(folder)
        .join(format!("{}.{extension}", ChatId::from_number(id)))
}

/// Saves chat `id` on `on` whole, then each record, and lets it go.
fn save(store: &mut FileStore, on: &Shelf, id: u64) {
    let chat = ChatId::from_number(id);
    store
        .snapshot(on, chat, &head(id), &snap(id))
        .expect("it's written");
    for record in written_records() {
        store
            .append(on, chat, &head(id), &record)
            .expect("it's added");
    }
    store.release(on, chat);
}

/// Chat `id` as it reads after [`save`].
fn saved(id: u64) -> Saved<Pin, Notes> {
    let journal = written_records().concat();
    decode(&snap(id), &journal).expect("it reads")
}

/// Whether it's [`StoreError::InUse`].
fn in_use<T>(result: Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::InUse))
}

#[test]
fn a_chat_is_kept_as_its_files() {
    let folder = Folder::new("files");
    let mut store = FileStore::new(folder.path());
    let work = shelf("work");
    save(&mut store, &work, 1);
    let stored = store.load(&work, ChatId::from_number(1)).expect("it loads");
    assert_eq!(stored.snapshot, snap(1));
    assert_eq!(stored.journal, written_records().concat());
    assert_eq!(decode(&stored.snapshot, &stored.journal), Ok(saved(1)));
    let root = folder.path();
    assert_eq!(
        fs::read(file(root, "work", 1, "head")).expect("a header"),
        head(1)
    );
    for extension in ["caf", "journal", "lock"] {
        assert!(file(root, "work", 1, extension).is_file(), "{extension}");
    }
    let listed = store.list(&work).expect("it lists");
    assert_eq!(listed, vec![(ChatId::from_number(1), head(1))]);
    assert_eq!(store.list(&shelf("never")).expect("it lists"), vec![]);
    let missing = store.load(&work, ChatId::from_number(2));
    assert!(matches!(missing, Err(StoreError::NotFound)));
    assert!(
        !file(root, "work", 2, "lock").exists(),
        "no lock left behind"
    );
}

#[test]
fn one_program_at_a_time_has_a_chat() {
    let folder = Folder::new("locks");
    let (work, chat) = (shelf("work"), ChatId::from_number(1));
    let mut first = FileStore::new(folder.path());
    let mut second = FileStore::new(folder.path());
    first
        .snapshot(&work, chat, &head(1), &snap(1))
        .expect("it's written");
    assert!(in_use(second.load(&work, chat)));
    assert!(in_use(second.snapshot(&work, chat, &head(1), &snap(1))));
    assert!(in_use(second.delete(&work, chat)));
    first.release(&work, chat);
    second.load(&work, chat).expect("it's free");
    assert!(in_use(first.load(&work, chat)));
    // Loading again what it has is fine.
    second.load(&work, chat).expect("it's its own");
    drop(second);
    first.load(&work, chat).expect("a store gone lets go");
}

#[test]
fn a_snapshot_replaces_the_old_with_its_journal_in_one_step() {
    let folder = Folder::new("atomic");
    let root = folder.path();
    let (work, chat) = (shelf("work"), ChatId::from_number(1));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    let (caf, journal) = (
        file(root, "work", 1, "caf"),
        file(root, "work", 1, "journal"),
    );
    let old = (
        fs::read(&caf).expect("one"),
        fs::read(&journal).expect("one"),
    );
    assert_eq!(old.0[..8], 1_u64.to_le_bytes(), "the first generation");
    assert_eq!(old.1[..8], 1_u64.to_le_bytes());

    // The next snapshot: a new generation, and an empty journal of it.
    let newer = encode_snapshot(&saved(1), Squeeze::Fast);
    store
        .snapshot(&work, chat, &head(1), &newer)
        .expect("it's written");
    let new = (
        fs::read(&caf).expect("one"),
        fs::read(&journal).expect("one"),
    );
    assert_eq!(new.0[..8], 2_u64.to_le_bytes());
    assert_eq!(new.1, 2_u64.to_le_bytes());
    store.release(&work, chat);

    // A crash after the snapshot, before the journal: the old journal
    // doesn't count.
    fs::write(&journal, &old.1).expect("the old journal");
    let stored = store.load(&work, chat).expect("it loads");
    assert_eq!((stored.snapshot, stored.journal), (newer.clone(), vec![]));
    assert_eq!(decode::<Pin, Notes>(&newer, &[]), Ok(saved(1)));
    // And the next append starts it again.
    let record = &written_records()[0];
    store
        .append(&work, chat, &head(1), record)
        .expect("it's added");
    assert_eq!(
        fs::read(&journal).expect("one"),
        [&2_u64.to_le_bytes()[..], record].concat()
    );
    store.release(&work, chat);

    // A crash before the snapshot was in place: the old one, whole.
    fs::write(&caf, &old.0).expect("the old snapshot");
    fs::write(&journal, &old.1).expect("the old journal");
    let stored = store.load(&work, chat).expect("it loads");
    assert_eq!(decode(&stored.snapshot, &stored.journal), Ok(saved(1)));
    store.release(&work, chat);

    // A journal left of a chat deleted halfway never counts for a new one.
    fs::remove_file(&caf).expect("the snapshot gone");
    fs::remove_file(file(root, "work", 1, "head")).expect("the header gone");
    store
        .snapshot(&work, chat, &head(1), &snap(1))
        .expect("it's written");
    assert_eq!(fs::read(&caf).expect("one")[..8], 2_u64.to_le_bytes());
    let stored = store.load(&work, chat).expect("it loads");
    assert!(stored.journal.is_empty());
}

#[test]
fn a_record_cut_off_by_a_crash_is_written_over() {
    let records = written_records();
    let half = records[0][..records[0].len() / 2].to_vec();
    let mut garbage = records[0].clone();
    let middle = garbage.len() / 2;
    garbage[middle] ^= 0x40;
    // Short of its length; as long as it should be, but zeros, as a file
    // system that writes sizes first leaves it; or garbage.
    for tail in [half, vec![0; 300], garbage] {
        let folder = Folder::new("torn");
        let root = folder.path();
        let (work, chat) = (shelf("work"), ChatId::from_number(1));
        let mut store = FileStore::new(root);
        save(&mut store, &work, 1);
        let journal = file(root, "work", 1, "journal");
        let whole = fs::read(&journal).expect("one");
        let torn = [&whole[..], &tail].concat();
        fs::write(&journal, &torn).expect("a torn record");
        let stored = store.load(&work, chat).expect("it loads");
        assert_eq!(
            fs::read(&journal).expect("one"),
            torn,
            "loading changes nothing"
        );
        assert_eq!(decode(&stored.snapshot, &stored.journal), Ok(saved(1)));
        // What's added next goes after the records that count.
        store
            .append(&work, chat, &head(1), &records[2])
            .expect("it's added");
        store
            .append(&work, chat, &head(1), &records[2])
            .expect("it's added");
        store.release(&work, chat);
        let stored = store.load(&work, chat).expect("it loads");
        let expected = [&whole[8..], &records[2], &records[2]].concat();
        assert_eq!(stored.journal, expected);
        assert!(decode::<Pin, Notes>(&stored.snapshot, &stored.journal).is_ok());
    }
}

#[test]
fn a_save_this_caf_cannot_read_is_left_byte_for_byte() {
    let folder = Folder::new("newer");
    let root = folder.path();
    let (work, chat) = (shelf("work"), ChatId::from_number(1));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    // A newer version's, with a record cut off at its end.
    let caf = file(root, "work", 1, "caf");
    let mut bytes = fs::read(&caf).expect("one");
    bytes[12..16].copy_from_slice(&(VERSION + 1).to_le_bytes());
    fs::write(&caf, bytes).expect("newer");
    let journal = file(root, "work", 1, "journal");
    let mut torn = fs::read(&journal).expect("one");
    torn.extend_from_slice(&[9, 9, 0, 0, 1, 2]);
    fs::write(&journal, torn).expect("torn");
    let files = || {
        let mut files: Vec<_> = (fs::read_dir(root.join("work")).expect("the shelf"))
            .map(|entry| {
                let path = entry.expect("a file").path();
                let bytes = fs::read(&path).expect("it reads");
                (path, bytes)
            })
            .collect();
        files.sort();
        files
    };
    let before = files();
    let loaded = carry_out::<Pin, Notes>(
        &mut store,
        StorageDirective::Load {
            ticket: Ticket(1),
            shelf: work.clone(),
            chat,
        },
    );
    let newer = CannotRestore::Newer {
        version: VERSION + 1,
    };
    assert!(
        matches!(&loaded, Some((_, StorageEvent::NotLoaded(why))) if *why == newer),
        "{loaded:?}"
    );
    let newer = StoreError::Newer {
        version: VERSION + 1,
    };
    let added = store.append(&work, chat, &head(1), &written_records()[0]);
    assert_eq!(format!("{added:?}"), format!("{:?}", Err::<(), _>(newer)));
    // Not held after: it let go of what it took to refuse, so another
    // finds it newer, not in use.
    store.release(&work, chat);
    let mut taker = FileStore::new(root);
    let added = taker.append(&work, chat, &head(1), &written_records()[0]);
    assert!(matches!(added, Err(StoreError::Newer { .. })), "{added:?}");
    let mut other = FileStore::new(root);
    let loaded = carry_out::<Pin, Notes>(
        &mut other,
        StorageDirective::Load {
            ticket: Ticket(2),
            shelf: work.clone(),
            chat,
        },
    );
    assert!(
        matches!(
            &loaded,
            Some((_, StorageEvent::NotLoaded(CannotRestore::Newer { .. })))
        ),
        "{loaded:?}"
    );
    // Squeezing leaves it too.
    age(root, 1);
    other.squeeze(Duration::ZERO).expect("it squeezes");
    store.release(&work, chat);
    assert_eq!(files(), before);
    // Marked by a newer store in a way this one doesn't know.
    let mut bytes = fs::read(&caf).expect("one");
    bytes[12..16].copy_from_slice(&VERSION.to_le_bytes());
    bytes[8] = 0x80;
    fs::write(&caf, bytes).expect("marked");
    age(root, 1);
    let before = files();
    let loaded = other.load(&work, chat);
    assert!(
        matches!(loaded, Err(StoreError::Newer { version }) if version > VERSION),
        "{loaded:?}"
    );
    other.squeeze(Duration::ZERO).expect("it squeezes");
    assert_eq!(files(), before);
}

/// Chat `id`'s files on `work`, made long since.
fn age(root: &Path, id: u64) {
    let long_ago = std::time::SystemTime::now() - Duration::from_secs(7200);
    for extension in ["caf", "journal"] {
        let opened = fs::File::options()
            .write(true)
            .open(file(root, "work", id, extension))
            .expect("it opens");
        opened.set_modified(long_ago).expect("it's aged");
    }
}

#[test]
fn a_snapshot_without_its_journal_is_damaged() {
    let folder = Folder::new("unpaired");
    let root = folder.path();
    let (work, chat) = (shelf("work"), ChatId::from_number(1));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    let (caf, journal) = (
        file(root, "work", 1, "caf"),
        file(root, "work", 1, "journal"),
    );
    let whole = fs::read(&journal).expect("one");
    let damaged = |store: &mut FileStore| {
        let loaded = store.load(&work, chat);
        matches!(loaded, Err(StoreError::Damaged(_)))
    };
    fs::remove_file(&journal).expect("removed");
    assert!(damaged(&mut store), "with none");
    let mut newer = whole.clone();
    newer[..8].copy_from_slice(&2_u64.to_le_bytes());
    fs::write(&journal, newer).expect("written");
    assert!(damaged(&mut store), "with a newer one");
    fs::write(&journal, &whole[..5]).expect("written");
    assert!(damaged(&mut store), "with one cut off");
    fs::write(&journal, &whole).expect("written");
    fs::write(&caf, [1, 0, 0]).expect("written");
    assert!(damaged(&mut store), "itself cut off");
    // Not held after: another can have it.
    assert!(damaged(&mut FileStore::new(root)));
}

#[test]
fn an_append_that_fails_adds_nothing() {
    let folder = Folder::new("failed");
    let root = folder.path();
    let (work, chat) = (shelf("work"), ChatId::from_number(1));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    let journal = file(root, "work", 1, "journal");
    let before = fs::read(&journal).expect("one");
    // Its header can't be written: the record isn't either, so trying
    // again doesn't add it twice.
    fs::create_dir(root.join(format!("work/{chat}.head.new"))).expect("in the way");
    let added = store.append(&work, chat, &head(1), &written_records()[0]);
    assert!(matches!(added, Err(StoreError::Io(_))));
    assert_eq!(fs::read(&journal).expect("one"), before);
    // A folder named as a header isn't listed.
    fs::create_dir(file(root, "work", 2, "head")).expect("a folder");
    assert_eq!(store.list(&work).expect("it lists").len(), 1);
}

#[test]
fn a_chat_deleted_is_gone_whole() {
    let folder = Folder::new("delete");
    let root = folder.path();
    let (work, chat) = (shelf("work"), ChatId::from_number(1));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    save(&mut store, &work, 2);
    store.load(&work, chat).expect("it loads");
    let left = root.join(format!("work/{chat}.caf.squeeze-1.new"));
    fs::write(&left, b"left by a crash").expect("a new file");
    store.delete(&work, chat).expect("it's deleted");
    assert!(!left.exists());
    for extension in ["head", "caf", "journal", "lock"] {
        assert!(!file(root, "work", 1, extension).exists(), "{extension}");
    }
    assert!(file(root, "work", 2, "caf").exists(), "only it");
    assert!(matches!(store.load(&work, chat), Err(StoreError::NotFound)));
    store.delete(&work, chat).expect("it's gone already");
    store.delete(&shelf("never"), chat).expect("so is this");
    // Unlocked, so another can make it again.
    let mut other = FileStore::new(root);
    other
        .snapshot(&work, chat, &head(1), &snap(1))
        .expect("it's written");
}

#[test]
fn a_shelf_moves_whole_to_a_new_one() {
    let folder = Folder::new("move");
    let root = folder.path();
    let (work, done) = (shelf("work"), shelf("done"));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    save(&mut store, &work, 2);
    let chat = ChatId::from_number(1);
    store.load(&work, chat).expect("it loads");
    store.move_shelf(&work, &done).expect("it moves");
    assert!(!root.join("work").exists());
    assert_eq!(store.list(&done).expect("it lists").len(), 2);
    // Its lock moved with it.
    let mut other = FileStore::new(root);
    assert!(in_use(other.load(&done, chat)));
    store
        .append(&done, chat, &head(1), &written_records()[0])
        .expect("it's added");
    other
        .load(&done, ChatId::from_number(2))
        .expect("that one's free");
    // Not a chat another program has open.
    assert!(in_use(other.move_shelf(&done, &work)));
    assert!(root.join("done").exists());
}

#[test]
fn a_shelf_moves_chat_by_chat_to_one_in_use() {
    let folder = Folder::new("merge");
    let root = folder.path();
    let (work, done) = (shelf("work"), shelf("done"));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    save(&mut store, &work, 2);
    save(&mut store, &done, 3);
    let chat = ChatId::from_number(1);
    store.load(&work, chat).expect("it loads");
    store.move_shelf(&work, &done).expect("it moves");
    assert!(!root.join("work").exists());
    let mut listed: Vec<ChatId> = (store.list(&done).expect("it lists").into_iter())
        .map(|(chat, _)| chat)
        .collect();
    listed.sort();
    assert_eq!(listed, [1, 2, 3].map(ChatId::from_number));
    let mut other = FileStore::new(root);
    assert!(in_use(other.load(&done, chat)), "its lock moved with it");
    store
        .append(&done, chat, &head(1), &written_records()[0])
        .expect("it's added");
    store.release(&done, chat);
    let stored = other.load(&done, chat).expect("it loads");
    assert_eq!(
        stored.journal,
        [written_records().concat(), written_records()[0].clone()].concat()
    );
    other.release(&done, chat);

    // A chat on both is a clash: what moved before it stays moved.
    save(&mut store, &work, 1);
    save(&mut store, &work, 0);
    assert!(in_use(store.move_shelf(&work, &done)));
    assert!(file(root, "done", 0, "caf").exists());
    assert!(file(root, "work", 1, "caf").exists());
}

#[test]
fn any_shelf_name_is_one_folder_of_its_own() {
    let folder = Folder::new("names");
    let root = folder.path();
    let long = "x".repeat(300);
    let longer = format!("{long}y");
    let hash = |name: &str| format!("{:x}", Sha256::digest(name.as_bytes()));
    let names = [
        ("work", "work".to_owned()),
        ("a/b c.d", "a%2Fb%20c%2Ed".to_owned()),
        ("..", "%2E%2E".to_owned()),
        ("~x", "%7Ex".to_owned()),
        ("Projekt ü", "%50rojekt%20%C3%BC".to_owned()),
        ("projekt ü", "projekt%20%C3%BC".to_owned()),
        (
            "/home/jack/projects/caf",
            "%2Fhome%2Fjack%2Fprojects%2Fcaf".to_owned(),
        ),
        (&long, format!("{}~{}", "x".repeat(136), hash(&long))),
        (&longer, format!("{}~{}", "x".repeat(136), hash(&longer))),
    ];
    let mut store = FileStore::new(root);
    for (id, (name, folder)) in (1..).zip(&names) {
        assert!(folder.len() <= 201);
        save(&mut store, &shelf(name), id);
        assert!(
            root.join(folder)
                .join(format!("{}.caf", ChatId::from_number(id)))
                .exists()
        );
        let listed = store.list(&shelf(name)).expect("it lists");
        assert_eq!(listed, vec![(ChatId::from_number(id), head(id))], "{name}");
    }
    assert_eq!(fs::read_dir(root).expect("the root").count(), names.len());
    let empty = store.snapshot(&shelf(""), ChatId::from_number(1), &head(1), &snap(1));
    assert!(matches!(empty, Err(StoreError::Io(_))));
}

#[test]
fn only_the_user_can_read_them() {
    let folder = Folder::new("modes");
    let root = folder.path().join("deep/chats");
    let mut store = FileStore::new(&root);
    save(&mut store, &shelf("work"), 1);
    let mode = |path: &Path| fs::metadata(path).expect("it's there").permissions().mode() & 0o777;
    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&root.join("work")), 0o700);
    for extension in ["head", "caf", "journal", "lock"] {
        assert_eq!(
            mode(&file(&root, "work", 1, extension)),
            0o600,
            "{extension}"
        );
    }
}

#[test]
fn links_are_never_followed() {
    let folder = Folder::new("links");
    let root = folder.path();
    let elsewhere = Folder::new("elsewhere");
    fs::create_dir_all(elsewhere.path()).expect("a folder");
    let mut store = FileStore::new(root);
    save(&mut store, &shelf("work"), 1);

    // A shelf's folder that's a link.
    std::os::unix::fs::symlink(elsewhere.path(), root.join("linked")).expect("a link");
    let wrote = store.snapshot(&shelf("linked"), ChatId::from_number(1), &head(1), &snap(1));
    assert!(matches!(wrote, Err(StoreError::Io(_))));
    assert!(matches!(
        store.list(&shelf("linked")),
        Err(StoreError::Io(_))
    ));
    assert_eq!(
        fs::read_dir(elsewhere.path()).expect("it's there").count(),
        0
    );

    // A chat's files that are links.
    let outside = elsewhere.path().join("outside");
    fs::write(&outside, snap(2)).expect("a file");
    std::os::unix::fs::symlink(&outside, file(root, "work", 2, "caf")).expect("a link");
    std::os::unix::fs::symlink(&outside, file(root, "work", 2, "head")).expect("a link");
    let loaded = store.load(&shelf("work"), ChatId::from_number(2));
    assert!(matches!(loaded, Err(StoreError::Io(_))));
    let listed = store.list(&shelf("work")).expect("it lists");
    assert_eq!(listed, vec![(ChatId::from_number(1), head(1))]);
    let wrote = store.snapshot(&shelf("work"), ChatId::from_number(2), &head(2), &snap(2));
    assert!(
        matches!(wrote, Err(StoreError::Io(_))),
        "a link isn't the store's"
    );
    assert_eq!(fs::read(&outside).expect("it's there"), snap(2));
}

#[test]
fn chats_left_alone_are_squeezed_once() {
    let folder = Folder::new("squeeze");
    let root = folder.path();
    let work = shelf("work");
    let mut store = FileStore::new(root);
    // Something big, with repeats far apart, that the strongest finds.
    let text = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/files.rs"))
        .expect("the source");
    let turns: Vec<_> = (0..40)
        .map(|n| {
            common::turn(
                n * 10,
                "Read it",
                vec![common::tool(n * 10 + 1, "files.rs", &text)],
            )
        })
        .collect();
    let big: Saved<Pin, Notes> = from(common::saved_with(1, turns));
    let chat = ChatId::from_number(1);
    let before = encode_snapshot(&big, Squeeze::Fast);
    store
        .snapshot(&work, chat, &head(1), &before)
        .expect("it's written");
    save(&mut store, &work, 2);
    let caf = file(root, "work", 1, "caf");
    let changed = fs::metadata(&caf)
        .expect("it's there")
        .modified()
        .expect("a time");

    // Held, or changed lately: left.
    let mut squeezer = FileStore::new(root);
    squeezer.squeeze(Duration::ZERO).expect("it squeezes");
    assert_eq!(
        fs::read(file(root, "work", 2, "caf")).expect("it's there")[8],
        1
    );
    assert_eq!(fs::read(&caf).expect("it's there")[8], 0);
    store.release(&work, chat);
    assert_eq!(
        squeezer
            .squeeze(Duration::from_secs(3600))
            .expect("it squeezes"),
        0
    );

    let saved = squeezer.squeeze(Duration::ZERO).expect("it squeezes");
    let after = fs::read(&caf).expect("it's there");
    assert_eq!(after[8], 1, "it's marked squeezed");
    assert_eq!(saved, (before.len() - (after.len() - 9)) as u64);
    assert!(saved > 0);
    assert_eq!(
        fs::metadata(&caf)
            .expect("it's there")
            .modified()
            .expect("a time"),
        changed
    );
    let stored = store.load(&work, chat).expect("it loads");
    assert_eq!(
        decode::<Pin, Notes>(&stored.snapshot, &stored.journal),
        Ok(big)
    );
    store.release(&work, chat);
    assert_eq!(
        squeezer.squeeze(Duration::ZERO).expect("it squeezes"),
        0,
        "once"
    );
}

#[test]
fn a_chat_left_without_a_header_is_listed() {
    let folder = Folder::new("headless");
    let root = folder.path();
    let mut store = FileStore::new(root);
    save(&mut store, &shelf("work"), 1);
    fs::remove_file(file(root, "work", 1, "head")).expect("removed");
    // With the header its snapshot has.
    let listed = store.list(&shelf("work")).expect("it lists");
    assert_eq!(listed, vec![(ChatId::from_number(1), head(1))]);
    // Or, if its snapshot can't be read, none.
    fs::write(file(root, "work", 1, "caf"), b"cut").expect("damaged");
    let listed = store.list(&shelf("work")).expect("it lists");
    assert_eq!(listed, vec![(ChatId::from_number(1), vec![])]);
}

#[test]
fn what_is_not_a_file_is_never_opened_as_one() {
    let folder = Folder::new("fifo");
    let root = folder.path();
    let mut store = FileStore::new(root);
    save(&mut store, &shelf("work"), 1);
    let fifo = |path: PathBuf| {
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &path,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .expect("a pipe");
    };
    fifo(file(root, "work", 2, "head"));
    fifo(file(root, "work", 3, "caf"));
    fifo(file(root, "work", 3, "lock"));
    let listed = store.list(&shelf("work")).expect("it lists, not waiting");
    assert_eq!(listed, vec![(ChatId::from_number(1), head(1))]);
    let loaded = store.load(&shelf("work"), ChatId::from_number(3));
    assert!(matches!(loaded, Err(StoreError::Io(_))), "{loaded:?}");
}

#[test]
fn squeezing_tidies_what_crashes_left() {
    let folder = Folder::new("tidy");
    let root = folder.path();
    let mut store = FileStore::new(root);
    save(&mut store, &shelf("work"), 1);
    // A lock of a chat with nothing else, held by nobody, and one held.
    let (orphan, held) = (file(root, "work", 2, "lock"), file(root, "work", 3, "lock"));
    fs::write(&orphan, b"").expect("a lock");
    let journal = file(root, "work", 2, "journal");
    fs::write(&journal, 1_u64.to_le_bytes()).expect("a journal");
    let mut holder = FileStore::new(root);
    holder
        .snapshot(&shelf("work"), ChatId::from_number(3), &head(3), &snap(3))
        .expect("it's written");
    fs::remove_file(file(root, "work", 3, "caf")).expect("half made");
    fs::remove_file(file(root, "work", 3, "journal")).expect("half made");
    fs::remove_file(file(root, "work", 3, "head")).expect("half made");
    // A squeeze's new file just made, maybe another's, at work.
    let new = root.join(format!("work/{}.caf.squeeze-1.new", ChatId::from_number(1)));
    fs::write(&new, b"working").expect("a new file");
    FileStore::new(root)
        .squeeze(Duration::from_secs(3600))
        .expect("it squeezes");
    assert!(!orphan.exists(), "given up");
    assert!(!journal.exists(), "given up before its snapshot");
    assert!(held.exists(), "held");
    assert!(new.exists(), "too new to be left by a crash");
    assert!(file(root, "work", 1, "lock").exists(), "a chat's");
}

#[test]
fn a_chat_added_to_lately_is_not_left_alone() {
    let folder = Folder::new("lately");
    let root = folder.path();
    let mut store = FileStore::new(root);
    save(&mut store, &shelf("work"), 1);
    let long_ago = std::time::SystemTime::now() - Duration::from_secs(7200);
    let age = |extension, when| {
        let opened = fs::File::options()
            .write(true)
            .open(file(root, "work", 1, extension))
            .expect("it opens");
        opened.set_modified(when).expect("it's aged");
    };
    age("caf", long_ago);
    let hour = Duration::from_secs(3600);
    assert_eq!(store.squeeze(hour).expect("it squeezes"), 0);
    assert_eq!(fs::read(file(root, "work", 1, "caf")).expect("one")[8], 0);
    age("journal", long_ago);
    store.squeeze(hour).expect("it squeezes");
    assert_eq!(fs::read(file(root, "work", 1, "caf")).expect("one")[8], 1);
}

#[test]
fn an_append_after_a_snapshot_that_failed_counts() {
    let folder = Folder::new("half");
    let root = folder.path();
    let (work, chat) = (shelf("work"), ChatId::from_number(1));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    store.load(&work, chat).expect("it loads");
    // Added to, so where its journal ends is known.
    let first = &written_records()[0];
    store
        .append(&work, chat, &head(1), first)
        .expect("it's added");
    // The new snapshot goes in, but its journal can't be emptied.
    let snapshot = encode_snapshot(&saved(1), Squeeze::Fast);
    let blocked = root.join(format!("work/{chat}.journal.new"));
    fs::create_dir(&blocked).expect("in the way");
    let wrote = store.snapshot(&work, chat, &head(1), &snapshot);
    assert!(matches!(wrote, Err(StoreError::Io(_))), "{wrote:?}");
    fs::remove_dir(&blocked).expect("out of the way");
    let record = &written_records()[2];
    store
        .append(&work, chat, &head(1), record)
        .expect("it's added");
    store.release(&work, chat);
    let stored = store.load(&work, chat).expect("it loads");
    assert_eq!(stored.journal, *record, "after the snapshot that went in");
    let mut expected = saved(1);
    expected.chat.state = caf_core::chat::ChatState::Closed;
    expected.data.title = "Docs too".into();
    expected.data.tags = vec![];
    assert_eq!(decode(&stored.snapshot, &stored.journal), Ok(expected));
}

/// Takes the lock that's `path`, as another program would.
fn flock(path: &Path, exclusive: bool) -> fs::File {
    let file = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .expect("it opens");
    let operation = if exclusive {
        rustix::fs::FlockOperation::NonBlockingLockExclusive
    } else {
        rustix::fs::FlockOperation::NonBlockingLockShared
    };
    rustix::fs::flock(&file, operation).expect("it's locked");
    file
}

#[test]
fn no_chat_comes_to_a_shelf_while_it_moves() {
    let folder = Folder::new("shelf-lock");
    let root = folder.path();
    let (work, done) = (shelf("work"), shelf("done"));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    let shelf_lock = root.join("work/.shelf.lock");
    // Moving: no new chat starts on it.
    let moving = flock(&shelf_lock, true);
    let wrote = store.snapshot(&work, ChatId::from_number(2), &head(2), &snap(2));
    assert!(in_use(wrote));
    assert!(in_use(store.load(&work, ChatId::from_number(1))));
    drop(moving);
    // A chat's lock being taken: the shelf doesn't move.
    let taking = flock(&shelf_lock, false);
    assert!(in_use(store.move_shelf(&work, &done)));
    drop(taking);
    store.move_shelf(&work, &done).expect("it moves");
}

#[test]
fn a_lock_taken_from_under_a_store_is_not_written_through() {
    let folder = Folder::new("taken");
    let root = folder.path();
    let (work, chat) = (shelf("work"), ChatId::from_number(1));
    let mut store = FileStore::new(root);
    save(&mut store, &work, 1);
    store.load(&work, chat).expect("it loads");
    let lock = file(root, "work", 1, "lock");
    fs::remove_file(&lock).expect("removed");
    let _other = flock(&lock, true);
    let journal = fs::read(file(root, "work", 1, "journal")).expect("one");
    let added = store.append(&work, chat, &head(1), &written_records()[0]);
    assert!(in_use(added));
    assert_eq!(
        fs::read(file(root, "work", 1, "journal")).expect("one"),
        journal
    );
}

#[test]
fn a_squeeze_never_writes_over_another_s_new_file() {
    let folder = Folder::new("exclusive");
    let root = folder.path();
    let mut store = FileStore::new(root);
    save(&mut store, &shelf("work"), 1);
    store.release(&shelf("work"), ChatId::from_number(1));
    age(root, 1);
    // Every name a squeeze in this program could take, taken.
    let chat = ChatId::from_number(1);
    for count in 0..1000 {
        let name = format!("work/{chat}.caf.squeeze-{}-{count}.new", std::process::id());
        fs::write(root.join(name), b"another's").expect("written");
    }
    let caf = fs::read(file(root, "work", 1, "caf")).expect("one");
    assert_eq!(store.squeeze(Duration::ZERO).expect("it squeezes"), 0);
    assert_eq!(fs::read(file(root, "work", 1, "caf")).expect("one"), caf);
    let name = format!("work/{chat}.caf.squeeze-{}-0.new", std::process::id());
    assert_eq!(fs::read(root.join(name)).expect("there"), b"another's");
}
