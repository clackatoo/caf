import type { Chat, Change, ItemId, QuestionId, TurnId } from "./types.ts";

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
export function apply(chat: Chat, change: Change): { chat: Chat } | { mismatch: Mismatch | "unknown-change" } {
  void chat;
  void change;
  throw new Error("todo");
}
