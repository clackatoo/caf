import type { Chat, ChangeNo, Numbered } from "./types.ts";

/**
 * A viewer's copy of one chat, and how far it has read: Rust's `ChatCopy`.
 * Every SDK turns the same messages into the same chat; the fixtures in
 * `fixtures/apply/` check it.
 */
export class ChatCopy {
  /** A copy of a chat sent whole, as in a `Picture`. */
  constructor(chat: Chat, latest: ChangeNo) {
    void chat;
    void latest;
    throw new Error("todo");
  }

  /** The chat, as far as the copy has read. A new object after every change. */
  get chat(): Chat {
    throw new Error("todo");
  }

  /** The last change the copy has. */
  get latest(): ChangeNo {
    throw new Error("todo");
  }

  /**
   * Applies changes, oldest first, skipping ones it already has.
   * `"need-picture"` if one is missing, doesn't fit, or is of a type this
   * SDK doesn't know: the copy keeps what it applied before then, and needs
   * the chat whole again.
   */
  take(changes: readonly Numbered[]): "ok" | "need-picture" {
    void changes;
    throw new Error("todo");
  }
}
