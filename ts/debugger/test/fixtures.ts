// caf-core's conformance fixtures (`fixtures/apply/`): real chats, change
// by change, as the core recorded them.

import { readdirSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import type { Chat, ChangeNo, Numbered } from '@clackatoo/caf'

// Under jsdom `import.meta.url` isn't a file URL, but vitest gives the folder.
const folder = resolve(import.meta.dirname, '../../../fixtures/apply') + '/'

export interface Fixture {
  name: string
  picture: { chat: Chat; latest: ChangeNo }
  /** The changes that fit, each with the chat after it. */
  steps: { change: Numbered; chat: Chat }[]
}

/** Every fixture whose changes all fit. */
export function fixtures(): Fixture[] {
  return readdirSync(folder)
    .filter((name) => name.endsWith('.json'))
    .sort()
    .map((name) => {
      const parsed = JSON.parse(readFileSync(folder + name, 'utf8')) as { picture: Fixture['picture']; steps: ({ change: Numbered } & ({ chat: Chat } | { mismatch: unknown }))[] }
      const steps = parsed.steps.filter((step): step is { change: Numbered; chat: Chat } => 'chat' in step)
      return { name, picture: parsed.picture, steps }
    })
}
