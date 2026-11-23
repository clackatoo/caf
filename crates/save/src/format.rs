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
    // Records are written in the version of the snapshot before them, so
    // they're read in its version too: one older than this CAF's is
    // migrated, as it's read, to this version's shape.
    let (version, frame) = open(snapshot)?;
    let inside = unpack(frame).map_err(|why| damaged("its snapshot", &why))?;
    let inside =
        migrate(version, inside, Root::Snapshot).map_err(|why| damaged("its snapshot", &why))?;
    let mut saved: Saved<A, D> =
        parse_app::<_, Saved<Anything, Anything>>(&inside, "its snapshot")?;
    // The data as it was saved, last set, and the app's items in tool
    // calls as the journal set them: to check the app's types keep all of
    // them.
    let mut data = parse::<DataOf>(&inside)
        .map_err(|why| damaged("its snapshot", &why))?
        .data;
    let mut tools_set = Vec::new();
    let (records, _) = records(journal)?;
    for (no, inside) in records.into_iter().enumerate() {
        let what = format!("its journal's record {}", no + 1);
        let inside = migrate(version, inside, Root::Record).map_err(|why| damaged(&what, &why))?;
        let entries: Vec<Entry<A, D>> =
            parse_app::<_, Vec<Entry<Anything, Anything>>>(&inside, &what)?;
        let raw: Vec<RawEntry> = parse(&inside).map_err(|why| damaged(&what, &why))?;
        for entry in raw {
            match entry.kind.as_str() {
                "Data" => data = entry.value,
                "Change" => tool_apps_changed(&entry.value, &mut tools_set),
                _ => {}
            }
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
    if !keeps(&data, &saved.data) {
        return Err(CannotRestore::AppData(
            "this version of the app can't keep all of it".into(),
        ));
    }
    let tools: Vec<(u64, &A)> = (saved.chat.turns.iter())
        .flat_map(|turn| &turn.items)
        .filter_map(|item| match &item.kind {
            ItemKind::Tool(tool) => tool.app.as_ref().map(|app| (item.id.0, app)),
            _ => None,
        })
        .collect();
    if !tools.is_empty() {
        // Read again only for them, which few chats have.
        let of: ChatOf = parse(&inside).map_err(|why| damaged("its snapshot", &why))?;
        let mut as_saved = BTreeMap::new();
        for item in of.chat.turns.iter().flat_map(|turn| &turn.items) {
            tool_app(item, &mut |id, app| {
                as_saved.insert(id, app.clone());
            });
        }
        as_saved.extend(tools_set);
        for (id, app) in tools {
            if !as_saved.get(&id).is_some_and(|saved| keeps(saved, app)) {
                return Err(CannotRestore::AppData(
                    "this version of the app can't keep all of an app item in a tool call".into(),
                ));
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

/// What a save's MessagePack holds at its root.
#[derive(Clone, Copy)]
enum Root {
    /// A snapshot: [`Saved`].
    Snapshot,
    /// A journal record: [`Entry`]s.
    Record,
}

/// `inside`, a snapshot or record of a save of `version`, in this
/// version's shape. What each version changed the shape of, rather than
/// only added to:
/// 5. the notes about settings
///    ([`Note::AgentChanged`](caf_core::chat::Note::AgentChanged),
///    [`Note::NotOffered`](caf_core::chat::Note::NotOffered),
///    [`Note::NotApplied`](caf_core::chat::Note::NotApplied)) hold the
///    choice with the agent's name for it, which older versions didn't
///    keep, rather than the choice alone.
/// 6. tool calls ([`ToolCall`](caf_core::tool::ToolCall)) hold where they
///    came from, what they do, who allowed them, their result as pieces
///    and their locations, rather than their output as text.
///
/// Only where a saved type has them is looked at: never inside what an
/// agent, a tool or the app wrote.
fn migrate(version: u32, inside: Vec<u8>, root: Root) -> Result<Vec<u8>, String> {
    if version >= 6 {
        return Ok(inside);
    }
    let mut tree: Tree = parse(&inside)?;
    let changed = std::cell::Cell::new(false);
    let mut note = |item: &mut Tree| {
        if version < 5 && name_note(item) {
            changed.set(true);
        }
        if reshape_tool_item(item) {
            changed.set(true);
        }
    };
    match root {
        Root::Snapshot => {
            if let Some(Tree::List(turns)) =
                tree.get_mut("chat").and_then(|chat| chat.get_mut("turns"))
            {
                turns.iter_mut().for_each(|turn| items(turn, &mut note));
            }
        }
        Root::Record => {
            if let Tree::List(entries) = &mut tree {
                for entry in entries {
                    if entry.get("type") != Some(&Tree::Str("Change".into())) {
                        continue;
                    }
                    let Some(change) = entry.get_mut("value") else {
                        continue;
                    };
                    let kind = match change.get("type") {
                        Some(Tree::Str(kind)) => kind.clone(),
                        _ => continue,
                    };
                    let Some(value) = change.get_mut("value") else {
                        continue;
                    };
                    match kind.as_str() {
                        "TurnAdded" => items(value, &mut note),
                        "TurnInserted" => {
                            if let Some(turn) = value.get_mut("turn") {
                                items(turn, &mut note);
                            }
                        }
                        "ItemAdded" => {
                            if let Some(item) = value.get_mut("item") {
                                note(item);
                            }
                        }
                        "ToolChanged" => {
                            if let Some(tool) = value.get_mut("tool")
                                && reshape_tool(tool)
                            {
                                changed.set(true);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    if !changed.get() {
        return Ok(inside);
    }
    named(&tree)
}

/// Does `each` to every item of `turn`.
fn items(turn: &mut Tree, each: &mut impl FnMut(&mut Tree)) {
    if let Some(Tree::List(items)) = turn.get_mut("items") {
        items.iter_mut().for_each(each);
    }
}

/// Makes `item`, if it's a tool call of the shape before version 6, a
/// tool call of this version's: whether it was.
fn reshape_tool_item(item: &mut Tree) -> bool {
    let Some(kind) = item.get_mut("kind") else {
        return false;
    };
    if kind.get("type") != Some(&Tree::Str("Tool".into())) {
        return false;
    }
    kind.get_mut("value").is_some_and(reshape_tool)
}

/// Makes `tool`, a tool call of the shape before version 6, one of this
/// version's: its output one piece of text, if it had any; its action not
/// known, as its title is the model's, not the tool's name; where it came
/// from and who allowed it not recorded.
/// Whether it was the old shape.
fn reshape_tool(tool: &mut Tree) -> bool {
    let Tree::Map(fields) = tool else {
        return false;
    };
    let Some(at) =
        (fields.iter()).position(|(key, _)| matches!(key, Tree::Str(key) if key == "output"))
    else {
        return false;
    };
    let (_, output) = fields.remove(at);
    let text = |key: &str| Tree::Str(key.into());
    let tagged = |kind: &str, value: Option<Tree>| {
        let mut map = vec![(text("type"), text(kind))];
        map.extend(value.map(|value| (text("value"), value)));
        Tree::Map(map)
    };
    let result = match output {
        Tree::Str(output) if !output.is_empty() => vec![tagged(
            "Text",
            Some(Tree::Map(vec![
                (text("text"), Tree::Str(output)),
                (text("cut"), Tree::Nil),
            ])),
        )],
        _ => Vec::new(),
    };
    let Tree::Map(fields) = tool else {
        return false;
    };
    fields.extend([
        (text("origin"), tagged("NotRecorded", None)),
        (text("action"), Tree::Nil),
        (text("input_cut"), Tree::Nil),
        (text("result"), Tree::List(result)),
        (text("locations"), Tree::List(Vec::new())),
        (text("locations_cut"), Tree::Int(0)),
        (text("decision"), tagged("NotRecorded", None)),
    ]);
    true
}

/// Makes `item`, if it's a note about a setting holding the choice
/// alone, as before version 5, hold it with no name: whether it did.
fn name_note(item: &mut Tree) -> bool {
    let Some(kind) = item.get_mut("kind") else {
        return false;
    };
    if kind.get("type") != Some(&Tree::Str("Note".into())) {
        return false;
    }
    let Some(note) = kind.get_mut("value") else {
        return false;
    };
    let about = matches!(
        note.get("type"),
        Some(Tree::Str(kind)) if ["AgentChanged", "NotOffered", "NotApplied"].contains(&kind.as_str())
    );
    let Some(choice) = note
        .get_mut("value")
        .filter(|choice| about && choice.get("choice").is_none())
    else {
        return false;
    };
    let alone = std::mem::take(choice);
    *choice = Tree::Map(vec![
        (Tree::Str("choice".into()), alone),
        (Tree::Str("name".into()), Tree::Nil),
    ]);
    true
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
    // A number in `raw` is written as MessagePack's own, unless something
    // in the build turns on serde_json's `arbitrary_precision`, which
    // would write it as a map, changing the format: a test makes sure.
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
        && let Ok(saved) = Tree::deserialize(&unknown.raw)
        && serde_json::to_value(&app)
            .ok()
            .and_then(|back| Tree::deserialize(back).ok())
            .is_some_and(|back| saved.within(&back))
    {
        item.kind = ItemKind::App(app);
    }
}

/// Whether `value`, written again, holds all of what was saved as
/// `saved`: so reading it lost nothing.
fn keeps<T: Serialize>(saved: &Tree, value: &T) -> bool {
    named(value)
        .and_then(|bytes| parse::<Tree>(&bytes))
        .is_ok_and(|back| saved.within(&back))
}

/// A snapshot, but only its data.
#[derive(Deserialize)]
struct DataOf {
    data: Tree,
}

/// A snapshot, but only its items, as saved.
#[derive(Deserialize)]
struct ChatOf {
    chat: TurnsOf,
}

/// A chat, but only its items, as saved.
#[derive(Deserialize)]
struct TurnsOf {
    turns: Vec<ItemsOf>,
}

/// A turn, but only its items, as saved.
#[derive(Deserialize)]
struct ItemsOf {
    items: Vec<Tree>,
}

/// A journal entry, but only its kind, and what it holds as saved.
#[derive(Deserialize)]
struct RawEntry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    value: Tree,
}

/// Notes the app's item in a tool call, as saved, if `item` is a tool call
/// that has one.
fn tool_app(item: &Tree, note: &mut impl FnMut(u64, &Tree)) {
    let kind = item.get("kind");
    if kind.and_then(|kind| kind.get("type")) == Some(&Tree::Str("Tool".into()))
        && let Some(Tree::Int(id)) = item.get("id")
        && let Some(app) = kind.and_then(|kind| kind.get("value")?.get("app"))
        && *app != Tree::Nil
    {
        note(*id as u64, app);
    }
}

/// Notes the apps' items in tool calls a change sets, as saved, in order.
fn tool_apps_changed(change: &Tree, set: &mut Vec<(u64, Tree)>) {
    let mut note = |id, app: &Tree| set.push((id, app.clone()));
    let Some(Tree::Str(kind)) = change.get("type") else {
        return;
    };
    let value = change.get("value");
    let turn = |turn: Option<&Tree>, note: &mut dyn FnMut(u64, &Tree)| {
        if let Some(Tree::List(items)) = turn.and_then(|turn| turn.get("items")) {
            items
                .iter()
                .for_each(|item| tool_app(item, &mut |id, app| note(id, app)));
        }
    };
    match kind.as_str() {
        "TurnAdded" => turn(value, &mut note),
        "TurnInserted" => turn(value.and_then(|value| value.get("turn")), &mut note),
        "ItemAdded" => {
            if let Some(item) = value.and_then(|value| value.get("item")) {
                tool_app(item, &mut note);
            }
        }
        "ToolChanged" => {
            if let Some(Tree::Int(id)) = value.and_then(|value| value.get("item"))
                && let Some(app) = value.and_then(|value| value.get("tool")?.get("app"))
                && *app != Tree::Nil
            {
                note(*id as u64, app);
            }
        }
        _ => {}
    }
}

/// What's saved, as anything is: what tells whether the app's type keeps
/// all of it.
#[derive(Debug, Default, Clone, PartialEq)]
enum Tree {
    /// Nothing.
    #[default]
    Nil,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Tree>),
    /// A map: its keys are anything MessagePack's can be, such as a list
    /// for a tuple, and compared as what they are, never as text.
    Map(Vec<(Tree, Tree)>),
}

impl Tree {
    /// A map's value, by a key that's text.
    fn get(&self, key: &str) -> Option<&Tree> {
        match self {
            Tree::Map(map) => (map.iter())
                .find(|(each, _)| matches!(each, Tree::Str(each) if each == key))
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// A map's value, by a key that's text, to change.
    fn get_mut(&mut self, key: &str) -> Option<&mut Tree> {
        match self {
            Tree::Map(map) => (map.iter_mut())
                .find(|(each, _)| matches!(each, Tree::Str(each) if each == key))
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// Whether `back` holds all of this: every key of a map, with what it
    /// holds; every list as long, with each of what it holds; and every
    /// thing else equal, a whole number held as a float of just the same
    /// value included, as a type widened from one to the other writes it.
    /// Keys `back` has that this hasn't, as a newer version adds, are
    /// fine; nothing else may differ.
    fn within(&self, back: &Tree) -> bool {
        match (self, back) {
            (Tree::Map(saved), Tree::Map(back)) => saved.iter().all(|(key, saved)| {
                (back.iter()).any(|(each, back)| key.same(each) && saved.within(back))
            }),
            (Tree::List(saved), Tree::List(back)) => {
                saved.len() == back.len()
                    && saved
                        .iter()
                        .zip(back)
                        .all(|(saved, back)| saved.within(back))
            }
            (Tree::Int(saved), Tree::Float(back)) => {
                // Both ways, so neither is rounded.
                *saved as f64 == *back && *back as i128 == *saved
            }
            (saved, back) => saved.same(back),
        }
    }

    /// Whether this is just what `other` is: floats bit for bit, but any
    /// not-a-number as any other.
    fn same(&self, other: &Tree) -> bool {
        match (self, other) {
            (Tree::Float(one), Tree::Float(other)) => {
                one.to_bits() == other.to_bits() || (one.is_nan() && other.is_nan())
            }
            (Tree::List(one), Tree::List(other)) => {
                one.len() == other.len()
                    && one.iter().zip(other).all(|(one, other)| one.same(other))
            }
            (Tree::Map(one), Tree::Map(other)) => {
                one.len() == other.len()
                    && one.iter().all(|(key, value)| {
                        (other.iter()).any(|(each, other)| key.same(each) && value.same(other))
                    })
            }
            (one, other) => one == other,
        }
    }
}

/// Written as it was read: what a migration leaves alone is written back
/// as MessagePack holding just what it held. A float is written as 64
/// bits, as it's read; a 32-bit one, which CAF's types never write, reads
/// back as the same value.
impl Serialize for Tree {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeMap, SerializeSeq};
        match self {
            Tree::Nil => serializer.serialize_unit(),
            Tree::Bool(value) => serializer.serialize_bool(*value),
            Tree::Int(value) => match (i64::try_from(*value), u64::try_from(*value)) {
                (Ok(value), _) => serializer.serialize_i64(value),
                (_, Ok(value)) => serializer.serialize_u64(value),
                _ => Err(S::Error::custom("a number past 64 bits")),
            },
            Tree::Float(value) => serializer.serialize_f64(*value),
            Tree::Str(value) => serializer.serialize_str(value),
            Tree::Bytes(value) => serializer.serialize_bytes(value),
            Tree::List(list) => {
                let mut seq = serializer.serialize_seq(Some(list.len()))?;
                list.iter()
                    .try_for_each(|each| seq.serialize_element(each))?;
                seq.end()
            }
            Tree::Map(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                (entries.iter()).try_for_each(|(key, value)| map.serialize_entry(key, value))?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Tree {
    fn deserialize<De: Deserializer<'de>>(deserializer: De) -> Result<Self, De::Error> {
        deserializer.deserialize_any(TreeVisitor)
    }
}

/// Reads a [`Tree`] from anything.
struct TreeVisitor;

impl<'de> Visitor<'de> for TreeVisitor {
    type Value = Tree;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("anything")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Tree, E> {
        Ok(Tree::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Tree, E> {
        Ok(Tree::Int(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Tree, E> {
        Ok(Tree::Int(value.into()))
    }

    fn visit_i128<E>(self, value: i128) -> Result<Tree, E> {
        Ok(Tree::Int(value))
    }

    fn visit_u128<E: serde::de::Error>(self, value: u128) -> Result<Tree, E> {
        i128::try_from(value)
            .map(Tree::Int)
            .map_err(|_| E::custom("a number past 127 bits"))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Tree, E> {
        Ok(Tree::Float(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Tree, E> {
        Ok(Tree::Str(value.to_owned()))
    }

    fn visit_bytes<E>(self, value: &[u8]) -> Result<Tree, E> {
        Ok(Tree::Bytes(value.to_owned()))
    }

    fn visit_none<E>(self) -> Result<Tree, E> {
        Ok(Tree::Nil)
    }

    fn visit_unit<E>(self) -> Result<Tree, E> {
        Ok(Tree::Nil)
    }

    fn visit_some<De: Deserializer<'de>>(self, inner: De) -> Result<Tree, De::Error> {
        Tree::deserialize(inner)
    }

    fn visit_newtype_struct<De: Deserializer<'de>>(self, inner: De) -> Result<Tree, De::Error> {
        Tree::deserialize(inner)
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Tree, S::Error> {
        let mut list = Vec::new();
        while let Some(each) = seq.next_element()? {
            list.push(each);
        }
        Ok(Tree::List(list))
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Tree, M::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = map.next_entry::<Tree, Tree>()? {
            entries.push(entry);
        }
        Ok(Tree::Map(entries))
    }
}

#[cfg(test)]
mod tests {
    use super::Tree::{self, Bool, Float, Int, List, Map, Nil, Str};
    use crate::Squeeze;

    #[test]
    fn what_holds_what() {
        let map = |entries: Vec<(Tree, Tree)>| Map(entries);
        let text = |text: &str| Str(text.into());
        // What was saved, what's written back, and whether it holds it.
        let cases = [
            (Float(f64::NAN), Float(-f64::NAN), true),
            (Float(0.0), Float(-0.0), false),
            (Float(-0.0), Float(0.0), false),
            (Float(1.5), Float(1.5), true),
            (Int(1), Float(1.0), true),
            (Int(-3), Float(-3.0), true),
            (Int(1), Float(1.5), false),
            (Float(1.0), Int(1), false),
            (Int((1 << 60) + 1), Float(((1_i64 << 60) + 1) as f64), false),
            (Int(1), Int(1), true),
            (Int(1), Bool(true), false),
            (Nil, map(vec![]), false),
            (map(vec![]), Nil, false),
            (map(vec![]), map(vec![(text("a"), Nil)]), true),
            (map(vec![(text("a"), Nil)]), map(vec![]), false),
            (List(vec![Int(1), Int(2)]), List(vec![Int(1)]), false),
            (List(vec![Int(1)]), Int(1), false),
            (
                map(vec![(List(vec![Int(1), Int(2)]), text("x"))]),
                map(vec![
                    (List(vec![Int(1), Int(2)]), text("x")),
                    (text("y"), Nil),
                ]),
                true,
            ),
            (
                map(vec![(Int(1), text("x"))]),
                map(vec![(text("1"), text("x"))]),
                false,
            ),
            (
                map(vec![(text("1"), text("x"))]),
                map(vec![(Int(1), text("x"))]),
                false,
            ),
            (
                map(vec![(Float(1.0), Nil)]),
                map(vec![(Int(1), Nil)]),
                false,
            ),
        ];
        for (saved, back, holds) in cases {
            assert_eq!(saved.within(&back), holds, "{saved:?} in {back:?}");
        }
    }

    #[test]
    fn a_note_about_a_setting_saved_before_version_5_reads_with_no_name() {
        use caf_core::chat::{ItemKind, Note};
        use caf_core::save::Saved;
        use caf_core::settings::SettingChoice;
        use serde_json::{Value, json};

        let model =
            json!({ "kind": { "type": "Model" }, "value": { "type": "Choice", "value": "opus" } });
        let note = |id: u64, kind: &str| {
            json!({
                "id": id, "window": { "start": null, "end": { "type": "Unknown" } }, "parent": null,
                "kind": { "type": "Note", "value": { "type": kind, "value": model } },
            })
        };
        let signed_in = json!({
            "id": 3, "window": { "start": null, "end": { "type": "Unknown" } }, "parent": null,
            "kind": { "type": "Note", "value": { "type": "SignedIn" } },
        });
        // What an agent sent, kept as it came, shaped like one: left alone.
        let raw = json!({ "type": "Note", "value": { "type": "NotOffered", "value": model } });
        let unknown = json!({
            "id": 4, "window": { "start": null, "end": { "type": "Unknown" } }, "parent": null,
            "kind": { "type": "Unknown", "value": { "what": "kind", "raw": { "kind": raw }, "summary": null } },
        });
        let turn = |id: u64, items: Vec<Value>| {
            json!({
                "id": id, "starter": { "type": "Caf" }, "life": { "type": "Finished" },
                "window": { "start": null, "end": { "type": "Unknown" } }, "items": items, "imported": false,
            })
        };
        let saved = json!({
            "header": {
                "chat": caf_core::ChatId::from_number(1), "agent": "claude", "profile": null, "cwd": "/work",
                "made": 0, "changed": 0, "label": null, "first": null, "turns": 1, "closed": false,
            },
            "chat": {
                "id": caf_core::ChatId::from_number(1), "agent": "claude", "cwd": "/work", "state": { "type": "Idle" },
                "turns": [turn(1, vec![note(2, "AgentChanged"), signed_in, unknown])],
                "usage": null, "questions": [],
            },
            "session": null,
            // As MessagePack writes no data.
            "data": [],
        });
        let change = |change: Value| json!({ "type": "Change", "value": change });
        let record = json!([
            change(
                json!({ "type": "ItemAdded", "value": { "turn": 1, "item": note(5, "NotOffered") } })
            ),
            change(json!({ "type": "TurnAdded", "value": turn(6, vec![note(7, "NotApplied")]) })),
            change(
                json!({ "type": "TurnInserted", "value": { "before": 6, "turn": turn(8, vec![note(9, "AgentChanged")]) } })
            ),
        ]);
        let snapshot = super::seal(4, &super::named(&saved).expect("written"), Squeeze::Fast);
        let frame = super::pack(&super::named(&record).expect("written"), Squeeze::Fast);
        let mut journal = u32::try_from(frame.len())
            .expect("small")
            .to_le_bytes()
            .to_vec();
        journal.extend_from_slice(&frame);

        let read: Saved = super::read(&snapshot, &journal).expect("it reads");
        let items: Vec<&ItemKind> = (read.chat.turns.iter())
            .flat_map(|turn| &turn.items)
            .map(|item| &item.kind)
            .collect();
        let choice = SettingChoice::model("opus");
        let named = |note: Note| ItemKind::Note(note);
        assert_eq!(
            items
                .iter()
                .filter(|kind| matches!(kind, ItemKind::Note(_)))
                .copied()
                .cloned()
                .collect::<Vec<_>>(),
            [
                named(Note::AgentChanged {
                    choice: choice.clone(),
                    name: None
                }),
                named(Note::SignedIn),
                named(Note::NotOffered {
                    choice: choice.clone(),
                    name: None
                }),
                named(Note::AgentChanged {
                    choice: choice.clone(),
                    name: None
                }),
                named(Note::NotApplied { choice, name: None }),
            ]
        );
        let Some(ItemKind::Unknown(unknown)) = items.get(2) else {
            panic!("an unknown item: {items:?}");
        };
        assert_eq!(unknown.raw, json!({ "kind": raw }));

        // A version 5 save is read as it is.
        let snapshot = super::seal(5, &super::named(&saved).expect("written"), Squeeze::Fast);
        assert!(super::read::<caf_core::NoAppItems, caf_core::NoAppData>(&snapshot, &[]).is_err());
    }
}
