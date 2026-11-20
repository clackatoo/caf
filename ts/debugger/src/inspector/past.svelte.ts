// The chats as they were at a position, built from the inspector
// channel's `Past` messages just as a viewer builds them from `/viewer`:
// the same `ToViewer` messages, into the SDK's `ChatCopy`. So looking
// back in time exercises `ChatCopy` and `apply` as a real viewer does.

import { ChatCopy, type ChatId, type ChatSummary, type DeepReadonly, type ToViewer, type Chat } from '@clackatoo/caf'

export class PastChats {
  /** The chats open at the position. */
  summaries = $state.raw<readonly ChatSummary[]>([])
  /** Each chat the core had at the position, open or closed. */
  chats = $state.raw<ReadonlyMap<ChatId, DeepReadonly<Chat>>>(new Map())
  /**
   * What went wrong, if the copies couldn't take what the server sent:
   * changes with a gap or that don't fit, which a viewer would answer by
   * asking for a picture. Here it's a bug in the server or CAF, so it's
   * shown rather than hidden.
   */
  problem = $state<string | null>(null)

  #copies = new Map<ChatId, ChatCopy>()
  /**
   * While taking a move back: the chats given a picture so far. Moving
   * back gives every chat the core had a Picture, so a copy not given one
   * is of a chat that didn't exist yet there.
   */
  #pictured: Set<ChatId> | null = null

  /**
   * A new position is coming. Moving back, or the first move after
   * following, every chat comes whole; moving forward, only what changed.
   */
  begin(whole: boolean): void {
    this.#pictured = whole ? new Set() : null
    this.problem = null
  }

  /** Takes one message. */
  take(message: ToViewer): void {
    switch (message.type) {
      case 'Welcome':
        this.summaries = message.value.chats
        break
      case 'Chats':
        this.summaries = message.value
        if (this.#pictured !== null) {
          // Moving back, chats newer than any open there may not exist
          // yet, and if none has changes, no Picture will come to say so.
          // (A closed one among them gets its Picture next.)
          const newest = Math.max(0, ...message.value.map((chat) => chat.id))
          for (const id of this.#copies.keys()) if (id > newest) this.#copies.delete(id)
          this.chats = new Map([...this.#copies].map(([id, copy]) => [id, copy.chat]))
        }
        break
      case 'Picture': {
        const copy = new ChatCopy(message.value.chat, message.value.latest)
        this.#copies.set(copy.chat.id, copy)
        this.#pictured?.add(copy.chat.id)
        this.#publish()
        break
      }
      case 'Changes': {
        const copy = this.#copies.get(message.value.chat)
        if (copy === undefined) {
          this.problem = `Changes came for chat ${message.value.chat} before its picture.`
          break
        }
        if (copy.take(message.value.changes) === 'need-picture') {
          const first = message.value.changes[0]?.no
          this.problem = `Chat ${message.value.chat}'s copy couldn't take the changes from ${first ?? '?'} on (it had ${copy.latest}).`
        }
        this.#publish()
        break
      }
      default:
        // Ended, Done and the rest aren't sent as Past.
        break
    }
  }

  /** Forgets everything: the next position sends every chat whole. */
  clear(): void {
    this.#copies.clear()
    this.#pictured = null
    this.summaries = []
    this.chats = new Map()
    this.problem = null
  }

  #publish(): void {
    const pictured = this.#pictured
    if (pictured !== null) for (const id of this.#copies.keys()) if (!pictured.has(id)) this.#copies.delete(id)
    this.chats = new Map([...this.#copies].map(([id, copy]) => [id, copy.chat]))
  }
}
