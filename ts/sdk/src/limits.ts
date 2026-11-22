// Helpers for account limits: how far through a window is, and how low a
// limit is. Here, once, so every viewer says the same of them. The pace
// matches caf-core's `LimitWindow::pace` exactly.

import type { DeepReadonly } from "./readonly.ts";
import type { LimitWindow, Wall, WindowKind } from "./types.ts";

/** How low a window is: `low` at 20% left or less, `out` at 10% or less. */
export type Lowness = "fine" | "low" | "out";

/** At or below this much left, a window is `low`. */
export const LOW_LEFT = 20;

/** At or below this much left, a window is `out`. */
export const OUT_LEFT = 10;

const HOUR = 3_600_000;
const WEEK = 7 * 24 * HOUR;

/** How long a window of this kind is, in milliseconds: null for one CAF doesn't know. */
export function length(kind: WindowKind): number | null {
  switch (kind.type) {
    case "FiveHours":
      return 5 * HOUR;
    case "Week":
    case "ModelWeek":
    case "ExtraWeek":
      return WEEK;
    case "Other":
      return null;
  }
}

/**
 * How much of the window has gone by at `now`, as a whole percent, 0–100:
 * where `used` would be at an even pace. Null with no reset or no length.
 */
export function pace(window: LimitWindow, now: Wall): number | null {
  const long = length(window.kind);
  if (window.resets === null || long === null) return null;
  const gone = now - (window.resets - long);
  return Math.min(100, Math.max(0, Math.round((100 * gone) / long)));
}

/** How much of the window is left, as a whole percent. */
export function left(window: LimitWindow): number {
  return window.state.type === "Refused" ? 0 : 100 - window.used;
}

/** How low the window is. A refused one is always `out`. */
export function lowness(window: LimitWindow): Lowness {
  const remaining = left(window);
  if (remaining <= OUT_LEFT) return "out";
  return remaining <= LOW_LEFT ? "low" : "fine";
}

/**
 * The window with the least left, the first of them on a tie: what a ring's dot shows. Null with none.
 * It takes limits read-only too, as a watch gives an agent's status.
 */
export function lowest<W extends DeepReadonly<LimitWindow>>(limits: { readonly windows: readonly W[] }): W | null {
  let least: W | null = null;
  for (const window of limits.windows) {
    if (least === null || left(window) < left(least)) least = window;
  }
  return least;
}

/** A window's name for showing: "5-hour", "Weekly", "Weekly · Opus", "Weekly · extra included", or the agent's own. */
export function label(kind: WindowKind): string {
  switch (kind.type) {
    case "FiveHours":
      return "5-hour";
    case "Week":
      return "Weekly";
    case "ModelWeek":
      return `Weekly · ${kind.value.charAt(0).toUpperCase()}${kind.value.slice(1)}`;
    case "ExtraWeek":
      return "Weekly · extra included";
    case "Other":
      return kind.value;
  }
}
