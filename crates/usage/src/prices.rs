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
//! they apply per request, and CAF has each turn's totals.

use std::io;
use std::path::Path;

use caf_core::time::Wall;
use caf_core::usage::{ModelPrice, PriceTable};

/// Where LiteLLM's table is fetched from: its main branch on GitHub.
pub const LITELLM: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// How often the usage driver fetches LiteLLM's table: once a day.
pub const FETCH_EVERY: std::time::Duration = std::time::Duration::from_hours(24);

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

/// The snapshot of LiteLLM's table bundled with CAF, cut to the fields CAF
/// prices with, every model's layer [`Bundled`](caf_core::usage::PriceLayer::Bundled).
#[must_use]
pub fn bundled() -> PriceTable {
    todo!()
}

/// The prices in force: the user's own, then LiteLLM's as last fetched,
/// then the bundled snapshot. A file that can't be read counts as absent,
/// and the next layer prices its models.
#[must_use]
pub fn load(folder: &Path) -> PriceTable {
    let _ = folder;
    todo!()
}

/// Reads LiteLLM's table, as fetched, into prices by its names for models:
/// each model with at least input and output rates, per million tokens.
///
/// # Errors
///
/// [`PriceError::NotTable`] if it isn't the table, or
/// [`PriceError::Empty`] if it prices nothing.
pub fn read_litellm(json: &[u8]) -> Result<Vec<(String, ModelPrice)>, PriceError> {
    let _ = json;
    todo!()
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
    let _ = (folder, json, at);
    todo!()
}

/// When LiteLLM's table was last fetched, if it ever was: what the usage
/// driver goes by to fetch it once a day, across every CAF app.
#[must_use]
pub fn fetched_at(folder: &Path) -> Option<Wall> {
    let _ = folder;
    todo!()
}

/// Sets the user's own price for a model, by the agent's id for it, or
/// with `None` clears it. Kept beside the ledger, under a lock, so every
/// CAF app agrees.
///
/// # Errors
///
/// What the system said, if it couldn't: the prices are as they were.
pub fn set_own(folder: &Path, model: &str, price: Option<ModelPrice>) -> io::Result<()> {
    let _ = (folder, model, price);
    todo!()
}
