// The usage helpers: sums of a summary's buckets, grouped one way or
// another, every period of the query present, in the query's own zone.

import { describe, expect, test } from "vitest";

import {
  cost,
  group,
  price,
  split,
  total,
  totals,
  turnTokens,
  type Bucket,
  type PricedModel,
  type Summary,
  type Tokens,
  type UsageQuery,
} from "../src/index.ts";

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

function tokens(fields: Partial<Tokens> = {}): Tokens {
  return { input: 0, cache_read: 0, cache_write: 0, output: 0, thinking: null, ...fields };
}

function bucket(fields: Partial<Bucket> = {}): Bucket {
  return {
    start: Date.UTC(2026, 10, 20),
    app: "clackatoo",
    agent: "claude-agent-acp",
    model: "claude-opus-5-5",
    fast: false,
    tokens: tokens({ input: 100, output: 10 }),
    reported: 1,
    estimated: 0,
    unpriced: 0,
    turns: 1,
    ...fields,
  };
}

function query(fields: Partial<UsageQuery> = {}): UsageQuery {
  return {
    since: Date.UTC(2026, 10, 20),
    until: Date.UTC(2026, 10, 23),
    zone: "UTC",
    by: { type: "Day" },
    apps: [],
    agents: [],
    ...fields,
  };
}

function summary(buckets: Bucket[], fields: Partial<UsageQuery> = {}, prices: PricedModel[] = []): Summary {
  return {
    query: query(fields),
    buckets,
    chats: { all: 1, by_app: [], by_agent: [] },
    prices,
    matched: [],
    fetched: null,
    unreadable: [],
  };
}

/** A zone's clock at `t`, as `2026-03-29 01:00`. */
function clock(t: number, zone: string): string {
  const parts = Object.fromEntries(
    new Intl.DateTimeFormat("en-US", {
      timeZone: zone,
      hourCycle: "h23",
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    })
      .formatToParts(t)
      .map((part) => [part.type, part.value]),
  );
  return `${parts.year}-${parts.month}-${parts.day} ${parts.hour}:${parts.minute}`;
}

describe("counting", () => {
  test("total counts every token once: thinking is part of output", () => {
    expect(total(tokens({ input: 1, cache_read: 20, cache_write: 300, output: 4000, thinking: 3000 }))).toBe(4321);
  });

  test("cost is what was reported and estimated together", () => {
    expect(cost(bucket({ reported: 1.5, estimated: 0.25 }))).toBe(1.75);
    expect(cost({ tokens: tokens(), reported: 0, estimated: 2, unpriced: 9, turns: 1 })).toBe(2);
  });

  test("a turn's tokens are every model's added up; thinking is unknown only if no model knows it", () => {
    const usage = {
      models: [
        { model: "opus", tokens: tokens({ input: 1, cache_read: 2, cache_write: 3, output: 4 }) },
        { model: "haiku", tokens: tokens({ input: 10, cache_read: 20, cache_write: 30, output: 40, thinking: 5 }) },
      ],
      cost: null,
      fast: false,
    };
    expect(turnTokens(usage)).toStrictEqual({ input: 11, cache_read: 22, cache_write: 33, output: 44, thinking: 5 });
    expect(turnTokens({ ...usage, models: [usage.models[0]!] }).thinking).toBeNull();
  });

  test("totals add up every bucket", () => {
    const all = totals(
      summary([
        bucket({ reported: 1, estimated: 0, unpriced: 0, turns: 2, tokens: tokens({ input: 5, thinking: 1 }) }),
        bucket({ reported: 0, estimated: 0.5, unpriced: 7, turns: 1, tokens: tokens({ output: 3 }) }),
      ]),
    );
    expect(all).toStrictEqual({
      tokens: tokens({ input: 5, output: 3, thinking: 1 }),
      reported: 1,
      estimated: 0.5,
      unpriced: 7,
      turns: 3,
    });
  });

  test("an empty summary adds up to nothing", () => {
    expect(totals(summary([]))).toStrictEqual({ tokens: tokens(), reported: 0, estimated: 0, unpriced: 0, turns: 0 });
  });
});

describe("grouping", () => {
  const buckets = [
    bucket({ app: "clackatoo", agent: "claude-agent-acp", model: "opus", reported: 3, tokens: tokens({ input: 10 }) }),
    bucket({ app: "eyespy", agent: "codex", model: "sol", reported: 1, tokens: tokens({ input: 900 }) }),
    bucket({ app: "clackatoo", agent: "codex", model: "sol", fast: true, reported: 0, estimated: 1.5, tokens: tokens({ input: 50 }) }),
  ];

  test("by app, agent and model, biggest cost first", () => {
    const keys = (by: "app" | "agent" | "model") => group(summary(buckets), by).map((each) => [each.key, cost(each.totals)]);
    expect(keys("app")).toStrictEqual([
      ["clackatoo", 4.5],
      ["eyespy", 1],
    ]);
    expect(keys("agent")).toStrictEqual([
      ["claude-agent-acp", 3],
      ["codex", 2.5],
    ]);
    expect(keys("model")).toStrictEqual([
      ["opus", 3],
      ["sol", 2.5],
    ]);
  });

  test("by tokens, biggest first", () => {
    expect(group(summary(buckets), "agent", "tokens").map((each) => [each.key, total(each.totals.tokens)])).toStrictEqual([
      ["codex", 950],
      ["claude-agent-acp", 10],
    ]);
  });

  test("by speed, as fast and standard", () => {
    expect(group(summary(buckets), "fast").map((each) => [each.key, cost(each.totals)])).toStrictEqual([
      ["standard", 4],
      ["fast", 1.5],
    ]);
  });

  test("ties go by the other measure, then by name, so the order never wavers", () => {
    const tied = [
      bucket({ model: "b", reported: 0, tokens: tokens({ input: 5 }) }),
      bucket({ model: "a", reported: 0, tokens: tokens({ input: 5 }) }),
      bucket({ model: "c", reported: 0, tokens: tokens({ input: 9 }) }),
    ];
    expect(group(summary(tied), "model").map((each) => each.key)).toStrictEqual(["c", "a", "b"]);
  });
});

describe("periods", () => {
  test("every day of the query is present, empty ones too, in time order", () => {
    const days = group(summary([bucket({ start: Date.UTC(2026, 10, 21), reported: 2 })]), "period");
    expect(days.map((each) => [Number(each.key), cost(each.totals)])).toStrictEqual([
      [Date.UTC(2026, 10, 20), 0],
      [Date.UTC(2026, 10, 21), 2],
      [Date.UTC(2026, 10, 22), 0],
    ]);
    expect(days[0]!.totals.turns).toBe(0);
  });

  test("a day is the zone's own", () => {
    const days = group(
      summary([], { since: Date.UTC(2026, 10, 20, 5), until: Date.UTC(2026, 10, 22, 5), zone: "America/New_York" }),
      "period",
    );
    expect(days.map((each) => clock(Number(each.key), "America/New_York"))).toStrictEqual([
      "2026-11-20 00:00",
      "2026-11-21 00:00",
    ]);
    expect(Number(days[0]!.key)).toBe(Date.UTC(2026, 10, 20, 5));
  });

  test("a period starting before since is the first: the day since falls in", () => {
    const days = group(summary([], { since: Date.UTC(2026, 10, 20, 15), until: Date.UTC(2026, 10, 21, 1) }), "period");
    expect(days.map((each) => Number(each.key))).toStrictEqual([Date.UTC(2026, 10, 20), Date.UTC(2026, 10, 21)]);
  });

  test("a day is 23 or 25 hours across a change of clocks", () => {
    const spring = group(
      summary([], { since: Date.UTC(2026, 2, 28), until: Date.UTC(2026, 2, 31), zone: "Europe/London" }),
      "period",
    ).map((each) => Number(each.key));
    expect(spring).toStrictEqual([Date.UTC(2026, 2, 28), Date.UTC(2026, 2, 29), Date.UTC(2026, 2, 29, 23), Date.UTC(2026, 2, 30, 23)]);
    expect(spring[2]! - spring[1]!).toBe(23 * HOUR);
    // The clocks go back on 25 October 2026.
    const autumn = group(
      summary([], { since: Date.UTC(2026, 9, 23, 23), until: Date.UTC(2026, 9, 27), zone: "Europe/London" }),
      "period",
    ).map((each) => Number(each.key));
    expect(autumn).toStrictEqual([Date.UTC(2026, 9, 23, 23), Date.UTC(2026, 9, 24, 23), Date.UTC(2026, 9, 26)]);
    expect(autumn[2]! - autumn[1]!).toBe(25 * HOUR);
  });

  test("a day whose midnight the clocks skip starts when they've moved on", () => {
    // Chile's clocks go from 00:00 to 01:00 on 6 September 2026.
    const days = group(
      summary([], { since: Date.UTC(2026, 8, 5, 12), until: Date.UTC(2026, 8, 7, 12), zone: "America/Santiago" }),
      "period",
    ).map((each) => Number(each.key));
    expect(days.map((t) => clock(t, "America/Santiago"))).toStrictEqual([
      "2026-09-05 00:00",
      "2026-09-06 01:00",
      "2026-09-07 00:00",
    ]);
    expect(clock(days[1]! - 1, "America/Santiago")).toBe("2026-09-05 23:59");
  });

  test("hours are the zone's own, half-hour offsets too", () => {
    const hours = group(
      summary([], { since: Date.UTC(2026, 0, 1, 0, 45), until: Date.UTC(2026, 0, 1, 2, 45), zone: "Asia/Kolkata", by: { type: "Hour" } }),
      "period",
    ).map((each) => Number(each.key));
    expect(hours).toStrictEqual([Date.UTC(2026, 0, 1, 0, 30), Date.UTC(2026, 0, 1, 1, 30), Date.UTC(2026, 0, 1, 2, 30)]);
  });

  test("every hour is present across a change of clocks", () => {
    const hours = group(
      summary([], { since: Date.UTC(2026, 2, 29, 0), until: Date.UTC(2026, 2, 29, 3), zone: "Europe/London", by: { type: "Hour" } }),
      "period",
    ).map((each) => clock(Number(each.key), "Europe/London"));
    expect(hours).toStrictEqual(["2026-03-29 00:00", "2026-03-29 02:00", "2026-03-29 03:00"]);
  });

  test("a bucket is in the period its start falls in", () => {
    const days = group(summary([bucket({ start: Date.UTC(2026, 10, 21, 7), reported: 4 })]), "period");
    expect(days.map((each) => cost(each.totals))).toStrictEqual([0, 4, 0]);
  });

  test("an empty query has no periods", () => {
    expect(group(summary([], { until: Date.UTC(2026, 10, 20) }), "period")).toStrictEqual([]);
  });

  test("a zone the system doesn't know throws a RangeError", () => {
    expect(() => group(summary([], { zone: "Mars/Olympus" }), "period")).toThrow(RangeError);
  });

  test("split gives each period's groups, every period present, for a stacked chart", () => {
    const buckets = [
      bucket({ start: Date.UTC(2026, 10, 20), agent: "codex", reported: 1 }),
      bucket({ start: Date.UTC(2026, 10, 20), agent: "claude-agent-acp", reported: 2 }),
      bucket({ start: Date.UTC(2026, 10, 22, 12), agent: "codex", reported: 5 }),
    ];
    expect(
      split(summary(buckets), "agent").map(({ start, groups }) => [start, groups.map((each) => [each.key, cost(each.totals)])]),
    ).toStrictEqual([
      [
        Date.UTC(2026, 10, 20),
        [
          ["claude-agent-acp", 2],
          ["codex", 1],
        ],
      ],
      [Date.UTC(2026, 10, 21), []],
      [Date.UTC(2026, 10, 22), [["codex", 5]]],
    ]);
  });

  test("90 days of days, and a day of hours", () => {
    const now = Date.UTC(2026, 10, 22, 9, 30);
    expect(group(summary([], { since: now - 90 * DAY, until: now, zone: "Europe/Paris" }), "period")).toHaveLength(91);
    expect(group(summary([], { since: now - DAY, until: now, by: { type: "Hour" } }), "period")).toHaveLength(25);
  });
});

describe("price", () => {
  const rates = { input: 5, cache_read: 0.5, cache_write: 6.25, output: 25, fast: null };
  const priced = (model: string): PricedModel => ({ model, price: rates, layer: { type: "Bundled" } });
  const prices = [priced("anthropic/claude-haiku-5-5"), priced("bedrock/claude-haiku-5-5"), priced("claude-opus-5-5")];
  const matched: [string, string][] = [
    ["claude-haiku-5-5-20261001", "bedrock/claude-haiku-5-5"],
    ["claude-opus-5-5", "claude-opus-5-5"],
    ["gpt-9", "openai/gpt-9"],
  ];
  const find = (model: string) => price({ ...summary([], {}, prices), matched }, model)?.model ?? null;

  test("is the entry the summary says priced the model, whatever its name", () => {
    expect(find("claude-haiku-5-5-20261001")).toBe("bedrock/claude-haiku-5-5");
    expect(find("claude-opus-5-5")).toBe("claude-opus-5-5");
  });

  test("is null for a model the summary matched to nothing, or to an entry it doesn't hold", () => {
    expect(find("claude-haiku-5-5")).toBeNull();
    expect(find("gpt-9")).toBeNull();
  });
});
