import { apply } from "./apply.ts";
import type { DeepReadonly } from "./readonly.ts";
import { isChange } from "./shape.ts";
import type { Chat, ChangeNo, Numbered } from "./types.ts";

/**
 * A viewer's copy of one chat, and how far it has read: Rust's `ChatCopy`.
 * Every SDK turns the same messages into the same chat; the fixtures in
 * `fixtures/apply/` check it.
 */
export class ChatCopy {
  #chat: DeepReadonly<Chat>;
  #latest: ChangeNo;

  /** A copy of a chat sent whole, as in a `Picture`. */
  constructor(chat: DeepReadonly<Chat>, latest: ChangeNo) {
    this.#chat = chat;
    this.#latest = latest;
  }

  /**
   * The chat, as far as the copy has read. A new object after every
   * change. It's read-only all the way down (`DeepReadonly`): it shares
   * its turns and items with the copy, so changing it would change the
   * copy.
   */
  get chat(): DeepReadonly<Chat> {
    return this.#chat;
  }

  /** The last change the copy has. */
  get latest(): ChangeNo {
    return this.#latest;
  }

  /**
   * Applies changes, oldest first, skipping ones it already has.
   * `"need-picture"` if one is missing, doesn't fit, or is of a type this
   * SDK doesn't know: the copy keeps what it applied before then, and needs
   * the chat whole again.
   *
   * A change that isn't shaped as CAF sends one, such as one missing its
   * number, or a known type without what `apply` reads, needs a picture
   * too: it never throws.
   */
  take(changes: readonly Numbered[]): "ok" | "need-picture" {
    for (const numbered of changes) {
      const no: unknown = numbered?.no;
      if (typeof no !== "number") return "need-picture";
      if (no <= this.#latest) continue;
      if (no !== this.#latest + 1) return "need-picture";
      // Its type is one this SDK knows, but not what it holds.
      if (!isChange(numbered.change)) return "need-picture";
      let applied: ReturnType<typeof apply>;
      try {
        applied = apply(this.#chat, numbered.change);
      } catch {
        // Only a bug of this SDK's gets here.
        return "need-picture";
      }
      if (!("chat" in applied)) return "need-picture";
      this.#chat = applied.chat;
      this.#latest = no;
    }
    return "ok";
  }
}
