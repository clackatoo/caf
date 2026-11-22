/// <reference types="node" />
// The shared fixture of paces: caf-core writes `fixtures/pace.json`, the
// pace and length of a window of every kind at many instants (before it
// starts, through it, at the edges of rounding, at its reset and after,
// and with no reset), and every SDK must work out the same. Write it
// again with `CAF_BLESS=1 cargo test -p caf-core the_pace_fixture`.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, test } from "vitest";

import { length, pace, type LimitWindow } from "../src/index.ts";

const file = fileURLToPath(new URL("../../../fixtures/pace.json", import.meta.url));
const cases = JSON.parse(readFileSync(file, "utf8")) as { window: LimitWindow; now: number; pace: number | null; length: number | null }[];

describe("paces, as caf-core works them out", () => {
  test("there are some", () => {
    expect(cases.length).toBeGreaterThan(0);
  });

  test.each(cases.map((each) => [`${JSON.stringify(each.window.kind)} resetting at ${each.window.resets} at ${each.now}`, each] as const))(
    "%s",
    (_, { window, now, pace: expected, length: long }) => {
      expect(length(window.kind), "its length").toBe(long);
      expect(pace(window, now), "its pace").toBe(expected);
    },
  );
});
