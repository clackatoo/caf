//! The format's bytes: the version in front, zstd's frames, MessagePack
//! inside them, and the envelope app items are saved in, as the crate's
//! docs say.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Cursor, Read};

use caf_core::chat::{Item, ItemKind, Turn, Unknown};
use caf_core::save::{CannotRestore, Entry, Header, Saved};
use caf_core::{AppData, AppItem, Change};
use serde::de::{DeserializeOwned, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{Squeeze, VERSION};

/// What a header or snapshot starts with.
const MAGIC: &[u8; 3] = b"CAF";
/// What a zstd frame starts with.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
/// The bit of a zstd frame's descriptor that says it carries a checksum.
const ZSTD_CHECKSUM: u8 = 0x04;
/// How far back zstd's strongest looks for repeats, as a power of two:
/// 128 MiB, the most any decoder reads without being told it may.
const HARDEST_WINDOW: u32 = 27;
/// What an app item's envelope says it is.
const APP: &str = "App";

/// Writes a header.
pub(crate) fn header(header: &Header) -> Result<Vec<u8>, String> {
    Ok(seal(VERSION, &named(header)?, Squeeze::Fast))
}

/// Reads a header.
pub(crate) fn read_header(bytes: &[u8]) -> Result<Header, CannotRestore> {
    let (_, frame) = open(bytes)?;
    let inside = unpack(frame).map_err(|why| damaged("its header", &why))?;
    parse(&inside).map_err(|why| damaged("its header", &why))
}

/// Writes a snapshot, enveloping its app items: in place if it's owned,
/// in a copy if not and it has any.
pub(crate) fn snapshot<A: AppItem, D: AppData>(
    mut saved: Cow<'_, Saved<A, D>>,
    squeeze: Squeeze,
) -> Result<Vec<u8>, String> {
    if saved.chat.turns.iter().any(has_app_items) {
        for turn in &mut saved.to_mut().chat.turns {
            envelop_turn(turn)?;
        }
    }
    Ok(seal(VERSION, &named(&*saved)?, squeeze))
}

/// Writes a journal record, enveloping its app items as [`snapshot`]
/// does.
pub(crate) fn record<A: AppItem, D: AppData>(
    mut entries: Cow<'_, [Entry<A, D>]>,
) -> Result<Vec<u8>, String> {
    if entries.iter().any(entry_has_app_items) {
        for entry in entries.to_mut() {
            envelop_entry(entry)?;
        }
    }
    let frame = pack(&named(&*entries)?, Squeeze::Fast);
    let length = u32::try_from(frame.len()).map_err(|_| "a record past 4 GiB".to_owned())?;
    let mut record = Vec::with_capacity(4 + frame.len());
    record.extend_from_slice(&length.to_le_bytes());
    record.extend_from_slice(&frame);
    Ok(record)
}

/// Reads a chat: its snapshot, then its journal's records, in order.
pub(crate) fn read<A: AppItem, D: AppData>(
    snapshot: &[u8],
    journal: &[u8],
) -> Result<Saved<A, D>, CannotRestore> {
    // Records are written in the version of the snapshot before them: only
    // one so far.
    let (_, frame) = open(snapshot)?;
    let inside = unpack(frame).map_err(|why| damaged("its snapshot", &why))?;
    let mut saved: Saved<A, D> =
        parse_app::<_, Saved<Anything, Anything>>(&inside, "its snapshot")?;
    // The data as it was saved, last set: to check the app's type keeps
    // all of it.
    let mut data = parse::<DataOf>(&inside)
        .map_err(|why| damaged("its snapshot", &why))?
        .data;
    let (records, _) = records(journal)?;
    for (no, inside) in records.into_iter().enumerate() {
        let what = format!("its journal's record {}", no + 1);
        let entries: Vec<Entry<A, D>> =
            parse_app::<_, Vec<Entry<Anything, Anything>>>(&inside, &what)?;
        let raw: Vec<RawEntry> = parse(&inside).map_err(|why| damaged(&what, &why))?;
        if let Some(last) = raw.into_iter().rev().find(|entry| entry.kind == "Data") {
            data = last.value;
        }
        for entry in entries {
            match entry {
                Entry::Change(change) => saved
                    .chat
                    .apply(&change)
                    .map_err(|mismatch| damaged(&what, &format!("{mismatch:?}")))?,
                Entry::Session(session) => saved.session = session,
                Entry::Data(data) => saved.data = data,
                _ => return Err(damaged(&what, "an entry this CAF can't apply")),
            }
        }
    }
    let kept = named(&saved.data).and_then(|bytes| parse::<Keys>(&bytes));
    if !kept.is_ok_and(|kept| data.kept_in(&kept)) {
        return Err(CannotRestore::AppData(
            "this version of the app can't keep all of it".into(),
        ));
    }
    for turn in &mut saved.chat.turns {
        for item in &mut turn.items {
            open_envelope(item);
        }
    }
    Ok(saved)
}

/// The header a snapshot holds, written as a header: what lists show of
/// a chat whose own header a crash took.
pub(crate) fn snapshot_header(snapshot: &[u8]) -> Result<Vec<u8>, CannotRestore> {
    /// A snapshot, but only its header.
    #[derive(Deserialize)]
    struct HeaderOf {
        header: Header,
    }
    let (_, frame) = open(snapshot)?;
    let inside = unpack(frame).map_err(|why| damaged("its snapshot", &why))?;
    let of: HeaderOf = parse(&inside).map_err(|why| damaged("its snapshot", &why))?;
    header(&of.header).map_err(|why| damaged("its header", &why))
}

/// Compresses a header or snapshot again, keeping what it holds.
pub(crate) fn squeeze(snapshot: &[u8], squeeze: Squeeze) -> Result<Vec<u8>, CannotRestore> {
    let (version, frame) = open(snapshot)?;
    let inside = unpack(frame).map_err(|why| damaged("its snapshot", &why))?;
    Ok(seal(version, &inside, squeeze))
}

/// The version of a snapshot, checked: what the store looks at before it
/// changes anything of a chat's.
pub(crate) fn version(snapshot: &[u8]) -> Result<u32, CannotRestore> {
    open(snapshot).map(|(version, _)| version)
}

/// How many bytes of `journal` are records that count: what follows is a
/// last record a crash cut off. The store appends after them.
pub(crate) fn kept(journal: &[u8]) -> Result<usize, CannotRestore> {
    records(journal).map(|(_, kept)| kept)
}

/// What a journal's records hold, in order, and how many of its bytes
/// they take, leaving out an end a crash cut off.
///
/// Each record is synced before the next is written, so a crash can cut
/// off only the last: short of its length, or, on a file system that
/// writes a file's size before its data, as long as it should be but
/// zeros or garbage. So the first place a record can't be read is the
/// end, cut off, unless a whole record that reads can be found after it:
/// then it's damage, such as a length gone wrong in the middle, and
/// nothing after it is ever dropped.
fn records(journal: &[u8]) -> Result<(Vec<Vec<u8>>, usize), CannotRestore> {
    let mut records = Vec::new();
    let mut at = 0;
    while at < journal.len() {
        match record_at(journal, at) {
            Ok((inside, next)) => {
                records.push(inside);
                at = next;
            }
            Err(why) => {
                if let Some(whole) = whole_record_after(journal, at) {
                    let what = format!("its journal's record {}", records.len() + 1);
                    let why = format!("{why}, with a whole record after it, at byte {whole}");
                    return Err(damaged(&what, &why));
                }
                break;
            }
        }
    }
    Ok((records, at))
}

/// The record at `at`: what it holds, and where the next starts.
fn record_at(journal: &[u8], at: usize) -> Result<(Vec<u8>, usize), String> {
    let Some((length, rest)) = journal[at..].split_first_chunk::<4>() else {
        return Err("it's cut off".into());
    };
    let length = u32::from_le_bytes(*length) as usize;
    if length == 0 {
        return Err("it's empty".into());
    }
    let Some(frame) = rest.get(..length) else {
        return Err("it's cut off".into());
    };
    unpack(frame).map(|inside| (inside, at + 4 + length))
}

/// Where a whole record that reads starts after a record at `bad` that
/// doesn't, if anywhere: each place a zstd frame starts is tried, with
/// the length before it.
fn whole_record_after(journal: &[u8], bad: usize) -> Option<usize> {
    let mut from = bad + 5;
    while let Some(found) = journal
        .get(from..)?
        .windows(4)
        .position(|at| at == ZSTD_MAGIC)
    {
        let start = from + found - 4;
        if record_at(journal, start).is_ok() {
            return Some(start);
        }
        from += found + 1;
    }
    None
}

/// `value` as MessagePack, with field names.
fn named<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, String> {
    rmp_serde::to_vec_named(value).map_err(|error| error.to_string())
}

/// The format's version, then `inside`, compressed.
fn seal(version: u32, inside: &[u8], squeeze: Squeeze) -> Vec<u8> {
    let frame = pack(inside, squeeze);
    let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + frame.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&frame);
    bytes
}

/// A header's or snapshot's version, checked, and its frame.
fn open(bytes: &[u8]) -> Result<(u32, &[u8]), CannotRestore> {
    let Some((magic, rest)) = bytes.split_first_chunk::<3>() else {
        return Err(CannotRestore::Damaged("it isn't a CAF save".into()));
    };
    let Some((version, frame)) = rest.split_first_chunk::<4>() else {
        return Err(CannotRestore::Damaged("it isn't a CAF save".into()));
    };
    if magic != MAGIC {
        return Err(CannotRestore::Damaged("it isn't a CAF save".into()));
    }
    match u32::from_le_bytes(*version) {
        0 => Err(CannotRestore::Damaged("it says it's of version 0".into())),
        version if version > VERSION => Err(CannotRestore::Newer { version }),
        version => Ok((version, frame)),
    }
}

/// One zstd frame of `inside`, with its checksum.
fn pack(inside: &[u8], squeeze: Squeeze) -> Vec<u8> {
    use zstd::zstd_safe::CParameter;
    let mut compressor = zstd::bulk::Compressor::new(0).expect("zstd starts");
    let mut set = |parameter| compressor.set_parameter(parameter).expect("zstd takes it");
    set(CParameter::ChecksumFlag(true));
    match squeeze {
        Squeeze::Fast => set(CParameter::CompressionLevel(3)),
        Squeeze::Hardest => {
            set(CParameter::CompressionLevel(zstd::zstd_safe::max_c_level()));
            set(CParameter::EnableLongDistanceMatching(true));
            set(CParameter::WindowLog(HARDEST_WINDOW));
        }
    }
    compressor.compress(inside).expect("zstd compresses")
}

/// What one zstd frame holds, checked against its checksum.
fn unpack(frame: &[u8]) -> Result<Vec<u8>, String> {
    if frame.len() < 5 {
        return Err("it's cut off".into());
    }
    if frame[..4] != ZSTD_MAGIC {
        return Err("it isn't zstd".into());
    }
    if frame[4] & ZSTD_CHECKSUM == 0 {
        return Err("it has no checksum".into());
    }
    match zstd::zstd_safe::find_frame_compressed_size(frame) {
        Ok(length) if length == frame.len() => {}
        Ok(_) => return Err("there's more after it".into()),
        Err(_) => return Err("it's cut off".into()),
    }
    let size = zstd::zstd_safe::get_frame_content_size(frame)
        .ok()
        .flatten();
    // Never trusting a damaged size with more than a little memory up front.
    let mut inside = Vec::with_capacity(size.unwrap_or(0).min(1 << 26) as usize);
    zstd::stream::read::Decoder::with_buffer(frame)
        .map_err(|error| error.to_string())?
        .single_frame()
        .read_to_end(&mut inside)
        .map_err(|error| error.to_string())?;
    Ok(inside)
}

/// `bytes` as a `T`, all of them.
fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let mut reader = rmp_serde::Deserializer::new(Cursor::new(bytes));
    let value = T::deserialize(&mut reader).map_err(|error| error.to_string())?;
    if reader.position() != bytes.len() as u64 {
        return Err("there's more after it".into());
    }
    Ok(value)
}

/// `bytes` as a `T`, the app's types in it included. If they're what
/// fails, as when `Like`, the same with app types that read anything,
/// reads it, the app's data can't be read; otherwise the save's damaged.
fn parse_app<T: DeserializeOwned, Like: DeserializeOwned>(
    bytes: &[u8],
    what: &str,
) -> Result<T, CannotRestore> {
    parse(bytes).map_err(|why| match parse::<Like>(bytes) {
        Ok(_) => CannotRestore::AppData(why),
        Err(_) => damaged(what, &why),
    })
}

/// A save that's damaged, saying where.
fn damaged(what: &str, why: &str) -> CannotRestore {
    CannotRestore::Damaged(format!("{what} is damaged: {why}"))
}

/// An app's type that reads anything: what tells a save the app can't
/// read from one that's damaged.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct Anything;

impl<'de> Deserialize<'de> for Anything {
    fn deserialize<De: Deserializer<'de>>(deserializer: De) -> Result<Self, De::Error> {
        IgnoredAny::deserialize(deserializer).map(|_| Self)
    }
}

impl AppItem for Anything {
    fn summary(&self) -> String {
        String::new()
    }
}

impl AppData for Anything {
    fn label(&self) -> Option<String> {
        None
    }
}

/// Whether a turn holds any app items.
fn has_app_items<A: AppItem>(turn: &Turn<A>) -> bool {
    (turn.items.iter()).any(|item| matches!(item.kind, ItemKind::App(_)))
}

/// Whether a journal entry adds any app items.
fn entry_has_app_items<A: AppItem, D: AppData>(entry: &Entry<A, D>) -> bool {
    match entry {
        Entry::Change(Change::TurnAdded(turn) | Change::TurnInserted { turn, .. }) => {
            has_app_items(turn)
        }
        Entry::Change(Change::ItemAdded { item, .. }) => matches!(item.kind, ItemKind::App(_)),
        _ => false,
    }
}

/// Envelopes a journal entry's app items.
fn envelop_entry<A: AppItem, D: AppData>(entry: &mut Entry<A, D>) -> Result<(), String> {
    match entry {
        Entry::Change(Change::TurnAdded(turn) | Change::TurnInserted { turn, .. }) => {
            envelop_turn(turn)
        }
        Entry::Change(Change::ItemAdded { item, .. }) => envelop(item),
        _ => Ok(()),
    }
}

/// Envelopes a turn's app items.
fn envelop_turn<A: AppItem>(turn: &mut Turn<A>) -> Result<(), String> {
    turn.items.iter_mut().try_for_each(envelop)
}

/// An app item, as it's saved: in an [`Unknown`] item's shape.
fn envelop<A: AppItem>(item: &mut Item<A>) -> Result<(), String> {
    let ItemKind::App(app) = &item.kind else {
        return Ok(());
    };
    let raw = serde_json::to_value(app)
        .map_err(|error| format!("an app item can't be written as JSON: {error}"))?;
    // What JSON can't hold comes back as something else, with no error:
    // a float that isn't finite as null, say. Writing is infallible, so
    // only a debug build can say so, here, where the app's tests find it.
    debug_assert!(
        A::deserialize(&raw).is_ok_and(|back| back == *app),
        "an app item doesn't read back from JSON as it was, as AppItem's rules say it must: {app:?}"
    );
    let envelope = serde_json::json!({ "what": APP, "raw": raw, "summary": app.summary() });
    let unknown = Unknown::deserialize(envelope).expect("an envelope is an unknown item");
    item.kind = ItemKind::Unknown(unknown);
    Ok(())
}

/// An app item saved in its envelope, as the app item again, if the
/// app's type reads it and keeps all of it: an older version of the app
/// that would drop what a newer one added leaves it unknown, and so
/// unchanged.
fn open_envelope<A: AppItem>(item: &mut Item<A>) {
    // An envelope always has a summary, and an agent's unknown item never
    // has: that, not `what`, which is the agent's to say, tells them apart.
    if let ItemKind::Unknown(unknown) = &item.kind
        && unknown.what == APP
        && unknown.summary.is_some()
        && let Ok(app) = A::deserialize(&unknown.raw)
        && let Ok(saved) = Keys::deserialize(&unknown.raw)
        && serde_json::to_value(&app)
            .ok()
            .and_then(|back| Keys::deserialize(back).ok())
            .is_some_and(|back| saved.kept_in(&back))
    {
        item.kind = ItemKind::App(app);
    }
}

/// A snapshot, but only its data.
#[derive(Deserialize)]
struct DataOf {
    data: Keys,
}

/// A journal entry, but only its kind, and its keys.
#[derive(Deserialize)]
struct RawEntry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    value: Keys,
}

/// The keys of what's saved, however deep, and nothing else: what tells
/// whether the app's type keeps all of it.
#[derive(Debug, Default)]
enum Keys {
    /// Anything but a map or list.
    #[default]
    Leaf,
    /// A map, by its keys, written as text.
    Map(BTreeMap<String, Keys>),
    /// A list.
    List(Vec<Keys>),
}

impl Keys {
    /// Whether every key here is in `back`, as deep as it goes: through
    /// maps, and lists as long as each other. Keys `back` has that this
    /// hasn't, as a newer version adds, are fine.
    fn kept_in(&self, back: &Keys) -> bool {
        match (self, back) {
            (Keys::Map(saved), Keys::Map(back)) => (saved.iter())
                .all(|(key, saved)| back.get(key).is_some_and(|back| saved.kept_in(back))),
            (Keys::Map(saved), _) => saved.is_empty(),
            (Keys::List(saved), Keys::List(back)) if saved.len() == back.len() => saved
                .iter()
                .zip(back)
                .all(|(saved, back)| saved.kept_in(back)),
            _ => true,
        }
    }
}

impl<'de> Deserialize<'de> for Keys {
    fn deserialize<De: Deserializer<'de>>(deserializer: De) -> Result<Self, De::Error> {
        deserializer.deserialize_any(KeysVisitor)
    }
}

/// Reads [`Keys`] from anything.
struct KeysVisitor;

impl<'de> Visitor<'de> for KeysVisitor {
    type Value = Keys;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("anything")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Keys, E> {
        Ok(Keys::Leaf)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Keys, E> {
        Ok(Keys::Leaf)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Keys, E> {
        Ok(Keys::Leaf)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Keys, E> {
        Ok(Keys::Leaf)
    }

    fn visit_str<E>(self, _: &str) -> Result<Keys, E> {
        Ok(Keys::Leaf)
    }

    fn visit_bytes<E>(self, _: &[u8]) -> Result<Keys, E> {
        Ok(Keys::Leaf)
    }

    fn visit_none<E>(self) -> Result<Keys, E> {
        Ok(Keys::Leaf)
    }

    fn visit_unit<E>(self) -> Result<Keys, E> {
        Ok(Keys::Leaf)
    }

    fn visit_some<De: Deserializer<'de>>(self, inner: De) -> Result<Keys, De::Error> {
        Keys::deserialize(inner)
    }

    fn visit_newtype_struct<De: Deserializer<'de>>(self, inner: De) -> Result<Keys, De::Error> {
        Keys::deserialize(inner)
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Keys, S::Error> {
        let mut list = Vec::new();
        while let Some(each) = seq.next_element()? {
            list.push(each);
        }
        Ok(Keys::List(list))
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Keys, M::Error> {
        let mut keys = BTreeMap::new();
        while let Some((key, value)) = map.next_entry::<KeyName, Keys>()? {
            keys.insert(key.0, value);
        }
        Ok(Keys::Map(keys))
    }
}

/// A map's key, written as text, whatever it was.
struct KeyName(String);

impl<'de> Deserialize<'de> for KeyName {
    fn deserialize<De: Deserializer<'de>>(deserializer: De) -> Result<Self, De::Error> {
        /// Reads a key.
        struct Name;
        impl Visitor<'_> for Name {
            type Value = KeyName;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a key")
            }
            fn visit_str<E>(self, key: &str) -> Result<KeyName, E> {
                Ok(KeyName(key.to_owned()))
            }
            fn visit_bytes<E>(self, key: &[u8]) -> Result<KeyName, E> {
                Ok(KeyName(format!("{key:?}")))
            }
            fn visit_i64<E>(self, key: i64) -> Result<KeyName, E> {
                Ok(KeyName(key.to_string()))
            }
            fn visit_u64<E>(self, key: u64) -> Result<KeyName, E> {
                Ok(KeyName(key.to_string()))
            }
            fn visit_f64<E>(self, key: f64) -> Result<KeyName, E> {
                Ok(KeyName(key.to_string()))
            }
            fn visit_bool<E>(self, key: bool) -> Result<KeyName, E> {
                Ok(KeyName(key.to_string()))
            }
        }
        deserializer.deserialize_any(Name)
    }
}
