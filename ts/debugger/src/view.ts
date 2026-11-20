// What the chat view may do, from where the app is.

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
