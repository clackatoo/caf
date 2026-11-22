// The limits helpers: how long each window is, how far through it is,
// how much of it is left and how low that is, which is lowest, and what
// each is called.

import { describe, expect, test } from "vitest";

import {
  label,
  left,
  length,
  lowest,
  lowness,
  pace,
  LOW_LEFT,
  OUT_LEFT,
  type AccountLimits,
  type DeepReadonly,
  type LimitState,
  type LimitWindow,
  type WindowKind,
} from "../src/index.ts";

const HOUR = 3_600_000;
const WEEK = 7 * 24 * HOUR;
const NOW = Date.UTC(2026, 10, 22, 12);

function window(fields: Partial<LimitWindow> = {}): LimitWindow {
  return { kind: { type: "FiveHours" }, used: 0, resets: NOW + HOUR, state: { type: "Fine" }, heard: NOW, ...fields };
}

function limits(...windows: LimitWindow[]): AccountLimits {
  return { windows, extra_usage: null, cleared: null };
}

const FINE: LimitState = { type: "Fine" };
const WARNING: LimitState = { type: "Warning" };
const REFUSED: LimitState = { type: "Refused" };

describe("length", () => {
  test("is five hours, or a week for every kind of week", () => {
    expect(length({ type: "FiveHours" })).toBe(5 * HOUR);
    expect(length({ type: "Week" })).toBe(WEEK);
    expect(length({ type: "ModelWeek", value: "opus" })).toBe(WEEK);
    expect(length({ type: "ExtraWeek" })).toBe(WEEK);
  });

  test("is unknown for a window CAF doesn't know", () => {
    expect(length({ type: "Other", value: "thirty_day" })).toBeNull();
  });
});

describe("pace", () => {
  test("is how much of the window has gone by, as a whole percent", () => {
    // Three of five hours left: two gone.
    expect(pace(window({ resets: NOW + 3 * HOUR }), NOW)).toBe(40);
    // A day into the week.
    expect(pace(window({ kind: { type: "Week" }, resets: NOW + 6 * 24 * HOUR }), NOW)).toBe(14);
    expect(pace(window({ kind: { type: "ModelWeek", value: "sonnet" }, resets: NOW + WEEK / 2 }), NOW)).toBe(50);
  });

  test("rounds to the nearest percent, a half up", () => {
    // 1.5% of five hours gone, then just under and just over.
    const half = (5 * HOUR * 15) / 1000;
    expect(pace(window({ resets: NOW + 5 * HOUR - half }), NOW)).toBe(2);
    expect(pace(window({ resets: NOW + 5 * HOUR - half + 1 }), NOW)).toBe(1);
    expect(pace(window({ resets: NOW + 5 * HOUR - half - 1 }), NOW)).toBe(2);
  });

  test("is kept to 0–100, before the window started and after it reset", () => {
    expect(pace(window({ resets: NOW + 6 * HOUR }), NOW)).toBe(0);
    expect(pace(window({ resets: NOW + 5 * HOUR }), NOW)).toBe(0);
    expect(pace(window({ resets: NOW }), NOW)).toBe(100);
    expect(pace(window({ resets: NOW - HOUR }), NOW)).toBe(100);
  });

  test("is unknown with no reset, or no length", () => {
    expect(pace(window({ resets: null }), NOW)).toBeNull();
    expect(pace(window({ kind: { type: "Other", value: "thirty_day" } }), NOW)).toBeNull();
  });
});

describe("left", () => {
  test("is what isn't used", () => {
    expect(left(window({ used: 0 }))).toBe(100);
    expect(left(window({ used: 63 }))).toBe(37);
    expect(left(window({ used: 100 }))).toBe(0);
  });

  test("is nothing once refused, whatever the share says", () => {
    expect(left(window({ used: 40, state: REFUSED }))).toBe(0);
  });
});

describe("lowness", () => {
  test(`is low at ${LOW_LEFT}% left or less, and out at ${OUT_LEFT}% or less`, () => {
    expect(lowness(window({ used: 79 }))).toBe("fine");
    expect(lowness(window({ used: 80 }))).toBe("low");
    expect(lowness(window({ used: 89 }))).toBe("low");
    expect(lowness(window({ used: 90 }))).toBe("out");
    expect(lowness(window({ used: 100 }))).toBe("out");
  });

  test("is out once refused, and the agent's warning alone changes nothing", () => {
    expect(lowness(window({ used: 0, state: REFUSED }))).toBe("out");
    expect(lowness(window({ used: 50, state: WARNING }))).toBe("fine");
  });
});

describe("lowest", () => {
  test("is the window with the least left", () => {
    const week = window({ kind: { type: "Week" }, used: 70 });
    expect(lowest(limits(window({ used: 20 }), week, window({ kind: { type: "ExtraWeek" }, used: 5 })))).toBe(week);
  });

  test("counts a refused window as nothing left", () => {
    const refused = window({ kind: { type: "ModelWeek", value: "opus" }, used: 30, state: REFUSED });
    expect(lowest(limits(window({ used: 95 }), refused))).toBe(refused);
  });

  test("is the first of them on a tie", () => {
    const first = window({ used: 50 });
    expect(lowest(limits(first, window({ kind: { type: "Week" }, used: 50, state: FINE })))).toBe(first);
  });

  test("is none with no windows", () => {
    expect(lowest(limits())).toBeNull();
  });

  test("takes limits read-only, as a watch gives them", () => {
    const shown: DeepReadonly<AccountLimits> = limits(window({ used: 10 }), window({ used: 60 }));
    expect(lowest(shown)?.used).toBe(60);
  });
});

describe("label", () => {
  test.each<[WindowKind, string]>([
    [{ type: "FiveHours" }, "5-hour"],
    [{ type: "Week" }, "Weekly"],
    [{ type: "ModelWeek", value: "opus" }, "Weekly · Opus"],
    [{ type: "ModelWeek", value: "fable" }, "Weekly · Fable"],
    [{ type: "ExtraWeek" }, "Weekly · extra included"],
    [{ type: "Other", value: "thirty_day" }, "thirty_day"],
  ])("%j is %s", (kind, name) => {
    expect(label(kind)).toBe(name);
  });
});
