// The chats as they were at a position, built from the inspector
// channel's `Past` messages just as a viewer builds them from `/viewer`:
// the same `ToViewer` messages, into the SDK's `ChatCopy`. So looking
// back in time exercises `ChatCopy` and `apply` as a real viewer does.

import { ChatCopy, type ChatId, type ChatSummary, type DeepReadonly, type ToViewer, type Chat } from '@clackatoo/caf'

export class PastChats {
  /** Every chat the core had at the position, closed ones too. */
  summaries = $state.raw<readonly ChatSummary[]>([])
  /** Each of those chats, as it was. */
  chats = $state.raw<ReadonlyMap<ChatId, DeepReadonly<Chat>>>(new Map())
  /**
   * What went wrong, if the copies couldn't take what the server sent:
   * changes with a gap or that don't fit, which a viewer would answer by
   * asking for a picture. Here it's a bug in the server or CAF, so it's
   * shown rather than hidden.
   */
  problem = $state<string | null>(null)

  #copies = new Map<ChatId, ChatCopy>()

  /** A new position is coming: what went wrong at the last is forgotten. */
  begin(): void {
    this.problem = null
  }

  /** Takes one message. */
  take(message: ToViewer): void {
    switch (message.type) {
      case 'Welcome':
        this.summaries = message.value.chats
        break
      case 'Chats':
        // Every chat the core had at the position, closed ones too: one
        // not in it wasn't opened yet there.
        this.summaries = message.value
        for (const id of this.#copies.keys()) if (!message.value.some((chat) => chat.id === id)) this.#copies.delete(id)
        this.#publish()
        break
      case 'Picture': {
        const copy = new ChatCopy(message.value.chat, message.value.latest)
        this.#copies.set(copy.chat.id, copy)
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

  #publish(): void {
    this.chats = new Map([...this.#copies].map(([id, copy]) => [id, copy.chat]))
  }
}
