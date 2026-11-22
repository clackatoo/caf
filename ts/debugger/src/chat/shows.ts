// What the chat view shows, for an app that draws more beside it.

import type { Chat, DeepReadonly } from '@clackatoo/caf'

/**
 * Whether the chat view shows its agent's install and sign-in, link,
 * code and paste box: on any chat but an archived one, restored or not,
 * or a closed one. An app that also shows the agents elsewhere, as the
 * debugger's panel does, can leave an agent's sign-in to the chat only
 * while this is true, or it shows nowhere.
 */
export function showsAgent(chat: DeepReadonly<Chat>): boolean {
  return chat.state.type !== 'Closed' && chat.archived === null
}
