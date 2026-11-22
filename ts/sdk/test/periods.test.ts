/// <reference types="node" />
// The shared fixture of period starts: caf-usage writes
// `fixtures/periods.json`, when the hour and the day each of many moments
// is in starts, in several zones, across changes of clocks, and every SDK
// must find the same starts for a summary's periods as the ledger gives
// its buckets.
//
// It also compares two copies of the time zone rules: those the fixture
// was written with, by jiff (the system's tz database, or jiff-tzdb's copy
// where there's none, as on Windows), and the ICU data of the Node (or
// browser) running this. A zone whose rules change in one and not yet the
// other fails here, though neither is wrong: bring whichever is behind up
// to date, then write the fixture again with
// `CAF_BLESS=1 cargo test -p caf-usage the_period_fixture`.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, test } from "vitest";

import { group, type Resolution, type Summary } from "../src/index.ts";

const file = fileURLToPath(new URL("../../../fixtures/periods.json", import.meta.url));
const cases = JSON.parse(readFileSync(file, "utf8")) as { zone: string; at: number; hour: number; day: number }[];

/** When the period `at` is in starts: the first of a query from `at`, an instant long. */
function start(at: number, zone: string, by: Resolution): number {
  const summary: Summary = {
    query: { since: at, until: at + 1, zone, by, apps: [], agents: [] },
    buckets: [],
    chats: { all: 0, by_app: [], by_agent: [] },
    prices: [],
    matched: [],
    fetched: null,
    unreadable: [],
  };
  return Number(group(summary, "period")[0]!.key);
}

describe("period starts, as the ledger finds them", () => {
  test("there are some", () => {
    expect(cases.length).toBeGreaterThan(0);
  });

  test.each(cases.map((each) => [`${each.zone} at ${each.at}`, each] as const))("%s", (_, { zone, at, hour, day }) => {
    expect(start(at, zone, { type: "Hour" }), "its hour").toBe(hour);
    expect(start(at, zone, { type: "Day" }), "its day").toBe(day);
  });
});
