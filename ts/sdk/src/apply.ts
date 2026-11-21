import type { DeepReadonly } from "./readonly.ts";
import type { Chat, Change, Item, ItemId, QuestionId, Turn, TurnId } from "./types.ts";

/** A chat, a turn and an item as `apply` takes them: read-only, or not. */
type RChat = DeepReadonly<Chat>;
type RTurn = DeepReadonly<Turn>;
type RItem = DeepReadonly<Item>;

/** A change that doesn't fit a chat: it names something the chat doesn't have. */
export type Mismatch =
  | { type: "NoTurn"; value: TurnId }
  | { type: "NoItem"; value: ItemId }
  | { type: "NotText"; value: ItemId }
  | { type: "NotTool"; value: ItemId }
  | { type: "NoQuestion"; value: QuestionId };

/**
 * Applies one change to a chat, as Rust's `Chat::apply` does.
 *
 * It never changes `chat`: it returns a new chat that shares every turn and
 * item the change didn't touch, so Svelte, React or plain DOM code can tell
 * what changed by comparing objects.
 *
 * Returns a `Mismatch`, and no chat, if the change names something the chat
 * doesn't have: the viewer has missed something and needs the chat whole.
 * A change of a type this SDK doesn't know is a mismatch too.
 */
export function apply(chat: Chat, change: Change): { chat: Chat } | { mismatch: Mismatch | "unknown-change" };
/** The same, for a read-only chat, such as a copy's: the chat it gives shares with it, so is read-only too. */
export function apply(chat: RChat, change: Change): { chat: RChat } | { mismatch: Mismatch | "unknown-change" };
export function apply(chat: RChat, change: Change): { chat: RChat } | { mismatch: Mismatch | "unknown-change" } {
  switch (change.type) {
    case "State":
      return { chat: { ...chat, state: change.value } };
    case "Settings":
      return { chat: { ...chat, settings: change.value } };
    case "TurnAdded":
      return { chat: { ...chat, turns: [...chat.turns, change.value] } };
    case "TurnInserted": {
      const { before, turn } = change.value;
      const at = turnAt(chat, before);
      if (at < 0) return { mismatch: { type: "NoTurn", value: before } };
      return { chat: { ...chat, turns: [...chat.turns.slice(0, at), turn, ...chat.turns.slice(at)] } };
    }
    case "TurnLife": {
      const { turn, life, end } = change.value;
      return withTurn(chat, turn, (each) => ({ ...each, life, window: { ...each.window, end } }));
    }
    case "TurnModel": {
      const { turn, model } = change.value;
      return withTurn(chat, turn, (each) => ({ ...each, model }));
    }
    case "ItemAdded": {
      const { turn, item } = change.value;
      return withTurn(chat, turn, (each) => ({ ...each, items: [...each.items, item] }));
    }
    case "Appended": {
      const { item, text } = change.value;
      return withItem(chat, item, (each) => {
        const kind = each.kind;
        if (kind.type !== "Words" && kind.type !== "Thinking") return { type: "NotText", value: item };
        return { ...each, kind: { ...kind, value: kind.value + text } };
      });
    }
    case "ToolChanged": {
      const { item, tool } = change.value;
      return withItem(chat, item, (each) => {
        if (each.kind.type !== "Tool") return { type: "NotTool", value: item };
        return { ...each, kind: { ...each.kind, value: tool } };
      });
    }
    case "ItemEnded": {
      const { item, end } = change.value;
      return withItem(chat, item, (each) => ({ ...each, window: { ...each.window, end } }));
    }
    case "Usage":
      return { chat: { ...chat, usage: change.value } };
    case "QuestionAsked":
      return { chat: { ...chat, questions: [...chat.questions, change.value] } };
    case "QuestionGone": {
      const id = change.value;
      if (!chat.questions.some((each) => each.id === id)) return { mismatch: { type: "NoQuestion", value: id } };
      return { chat: { ...chat, questions: chat.questions.filter((each) => each.id !== id) } };
    }
    default:
      // A later CAF's change, or something that isn't a change at all.
      return { mismatch: "unknown-change" };
  }
}

/** Where a turn is, searching from the newest as the core does, or -1. */
function turnAt(chat: RChat, id: TurnId): number {
  return lastIndex(chat.turns, id);
}

/** The chat with one turn replaced by `change(turn)`, sharing every other. */
function withTurn(chat: RChat, id: TurnId, change: (turn: RTurn) => RTurn): { chat: RChat } | { mismatch: Mismatch } {
  const at = turnAt(chat, id);
  const turn = chat.turns[at];
  if (turn === undefined) return { mismatch: { type: "NoTurn", value: id } };
  return { chat: { ...chat, turns: replaced(chat.turns, at, change(turn)) } };
}

/**
 * The chat with one item, in whichever turn it is, replaced by
 * `change(item)`, sharing every other turn and item. `change` may refuse
 * with a mismatch instead.
 */
function withItem(
  chat: RChat,
  id: ItemId,
  change: (item: RItem) => RItem | Mismatch,
): { chat: RChat } | { mismatch: Mismatch } {
  // Newest first, as the core searches.
  for (let t = chat.turns.length - 1; t >= 0; t--) {
    const turn = chat.turns[t]!;
    const at = lastIndex(turn.items, id);
    const item = turn.items[at];
    if (item === undefined) continue;
    const changed = change(item);
    if (!("id" in changed)) return { mismatch: changed };
    const items = replaced(turn.items, at, changed);
    return { chat: { ...chat, turns: replaced(chat.turns, t, { ...turn, items }) } };
  }
  return { mismatch: { type: "NoItem", value: id } };
}

/** Where the last of `list` with this id is, or -1. */
function lastIndex(list: readonly { id: number }[], id: number): number {
  for (let at = list.length - 1; at >= 0; at--) if (list[at]!.id === id) return at;
  return -1;
}

/** A copy of `list` with the one at `at` replaced. */
function replaced<T>(list: readonly T[], at: number, by: T): T[] {
  const copy = list.slice();
  copy[at] = by;
  return copy;
}
