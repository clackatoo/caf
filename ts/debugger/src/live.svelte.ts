// The live chats, over `/viewer`: exactly what any host's viewer does,
// with the TypeScript caf-sdk's `Connection`.

import {
  CafError,
  Connection,
  webSocket,
  type AgentName,
  type Chat,
  type ChatId,
  type ChatSummary,
  type DeepReadonly,
  type Watching,
} from '@clackatoo/caf'
import type { LinkState } from './link.ts'

export class Live {
  link = $state<LinkState>('connecting')
  agents = $state.raw<readonly AgentName[]>([])
  chats = $state.raw<readonly Readonly<ChatSummary>[]>([])
  /** The chat being watched, once it has come. */
  chat = $state.raw<DeepReadonly<Chat> | null>(null)
  /** Why connecting failed, if it did. */
  problem = $state<string | null>(null)

  #caf: Connection | null = null
  #watching: Watching | null = null
  #watched: ChatId | null = null

  /** Connects to `url`; resolves once welcomed, or with `problem` set. */
  async connect(url: string): Promise<void> {
    this.link = 'connecting'
    this.problem = null
    try {
      const caf = await Connection.open(await webSocket(url), { signal: AbortSignal.timeout(10_000) })
      this.#caf = caf
      this.agents = caf.agents
      this.chats = caf.chats
      caf.onChats((chats) => (this.chats = chats))
      caf.onClose(() => {
        if (this.#caf !== caf) return
        this.link = 'closed'
        this.#caf = null
        this.#watching = null
      })
      this.link = 'open'
      if (this.#watched !== null) this.watch(this.#watched)
    } catch (error) {
      this.link = 'closed'
      this.problem = error instanceof Error ? error.message : String(error)
    }
  }

  /** Follows one chat, in place of the one before. */
  watch(chat: ChatId | null): void {
    if (this.#watched === chat && this.#watching !== null) return
    this.#watching?.stop()
    this.#watching = null
    this.#watched = chat
    this.chat = null
    if (chat === null || this.#caf === null) return
    this.#watching = this.#caf.watch(chat, (event) => {
      if (this.#watched !== chat) return
      if (event.type === 'Chat') this.chat = event.chat
    })
  }

  /** The connection, for an action: rejects as `Disconnected` without one. */
  get caf(): Connection {
    if (this.#caf === null) throw new CafError({ type: 'Disconnected' })
    return this.#caf
  }

  close(): void {
    this.#caf?.disconnect()
  }
}
