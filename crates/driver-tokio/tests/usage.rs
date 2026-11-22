//! Usage: the ledger and the prices, as CAF on tokio keeps them, against
//! the fake adapter. Every ledger is in the test's own folder.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use caf_core::OpenChat;
use caf_core::chat::{NoAppItems, UserMessage};
use caf_core::event::Event;
use caf_core::time::Wall;
use caf_core::usage::{BadQuery, ModelPrice, PriceLayer, PriceTable, UsageQuery};
use caf_driver_tokio::{Caf, Error, Step, Steps};
use serde_json::{Value, json};

use common::*;

/// The wall clock now.
fn now() -> Wall {
    let since = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    Wall(i64::try_from(since.as_millis()).unwrap())
}

/// The hour either side of now, by day in UTC.
fn lately() -> UsageQuery {
    UsageQuery::new(Wall(now().0 - 3_600_000), Wall(now().0 + 3_600_000), "UTC")
}

/// Ends the prompt's turn, saying what it spent: `output` tokens out.
fn end_turn_spending(output: u64) -> Value {
    json!({"respond": {"to": "prompt", "result": {"stopReason": "end_turn", "usage": {
        "totalTokens": 100 + output,
        "inputTokens": 100,
        "outputTokens": output,
    }}}})
}

/// Every price table the core is given, as steps show them.
#[derive(Clone, Default)]
struct Prices(Arc<Mutex<Vec<PriceTable>>>);

impl Steps<NoAppItems> for Prices {
    fn step(&mut self, step: Step<'_, NoAppItems>) {
        if let Event::Prices(prices) = step.event {
            self.0.lock().unwrap().push(prices.clone());
        }
    }
}

impl Prices {
    fn given(&self) -> Vec<PriceTable> {
        self.0.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn with_no_ledger_the_prices_are_the_bundled_snapshot() {
    let given = Prices::default();
    let (caf, running) = Caf::start_watching(config_for(host(), Vec::new()), given.clone());
    let bundled = caf_usage::prices::bundled();
    assert_eq!(caf.prices().await.unwrap(), bundled);
    assert_eq!(
        given.given(),
        [bundled],
        "given to the core once, as it starts"
    );
    shut_down(&caf, running).await;
}

#[tokio::test]
async fn the_prices_are_read_from_the_ledgers_folder_as_caf_starts() {
    let dir = Dir::new();
    let folder = dir.path("usage");
    caf_usage::prices::set_own(&folder, "claude-test-1", Some(ModelPrice::new(3.0, 4.0))).unwrap();
    let (caf, running) = Caf::start(config_with_ledger(&dir, host(), Vec::new()));
    let prices = caf.prices().await.unwrap();
    assert_eq!(prices, caf_usage::prices::load(&folder));
    let own = prices.find("claude-test-1").expect("the user's own");
    assert_eq!((own.price.input, own.layer), (3.0, PriceLayer::Own));
    shut_down(&caf, running).await;
}

#[tokio::test]
async fn a_price_set_is_in_force_once_set_and_every_app_sees_it() {
    let dir = Dir::new();
    let given = Prices::default();
    let config = config_with_ledger(&dir, host(), Vec::new());
    let (caf, running) = Caf::start_watching(config, given.clone());
    let model = "claude-test-1";
    assert!(caf.prices().await.unwrap().find(model).is_none());
    caf.set_price(model.into(), Some(ModelPrice::new(5.0, 6.0)))
        .await
        .unwrap();
    let prices = caf.prices().await.unwrap();
    let own = prices.find(model).expect("set");
    assert_eq!((own.price.output, own.layer), (6.0, PriceLayer::Own));
    assert_eq!(given.given().last(), Some(&prices), "the core has it");
    let elsewhere = caf_usage::prices::load(&dir.path("usage"));
    assert_eq!(elsewhere.find(model), Some(own), "beside the ledger");
    caf.set_price(model.into(), None).await.unwrap();
    assert!(caf.prices().await.unwrap().find(model).is_none(), "cleared");
    assert_eq!(given.given().len(), 3, "as it started, and each time");
    shut_down(&caf, running).await;
}

#[tokio::test]
async fn with_no_ledger_theres_no_summary_and_no_price_of_ones_own() {
    let (caf, running) = Caf::start(config_for(host(), Vec::new()));
    assert_eq!(caf.usage(lately()).await, Err(Error::NoLedger));
    let price = Some(ModelPrice::new(1.0, 2.0));
    assert_eq!(
        caf.set_price("claude-test-1".into(), price).await,
        Err(Error::NoLedger)
    );
    shut_down(&caf, running).await;
}

#[tokio::test]
async fn a_price_that_cant_be_written_says_why() {
    let dir = Dir::new();
    // A file where the ledger's folder should be.
    std::fs::write(dir.path("usage"), "in the way").unwrap();
    let (caf, running) = Caf::start(config_with_ledger(&dir, host(), Vec::new()));
    let set = caf
        .set_price("claude-test-1".into(), Some(ModelPrice::new(1.0, 2.0)))
        .await;
    assert!(matches!(set, Err(Error::Price(_))), "{set:?}");
    assert!(caf.prices().await.unwrap().find("claude-test-1").is_none());
    shut_down(&caf, running).await;
}

#[tokio::test]
async fn a_summary_cant_be_made_for_a_bad_query() {
    let dir = Dir::new();
    let (caf, running) = Caf::start(config_with_ledger(&dir, host(), Vec::new()));
    let mut query = lately();
    query.zone = "Nowhere/Atlantis".into();
    assert_eq!(
        caf.usage(query).await,
        Err(Error::BadQuery(BadQuery::Zone("Nowhere/Atlantis".into())))
    );
    let mut query = lately();
    query.until = Wall(query.since.0 + 401 * 86_400_000);
    assert_eq!(
        caf.usage(query).await,
        Err(Error::BadQuery(BadQuery::Period))
    );
    let summary = caf.usage(lately()).await.unwrap();
    assert!(summary.buckets.is_empty(), "nothing was spent");
    shut_down(&caf, running).await;
}

#[tokio::test]
async fn once_caf_has_shut_down_usage_is_gone() {
    let dir = Dir::new();
    let (caf, running) = Caf::start(config_with_ledger(&dir, host(), Vec::new()));
    let gone = caf.clone();
    shut_down(&caf, running).await;
    assert_eq!(gone.usage(lately()).await, Err(Error::Gone));
    assert_eq!(gone.prices().await, Err(Error::Gone));
    assert_eq!(gone.set_price("m".into(), None).await, Err(Error::Gone));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_turns_usage_reaches_the_ledger() {
    let dir = Dir::new();
    let script = dir.script(
        "script.jsonl",
        &after_handshake(&[
            prompt(),
            end_turn_spending(5),
            prompt(),
            end_turn_spending(7),
        ]),
    );
    let config = config_with_ledger(&dir, host(), vec![fake(&dir, &script)]);
    let (caf, running) = Caf::start(config);
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    for turn in 1..=2 {
        chat.send(UserMessage::text("Go")).await.unwrap();
        until(&chat, &mut watch, "the turn to finish", |now| {
            now.turns.len() == turn && finished(now)
        })
        .await;
    }
    shut_down(&caf, running).await;
    // Every line is written by the time CAF has shut down.
    let summary =
        caf_usage::summarise(&dir.path("usage"), &lately(), &PriceTable::default()).unwrap();
    let turns: u64 = summary.buckets.iter().map(|bucket| bucket.turns).sum();
    let output: u64 = (summary.buckets.iter())
        .map(|bucket| bucket.tokens.output)
        .sum();
    assert_eq!((turns, output), (2, 12));
    assert!(
        (summary.buckets.iter()).all(|bucket| bucket.app == host().name),
        "as this app"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ledger_that_cant_be_written_never_holds_up_a_chat() {
    let dir = Dir::new();
    std::fs::write(dir.path("usage"), "in the way").unwrap();
    let script = dir.script(
        "script.jsonl",
        &after_handshake(&[
            prompt(),
            end_turn_spending(5),
            prompt(),
            end_turn_spending(7),
        ]),
    );
    let config = config_with_ledger(&dir, host(), vec![fake(&dir, &script)]);
    let (caf, running) = Caf::start(config);
    let chat = caf.open_chat(OpenChat::new("fake", &dir.0)).await.unwrap();
    let mut watch = chat.watch();
    for turn in 1..=2 {
        chat.send(UserMessage::text("Go")).await.unwrap();
        let now = until(&chat, &mut watch, "the turn to finish", |now| {
            now.turns.len() == turn && finished(now)
        })
        .await;
        assert_eq!(
            now.spend.turns,
            u64::try_from(turn).unwrap(),
            "spend is kept"
        );
    }
    // Shutting down tries the lines once more, and isn't held up either.
    let shut = tokio::time::timeout(Duration::from_secs(30), shut_down(&caf, running));
    shut.await.expect("shut down");
}
