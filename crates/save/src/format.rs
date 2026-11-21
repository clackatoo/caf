//! The format's bytes: the version in front, zstd's frames, MessagePack
//! inside them, and the envelope app items are saved in, as the crate's
//! docs say.

use std::borrow::Cow;
use std::io::{Cursor, Read};

use caf_core::chat::{Item, ItemKind, Turn, Unknown};
use caf_core::save::{CannotRestore, Entry, Header, Saved};
use caf_core::{AppData, AppItem, Change};
use serde::de::{DeserializeOwned, IgnoredAny};
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
    for (no, frame) in records(journal)?.into_iter().enumerate() {
        let what = format!("its journal's record {}", no + 1);
        let inside = unpack(frame).map_err(|why| damaged(&what, &why))?;
        let entries: Vec<Entry<A, D>> =
            parse_app::<_, Vec<Entry<Anything, Anything>>>(&inside, &what)?;
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
    for turn in &mut saved.chat.turns {
        for item in &mut turn.items {
            open_envelope(item);
        }
    }
    Ok(saved)
}

/// Compresses a header or snapshot again, keeping what it holds.
pub(crate) fn squeeze(snapshot: &[u8], squeeze: Squeeze) -> Result<Vec<u8>, CannotRestore> {
    let (version, frame) = open(snapshot)?;
    let inside = unpack(frame).map_err(|why| damaged("its snapshot", &why))?;
    Ok(seal(version, &inside, squeeze))
}

/// How many bytes of `journal` are whole records: those after are one
/// cut off as it was written. Only its framing is read.
pub(crate) fn whole_records(journal: &[u8]) -> usize {
    let mut at = 0;
    while let Some(length) = journal.get(at..at + 4) {
        let length = u32::from_le_bytes(length.try_into().expect("four bytes")) as usize;
        if journal.len() - at - 4 < length {
            break;
        }
        at += 4 + length;
    }
    at
}

/// A journal's records' frames, in order, leaving out a last one cut off.
fn records(journal: &[u8]) -> Result<Vec<&[u8]>, CannotRestore> {
    let whole = whole_records(journal);
    let mut frames = Vec::new();
    let mut at = 0;
    while at < whole {
        let length = u32::from_le_bytes(journal[at..at + 4].try_into().expect("four bytes"));
        if length == 0 {
            let what = format!("its journal's record {}", frames.len() + 1);
            return Err(damaged(&what, "it's empty"));
        }
        frames.push(&journal[at + 4..at + 4 + length as usize]);
        at += 4 + length as usize;
    }
    Ok(frames)
}

/// `bytes` as MessagePack, with field names.
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
    let envelope = serde_json::json!({ "what": APP, "raw": raw, "summary": app.summary() });
    let unknown = Unknown::deserialize(envelope).expect("an envelope is an unknown item");
    item.kind = ItemKind::Unknown(unknown);
    Ok(())
}

/// An app item saved in its envelope, as the app item again, if the
/// app's type reads it.
fn open_envelope<A: AppItem>(item: &mut Item<A>) {
    if let ItemKind::Unknown(unknown) = &item.kind
        && unknown.what == APP
        && unknown.summary.is_some()
        && let Ok(app) = A::deserialize(&unknown.raw)
    {
        item.kind = ItemKind::App(app);
    }
}
