//! The prices cost is estimated with, in three layers, first match wins:
//!
//! 1. **the user's own**, in `prices/own.json` beside the ledger, so every
//!    CAF app agrees ([`set_own`]);
//! 2. **LiteLLM's table**, as last fetched, in `prices/litellm.json`
//!    ([`take_fetched`]): fetched by the usage driver from [`LITELLM`] once
//!    a day, when the host allows it;
//! 3. **the snapshot bundled with CAF** ([`bundled`]), taken from the same
//!    table when CAF was released: what prices are before the first fetch,
//!    offline, or with fetching off.
//!
//! [`load`] puts them together into the [`PriceTable`] the core is given.
//! Only the fields CAF prices with are read from LiteLLM's table: the
//! per-token rates for input, cache reads and writes and output, and fast
//! mode's multiplier. Its rates for prompts past a length are left out:
//! they apply per request, and CAF has each turn's totals. So are its
//! one-hour cache writes: ACP doesn't say which kind a turn made.
//!
//! # The files
//!
//! ```text
//! prices/own.json       {"models": {"<model>": ModelPrice}}, the user's own
//! prices/litellm.json   {"fetched": Wall, "table": {...}}, LiteLLM's table
//!                       as fetched, cut to the fields CAF reads
//! ```
//!
//! Each is written whole and replaced in one step, under the folder's
//! lock, so a reader sees one whole file or the other. Fields are only
//! ever added: readers ignore those they don't know, and the user's own
//! prices keep them when one model's price is set.
//!
//! # The bundled snapshot
//!
//! `prices/bundled.json` in this crate, in LiteLLM's shape, cut to the
//! fields CAF reads and the models of the providers CAF's agents use
//! (Anthropic, OpenAI, Google's Gemini and xAI). To refresh it, fetch the
//! table and run the ignored test that rewrites it:
//!
//! ```sh
//! curl -fL -o /tmp/litellm.json \
//!   https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json
//! CAF_LITELLM=/tmp/litellm.json cargo test -p caf-usage -- --ignored refresh_bundled
//! ```

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use caf_core::time::Wall;
use caf_core::usage::{ModelPrice, PriceLayer, PriceTable, PricedModel};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::files;

#[cfg(test)]
mod tests;

/// Where LiteLLM's table is fetched from: its main branch on GitHub.
pub const LITELLM: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// How often the usage driver fetches LiteLLM's table: once a day.
pub const FETCH_EVERY: std::time::Duration = std::time::Duration::from_hours(24);

/// The folder prices are kept in, inside the ledger's.
const FOLDER: &str = "prices";
/// The user's own prices.
const OWN: &str = "own.json";
/// LiteLLM's table, as last fetched.
const FETCHED: &str = "litellm.json";
/// The snapshot bundled with CAF.
const BUNDLED: &str = include_str!("../prices/bundled.json");
/// LiteLLM's example entry, which documents the fields: no model.
const SAMPLE: &str = "sample_spec";

/// LiteLLM's table can't be read.
#[non_exhaustive]
#[derive(Debug)]
pub enum PriceError {
    /// It isn't the JSON object of models LiteLLM's table is, as serde says.
    NotTable(String),
    /// It prices no model at all, so it's taken to be broken, not used,
    /// and the last table fetched is kept.
    Empty,
    /// Writing it failed, as the system says.
    Io(io::Error),
}

impl std::fmt::Display for PriceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotTable(why) => write!(f, "not LiteLLM's price table: {why}"),
            Self::Empty => f.write_str("LiteLLM's price table prices no model"),
            Self::Io(error) => write!(f, "couldn't keep LiteLLM's price table: {error}"),
        }
    }
}

impl std::error::Error for PriceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::NotTable(_) | Self::Empty => None,
        }
    }
}

impl From<io::Error> for PriceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// One model's entry in LiteLLM's table, as read: only the fields CAF
/// prices with. Rates are US dollars per token.
#[derive(Deserialize)]
struct Entry {
    input_cost_per_token: f64,
    output_cost_per_token: f64,
    #[serde(default)]
    cache_read_input_token_cost: Option<f64>,
    #[serde(default)]
    cache_creation_input_token_cost: Option<f64>,
    /// Read loosely: it holds other providers' numbers too, of any shape.
    #[serde(default)]
    provider_specific_entry: Option<Value>,
}

/// An entry as CAF keeps it, cut to what it reads: LiteLLM's shape still.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Trimmed {
    input_cost_per_token: f64,
    output_cost_per_token: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_read_input_token_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_creation_input_token_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_specific_entry: Option<Fast>,
}

/// Fast mode's multiplier, where LiteLLM keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Fast {
    fast: f64,
}

impl Entry {
    /// The entry, cut, if its rates are rates: none negative, and fast
    /// mode's multiplier, if any, above nothing.
    fn trimmed(self) -> Option<Trimmed> {
        let rates = [
            Some(self.input_cost_per_token),
            Some(self.output_cost_per_token),
            self.cache_read_input_token_cost,
            self.cache_creation_input_token_cost,
        ];
        if !rates
            .into_iter()
            .flatten()
            .all(|rate| rate.is_finite() && rate >= 0.0)
        {
            return None;
        }
        let fast = self
            .provider_specific_entry
            .as_ref()
            .and_then(|entry| entry.get("fast"))
            .and_then(Value::as_f64)
            .filter(|fast| fast.is_finite() && *fast > 0.0);
        Some(Trimmed {
            input_cost_per_token: self.input_cost_per_token,
            output_cost_per_token: self.output_cost_per_token,
            cache_read_input_token_cost: self.cache_read_input_token_cost,
            cache_creation_input_token_cost: self.cache_creation_input_token_cost,
            provider_specific_entry: fast.map(|fast| Fast { fast }),
        })
    }
}

impl Trimmed {
    /// Its prices, per million tokens, the cache's at the input rate where
    /// it gives none.
    fn price(&self) -> ModelPrice {
        let input = self.input_cost_per_token;
        let mut price =
            ModelPrice::new(per_million(input), per_million(self.output_cost_per_token));
        price.cache_read = per_million(self.cache_read_input_token_cost.unwrap_or(input));
        price.cache_write = per_million(self.cache_creation_input_token_cost.unwrap_or(input));
        price.fast = self.provider_specific_entry.as_ref().map(|fast| fast.fast);
        price
    }
}

/// A rate per token as one per million, to a billionth of a dollar, so
/// `3e-7` is `0.3`, not `0.30000000000000004`.
fn per_million(per_token: f64) -> f64 {
    (per_token * 1e15).round() / 1e9
}

/// The entries of LiteLLM's table that price a model, cut. An entry that
/// doesn't is left out, not an error.
fn entries(json: &[u8]) -> Result<BTreeMap<String, Trimmed>, PriceError> {
    let table: BTreeMap<String, &RawValue> =
        serde_json::from_slice(json).map_err(|error| PriceError::NotTable(error.to_string()))?;
    let entries: BTreeMap<String, Trimmed> = table
        .into_iter()
        .filter(|(model, _)| model != SAMPLE && !model.is_empty())
        .filter_map(|(model, entry)| {
            let entry: Entry = serde_json::from_str(entry.get()).ok()?;
            Some((model, entry.trimmed()?))
        })
        .collect();
    if entries.is_empty() {
        return Err(PriceError::Empty);
    }
    Ok(entries)
}

/// Prices by model, from `layer`, as a table.
fn table(prices: Vec<(String, ModelPrice)>, layer: PriceLayer) -> PriceTable {
    let mut table = PriceTable::default();
    table.models = prices
        .into_iter()
        .map(|(model, price)| PricedModel::new(model, price, layer))
        .collect();
    table
        .models
        .sort_by(|one, other| one.model.cmp(&other.model));
    table
}

/// The snapshot of LiteLLM's table bundled with CAF, cut to the fields CAF
/// prices with, every model's layer [`Bundled`](caf_core::usage::PriceLayer::Bundled).
#[must_use]
pub fn bundled() -> PriceTable {
    static TABLE: OnceLock<PriceTable> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            // It reads: a test checks it.
            let prices = read_litellm(BUNDLED.as_bytes()).unwrap_or_default();
            table(prices, PriceLayer::Bundled)
        })
        .clone()
}

/// The prices in force: the user's own, then LiteLLM's as last fetched,
/// then the bundled snapshot. A file that can't be read counts as absent,
/// and the next layer prices its models.
#[must_use]
pub fn load(folder: &Path) -> PriceTable {
    let folder = folder.join(FOLDER);
    let mut models: BTreeMap<String, PricedModel> = BTreeMap::new();
    for (model, price) in own(&folder) {
        models.insert(
            model.clone(),
            PricedModel::new(model, price, PriceLayer::Own),
        );
    }
    let fetched = fetched(&folder);
    if let Some((_, prices)) = &fetched {
        for (model, price) in prices {
            models
                .entry(model.clone())
                .or_insert_with(|| PricedModel::new(model, price.clone(), PriceLayer::Fetched));
        }
    }
    for priced in bundled().models {
        models.entry(priced.model.clone()).or_insert(priced);
    }
    let mut table = PriceTable::default();
    table.models = models.into_values().collect();
    table.fetched = fetched.map(|(at, _)| at);
    table
}

/// The user's own prices, as kept: none if the file isn't there or doesn't
/// read, and a model's left out if its price doesn't.
fn own(folder: &Path) -> Vec<(String, ModelPrice)> {
    let Ok(Some(Value::Object(mut file))) = read_own(folder) else {
        return Vec::new();
    };
    let Some(Value::Object(models)) = file.remove("models") else {
        return Vec::new();
    };
    models
        .into_iter()
        .filter_map(|(model, price)| Some((model, serde_json::from_value(price).ok()?)))
        .collect()
}

/// The user's own prices file, as JSON: `None` if it isn't there.
fn read_own(folder: &Path) -> io::Result<Option<Value>> {
    let Some(bytes) = files::read(&folder.join(OWN))? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// LiteLLM's table as last fetched, as kept.
#[derive(Serialize, Deserialize)]
struct Kept<T> {
    /// When it was fetched.
    fetched: Wall,
    /// The table, cut.
    table: T,
}

/// LiteLLM's table as last fetched, and when: `None` if it isn't there or
/// doesn't read.
fn fetched(folder: &Path) -> Option<(Wall, Vec<(String, ModelPrice)>)> {
    let bytes = files::read(&folder.join(FETCHED)).ok()??;
    let kept: Kept<&RawValue> = serde_json::from_slice(&bytes).ok()?;
    let prices = read_litellm(kept.table.get().as_bytes()).ok()?;
    Some((kept.fetched, prices))
}

/// Reads LiteLLM's table, as fetched, into prices by its names for models:
/// each model with at least input and output rates, per million tokens.
///
/// # Errors
///
/// [`PriceError::NotTable`] if it isn't the table, or
/// [`PriceError::Empty`] if it prices nothing.
pub fn read_litellm(json: &[u8]) -> Result<Vec<(String, ModelPrice)>, PriceError> {
    Ok(entries(json)?
        .into_iter()
        .map(|(model, entry)| {
            let price = entry.price();
            (model, price)
        })
        .collect())
}

/// Keeps LiteLLM's table as just fetched, at `at`, if it reads: written
/// beside the ledger whole, replacing the last in one step. The next
/// [`load`] uses it.
///
/// # Errors
///
/// Why it isn't kept: it doesn't read ([`read_litellm`]), or writing it
/// failed. The last table fetched stays.
pub fn take_fetched(folder: &Path, json: &[u8], at: Wall) -> Result<(), PriceError> {
    let table = entries(json)?;
    let bytes = serde_json::to_vec(&Kept { fetched: at, table })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let folder = folder.join(FOLDER);
    files::make_folder(&folder)?;
    let _lock = files::lock(&folder)?;
    files::replace(&folder, FETCHED, &bytes)?;
    Ok(())
}

/// When LiteLLM's table was last fetched, if it ever was: what the usage
/// driver goes by to fetch it once a day, across every CAF app.
#[must_use]
pub fn fetched_at(folder: &Path) -> Option<Wall> {
    let bytes = files::read(&folder.join(FOLDER).join(FETCHED)).ok()??;
    let kept: Kept<serde::de::IgnoredAny> = serde_json::from_slice(&bytes).ok()?;
    Some(kept.fetched)
}

/// Sets the user's own price for a model, by the agent's id for it, or
/// with `None` clears it. Kept beside the ledger, under a lock, so every
/// CAF app agrees.
///
/// # Errors
///
/// What the system said, if it couldn't: the prices are as they were.
/// [`InvalidInput`](io::ErrorKind::InvalidInput) for a model with no name
/// or a price that's negative or not a number, and
/// [`InvalidData`](io::ErrorKind::InvalidData) if the prices kept don't
/// read, so as not to write over them.
pub fn set_own(folder: &Path, model: &str, price: Option<ModelPrice>) -> io::Result<()> {
    let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, why.to_owned());
    if model.is_empty() {
        return Err(invalid("a price needs a model"));
    }
    if let Some(price) = &price {
        let rates = [
            Some(price.input),
            Some(price.cache_read),
            Some(price.cache_write),
            Some(price.output),
            price.fast,
        ];
        if !rates
            .into_iter()
            .flatten()
            .all(|rate| rate.is_finite() && rate >= 0.0)
        {
            return Err(invalid("a price is a number, not below nothing"));
        }
    }
    let folder = folder.join(FOLDER);
    files::make_folder(&folder)?;
    let _lock = files::lock(&folder)?;
    let mut file = match read_own(&folder)? {
        Some(Value::Object(file)) => file,
        None => Map::new(),
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the user's own prices aren't an object",
            ));
        }
    };
    let models = file
        .entry("models")
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(models) = models else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the user's own prices have no models",
        ));
    };
    match price {
        Some(price) => {
            let price = serde_json::to_value(price).map_err(io::Error::other)?;
            models.insert(model.to_owned(), price);
        }
        None => {
            models.remove(model);
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
    bytes.push(b'\n');
    files::replace(&folder, OWN, &bytes)
}
