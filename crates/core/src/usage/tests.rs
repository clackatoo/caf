//! Tests of usage's arithmetic: totals, sums, and pricing by the table.

use crate::chat::Cost;
use crate::time::Wall;
use crate::usage::{
    Bucket, ChatCounts, Estimate, ModelPrice, ModelTokens, PriceLayer, PriceTable, PricedModel,
    Resolution, Spend, Summary, Tokens, TurnUsage, UsageQuery,
};

/// Tokens of each kind, thinking unknown.
fn tokens(input: u64, cache_read: u64, cache_write: u64, output: u64) -> Tokens {
    Tokens {
        input,
        cache_read,
        cache_write,
        output,
        thinking: None,
    }
}

/// A turn's usage, with no reported cost.
fn usage(rows: &[(&str, Tokens)], fast: bool) -> TurnUsage {
    TurnUsage {
        models: rows
            .iter()
            .map(|(model, tokens)| ModelTokens {
                model: (*model).into(),
                tokens: tokens.clone(),
            })
            .collect(),
        cost: None,
        fast,
    }
}

/// A table of these, from the user's own layer, sorted as a table is.
fn table(models: &[(&str, ModelPrice)]) -> PriceTable {
    let mut models: Vec<PricedModel> = models
        .iter()
        .map(|(model, price)| PricedModel {
            model: (*model).into(),
            price: price.clone(),
            layer: PriceLayer::Own,
        })
        .collect();
    models.sort_by(|one, other| one.model.cmp(&other.model));
    PriceTable {
        models,
        fetched: None,
    }
}

/// The name the table found `model` by.
fn found<'a>(table: &'a PriceTable, model: &str) -> Option<&'a str> {
    table.find(model).map(|priced| priced.model.as_str())
}

#[test]
fn a_total_counts_thinking_once() {
    let tokens = Tokens {
        thinking: Some(30),
        ..tokens(1, 20, 300, 4000)
    };
    assert_eq!(tokens.total(), 4321);
}

#[test]
fn adding_sums_every_kind() {
    let mut sum = tokens(1, 2, 3, 4);
    sum.add(&tokens(10, 20, 30, 40));
    assert_eq!(sum, tokens(11, 22, 33, 44));
}

#[test]
fn thinking_stays_unknown_only_if_neither_knows_it() {
    let known = Tokens {
        thinking: Some(5),
        ..tokens(0, 0, 0, 10)
    };
    let mut neither = tokens(0, 0, 0, 1);
    neither.add(&tokens(0, 0, 0, 1));
    assert_eq!(neither.thinking, None);

    let mut first = known.clone();
    first.add(&tokens(0, 0, 0, 1));
    assert_eq!(first.thinking, Some(5));

    let mut second = tokens(0, 0, 0, 1);
    second.add(&known);
    assert_eq!(second.thinking, Some(5));

    let mut both = known.clone();
    both.add(&known);
    assert_eq!(both.thinking, Some(10));
}

#[test]
fn a_turns_tokens_are_every_models_added_up() {
    let turn = usage(
        &[("one", tokens(1, 2, 3, 4)), ("two", tokens(10, 20, 30, 40))],
        false,
    );
    assert_eq!(turn.tokens(), tokens(11, 22, 33, 44));
}

#[test]
fn a_spends_cost_is_reported_and_estimated_together() {
    let spend = Spend {
        reported: 1.25,
        estimated: 0.5,
        ..Spend::default()
    };
    assert_eq!(spend.cost(), 1.75);
}

#[test]
fn a_new_price_charges_input_rates_for_the_cache() {
    assert_eq!(
        ModelPrice::new(3.0, 15.0),
        ModelPrice {
            input: 3.0,
            cache_read: 3.0,
            cache_write: 3.0,
            output: 15.0,
            fast: None,
        }
    );
}

#[test]
fn a_new_query_is_by_day_for_everyone() {
    let query = UsageQuery::new(Wall(1), Wall(2), "Europe/London");
    assert_eq!(
        query,
        UsageQuery {
            since: Wall(1),
            until: Wall(2),
            zone: "Europe/London".into(),
            by: Resolution::Day,
            apps: Vec::new(),
            agents: Vec::new(),
        }
    );
}

#[test]
fn a_model_is_found_by_its_exact_name_first() {
    let table = table(&[
        ("anthropic/claude-haiku-5-5", ModelPrice::new(1.0, 5.0)),
        ("claude-haiku-5-5", ModelPrice::new(2.0, 5.0)),
    ]);
    assert_eq!(found(&table, "claude-haiku-5-5"), Some("claude-haiku-5-5"));
    assert_eq!(
        found(&table, "anthropic/claude-haiku-5-5"),
        Some("anthropic/claude-haiku-5-5")
    );
}

#[test]
fn a_model_is_found_with_the_first_provider_sorted() {
    let table = table(&[
        ("vertex_ai/claude-haiku-5-5", ModelPrice::new(2.0, 5.0)),
        ("anthropic/claude-haiku-5-5", ModelPrice::new(1.0, 5.0)),
    ]);
    assert_eq!(
        found(&table, "claude-haiku-5-5"),
        Some("anthropic/claude-haiku-5-5")
    );
}

#[test]
fn a_provider_is_one_whole_name() {
    let table = table(&[
        (
            "openrouter/anthropic/claude-haiku-5-5",
            ModelPrice::new(1.0, 5.0),
        ),
        ("big-claude-haiku-5-5", ModelPrice::new(1.0, 5.0)),
        ("/claude-haiku-5-5", ModelPrice::new(1.0, 5.0)),
    ]);
    assert_eq!(found(&table, "claude-haiku-5-5"), None);
}

#[test]
fn a_dated_model_is_found_without_its_date() {
    let table = table(&[
        ("anthropic/claude-haiku-5-5", ModelPrice::new(1.0, 5.0)),
        ("claude-sonnet-5-5-20261001", ModelPrice::new(3.0, 15.0)),
    ]);
    assert_eq!(
        found(&table, "claude-haiku-5-5-20261001"),
        Some("anthropic/claude-haiku-5-5")
    );
    // Its own date's entry, if the table has one, before the undated.
    assert_eq!(
        found(&table, "claude-sonnet-5-5-20261001"),
        Some("claude-sonnet-5-5-20261001")
    );
    // Only a date: eight digits.
    assert_eq!(found(&table, "claude-haiku-5-5-2026100"), None);
    assert_eq!(found(&table, "claude-haiku-5-5-2026100x"), None);
    assert_eq!(found(&table, "claude-opus-5-5-20261001"), None);
}

#[test]
fn an_estimate_prices_each_kind_at_its_rate_per_million() {
    let price = ModelPrice {
        input: 3.0,
        cache_read: 0.3,
        cache_write: 3.75,
        output: 15.0,
        fast: None,
    };
    let table = table(&[("claude-sonnet-5-5", price)]);
    let turn = usage(
        &[(
            "claude-sonnet-5-5",
            tokens(1_000_000, 2_000_000, 400_000, 100_000),
        )],
        false,
    );
    let estimate = table.estimate(&turn);
    assert!((estimate.usd - (3.0 + 0.6 + 1.5 + 1.5)).abs() < 1e-9);
    assert_eq!(estimate.unpriced, 0);
}

#[test]
fn an_estimate_is_at_the_fast_rate_only_in_fast_mode_with_one() {
    let fast = ModelPrice {
        fast: Some(6.0),
        ..ModelPrice::new(5.0, 25.0)
    };
    let table = table(&[
        ("claude-opus-5-5", fast),
        ("claude-haiku-5-5", ModelPrice::new(1.0, 5.0)),
    ]);
    let rows = [
        ("claude-opus-5-5", tokens(1_000_000, 0, 0, 0)),
        ("claude-haiku-5-5", tokens(1_000_000, 0, 0, 0)),
    ];
    let slow = table.estimate(&usage(&rows, false));
    assert!((slow.usd - 6.0).abs() < 1e-9);
    // Haiku has no fast rate: it's at its own.
    let quick = table.estimate(&usage(&rows, true));
    assert!((quick.usd - 31.0).abs() < 1e-9);
}

#[test]
fn tokens_on_models_with_no_price_are_unpriced() {
    let table = table(&[("claude-haiku-5-5", ModelPrice::new(1.0, 5.0))]);
    let turn = usage(
        &[
            ("claude-haiku-5-5", tokens(1_000_000, 0, 0, 0)),
            ("mystery", tokens(1, 2, 3, 4)),
        ],
        false,
    );
    let estimate = table.estimate(&turn);
    assert!((estimate.usd - 1.0).abs() < 1e-9);
    assert_eq!(estimate.unpriced, 10);
}

#[test]
fn a_reported_cost_plays_no_part_in_an_estimate() {
    let table = PriceTable::default();
    let mut turn = usage(&[("claude-haiku-5-5", tokens(1, 0, 0, 0))], false);
    turn.cost = Some(Cost {
        amount: 1.0,
        currency: "USD".into(),
    });
    assert_eq!(
        table.estimate(&turn),
        Estimate {
            usd: 0.0,
            unpriced: 1,
        }
    );
}

#[test]
fn a_new_priced_model_is_as_given() {
    let price = ModelPrice::new(1.0, 5.0);
    assert_eq!(
        PricedModel::new("claude-haiku-5-5", price.clone(), PriceLayer::Bundled),
        PricedModel {
            model: "claude-haiku-5-5".into(),
            price,
            layer: PriceLayer::Bundled,
        }
    );
}

#[test]
fn a_new_bucket_is_empty() {
    let bucket = Bucket::new(
        Wall(7),
        "clackatoo",
        "claude-agent-acp",
        "claude-haiku-5-5",
        true,
    );
    assert_eq!(
        bucket,
        Bucket {
            start: Wall(7),
            app: "clackatoo".into(),
            agent: "claude-agent-acp".into(),
            model: "claude-haiku-5-5".into(),
            fast: true,
            tokens: Tokens::default(),
            reported: 0.0,
            estimated: 0.0,
            unpriced: 0,
            turns: 0,
        }
    );
}

#[test]
fn a_new_summary_is_of_nothing() {
    let query = UsageQuery::new(Wall(1), Wall(2), "UTC");
    assert_eq!(
        Summary::new(query.clone()),
        Summary {
            query,
            buckets: Vec::new(),
            chats: ChatCounts::default(),
            prices: Vec::new(),
            fetched: None,
            unreadable: Vec::new(),
        }
    );
}

#[test]
fn a_prices_cost_is_its_rates_per_million() {
    let price = ModelPrice {
        fast: Some(2.0),
        ..ModelPrice::new(2.0, 10.0)
    };
    let tokens = tokens(500_000, 0, 0, 100_000);
    assert!((price.cost(&tokens, false) - 2.0).abs() < 1e-9);
    assert!((price.cost(&tokens, true) - 4.0).abs() < 1e-9);
}
