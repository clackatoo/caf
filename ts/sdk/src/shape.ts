// Checks that what CAF sent has the shape this SDK relies on: ids where it
// looks things up, lists where it adds to them, and the type of whatever
// it reads. What it never reads is the app's to draw, so isn't checked.

import type { AgentStatus, Chat, ChatSummary, Item, Rights, SavedChat, Summary, Turn } from "./types.ts";

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
  return (
    isRecord(value) && isString(value.id) && isArrayOf(value.turns, isTurn) && isArrayOf(value.questions, hasId)
  );
}

export function isSummary(value: unknown): value is ChatSummary {
  return isRecord(value) && isString(value.id) && isString(value.agent) && isString(value.cwd);
}

/** A saved chat, as `Saved` lists them. */
export function isSaved(value: unknown): value is SavedChat {
  return (
    isRecord(value) &&
    isString(value.shelf) &&
    isRecord(value.header) &&
    isString(value.header.chat) &&
    isTagged(value.state)
  );
}

/**
 * An agent's status. Its states are only the app's to draw, so any of
 * them passes, even of a variant this SDK doesn't know. `can_sign_in`
 * decides whether an app offers Sign in, so it must be a boolean.
 */
export function isStatus(value: unknown): value is AgentStatus {
  return (
    isRecord(value) &&
    isString(value.name) &&
    isTagged(value.install) &&
    isTagged(value.sign_in) &&
    typeof value.can_sign_in === "boolean"
  );
}

/**
 * What a viewer may do: an app offers what these say, so each must be a
 * boolean. `usage` came later, so may be absent, which reads as `false`.
 */
export function isRights(value: unknown): value is Rights {
  return (
    isRecord(value) &&
    typeof value.act === "boolean" &&
    typeof value.agents === "boolean" &&
    (value.usage === undefined || typeof value.usage === "boolean")
  );
}

/** Token counts: what the usage helpers add up. */
function isTokens(value: unknown): boolean {
  return (
    isRecord(value) &&
    isNumber(value.input) &&
    isNumber(value.cache_read) &&
    isNumber(value.cache_write) &&
    isNumber(value.output) &&
    (value.thinking === null || isNumber(value.thinking))
  );
}

/** One bucket of a summary: every figure the helpers add up, and every key they group by. */
function isBucket(value: unknown): boolean {
  return (
    isRecord(value) &&
    isNumber(value.start) &&
    isString(value.app) &&
    isString(value.agent) &&
    isString(value.model) &&
    typeof value.fast === "boolean" &&
    isTokens(value.tokens) &&
    isNumber(value.reported) &&
    isNumber(value.estimated) &&
    isNumber(value.unpriced) &&
    isNumber(value.turns)
  );
}

/** A model's price: every rate a number, fast mode's or null. */
function isPriced(value: unknown): boolean {
  if (!isRecord(value) || !isString(value.model) || !isTagged(value.layer) || !isRecord(value.price)) return false;
  const price = value.price;
  return (
    isNumber(price.input) &&
    isNumber(price.cache_read) &&
    isNumber(price.cache_write) &&
    isNumber(price.output) &&
    (price.fast === null || isNumber(price.fast))
  );
}

/** A name and its count, as a summary counts chats per app and agent. */
function isCounted(value: unknown): boolean {
  return Array.isArray(value) && value.length === 2 && isString(value[0]) && isNumber(value[1]);
}

/**
 * A usage summary, whole: its query, buckets, chats, prices and which
 * entry priced each model, which the helpers and every usage page read,
 * and the files it couldn't read.
 */
export function isUsageSummary(value: unknown): value is Summary {
  if (!isRecord(value) || !isRecord(value.query) || !isRecord(value.chats)) return false;
  const { query, chats } = value;
  return (
    isNumber(query.since) &&
    isNumber(query.until) &&
    isString(query.zone) &&
    isTagged(query.by) &&
    isArrayOf(query.apps, isString) &&
    isArrayOf(query.agents, isString) &&
    Array.isArray(value.buckets) &&
    value.buckets.every(isBucket) &&
    isNumber(chats.all) &&
    Array.isArray(chats.by_app) &&
    chats.by_app.every(isCounted) &&
    Array.isArray(chats.by_agent) &&
    chats.by_agent.every(isCounted) &&
    Array.isArray(value.prices) &&
    value.prices.every(isPriced) &&
    Array.isArray(value.matched) &&
    value.matched.every((pair) => Array.isArray(pair) && pair.length === 2 && isString(pair[0]) && isString(pair[1])) &&
    (value.fetched === null || isNumber(value.fetched)) &&
    isArrayOf(value.unreadable, isString)
  );
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
    case "Settings":
      return isRecord(held);
    case "TurnAdded":
      return isTurn(held);
    case "TurnInserted":
      return isNumber(fields.before) && isTurn(fields.turn);
    case "TurnLife":
      return isNumber(fields.turn) && isTagged(fields.life) && isTagged(fields.end);
    case "TurnModel":
      return isNumber(fields.turn) && isRecord(fields.model);
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
    case "TurnSpent":
      return isNumber(fields.turn) && isRecord(fields.usage) && Array.isArray(fields.usage.models);
    case "Spend":
      return isRecord(held);
    case "QuestionAsked":
      return hasId(held);
    case "QuestionGone":
      return isNumber(held);
    default:
      return true;
  }
}
