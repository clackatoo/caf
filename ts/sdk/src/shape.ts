// Checks that what CAF sent has the shape this SDK relies on: ids where it
// looks things up, lists where it adds to them, and the type of whatever
// it reads. What it never reads is the app's to draw, so isn't checked.

import type { Chat, ChatSummary, Item, Turn } from "./types.ts";

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isNumber(value: unknown): value is number {
  return typeof value === "number";
}

export function isString(value: unknown): value is string {
  return typeof value === "string";
}

export function isArrayOf<T>(value: unknown, each: (value: unknown) => value is T): value is T[] {
  return Array.isArray(value) && value.every(each);
}

/** A `{ type, value }` enum, of any variant. */
export function isTagged(value: unknown): value is { type: string } {
  return isRecord(value) && isString(value.type);
}

function hasId(value: unknown): value is Record<string, unknown> & { id: number } {
  return isRecord(value) && isNumber(value.id);
}

/** An item: words and thinking hold text, which `Appended` adds to. */
export function isItem(value: unknown): value is Item {
  if (!hasId(value) || !isRecord(value.window) || !isTagged(value.kind)) return false;
  const kind = value.kind as { type: string; value?: unknown };
  return (kind.type !== "Words" && kind.type !== "Thinking") || isString(kind.value);
}

/** A turn: its window is one `TurnLife` ends. */
export function isTurn(value: unknown): value is Turn {
  return hasId(value) && isRecord(value.window) && isArrayOf(value.items, isItem);
}

export function isChat(value: unknown): value is Chat {
  return hasId(value) && isArrayOf(value.turns, isTurn) && isArrayOf(value.questions, hasId);
}

export function isSummary(value: unknown): value is ChatSummary {
  return isRecord(value) && isNumber(value.id) && isString(value.agent) && isString(value.cwd);
}

/**
 * Whether a change of a type this SDK knows holds what `apply` reads. A
 * type it doesn't know passes: `apply` says it doesn't know it.
 */
export function isChange(value: unknown): boolean {
  if (!isTagged(value)) return false;
  const held: unknown = (value as { value?: unknown }).value;
  const fields = isRecord(held) ? held : {};
  switch (value.type) {
    case "State":
      return isTagged(held);
    case "TurnAdded":
      return isTurn(held);
    case "TurnInserted":
      return isNumber(fields.before) && isTurn(fields.turn);
    case "TurnLife":
      return isNumber(fields.turn) && isTagged(fields.life) && isTagged(fields.end);
    case "ItemAdded":
      return isNumber(fields.turn) && isItem(fields.item);
    case "Appended":
      return isNumber(fields.item) && isString(fields.text);
    case "ToolChanged":
      return isNumber(fields.item) && isRecord(fields.tool);
    case "ItemEnded":
      return isNumber(fields.item) && isTagged(fields.end);
    case "Usage":
      return isRecord(held);
    case "QuestionAsked":
      return hasId(held);
    case "QuestionGone":
      return isNumber(held);
    default:
      return true;
  }
}
