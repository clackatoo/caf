//! Usage: what each turn spent, what each chat has spent so far, the
//! prices CAF estimates cost with, and the shapes of the usage ledger every
//! CAF app on a machine shares.
//!
//! # Where it comes from
//!
//! - **Tokens, per turn.** When a turn ends, the agent's answer to the
//!   prompt carries its tokens (ACP's `usage`, still unstable), and an
//!   agent that splits them by model says so too (`_meta.quota.model_usage`,
//!   which Claude's and Codex's adapters both send). The split is the
//!   fuller count, with subagents and compaction in it, so it's what CAF
//!   keeps when there is one; otherwise one row, against the turn's model
//!   ([`TurnUsage::models`]). An agent that says nothing gives no record.
//! - **Cost, as the agent reports it.** Claude says what its session has
//!   cost so far ([`Usage::cost`](crate::chat::Usage::cost)); a turn's cost
//!   is how much that rose between the turn's start and end. If it went
//!   down, the session was replaced, and the turn's cost is what it says
//!   at the end ([`TurnUsage::cost`]).
//! - **Cost, estimated.** A turn with no reported cost is priced from a
//!   [`PriceTable`]: the user's own prices, then the table fetched from
//!   LiteLLM, then the snapshot bundled with CAF. Estimates are never
//!   stored: they're worked out when shown, with the prices of the time,
//!   so a price corrected corrects every figure ([`Spend`], [`Summary`]).
//!
//! # The ledger
//!
//! Every turn's record also goes to the **usage ledger**: one per user and
//! machine, shared by every CAF app on it, so usage can be summed by app.
//! The core says what to write ([`LedgerLine`], through
//! [`UsageDirective::Record`](crate::directive::UsageDirective::Record));
//! the usage driver writes it, and a summary is read from it
//! ([`UsageQuery`], [`Summary`]) without the core, as it touches no chat.
//! It never holds what was said, or the folder a chat works in.
//!
//! # Guarantees
//!
//! Each of these has a test.
//!
//! 1. **A turn's usage is recorded once, when it ends**, and only if the
//!    agent said what it spent: on the turn, in the chat's [`Spend`], and
//!    as one ledger line.
//! 2. **A chat's spend is the sum of its turns' records**, priced with the
//!    table the core has now: it's sent again whenever that changes.
//! 3. **A reported cost is never replaced by an estimate**, and an estimate
//!    is never stored.
//! 4. **Every number here is below 2^53**, as everything CAF writes is: a
//!    count an agent gives that isn't is unreadable, and the turn gets no
//!    record, with a report.

use serde::{Deserialize, Serialize};

use crate::chat::Cost;
use crate::ids::{AgentName, ChatId, TurnId};
use crate::time::Wall;

/// Token counts, by kind. Every count is a whole number of tokens.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Tokens {
    /// Input read fresh, not from the cache.
    pub input: u64,
    /// Input read from the cache.
    pub cache_read: u64,
    /// Input written to the cache.
    pub cache_write: u64,
    /// Output, thinking included.
    pub output: u64,
    /// How much of `output` was thinking, if the agent says.
    pub thinking: Option<u64>,
}

impl Tokens {
    /// Every token: input, both cache counts and output. Thinking is part
    /// of output, so isn't counted again.
    #[must_use]
    pub fn total(&self) -> u64 {
        todo!()
    }

    /// Adds another's counts to these. Thinking stays unknown only if
    /// neither knows it.
    pub fn add(&mut self, other: &Tokens) {
        let _ = other;
        todo!()
    }
}

/// What a turn spent on one model.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ModelTokens {
    /// The model, by the agent's id for it, such as `claude-haiku-5-5`.
    pub model: String,
    /// Its tokens.
    pub tokens: Tokens,
}

/// What a turn spent: its [`spent`](crate::chat::Turn::spent), recorded
/// when it ends, if the agent said.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TurnUsage {
    /// Its tokens, one row per model. They're the agent's split by model,
    /// if it gave one, which counts the subagents and compaction the turn
    /// set off; otherwise one row, of its totals, against the turn's model
    /// (or `unknown`, if the agent never said which). Never empty.
    pub models: Vec<ModelTokens>,
    /// What it cost, as the agent reported it, if it did: how much its
    /// session's cost rose over the turn. Never an estimate. A cost in a
    /// currency other than US dollars is kept here as reported, but
    /// counted as unreported in [`Spend`] and [`Summary`], which estimate
    /// it instead: CAF converts no currency.
    pub cost: Option<Cost>,
    /// Whether fast mode was on, by the settings in effect when it was
    /// sent: what prices it at the fast rate.
    pub fast: bool,
}

impl TurnUsage {
    /// Its tokens, every model's added up.
    #[must_use]
    pub fn tokens(&self) -> Tokens {
        todo!()
    }
}

/// What a chat has spent, over the turns that have a record: its
/// [`spend`](crate::chat::Chat::spend). Money is in US dollars.
///
/// It's worked out by the core, with the prices it has now, and sent again
/// ([`Change::Spend`](crate::Change::Spend)) whenever a turn adds to it or
/// the prices change, so every viewer shows the same figure. It's saved
/// with the chat, but never trusted: a restored chat's is worked out again
/// from its turns, so the turns stay the only record. Turns from
/// before CAF recorded usage, and imported ones, have no record: it counts
/// from when recording began.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Spend {
    /// Every turn's tokens, added up.
    pub tokens: Tokens,
    /// The cost the agent reported, added up.
    pub reported: f64,
    /// The cost estimated for turns it didn't report, added up.
    pub estimated: f64,
    /// Tokens no price covered: of turns with no reported cost, on models
    /// the price table doesn't know. Their cost is in neither figure.
    pub unpriced: u64,
    /// How many turns it adds up.
    pub turns: u64,
}

impl Spend {
    /// The cost, reported and estimated together.
    #[must_use]
    pub fn cost(&self) -> f64 {
        todo!()
    }
}

/// A model's prices, in US dollars per million tokens.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ModelPrice {
    /// Input read fresh.
    pub input: f64,
    /// Input read from the cache.
    pub cache_read: f64,
    /// Input written to the cache, at the five-minute rate: ACP doesn't say
    /// which kind of write a turn made.
    pub cache_write: f64,
    /// Output, thinking included.
    pub output: f64,
    /// What fast mode multiplies every rate by, if the model has it.
    pub fast: Option<f64>,
}

impl ModelPrice {
    /// Prices of `input` and `output` per million tokens, with the cache
    /// rates the same as `input` and no fast mode: the least a user need
    /// say to price a model.
    #[must_use]
    pub fn new(input: f64, output: f64) -> Self {
        let _ = (input, output);
        todo!()
    }
}

/// Where a price came from, first match wins.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum PriceLayer {
    /// The user's own, kept beside the ledger, so every app agrees.
    Own,
    /// LiteLLM's table, as last fetched.
    Fetched,
    /// The snapshot of LiteLLM's table that came with CAF.
    Bundled,
}

/// One model's price, and where it came from.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PricedModel {
    /// The model, by the table's name for it, such as
    /// `anthropic/claude-haiku-5-5`.
    pub model: String,
    /// Its price.
    pub price: ModelPrice,
    /// Where it came from.
    pub layer: PriceLayer,
}

/// The prices CAF estimates cost with: each model's, from the first layer
/// that has it. The usage driver builds it and hands it to the core
/// ([`Event::Prices`](crate::Event::Prices)); until then, the core has
/// none, and nothing is estimated.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PriceTable {
    /// Every model it prices, sorted by name.
    pub models: Vec<PricedModel>,
    /// When LiteLLM's table was last fetched, if ever.
    pub fetched: Option<Wall>,
}

/// A turn's cost, estimated.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Estimate {
    /// What the models the table knows cost, in US dollars.
    pub usd: f64,
    /// Tokens on models it doesn't know.
    pub unpriced: u64,
}

impl PriceTable {
    /// A model's price, by the agent's id for it: the table's entry of
    /// that name; else one that's it with a provider before it
    /// (`anthropic/claude-haiku-5-5`); else, if it ends in a date
    /// (`claude-haiku-5-5-20261001`), the same without it. A name that
    /// matches more than one entry with a provider is priced by the first,
    /// sorted by name.
    #[must_use]
    pub fn find(&self, model: &str) -> Option<&PricedModel> {
        let _ = model;
        todo!()
    }

    /// What a turn would cost at these prices: each model's tokens at its
    /// rates, times its fast multiplier if fast mode was on. Its reported
    /// cost, if any, plays no part: [`Spend`] and [`Summary`] use an
    /// estimate only for turns without one.
    #[must_use]
    pub fn estimate(&self, usage: &TurnUsage) -> Estimate {
        let _ = usage;
        todo!()
    }
}

/// One turn, as the usage ledger keeps it: a line in its app's file for
/// the month the turn ended in.
///
/// Lines never change once written. A later CAF may add fields, which
/// earlier ones ignore, so every line ever written is read by every CAF.
/// The same turn written twice, say after a crash, counts once: a reader
/// keeps the first line for each `(chat, turn)`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct LedgerLine {
    /// When the turn ended.
    pub at: Wall,
    /// The app, by its host's [`name`](crate::Host::name), such as
    /// `clackatoo`.
    pub app: String,
    /// The chat.
    pub chat: ChatId,
    /// The turn.
    pub turn: TurnId,
    /// The agent, by the app's name for it.
    pub agent: AgentName,
    /// The agent's built-in profile, if it has one, such as
    /// `claude-agent-acp`: what tells the same agent apart across apps
    /// that name it differently.
    pub profile: Option<String>,
    /// What the turn spent.
    pub usage: TurnUsage,
}

/// What a usage summary covers.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct UsageQuery {
    /// From when, inclusive.
    pub since: Wall,
    /// Until when, exclusive.
    pub until: Wall,
    /// The time zone days begin in, as an IANA name such as
    /// `Europe/London`, so a day is the user's own.
    pub zone: String,
    /// Whether buckets are hours or days.
    pub by: Resolution,
    /// Only these apps, by name; all if empty.
    pub apps: Vec<String>,
    /// Only these agents, by [`Bucket::agent`]; all if empty.
    pub agents: Vec<String>,
}

impl UsageQuery {
    /// Every app and agent between `since` and `until`, by day in `zone`.
    #[must_use]
    pub fn new(since: Wall, until: Wall, zone: impl Into<String>) -> Self {
        let _ = (since, until, zone.into());
        todo!()
    }
}

/// How finely a summary is cut.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum Resolution {
    /// By hour.
    Hour,
    /// By day, in the query's time zone.
    Day,
}

/// A query a summary can't be made for.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "value")]
pub enum BadQuery {
    /// The time zone isn't one CAF knows.
    Zone(String),
    /// `until` isn't after `since`, or the period is longer than 400 days.
    Period,
}

/// Usage over a period: what the usage page is drawn from. Every view of
/// it (by agent, app, model or day; totals by kind; shares) is a sum of
/// its buckets, which the SDKs have helpers for.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Summary {
    /// What it covers.
    pub query: UsageQuery,
    /// The buckets, by start, then app, agent, model and fast mode. Only
    /// those with turns in them.
    pub buckets: Vec<Bucket>,
    /// How many chats it covers. Not a sum of buckets: a chat spanning
    /// days or models is in several.
    pub chats: ChatCounts,
    /// The price of every model in it that has one, as used.
    pub prices: Vec<PricedModel>,
    /// When LiteLLM's table was last fetched, if ever.
    pub fetched: Option<Wall>,
    /// Ledger files that couldn't be read, by name, such as
    /// `eyespy/2026-10.jsonl.zst`: the summary is partial if any are here.
    pub unreadable: Vec<String>,
}

/// One period's usage by one app, agent, model and fast mode.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Bucket {
    /// When the period starts.
    pub start: Wall,
    /// The app, by name.
    pub app: String,
    /// The agent: its built-in profile if it has one, such as
    /// `claude-agent-acp`, so one agent named differently by two apps is
    /// one; otherwise the app's name for it.
    pub agent: String,
    /// The model, by the agent's id for it.
    pub model: String,
    /// Whether fast mode was on.
    pub fast: bool,
    /// The tokens.
    pub tokens: Tokens,
    /// The cost agents reported, in US dollars. A turn's reported cost on
    /// several models is shared between their buckets in proportion to
    /// their estimates, or to their tokens if none has a price.
    pub reported: f64,
    /// The cost estimated for turns with none reported, in US dollars.
    pub estimated: f64,
    /// Tokens no price covered.
    pub unpriced: u64,
    /// How many turns.
    pub turns: u64,
}

/// How many chats a summary covers.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ChatCounts {
    /// In all.
    pub all: u64,
    /// Per app, by name.
    pub by_app: Vec<(String, u64)>,
    /// Per agent, as [`Bucket::agent`].
    pub by_agent: Vec<(String, u64)>,
}
