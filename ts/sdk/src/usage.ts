// Helpers for usage summaries: every view of the usage page is a sum of a
// summary's buckets, grouped one way or another. They're here, once, so no
// viewer writes its own sums and gets them subtly different.

import type { Bucket, Spend, Summary, Tokens, TurnUsage } from "./types.ts";

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
  void summary;
  throw new Error("todo: feature 6");
}

/**
 * The buckets grouped by one thing, biggest cost first (by tokens, if
 * `by` is "tokens"), or for "period" in time order, with every period of
 * the query present, empty ones too, so a chart has no gaps.
 */
export function group(summary: Summary, by: GroupBy, order?: "cost" | "tokens"): Group[] {
  void summary;
  void by;
  void order;
  throw new Error("todo: feature 6");
}

/**
 * The buckets grouped by period, and within each by a second thing: a
 * stacked chart's bars, or the by-day table split by agent or app.
 */
export function split(summary: Summary, by: Exclude<GroupBy, "period">): { start: number; groups: Group[] }[] {
  void summary;
  void by;
  throw new Error("todo: feature 6");
}

/** A bucket's or group's cost, reported and estimated together. */
export function cost(of: Bucket | Totals | Spend): number {
  void of;
  throw new Error("todo: feature 6");
}

/** Every token in these counts: input, both cache counts and output (thinking is part of output). */
export function total(tokens: Tokens): number {
  void tokens;
  throw new Error("todo: feature 6");
}

/** A turn's tokens, every model's added up. */
export function turnTokens(usage: TurnUsage): Tokens {
  void usage;
  throw new Error("todo: feature 6");
}
