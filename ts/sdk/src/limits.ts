// Helpers for account limits: how far through a window is, and how low a
// limit is. Here, once, so every viewer says the same of them. The pace
// matches caf-core's `LimitWindow::pace` exactly.

import type { AccountLimits, LimitWindow, Wall, WindowKind } from "./types.ts";

/** How low a window is: `low` at 20% left or less, `out` at 10% or less. */
export type Lowness = "fine" | "low" | "out";

/** At or below this much left, a window is `low`. */
export const LOW_LEFT = 20;

/** At or below this much left, a window is `out`. */
export const OUT_LEFT = 10;

/** How long a window of this kind is, in milliseconds: null for one CAF doesn't know. */
export function length(kind: WindowKind): number | null {
  void kind;
  throw new Error("todo");
}

/**
 * How much of the window has gone by at `now`, as a whole percent, 0–100:
 * where `used` would be at an even pace. Null with no reset or no length.
 */
export function pace(window: LimitWindow, now: Wall): number | null {
  void window, now;
  throw new Error("todo");
}

/** How much of the window is left, as a whole percent. */
export function left(window: LimitWindow): number {
  void window;
  throw new Error("todo");
}

/** How low the window is. A refused one is always `out`. */
export function lowness(window: LimitWindow): Lowness {
  void window;
  throw new Error("todo");
}

/** The window with the least left, the first of them on a tie: what a ring's dot shows. Null with none. */
export function lowest(limits: AccountLimits): LimitWindow | null {
  void limits;
  throw new Error("todo");
}

/** A window's name for showing: "5-hour", "Weekly", "Weekly · Opus", "Weekly · extra included", or the agent's own. */
export function label(kind: WindowKind): string {
  void kind;
  throw new Error("todo");
}
