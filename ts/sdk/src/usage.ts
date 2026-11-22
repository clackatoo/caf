// Helpers for usage summaries: every view of the usage page is a sum of a
// summary's buckets, grouped one way or another. They're here, once, so no
// viewer writes its own sums and gets them subtly different.

import type { Bucket, PricedModel, Spend, Summary, Tokens, TurnUsage, UsageQuery } from "./types.ts";

/** What a summary is grouped by. */
export type GroupBy = "app" | "agent" | "model" | "period" | "fast";

/** A group's sums: the same figures as a bucket, added up. */
export interface Totals {
  readonly tokens: Tokens;
  /** Cost agents reported, in US dollars. */
  readonly reported: number;
  /** Cost estimated for turns without, in US dollars. */
  readonly estimated: number;
  /** Tokens no price covered. */
  readonly unpriced: number;
  readonly turns: number;
}

/** One group: its key (an app's name, an agent, a model, a period's start as text, or "fast"/"standard") and its sums. */
export interface Group {
  readonly key: string;
  readonly totals: Totals;
}

/** Every bucket added up. */
export function totals(summary: Summary): Totals {
  return sum(summary.buckets);
}

/**
 * The buckets grouped by one thing, biggest cost first (by tokens, if
 * `by` is "tokens"), or for "period" in time order, with every period of
 * the query present, empty ones too, so a chart has no gaps.
 */
export function group(summary: Summary, by: GroupBy, order?: "cost" | "tokens"): Group[] {
  if (by === "period") {
    return binned(summary).map(({ start, buckets }) => ({ key: String(start), totals: sum(buckets) }));
  }
  return grouped(summary.buckets, by, order ?? "cost");
}

/**
 * The buckets grouped by period, and within each by a second thing: a
 * stacked chart's bars, or the by-day table split by agent or app.
 */
export function split(summary: Summary, by: Exclude<GroupBy, "period">): { start: number; groups: Group[] }[] {
  return binned(summary).map(({ start, buckets }) => ({ start, groups: grouped(buckets, by, "cost") }));
}

/** A bucket's or group's cost, reported and estimated together. */
export function cost(of: Bucket | Totals | Spend): number {
  return of.reported + of.estimated;
}

/** Every token in these counts: input, both cache counts and output (thinking is part of output). */
export function total(tokens: Tokens): number {
  return tokens.input + tokens.cache_read + tokens.cache_write + tokens.output;
}

/** A turn's tokens, every model's added up. */
export function turnTokens(usage: TurnUsage): Tokens {
  return usage.models.reduce((all, each) => added(all, each.tokens), NO_TOKENS);
}

/**
 * The price that priced a model in a summary, by the agent's id for it as
 * a bucket names it: the entry `matched` names, as the core found it.
 * Null if it had none.
 */
export function price(summary: Summary, model: string): PricedModel | null {
  const entry = summary.matched.find(([id]) => id === model)?.[1];
  return entry === undefined ? null : (summary.prices.find((each) => each.model === entry) ?? null);
}

const NO_TOKENS: Tokens = { input: 0, cache_read: 0, cache_write: 0, output: 0, thinking: null };

/** Two counts added up. Thinking stays unknown only if neither knows it. */
function added(a: Tokens, b: Tokens): Tokens {
  return {
    input: a.input + b.input,
    cache_read: a.cache_read + b.cache_read,
    cache_write: a.cache_write + b.cache_write,
    output: a.output + b.output,
    thinking: a.thinking === null && b.thinking === null ? null : (a.thinking ?? 0) + (b.thinking ?? 0),
  };
}

function sum(buckets: readonly Bucket[]): Totals {
  let tokens = NO_TOKENS;
  let reported = 0;
  let estimated = 0;
  let unpriced = 0;
  let turns = 0;
  for (const bucket of buckets) {
    tokens = added(tokens, bucket.tokens);
    reported += bucket.reported;
    estimated += bucket.estimated;
    unpriced += bucket.unpriced;
    turns += bucket.turns;
  }
  return { tokens, reported, estimated, unpriced, turns };
}

/** What a bucket is grouped under, by anything but its period. */
function keyOf(bucket: Bucket, by: Exclude<GroupBy, "period">): string {
  switch (by) {
    case "app":
      return bucket.app;
    case "agent":
      return bucket.agent;
    case "model":
      return bucket.model;
    case "fast":
      return bucket.fast ? "fast" : "standard";
  }
}

/** Buckets grouped by a key, biggest first by `order`, then by the other measure, then by key. */
function grouped(buckets: readonly Bucket[], by: Exclude<GroupBy, "period">, order: "cost" | "tokens"): Group[] {
  const keyed = new Map<string, Bucket[]>();
  for (const bucket of buckets) {
    const key = keyOf(bucket, by);
    const list = keyed.get(key);
    if (list === undefined) keyed.set(key, [bucket]);
    else list.push(bucket);
  }
  const groups = [...keyed].map(([key, list]) => ({ key, totals: sum(list) }));
  const measures = (totals: Totals) =>
    order === "cost" ? [cost(totals), total(totals.tokens)] : [total(totals.tokens), cost(totals)];
  return groups.sort((a, b) => {
    const [a1, a2] = measures(a.totals);
    const [b1, b2] = measures(b.totals);
    return b1! - a1! || b2! - a2! || compare(a.key, b.key);
  });
}

/** Text in a fixed order, whatever the locale. */
function compare(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

/**
 * Every period of the summary's query, in time order, with the buckets in
 * each: a bucket is in the period its start falls in.
 */
function binned(summary: Summary): { start: number; buckets: Bucket[] }[] {
  const starts = periods(summary.query);
  const bins = starts.map((start) => ({ start, buckets: [] as Bucket[] }));
  for (const bucket of summary.buckets) {
    // The last period starting at or before it; the first, for any
    // before them all, which CAF never sends.
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid]! <= bucket.start) lo = mid;
      else hi = mid - 1;
    }
    bins[lo]?.buckets.push(bucket);
  }
  return bins;
}

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
/** More periods than any query CAF answers has: it refuses one longer than 400 days. */
const MOST = 400 * 24 + 48;

/**
 * The start of every period a query covers, in time order: each hour, or
 * each day in its zone, that `[since, until)` touches, the first starting
 * at or before `since`. A day is the zone's own, 23 or 25 hours across a
 * change of clocks.
 */
function periods(query: UsageQuery): number[] {
  const { since, until, zone } = query;
  const day = query.by.type === "Day";
  const floor = day ? (t: number) => dayStart(t, zone) : (t: number) => hourStart(t, zone);
  const starts: number[] = [];
  let start = floor(since);
  while (start < until && starts.length < MOST) {
    starts.push(start);
    // A day or an hour on, then back to where that period starts, which
    // is later however the clocks changed between.
    const next = floor(start + (day ? DAY + 3 * HOUR : HOUR));
    start = next > start ? next : start + HOUR;
  }
  return starts;
}

/** The zone's clock at `t`: its date and time, as the milliseconds a UTC clock showing them would be at. */
function wall(t: number, zone: string): number {
  const parts: Record<string, number> = {};
  for (const part of formatter(zone).formatToParts(t)) parts[part.type] = Number(part.value);
  const { year, month, day, hour, minute, second } = parts as Record<string, number>;
  return Date.UTC(year!, month! - 1, day!, hour!, minute!, second!) + (((t % 1000) + 1000) % 1000);
}

const formatters = new Map<string, Intl.DateTimeFormat>();

function formatter(zone: string): Intl.DateTimeFormat {
  let made = formatters.get(zone);
  if (made === undefined) {
    made = new Intl.DateTimeFormat("en-US", {
      timeZone: zone,
      hourCycle: "h23",
      year: "numeric",
      month: "numeric",
      day: "numeric",
      hour: "numeric",
      minute: "numeric",
      second: "numeric",
    });
    formatters.set(zone, made);
  }
  return made;
}

/** The start of the zone's hour `t` is in. */
function hourStart(t: number, zone: string): number {
  const local = wall(t, zone);
  return t - (((local % HOUR) + HOUR) % HOUR);
}

/**
 * The start of the zone's day `t` is in: the first moment its clock shows
 * that date, which is midnight unless the clocks skip it.
 */
function dayStart(t: number, zone: string): number {
  const date = Math.floor(wall(t, zone) / DAY) * DAY;
  // Midnight, at the offset in force then: found in a step or two.
  let guess = t - (wall(t, zone) - date);
  for (let n = 0; n < 3; n++) {
    const off = wall(guess, zone) - guess;
    const next = date - off;
    if (next === guess) break;
    guess = next;
  }
  if (wall(guess, zone) === date && localDate(guess - 1, zone) < date) return guess;
  // The clocks skipped midnight, or it's ambiguous: the first moment of
  // the date, searched for to the minute, then the millisecond.
  let lo = date - 15 * HOUR;
  let hi = Math.min(t, date + 15 * HOUR);
  while (hi - lo > 1) {
    const mid = Math.floor((lo + hi) / 2);
    if (localDate(mid, zone) >= date) hi = mid;
    else lo = mid;
  }
  return hi;
}

/** The zone's date at `t`, as a UTC midnight. */
function localDate(t: number, zone: string): number {
  return Math.floor(wall(t, zone) / DAY) * DAY;
}
