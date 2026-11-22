// The live chats, over `/viewer`: exactly what any host's viewer does,
// with the TypeScript caf-sdk's `Connection`.

import {
  CafError,
  Connection,
  type AgentStatus,
  type Chat,
  type ChatId,
  type ChatSummary,
  type DeepReadonly,
  type Rights,
  type SavedChat,
  type Transport,
  type Watching,
} from '@clackatoo/caf'
import type { LinkState } from './link.ts'

export class Live {
  link = $state<LinkState>('connecting')
  /** Every agent on offer, with whether it's installed and signed in. */
  agents = $state.raw<readonly DeepReadonly<AgentStatus>[]>([])
  /** What the host lets this page do, once connected. */
  may = $state.raw<Readonly<Rights>>({ act: false, agents: false, usage: false })
  chats = $state.raw<readonly Readonly<ChatSummary>[]>([])
  /** The saved chats on the shelves the host lists, open or not. */
  saved = $state.raw<readonly DeepReadonly<SavedChat>[]>([])
  /** The chat being watched, once it has come. */
  chat = $state.raw<DeepReadonly<Chat> | null>(null)
  /** Why connecting failed, if it did. */
  problem = $state<string | null>(null)

  #caf: Connection | null = null
  #watching: Watching | null = null
  #watched: ChatId | null = null

  /**
   * Connects over the transport `open` gives, such as `webSocket(url)`;
   * resolves once welcomed, or with `problem` set. The chat chosen before,
   * if any, is watched again.
   */
  async connect(open: () => Promise<Transport>): Promise<void> {
    this.link = 'connecting'
    this.problem = null
    try {
      const caf = await Connection.open(await open(), { signal: AbortSignal.timeout(10_000) })
      this.#caf = caf
      this.agents = caf.agents
      this.may = caf.may
      this.chats = caf.chats
      // CAF says them right after the agents, which may be before `open`
      // resolves or after: both are followed.
      this.saved = caf.saved
      caf.onChats((chats) => (this.chats = chats))
      caf.onAgents((agents) => (this.agents = agents))
      caf.onSaved((saved) => (this.saved = saved))
      caf.onClose(() => {
        if (this.#caf !== caf) return
        // The chat shown stays as it was, read-only (see `canAct`) until
        // the link is back.
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

  /**
   * Follows one chat, in place of the one before. With the link down, it's
   * remembered, and nothing is shown for it until the link is back. A chat
   * whose watch has ended, as one closed and put away does, stays shown
   * as it was last, and is watched afresh when it's chosen again, such as
   * once it's restored, under the same id: shown as it was meanwhile, so a
   * view drawing it, and what's being written in it, stay.
   */
  watch(chat: ChatId | null): void {
    if (this.#watched === chat && this.#watching !== null) return
    this.#watching?.stop()
    this.#watching = null
    this.#watched = chat
    if (this.chat?.id !== chat) this.chat = null
    if (chat === null || this.#caf === null) return
    const watching = this.#caf.watch(chat, (event) => {
      if (this.#watched !== chat) return
      if (event.type === 'Chat') this.chat = event.chat
      else if (event.type === 'Ended' && this.#watching === watching) this.#watching = null
    })
    this.#watching = watching
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
