// The chat view draws every chat in caf-core's conformance fixtures, at
// every change: real chats, as the core builds them.

import { cleanup } from '@testing-library/svelte'
import { afterEach, describe, expect, it } from 'vitest'
import { fixtures } from '../fixtures.ts'
import { draw } from './build.ts'

afterEach(cleanup)

const all = fixtures()

describe('every fixture', () => {
  it('there are some', () => expect(all.length).toBeGreaterThan(100))

  for (const fixture of all) {
    it(`draws ${fixture.name}`, () => {
      for (const chat of [fixture.picture.chat, ...fixture.steps.map((step) => step.chat)]) {
        const { container, unmount } = draw(chat)
        expect(container.querySelectorAll('.turn')).toHaveLength(chat.turns.length)
        const items = chat.turns.reduce((n, turn) => n + turn.items.length, 0)
        const drawn = container.querySelectorAll('.items > *')
        expect(drawn.length).toBe(items)
        unmount()
      }
      // Some are big, such as a chatty command's output at every change,
      // and the machine may be busy.
    }, 60_000)
  }
})
