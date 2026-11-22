//! Tests of prices: reading LiteLLM's table, the layers, and the user's
//! own.

use std::collections::BTreeMap;

use caf_core::time::Wall;
use caf_core::usage::{ModelPrice, PriceLayer, PriceTable};
use serde_json::{Value, json};

use super::{PriceError, bundled, entries, fetched_at, load, read_litellm, set_own, take_fetched};
use crate::testing::Folder;

/// The providers whose models the bundled snapshot keeps: those CAF's
/// agents use, by LiteLLM's names for them.
const BUNDLED_PROVIDERS: [&str; 5] = [
    "anthropic",
    "openai",
    "gemini",
    "vertex_ai-language-models",
    "xai",
];

/// A little of LiteLLM's table, as it comes.
fn sample() -> Value {
    json!({
        "sample_spec": {
            "input_cost_per_token": 0.0,
            "output_cost_per_token": 0.0,
            "litellm_provider": "one of https://docs.litellm.ai/docs/providers",
        },
        "claude-opus-5-5": {
            "input_cost_per_token": 5e-06,
            "output_cost_per_token": 2.5e-05,
            "cache_read_input_token_cost": 5e-07,
            "cache_creation_input_token_cost": 6.25e-06,
            "cache_creation_input_token_cost_above_1hr": 1e-05,
            "input_cost_per_token_above_200k_tokens": 1e-05,
            "litellm_provider": "anthropic",
            "mode": "chat",
            "provider_specific_entry": {"us": 1.1, "fast": 2},
        },
        "gpt-5": {
            "input_cost_per_token": 1.25e-06,
            "output_cost_per_token": 1e-05,
            "cache_read_input_token_cost": 1.25e-07,
            "litellm_provider": "openai",
        },
        "gemini/gemini-2.5-pro": {
            "input_cost_per_token": 3e-07,
            "output_cost_per_token": 1e-05,
            "provider_specific_entry": {"realtime_ga_only": 1},
        },
        "no-output": {"input_cost_per_token": 1e-06},
        "words-not-numbers": {"input_cost_per_token": "free", "output_cost_per_token": 0.0},
        "negative": {"input_cost_per_token": -1e-06, "output_cost_per_token": 1e-06},
        "not-an-entry": 7,
    })
}

/// `json` as bytes.
fn bytes(json: &Value) -> Vec<u8> {
    serde_json::to_vec(json).expect("JSON writes")
}

/// The model's price, and its layer, in a table.
fn priced(table: &PriceTable, model: &str) -> Option<(ModelPrice, PriceLayer)> {
    table
        .models
        .iter()
        .find(|priced| priced.model == model)
        .map(|priced| (priced.price.clone(), priced.layer))
}

/// A price with every rate given.
fn rates(
    input: f64,
    cache_read: f64,
    cache_write: f64,
    output: f64,
    fast: Option<f64>,
) -> ModelPrice {
    let mut price = ModelPrice::new(input, output);
    price.cache_read = cache_read;
    price.cache_write = cache_write;
    price.fast = fast;
    price
}

/// A price of `input` and `output` per million tokens.
fn price(input: f64, output: f64) -> ModelPrice {
    ModelPrice::new(input, output)
}

#[test]
fn litellms_table_is_read_per_million_tokens() {
    let prices: BTreeMap<String, ModelPrice> = read_litellm(&bytes(&sample()))
        .expect("it reads")
        .into_iter()
        .collect();
    let names: Vec<&str> = prices.keys().map(String::as_str).collect();
    assert_eq!(names, ["claude-opus-5-5", "gemini/gemini-2.5-pro", "gpt-5"]);
    assert_eq!(
        prices["claude-opus-5-5"],
        rates(5.0, 0.5, 6.25, 25.0, Some(2.0))
    );
    // No cache write rate: the input rate. And to a billionth, so 3e-7
    // is 0.3 exactly.
    assert_eq!(prices["gpt-5"], rates(1.25, 0.125, 1.25, 10.0, None));
    assert_eq!(prices["gemini/gemini-2.5-pro"], price(0.3, 10.0));
}

#[test]
fn a_table_that_isnt_one_is_refused() {
    assert!(matches!(
        read_litellm(b"[1, 2]"),
        Err(PriceError::NotTable(_))
    ));
    assert!(matches!(
        read_litellm(b"{\"cut off"),
        Err(PriceError::NotTable(_))
    ));
    let nothing =
        json!({"sample_spec": {"input_cost_per_token": 0.0, "output_cost_per_token": 0.0}});
    assert!(matches!(
        read_litellm(&bytes(&nothing)),
        Err(PriceError::Empty)
    ));
}

#[test]
fn the_bundled_snapshot_reads_and_prices_claude_fast() {
    let table = bundled();
    // 270 when it was last refreshed: Anthropic's, OpenAI's, Gemini's and
    // xAI's models, and no one else's.
    assert!(
        (200..1000).contains(&table.models.len()),
        "{} models",
        table.models.len()
    );
    assert!(
        table
            .models
            .iter()
            .all(|priced| !priced.model.starts_with("bedrock/")
                && !priced.model.starts_with("azure/"))
    );
    assert!(
        table
            .models
            .iter()
            .all(|priced| priced.layer == PriceLayer::Bundled)
    );
    assert!(
        table
            .models
            .windows(2)
            .all(|pair| pair[0].model < pair[1].model)
    );
    let (opus, _) = priced(&table, "claude-opus-5-5").expect("Opus 5.5 is priced");
    assert_eq!(opus.fast, Some(2.0));
    for model in [
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "gpt-5",
        "gemini-2.5-pro",
    ] {
        assert!(table.find(model).is_some(), "{model} isn't priced");
    }
    // Its file is LiteLLM's shape, so it reads as a fetched one does.
    let file = super::BUNDLED.as_bytes();
    assert_eq!(
        read_litellm(file).expect("it reads").len(),
        table.models.len()
    );
}

#[test]
fn with_nothing_kept_prices_are_the_bundled_snapshots() {
    let folder = Folder::new("bundled-only");
    let table = load(folder.path());
    assert_eq!(table, bundled());
    assert_eq!(table.fetched, None);
    assert_eq!(fetched_at(folder.path()), None);
}

#[test]
fn the_users_own_then_the_fetched_then_the_bundled() {
    let folder = Folder::new("layers");
    let fetched = json!({
        "claude-opus-5-5": {"input_cost_per_token": 1e-06, "output_cost_per_token": 2e-06},
        "brand-new": {"input_cost_per_token": 3e-06, "output_cost_per_token": 4e-06},
    });
    take_fetched(folder.path(), &bytes(&fetched), Wall(1234)).expect("it's kept");
    set_own(folder.path(), "brand-new", Some(price(9.0, 9.0))).expect("it's set");
    set_own(folder.path(), "my-model", Some(price(7.0, 8.0))).expect("it's set");

    let table = load(folder.path());
    assert_eq!(table.fetched, Some(Wall(1234)));
    assert_eq!(fetched_at(folder.path()), Some(Wall(1234)));
    assert_eq!(
        priced(&table, "brand-new"),
        Some((price(9.0, 9.0), PriceLayer::Own))
    );
    assert_eq!(
        priced(&table, "my-model"),
        Some((price(7.0, 8.0), PriceLayer::Own))
    );
    assert_eq!(
        priced(&table, "claude-opus-5-5"),
        Some((price(1.0, 2.0), PriceLayer::Fetched))
    );
    let (_, layer) = priced(&table, "claude-haiku-5-5").expect("it's bundled");
    assert_eq!(layer, PriceLayer::Bundled);
    assert!(
        table
            .models
            .windows(2)
            .all(|pair| pair[0].model < pair[1].model)
    );
}

#[test]
fn clearing_the_users_own_price_lets_the_next_layer_price_it() {
    let folder = Folder::new("clear");
    set_own(folder.path(), "claude-opus-5-5", Some(price(1.0, 1.0))).expect("it's set");
    set_own(folder.path(), "mine", Some(price(2.0, 2.0))).expect("it's set");
    assert_eq!(
        priced(&load(folder.path()), "claude-opus-5-5").map(|(_, layer)| layer),
        Some(PriceLayer::Own)
    );

    set_own(folder.path(), "claude-opus-5-5", None).expect("it's cleared");
    set_own(folder.path(), "mine", None).expect("it's cleared");
    set_own(folder.path(), "never-set", None).expect("clearing nothing is fine");
    let table = load(folder.path());
    assert_eq!(
        priced(&table, "claude-opus-5-5").map(|(_, layer)| layer),
        Some(PriceLayer::Bundled)
    );
    assert_eq!(priced(&table, "mine"), None);
}

#[test]
fn the_users_own_prices_keep_what_a_later_caf_added() {
    let folder = Folder::new("own-later");
    let prices = folder.path().join("prices");
    std::fs::create_dir_all(&prices).expect("it's made");
    let later = json!({
        "models": {
            "theirs": {
                "input": 1.0, "cache_read": 1.0, "cache_write": 1.0, "output": 2.0,
                "fast": null, "long_context": 3.0,
            },
        },
        "currency_note": "kept",
    });
    std::fs::write(prices.join("own.json"), bytes(&later)).expect("it's written");

    set_own(folder.path(), "mine", Some(price(5.0, 6.0))).expect("it's set");
    let file: Value =
        serde_json::from_slice(&std::fs::read(prices.join("own.json")).expect("it's there"))
            .expect("it's JSON");
    assert_eq!(file["currency_note"], "kept");
    assert_eq!(file["models"]["theirs"]["long_context"], 3.0);
    let table = load(folder.path());
    assert_eq!(
        priced(&table, "theirs"),
        Some((price(1.0, 2.0), PriceLayer::Own))
    );
}

#[test]
fn prices_kept_that_dont_read_are_left_alone() {
    let folder = Folder::new("own-broken");
    let prices = folder.path().join("prices");
    std::fs::create_dir_all(&prices).expect("it's made");
    std::fs::write(prices.join("own.json"), b"{\"models\": {").expect("it's written");
    std::fs::write(prices.join("litellm.json"), b"{\"fetched\": 5, \"tab").expect("it's written");

    // Read as absent: the bundled snapshot prices everything.
    assert_eq!(load(folder.path()), bundled());
    assert_eq!(fetched_at(folder.path()), None);
    // Never written over.
    let error = set_own(folder.path(), "mine", Some(price(1.0, 1.0))).expect_err("it refuses");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        std::fs::read(prices.join("own.json")).expect("it's there"),
        b"{\"models\": {"
    );
}

#[test]
fn a_price_that_isnt_one_is_refused() {
    let folder = Folder::new("own-bad");
    let negative = rates(1.0, -1.0, 1.0, 1.0, None);
    let no_fast = rates(1.0, 1.0, 1.0, 1.0, Some(0.0));
    for price in [
        negative,
        no_fast,
        price(f64::NAN, 1.0),
        price(1.0, f64::INFINITY),
    ] {
        let error = set_own(folder.path(), "mine", Some(price)).expect_err("it refuses");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
    let error = set_own(folder.path(), "", Some(price(1.0, 1.0))).expect_err("it refuses");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!folder.path().join("prices/own.json").exists());
}

#[test]
fn a_fetched_table_that_doesnt_read_isnt_kept() {
    let folder = Folder::new("fetched-bad");
    let good = json!({"m": {"input_cost_per_token": 1e-06, "output_cost_per_token": 2e-06}});
    take_fetched(folder.path(), &bytes(&good), Wall(1)).expect("it's kept");

    assert!(matches!(
        take_fetched(folder.path(), b"<html>rate limited</html>", Wall(2)),
        Err(PriceError::NotTable(_))
    ));
    assert!(matches!(
        take_fetched(folder.path(), b"{}", Wall(3)),
        Err(PriceError::Empty)
    ));
    let table = load(folder.path());
    assert_eq!(table.fetched, Some(Wall(1)));
    assert_eq!(
        priced(&table, "m"),
        Some((price(1.0, 2.0), PriceLayer::Fetched))
    );
}

#[test]
fn a_fetched_table_is_kept_cut_to_what_caf_reads() {
    let folder = Folder::new("fetched-cut");
    take_fetched(folder.path(), &bytes(&sample()), Wall(9)).expect("it's kept");
    let kept: Value = serde_json::from_slice(
        &std::fs::read(folder.path().join("prices/litellm.json")).expect("it's there"),
    )
    .expect("it's JSON");
    assert_eq!(kept["fetched"], 9);
    assert_eq!(
        kept["table"]["claude-opus-5-5"],
        json!({
            "input_cost_per_token": 5e-06,
            "output_cost_per_token": 2.5e-05,
            "cache_read_input_token_cost": 5e-07,
            "cache_creation_input_token_cost": 6.25e-06,
            "provider_specific_entry": {"fast": 2.0},
        })
    );
    assert_eq!(
        kept["table"].as_object().map(|table| table.len()),
        Some(3),
        "only the entries that price a model"
    );
    // And no new file left behind.
    let names: Vec<String> = std::fs::read_dir(folder.path().join("prices"))
        .expect("it's there")
        .map(|entry| {
            entry
                .expect("it reads")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| !name.starts_with('.'))
        .collect();
    assert_eq!(names, ["litellm.json"]);
}

/// Rewrites the bundled snapshot from LiteLLM's table, fetched to the file
/// `CAF_LITELLM` names: see the module's docs.
#[test]
#[ignore = "rewrites prices/bundled.json from a table fetched to $CAF_LITELLM"]
fn refresh_bundled() {
    let path = std::env::var_os("CAF_LITELLM").expect("CAF_LITELLM names the fetched table");
    let table: BTreeMap<String, Value> =
        serde_json::from_slice(&std::fs::read(path).expect("it reads")).expect("it's JSON");
    let kept: BTreeMap<String, Value> = table
        .into_iter()
        .filter(|(_, entry)| {
            let provider = entry.get("litellm_provider").and_then(Value::as_str);
            let mode = entry.get("mode").and_then(Value::as_str);
            provider.is_some_and(|provider| BUNDLED_PROVIDERS.contains(&provider))
                && matches!(mode, Some("chat" | "responses"))
        })
        .collect();
    let entries =
        entries(&bytes(&serde_json::to_value(kept).expect("it's JSON"))).expect("it prices models");
    // One model a line, sorted, so a refresh diffs by model.
    let mut file = String::from("{\n");
    let last = entries.len() - 1;
    for (at, (model, entry)) in entries.iter().enumerate() {
        let model = serde_json::to_string(model).expect("it writes");
        let entry = serde_json::to_string(entry).expect("it writes");
        let comma = if at == last { "" } else { "," };
        file.push_str(&format!("  {model}: {entry}{comma}\n"));
    }
    file.push_str("}\n");
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prices/bundled.json");
    std::fs::write(out, file).expect("it's written");
}

#[test]
fn a_pipe_where_a_prices_file_should_be_never_hangs() {
    let folder = Folder::new("prices-pipe");
    let prices = folder.path().join("prices");
    std::fs::create_dir_all(&prices).expect("it's made");
    crate::testing::pipe(&prices.join("own.json"));
    crate::testing::pipe(&prices.join("litellm.json"));
    let path = folder.path().to_owned();
    let limit = std::time::Duration::from_secs(10);

    let (table, at, set) = crate::testing::within(limit, move || {
        (
            load(&path),
            fetched_at(&path),
            set_own(&path, "mine", Some(price(1.0, 1.0))),
        )
    });
    // Read as absent; never written over.
    assert_eq!(table, bundled());
    assert_eq!(at, None);
    let error = set.expect_err("a pipe isn't read as prices");
    crate::testing::names_only(&error, folder.path(), "prices/own.json");
}

#[test]
fn a_prices_lock_held_elsewhere_is_given_up_on() {
    let folder = Folder::new("prices-held");
    let prices = folder.path().join("prices");
    std::fs::create_dir_all(&prices).expect("it's made");
    let _held = crate::testing::hold(&prices.join(".lock"));
    let path = folder.path().to_owned();
    let limit = std::time::Duration::from_secs(15);

    let started = std::time::Instant::now();
    let (own, fetched) = crate::testing::within(limit, move || {
        std::thread::scope(|scope| {
            let own = scope.spawn(|| set_own(&path, "mine", Some(price(1.0, 1.0))));
            let fetched = scope.spawn(|| {
                let table =
                    json!({"m": {"input_cost_per_token": 1e-06, "output_cost_per_token": 1e-06}});
                take_fetched(&path, &bytes(&table), Wall(1))
            });
            (
                own.join().expect("it ends"),
                fetched.join().expect("it ends"),
            )
        })
    });
    let took = started.elapsed();
    let own = own.expect_err("it gives up");
    assert_eq!(own.kind(), std::io::ErrorKind::TimedOut);
    crate::testing::names_only(&own, folder.path(), "prices/.lock");
    let Err(PriceError::Io(fetched)) = fetched else {
        panic!("it gives up: {fetched:?}");
    };
    assert_eq!(fetched.kind(), std::io::ErrorKind::TimedOut);
    crate::testing::names_only(&fetched, folder.path(), "prices/.lock");
    assert!(took >= crate::files::LOCK_WAIT, "{took:?}");
    assert!(!prices.join("own.json").exists());
}
