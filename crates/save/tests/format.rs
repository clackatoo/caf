//! The format: what's written reads back, and what's damaged says so.

mod common;

use caf_core::chat::ItemKind;
use caf_core::save::{CannotRestore, Entry, Header, Saved};
use caf_core::{Chat, NoAppItems};
use caf_save::{
    Squeeze, VERSION, decode, decode_header, encode_header, encode_record, encode_snapshot,
    recompress,
};
use common::{Moved, Notes, Pin, Project, from, records, small};
use serde_json::json;

/// `saved` with `entries` applied, as the journal should leave it.
fn applied(mut saved: Saved<Pin, Notes>, entries: &[Entry<Pin, Notes>]) -> Saved<Pin, Notes> {
    for entry in entries {
        match entry {
            Entry::Change(change) => saved.chat.apply(change).expect("it fits"),
            Entry::Session(session) => saved.session.clone_from(session),
            Entry::Data(data) => saved.data = data.clone(),
            _ => unreachable!(),
        }
    }
    saved
}

/// What a snapshot holds, uncompressed.
fn inside(snapshot: &[u8]) -> Vec<u8> {
    zstd::stream::decode_all(&snapshot[7..]).expect("one frame")
}

/// The item with this id.
fn item<A: caf_core::AppItem>(chat: &Chat<A>, id: u64) -> &ItemKind<A> {
    let found = (chat.turns.iter())
        .flat_map(|turn| &turn.items)
        .find(|item| item.id.0 == id);
    &found.expect("the item is there").kind
}

#[test]
fn a_snapshot_reads_back_as_it_was() {
    let saved = small(1);
    for squeeze in [Squeeze::Fast, Squeeze::Hardest] {
        let snapshot = encode_snapshot(&saved, squeeze);
        assert_eq!(&snapshot[..3], b"CAF");
        assert_eq!(snapshot[3..7], VERSION.to_le_bytes());
        assert_eq!(decode::<Pin, Notes>(&snapshot, &[]), Ok(saved.clone()));
    }
}

#[test]
fn a_header_reads_back_as_it_was() {
    let header: Header = from(common::header(7));
    assert_eq!(decode_header(&encode_header(&header)), Ok(header));
}

#[test]
fn a_journal_reads_back_applied_in_order() {
    let saved = small(1);
    let records = records();
    let journal: Vec<u8> = records
        .iter()
        .flat_map(|each| encode_record(each))
        .collect();
    let read = decode::<Pin, Notes>(&encode_snapshot(&saved, Squeeze::Fast), &journal);
    let read = read.expect("it reads");
    assert_eq!(read, applied(saved, &records.concat()));
    let place = |place: &str, zoom| {
        ItemKind::App(Pin {
            place: place.into(),
            zoom,
        })
    };
    assert_eq!(item(&read.chat, 5), &place("Kyiv", 9));
    assert_eq!(item(&read.chat, 12), &place("Lviv", 4));
    assert_eq!(item(&read.chat, 11), &ItemKind::Words("Sure, now.".into()));
    assert_eq!(read.session.expect("a session").0, "s2");
    assert_eq!(read.data.title, "Docs too");
}

#[test]
fn many_records_read_back() {
    let saved = small(1);
    let appended: Vec<Entry<Pin, Notes>> = from(json!([
        { "type": "Change", "value": { "type": "Appended", "value": { "item": 2, "text": " more" } } },
    ]));
    let record = encode_record(&appended);
    let journal = record.repeat(2000);
    let read = decode::<Pin, Notes>(&encode_snapshot(&saved, Squeeze::Fast), &journal);
    let words = format!("On it.{}", " more".repeat(2000));
    assert_eq!(
        item(&read.expect("it reads").chat, 2),
        &ItemKind::Words(words)
    );
}

#[test]
fn a_last_record_cut_off_anywhere_is_left_out() {
    let saved = small(1);
    let snapshot = encode_snapshot(&saved, Squeeze::Fast);
    let records = records();
    let encoded: Vec<Vec<u8>> = records.iter().map(|each| encode_record(each)).collect();
    let journal = encoded.concat();
    for cut in 0..=journal.len() {
        // The records wholly before the cut.
        let mut whole = 0;
        let mut length = 0;
        while whole < encoded.len() && length + encoded[whole].len() <= cut {
            length += encoded[whole].len();
            whole += 1;
        }
        let read = decode::<Pin, Notes>(&snapshot, &journal[..cut]);
        assert_eq!(
            read,
            Ok(applied(saved.clone(), &records[..whole].concat())),
            "cut at {cut}"
        );
    }
}

#[test]
fn damage_anywhere_but_a_cut_off_end_is_damage() {
    let saved = small(1);
    let snapshot = encode_snapshot(&saved, Squeeze::Fast);
    let encoded: Vec<Vec<u8>> = records().iter().map(|each| encode_record(each)).collect();
    let journal = encoded.concat();
    let damaged = |result: Result<Saved<Pin, Notes>, CannotRestore>| {
        matches!(result, Err(CannotRestore::Damaged(_)))
    };

    // A byte wrong in the middle of every record but the last, which is
    // one a crash cut off, and in the snapshot.
    let mut start = 0;
    for record in &encoded[..encoded.len() - 1] {
        let mut wrong = journal.clone();
        wrong[start + record.len() / 2] ^= 0x40;
        assert!(damaged(decode(&snapshot, &wrong)), "the record at {start}");
        start += record.len();
    }

    let mut wrong = snapshot.clone();
    let middle = wrong.len() / 2;
    wrong[middle] ^= 0x40;
    assert!(damaged(decode(&wrong, &journal)));

    // Not a save, a frame without a checksum, a frame with more after it.
    assert!(damaged(decode(b"PNG\x01\0\0\0", &[])));
    assert!(damaged(decode(b"CA", &[])));
    let unchecked = zstd::bulk::compress(&inside(&snapshot), 3).expect("it compresses");
    assert!(damaged(decode(&[&snapshot[..7], &unchecked].concat(), &[])));
    assert!(damaged(decode(&[&snapshot[..], b"\0"].concat(), &[])));

    // A record shorter than it says, followed by another.
    let mut short = encoded[0].clone();
    short.truncate(short.len() - 1);
    let length = u32::try_from(short.len() - 4).expect("small");
    short[..4].copy_from_slice(&length.to_le_bytes());
    assert!(damaged(decode(
        &snapshot,
        &[short, encoded[1].clone()].concat()
    )));

    // An empty record, or zeros, with a whole record after.
    let zeros = [&[0; 4][..], &encoded[1]].concat();
    assert!(damaged(decode(&snapshot, &zeros)));
    let zeros = [&encoded[0][..], &[0; 30], &encoded[1]].concat();
    assert!(damaged(decode(&snapshot, &zeros)));
    // A length gone wrong, longer than what's left, with whole records
    // after it: never dropped as a cut-off end.
    let mut long = journal.clone();
    long[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(damaged(decode(&snapshot, &long)));
}

#[test]
fn an_end_a_crash_left_with_nothing_whole_after_is_left_out() {
    let saved = small(1);
    let snapshot = encode_snapshot(&saved, Squeeze::Fast);
    let records = records();
    let first = encode_record(&records[0]);
    let expected = Ok(applied(saved, &records[0]));
    let mut garbage = encode_record(&records[1]);
    let middle = garbage.len() / 2;
    garbage[middle] ^= 0x40;
    // Zeros, then data that isn't a whole record; garbage, then zeros.
    for tail in [
        [&[0; 12][..], &[7, 1, 2, 3]].concat(),
        [&[0; 4][..], &garbage].concat(),
        [&garbage[..], &[0; 40]].concat(),
    ] {
        let read = decode::<Pin, Notes>(&snapshot, &[&first[..], &tail].concat());
        assert_eq!(read, expected);
    }
}

#[test]
fn a_last_record_a_crash_left_whole_in_length_but_not_in_bytes_is_left_out() {
    let saved = small(1);
    let snapshot = encode_snapshot(&saved, Squeeze::Fast);
    let records = records();
    let encoded: Vec<Vec<u8>> = records.iter().map(|each| encode_record(each)).collect();
    let before_last = encoded[..2].concat();
    let expected = Ok(applied(saved, &records[..2].concat()));
    let last = &encoded[2];
    // Garbage anywhere in it after its length. A length made shorter
    // leaves bytes after it, so it isn't the last: that's damage.
    for at in 4..last.len() {
        let mut wrong = last.clone();
        wrong[at] ^= 0x40;
        let read = decode::<Pin, Notes>(&snapshot, &[&before_last[..], &wrong].concat());
        assert_eq!(read, expected, "a byte wrong at {at}");
    }
    // Zeros, as long as it, or any length.
    for zeros in [1, 4, last.len(), 1000] {
        let read = decode::<Pin, Notes>(&snapshot, &[&before_last[..], &vec![0; zeros]].concat());
        assert_eq!(read, expected, "{zeros} zeros");
    }
}

#[test]
fn a_change_that_does_not_fit_is_damage() {
    let snapshot = encode_snapshot(&small(1), Squeeze::Fast);
    let stray: Vec<Entry<Pin, Notes>> = from(json!([
        { "type": "Change", "value": { "type": "Appended", "value": { "item": 99, "text": "!" } } },
    ]));
    let read = decode::<Pin, Notes>(&snapshot, &encode_record(&stray));
    assert!(matches!(read, Err(CannotRestore::Damaged(why)) if why.contains("NoItem")));
}

#[test]
fn a_newer_version_is_left_for_a_newer_caf() {
    let saved = small(1);
    let mut snapshot = encode_snapshot(&saved, Squeeze::Fast);
    snapshot[3..7].copy_from_slice(&(VERSION + 1).to_le_bytes());
    let newer = CannotRestore::Newer {
        version: VERSION + 1,
    };
    assert_eq!(decode::<Pin, Notes>(&snapshot, &[]), Err(newer.clone()));
    assert_eq!(recompress(&snapshot, Squeeze::Hardest), Err(newer.clone()));
    let mut header = encode_header(&saved.header);
    header[3..7].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert_eq!(decode_header(&header), Err(newer));
    header[3..7].copy_from_slice(&0_u32.to_le_bytes());
    assert!(matches!(
        decode_header(&header),
        Err(CannotRestore::Damaged(_))
    ));
}

#[test]
fn data_the_app_cannot_read_is_said_so() {
    let snapshot = encode_snapshot(&small(1), Squeeze::Fast);
    let read = decode::<Pin, Project>(&snapshot, &[]);
    assert!(matches!(read, Err(CannotRestore::AppData(why)) if why.contains("path")));

    // In the journal too, set by a later version.
    let later: Vec<Entry<Pin, Project>> =
        from(json!([{ "type": "Data", "value": { "path": "/work" } }]));
    let read = decode::<Pin, Notes>(&snapshot, &encode_record(&later));
    assert!(matches!(read, Err(CannotRestore::AppData(why)) if why.contains("title")));
}

#[test]
fn an_app_item_the_app_cannot_read_is_kept_unchanged() {
    let saved = small(1);
    let records = records();
    let journal: Vec<u8> = records
        .iter()
        .flat_map(|each| encode_record(each))
        .collect();
    let snapshot = encode_snapshot(&saved, Squeeze::Fast);

    // The app's new version can't read its old items: they're unknown.
    let read = decode::<Moved, Notes>(&snapshot, &journal).expect("it reads");
    let ItemKind::Unknown(unknown) = item(&read.chat, 5) else {
        panic!("{:?}", item(&read.chat, 5));
    };
    assert_eq!(unknown.what, "App");
    assert_eq!(unknown.raw, json!({ "place": "Kyiv", "zoom": 9 }));
    assert_eq!(unknown.summary.as_deref(), Some("Moved the map to Kyiv"));
    assert!(matches!(item(&read.chat, 12), ItemKind::Unknown(_)));

    // Saved again by it, and read by a version that can, they're back.
    let again = encode_snapshot(&read, Squeeze::Fast);
    assert_eq!(
        decode::<Pin, Notes>(&again, &[]),
        Ok(applied(saved.clone(), &records.concat()))
    );
    // As records too.
    let entries: Vec<Entry<Moved, Notes>> = from(json!([
        { "type": "Change", "value": { "type": "TurnAdded", "value": read.chat.turns[1] } },
    ]));
    let read = decode::<Pin, Notes>(&snapshot, &encode_record(&entries)).expect("it reads");
    let place = ItemKind::App(Pin {
        place: "Lviv".into(),
        zoom: 4,
    });
    assert_eq!(item(&read.chat, 12), &place);

    // A program that knows nothing of the app's items reads them as
    // unknown, with what they are.
    let read = decode::<NoAppItems, Notes>(&snapshot, &[]).expect("it reads");
    assert!(matches!(item(&read.chat, 5), ItemKind::Unknown(unknown) if unknown.summary.is_some()));
}

#[test]
fn an_unknown_item_from_the_agent_stays_unknown() {
    let read = decode::<Pin, Notes>(&encode_snapshot(&small(1), Squeeze::Fast), &[]);
    let read = read.expect("it reads");
    let ItemKind::Unknown(unknown) = item(&read.chat, 6) else {
        panic!("{:?}", item(&read.chat, 6));
    };
    assert_eq!(
        (unknown.what.as_str(), unknown.summary.as_deref()),
        ("plan", None)
    );
}

#[test]
fn an_agent_unknown_item_said_to_be_app_stays_the_agent_s() {
    // It reads as the app's type, but it's the agent's: it has no summary,
    // as an envelope always has.
    let unknown = json!({ "type": "Unknown", "value": {
        "what": "App", "raw": { "place": "Kyiv", "zoom": 9 }, "summary": null,
    }});
    let saved: Saved<Pin, Notes> = from(common::saved_with(
        1,
        vec![common::turn(1, "Hi", vec![common::item(2, unknown)])],
    ));
    let read = decode::<Pin, Notes>(&encode_snapshot(&saved, Squeeze::Fast), &[]);
    assert_eq!(read, Ok(saved));
}

/// An app's item JSON can't hold.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Odd {
    value: f64,
}

impl caf_core::AppItem for Odd {
    fn summary(&self) -> String {
        "Odd".into()
    }
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "doesn't read back from JSON")]
fn an_app_item_json_cannot_hold_is_found_in_debug_builds() {
    let odd = json!({ "type": "App", "value": { "value": 1.0 } });
    let mut saved: Saved<Odd, Notes> = from(common::saved_with(
        1,
        vec![common::turn(1, "Hi", vec![common::item(2, odd)])],
    ));
    saved.chat.turns[0].items[0].kind = ItemKind::App(Odd { value: f64::NAN });
    let _ = encode_snapshot(&saved, Squeeze::Fast);
}

#[test]
fn what_is_saved_has_its_field_names() {
    let saved = small(1);
    let snapshot = inside(&encode_snapshot(&saved, Squeeze::Fast));
    let has = |bytes: &[u8], name: &str| bytes.windows(name.len()).any(|at| at == name.as_bytes());
    for name in [
        "header", "chat", "turns", "items", "window", "session", "data", "title",
    ] {
        assert!(has(&snapshot, name), "{name}");
    }
    // An app item's envelope, and the item inside it.
    for name in [
        "what",
        "raw",
        "summary",
        "place",
        "zoom",
        "Moved the map to Kyiv",
    ] {
        assert!(has(&snapshot, name), "{name}");
    }
    let record = encode_record(&records()[1]);
    let record = zstd::stream::decode_all(&record[4..]).expect("one frame");
    for name in ["ItemAdded", "turn", "item", "place", "Session"] {
        assert!(has(&record, name), "{name}");
    }
    let header = inside(&encode_header(&saved.header));
    for name in [
        "agent", "profile", "cwd", "made", "changed", "label", "first", "closed",
    ] {
        assert!(has(&header, name), "{name}");
    }
}

#[test]
fn compressing_again_keeps_what_it_holds() {
    let snapshot = encode_snapshot(&small(1), Squeeze::Fast);
    let squeezed = recompress(&snapshot, Squeeze::Hardest).expect("it squeezes");
    assert_eq!(squeezed[..7], snapshot[..7]);
    assert_eq!(inside(&squeezed), inside(&snapshot));
    assert_eq!(decode::<Pin, Notes>(&squeezed, &[]), Ok(small(1)));
    let damaged = recompress(&snapshot[..snapshot.len() - 1], Squeeze::Hardest);
    assert!(matches!(damaged, Err(CannotRestore::Damaged(_))));
}

/// The app's item as an older version of it had it: without `zoom`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct OldPin {
    place: String,
}

impl caf_core::AppItem for OldPin {
    fn summary(&self) -> String {
        self.place.clone()
    }
}

/// The app's item as a newer version has it: with `tags` too.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct NewerPin {
    place: String,
    zoom: u8,
    #[serde(default)]
    tags: Vec<String>,
}

impl caf_core::AppItem for NewerPin {
    fn summary(&self) -> String {
        self.place.clone()
    }
}

/// The app's data as an older version had it: without `tags`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct OldNotes {
    title: String,
}

impl caf_core::AppData for OldNotes {
    fn label(&self) -> Option<String> {
        None
    }
}

/// The app's data as a newer version has it: with `pinned` too.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct NewerNotes {
    title: String,
    tags: Vec<String>,
    #[serde(default)]
    pinned: bool,
}

impl caf_core::AppData for NewerNotes {
    fn label(&self) -> Option<String> {
        None
    }
}

#[test]
fn an_app_item_an_older_app_would_lose_some_of_stays_unknown() {
    let snapshot = encode_snapshot(&small(1), Squeeze::Fast);
    let old = decode::<OldPin, Notes>(&snapshot, &[]).expect("it reads");
    let ItemKind::Unknown(unknown) = item(&old.chat, 5) else {
        panic!("{:?}", item(&old.chat, 5));
    };
    assert_eq!(unknown.raw, json!({ "place": "Kyiv", "zoom": 9 }));
    // Saved again by it, it's whole for the version that wrote it.
    let again = encode_snapshot(&old, Squeeze::Fast);
    assert_eq!(decode::<Pin, Notes>(&again, &[]), Ok(small(1)));
    // A newer version, which adds a field, reads it.
    let newer = decode::<NewerPin, Notes>(&snapshot, &[]).expect("it reads");
    let place = NewerPin {
        place: "Kyiv".into(),
        zoom: 9,
        tags: vec![],
    };
    assert_eq!(item(&newer.chat, 5), &ItemKind::App(place));
}

#[test]
fn data_an_older_app_would_lose_some_of_is_not_restored() {
    let snapshot = encode_snapshot(&small(1), Squeeze::Fast);
    let lost = CannotRestore::AppData("this version of the app can't keep all of it".into());
    assert_eq!(decode::<Pin, OldNotes>(&snapshot, &[]), Err(lost.clone()));
    let newer = decode::<Pin, NewerNotes>(&snapshot, &[]).expect("a newer app reads it");
    assert_eq!(newer.data.tags, ["work"]);

    // What counts is the data last set: in the journal, it's checked; and
    // once set again whole, the snapshot's is gone.
    let old: Saved<Pin, OldNotes> = from(json!({
        "header": common::header(1),
        "chat": serde_json::to_value(&small(1).chat).expect("JSON"),
        "session": null,
        "data": { "title": "Old" },
    }));
    let snapshot = encode_snapshot(&old, Squeeze::Fast);
    let set: Vec<Entry<Pin, Notes>> =
        from(json!([{ "type": "Data", "value": { "title": "New", "tags": ["a"] } }]));
    let record = encode_record(&set);
    assert_eq!(decode::<Pin, OldNotes>(&snapshot, &record), Err(lost));
    let again: Vec<Entry<Pin, OldNotes>> =
        from(json!([{ "type": "Data", "value": { "title": "Again" } }]));
    let journal = [record, encode_record(&again)].concat();
    let read = decode::<Pin, OldNotes>(&snapshot, &journal).expect("it reads");
    assert_eq!(read.data.title, "Again");
}
