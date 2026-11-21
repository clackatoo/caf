//! What the app's types read back must keep: all of what was saved, or
//! it isn't taken as read.

mod common;

use caf_core::chat::ItemKind;
use caf_core::save::{CannotRestore, Entry, Saved};
use caf_core::{AppData, AppItem};
use caf_save::{Squeeze, decode, encode_record, encode_snapshot};
use common::from;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

/// An app's item or data as any JSON at all: what wrote the saves here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
struct Json(Value);

impl AppItem for Json {
    fn summary(&self) -> String {
        "Something".into()
    }
}

impl AppData for Json {
    fn label(&self) -> Option<String> {
        None
    }
}

/// Declares a type the app reads it as, as an app item and as data.
macro_rules! app {
    ($ty:ident) => {
        impl AppItem for $ty {
            fn summary(&self) -> String {
                stringify!($ty).into()
            }
        }
        impl AppData for $ty {
            fn label(&self) -> Option<String> {
                None
            }
        }
    };
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Inner {
    a: u8,
}

/// A map in a map.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Nested {
    inner: Inner,
}
app!(Nested);

/// Maps in a list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Listed {
    items: Vec<Inner>,
}
app!(Listed);

/// A list read as how long it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Counted {
    #[serde(deserialize_with = "count")]
    items: usize,
}
app!(Counted);

fn count<'de, De: Deserializer<'de>>(deserializer: De) -> Result<usize, De::Error> {
    Vec::<Value>::deserialize(deserializer).map(|list| list.len())
}

/// A list read as nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Dropped {
    #[serde(deserialize_with = "nothing")]
    items: Option<u8>,
}
app!(Dropped);

fn nothing<'de, De: Deserializer<'de>>(deserializer: De) -> Result<Option<u8>, De::Error> {
    Value::deserialize(deserializer).map(|_| None)
}

/// A list read as its first two.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Shortened {
    #[serde(deserialize_with = "first_two")]
    items: Vec<u8>,
}
app!(Shortened);

fn first_two<'de, De: Deserializer<'de>>(deserializer: De) -> Result<Vec<u8>, De::Error> {
    Vec::<u8>::deserialize(deserializer).map(|list| list.into_iter().take(2).collect())
}

/// A mode, any it doesn't know read as another.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
enum Mode {
    #[default]
    Map,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Moded {
    mode: Mode,
}
app!(Moded);

/// A newer version, with a field the save didn't have.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Newer {
    inner: Inner,
    #[serde(default)]
    added: Vec<String>,
}
app!(Newer);

/// Whether a type takes the app item a snapshot holds as its own.
type Takes = fn(&[u8]) -> bool;

/// How a snapshot's data reads as a type.
type Reads = fn(&[u8]) -> Result<(), CannotRestore>;

/// What's saved, and the types that read it without keeping it all.
fn cases() -> Vec<(Value, Takes)> {
    vec![
        (json!({ "inner": { "a": 1, "b": 2 } }), is_kept::<Nested>),
        (
            json!({ "items": [{ "a": 1 }, { "a": 2, "b": 3 }] }),
            is_kept::<Listed>,
        ),
        (json!({ "items": [1, 2, 3] }), is_kept::<Counted>),
        (json!({ "items": [1, 2, 3] }), is_kept::<Dropped>),
        (json!({ "items": [1, 2, 3] }), is_kept::<Shortened>),
        (json!({ "mode": "Satellite" }), is_kept::<Moded>),
    ]
}

/// A chat saved with `item` as an app item, and as its data.
fn saved(item: &Value) -> Vec<u8> {
    let kind = json!({ "type": "App", "value": item });
    let mut saved = common::saved_with(1, vec![common::turn(1, "Hi", vec![common::item(2, kind)])]);
    saved["data"] = item.clone();
    encode_snapshot(&from::<Saved<Json, Json>>(saved), Squeeze::Fast)
}

/// Whether `T` takes the app item it reads from `snapshot` as its own.
fn is_kept<T: AppItem + AppData>(snapshot: &[u8]) -> bool {
    let read = decode::<T, Json>(snapshot, &[]).expect("it reads");
    matches!(read.chat.turns[0].items[0].kind, ItemKind::App(_))
}

#[test]
fn an_app_item_read_back_short_of_what_was_saved_stays_unknown() {
    for (item, is_kept) in cases() {
        assert!(!is_kept(&saved(&item)), "{item}");
    }
    // What's read back whole is the app's.
    let fine = json!({ "inner": { "a": 1 } });
    assert!(is_kept::<Nested>(&saved(&fine)));
    assert!(is_kept::<Newer>(&saved(&fine)), "a field added is fine");
}

#[test]
fn data_read_back_short_of_what_was_saved_is_not_restored() {
    let lost = CannotRestore::AppData("this version of the app can't keep all of it".into());
    let read = |item: &Value, as_: Reads| as_(&saved(item));
    fn data<T: AppItem + AppData>(snapshot: &[u8]) -> Result<(), CannotRestore> {
        decode::<Json, T>(snapshot, &[]).map(|_| ())
    }
    let cases: [(Value, Reads); 6] = [
        (json!({ "inner": { "a": 1, "b": 2 } }), data::<Nested>),
        (
            json!({ "items": [{ "a": 1 }, { "a": 2, "b": 3 }] }),
            data::<Listed>,
        ),
        (json!({ "items": [1, 2, 3] }), data::<Counted>),
        (json!({ "items": [1, 2, 3] }), data::<Dropped>),
        (json!({ "items": [1, 2, 3] }), data::<Shortened>),
        (json!({ "mode": "Satellite" }), data::<Moded>),
    ];
    for (item, as_) in cases {
        assert_eq!(read(&item, as_), Err(lost.clone()), "{item}");
    }
    let fine = json!({ "inner": { "a": 1 } });
    assert_eq!(read(&fine, data::<Nested>), Ok(()));
    assert_eq!(read(&fine, data::<Newer>), Ok(()), "a field added is fine");
}

/// A chat with a tool call holding `app`.
fn with_tool(app: &Value) -> Saved<Json, Json> {
    let tool = json!({ "type": "Tool", "value": {
        "title": "Move the map",
        "kind": { "type": "Other" },
        "status": { "type": "Done" },
        "input": null,
        "output": "",
        "app": app,
    }});
    from(common::saved_with(
        1,
        vec![common::turn(1, "Hi", vec![common::item(2, tool)])],
    ))
}

#[test]
fn an_app_item_in_a_tool_call_read_back_short_is_not_restored() {
    let lost = CannotRestore::AppData(
        "this version of the app can't keep all of an app item in a tool call".into(),
    );
    let (fine, short) = (
        json!({ "inner": { "a": 1 } }),
        json!({ "inner": { "a": 1, "b": 2 } }),
    );
    // In the snapshot.
    let mut saved = with_tool(&short);
    saved.data = Json(json!({ "inner": { "a": 1 } }));
    let snapshot = encode_snapshot(&saved, Squeeze::Fast);
    assert_eq!(
        decode::<Nested, Nested>(&snapshot, &[]).map(|_| ()),
        Err(lost.clone())
    );
    assert!(
        decode::<Newer, Nested>(&snapshot, &[]).is_err(),
        "it's short for a newer one too"
    );

    // Changed in the journal, either way.
    let mut saved = with_tool(&fine);
    saved.data = Json(json!({ "inner": { "a": 1 } }));
    let snapshot = encode_snapshot(&saved, Squeeze::Fast);
    assert!(decode::<Nested, Nested>(&snapshot, &[]).is_ok());
    let changed = |app: &Value| {
        let entries: Vec<Entry<Json, Json>> = from(json!([
            { "type": "Change", "value": { "type": "ToolChanged", "value": {
                "item": 2,
                "tool": {
                    "title": "Move the map",
                    "kind": { "type": "Other" },
                    "status": { "type": "Done" },
                    "input": null,
                    "output": "moved",
                    "app": app,
                },
            } } },
        ]));
        encode_record(&entries)
    };
    let read = decode::<Nested, Nested>(&snapshot, &changed(&short));
    assert_eq!(read.map(|_| ()), Err(lost));
    let read = decode::<Nested, Nested>(&snapshot, &[changed(&short), changed(&fine)].concat());
    assert!(read.is_ok(), "what counts is what it was set to last");
}

/// Data keyed by something other than text: a grid's cells.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Grid {
    cells: std::collections::BTreeMap<(u32, u32), String>,
}
app!(Grid);

/// Data whose count was a whole number, and is a float now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Whole {
    count: u32,
}
app!(Whole);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Widened {
    count: f64,
}
app!(Widened);

/// A chat with `data` in its data slot, and nothing else of the app's.
fn with_data<D: AppData>(data: D) -> Vec<u8> {
    let mut json = common::saved_with(1, vec![]);
    json["data"] = serde_json::to_value(D::default()).expect("JSON");
    let mut saved: Saved<Json, D> = from(json);
    saved.data = data;
    encode_snapshot(&saved, Squeeze::Fast)
}

#[test]
fn data_keyed_by_anything_reads() {
    let grid = Grid {
        cells: [((1, 2), "a".to_owned()), ((3, 4), "b".to_owned())].into(),
    };
    let read = decode::<Json, Grid>(&with_data(grid.clone()), &[]).expect("it reads");
    assert_eq!(read.data, grid);
}

#[test]
fn a_whole_number_read_as_a_float_of_the_same_value_is_kept() {
    let snapshot = with_data(Whole { count: 3 });
    let read = decode::<Json, Widened>(&snapshot, &[]).expect("it reads");
    assert_eq!(read.data, Widened { count: 3.0 });
    // Not back the other way: a float isn't a whole number.
    let snapshot = with_data(Widened { count: 3.0 });
    assert!(decode::<Json, Whole>(&snapshot, &[]).is_err());
}

#[test]
fn an_app_item_in_a_tool_call_added_by_the_journal_is_checked_too() {
    let lost = CannotRestore::AppData(
        "this version of the app can't keep all of an app item in a tool call".into(),
    );
    let snapshot = encode_snapshot(
        &from::<Saved<Json, Json>>(common::saved_with(1, vec![])),
        Squeeze::Fast,
    );
    let tool = |id: u64, app: &Value| {
        common::item(
            id,
            json!({ "type": "Tool", "value": {
                "title": "Move the map",
                "kind": { "type": "Other" },
                "status": { "type": "Done" },
                "input": null,
                "output": "",
                "app": app,
            }}),
        )
    };
    let (fine, short) = (
        json!({ "inner": { "a": 1 } }),
        json!({ "inner": { "a": 1, "b": 2 } }),
    );
    let added = |app: &Value| {
        let entries: Vec<Entry<Json, Json>> = from(json!([
            { "type": "Change", "value": { "type": "TurnAdded", "value":
                common::turn(10, "Move it", vec![]) } },
            { "type": "Change", "value": { "type": "ItemAdded", "value": {
                "turn": 10, "item": tool(11, app),
            } } },
        ]));
        encode_record(&entries)
    };
    let in_turn = |app: &Value| {
        let entries: Vec<Entry<Json, Json>> = from(json!([
            { "type": "Change", "value": { "type": "TurnAdded", "value":
                common::turn(10, "Move it", vec![tool(11, app)]) } },
        ]));
        encode_record(&entries)
    };
    for record in [&added as &dyn Fn(&Value) -> Vec<u8>, &in_turn] {
        let read = decode::<Nested, Json>(&snapshot, &record(&short));
        assert_eq!(read.map(|_| ()), Err(lost.clone()));
        assert!(decode::<Nested, Json>(&snapshot, &record(&fine)).is_ok());
    }
}
