// What the chat view may do, from where the app is.

import type { ChatId, Shelf } from '@clackatoo/caf'
import { orRestored, restoreOnceFree, type oneAtATime } from './chat/actions.ts'
import type { LinkState } from './link.ts'

/**
 * Whether the user can act on chats: only live, following (not looking
 * back), and with `/viewer` up. Otherwise the chat view is read-only: a
 * chat shown while the link is down is as it was when the link dropped,
 * and anything asked of it would fail.
 */
export function canAct(where: { mode: 'Live' | 'Replay' | null; following: boolean; viewer: LinkState | null }): boolean {
  return where.mode === 'Live' && where.following && where.viewer === 'open'
}

/**
 * The live chat, to show: only while it's the one chosen, never one
 * watched before it, nor one watched again late.
 */
export function shownChat<C extends { id: ChatId }>(selected: ChatId | null, live: C | null | undefined): C | null {
  return live != null && live.id === selected ? live : null
}

/** What acting on the chat chosen needs. */
export interface ActorParts {
  /** The chat chosen now. */
  chosen(): ChatId | null
  /** The shelf it's saved on, if it is. */
  shelf(chat: ChatId): Shelf | undefined
  /** Restores it. */
  restore(chat: ChatId, shelf: Shelf): Promise<unknown>
  /** Watches it, again. */
  watch(chat: ChatId): void
  /** The restores under way, one per chat. */
  restoring: ReturnType<typeof oneAtATime<ChatId>>
}

/**
 * Acts on the chat chosen, restoring it first if it's saved and CAF no
 * longer has it, as when it was put away as it was archived: sending to
 * it, or unarchiving it, then finds it, archived still. Two actions at
 * once restore it once. Its watch may have ended as it was put away, so
 * it's watched again, once restored and once acted on, however that went,
 * unless another chat is chosen by then.
 */
export function chatActor(parts: ActorParts): <T>(act: (chat: ChatId) => Promise<T>) => Promise<T> {
  return async (act) => {
    const chat = parts.chosen()
    if (chat === null) throw new Error('No chat is chosen.')
    const again = () => {
      if (parts.chosen() === chat) parts.watch(chat)
    }
    const shelf = parts.shelf(chat)
    const restore =
      shelf === undefined
        ? undefined
        : () =>
            parts.restoring(chat, async () => {
              await restoreOnceFree(() => parts.restore(chat, shelf))
              again()
            })
    try {
      return await orRestored(() => act(chat), restore)
    } finally {
      again()
    }
  }
}
