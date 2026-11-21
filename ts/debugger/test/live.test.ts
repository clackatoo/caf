// The live chats over /viewer: while the link is down, a chat chosen
// isn't shown as the one before it, and nothing can be done.

import { describe, expect, it } from 'vitest'
import type { Transport } from '@clackatoo/caf'
import { Live } from '../src/live.svelte.ts'
import { canAct } from '../src/view.ts'

const one = '00000000-0000-0000-0000-000000000001'
const two = '00000000-0000-0000-0000-000000000002'

/** A saved chat, as `Saved` lists it. */
function saved(chat: string, state: { type: string } = { type: 'Saved' }) {
  const header = { chat, agent: 'fake', profile: null, cwd: '/a', made: 1, changed: 2, label: null, first: 'hi', turns: 1, closed: false }
  return { shelf: 'work', header, state }
}

/**
 * A transport CAF welcomes at once, and whose link the test can drop.
 * Given `first`, CAF says those right after the agents.
 */
function transport(first: unknown[] = []): Transport & { drop(): void; deliver(message: unknown): void; sent: { type: string }[] } {
  const messages: ((text: string) => void)[] = []
  const closes: (() => void)[] = []
  const sent: { type: string }[] = []
  return {
    sent,
    send(text) {
      const message = JSON.parse(text) as { type: string }
      sent.push(message)
      if (message.type === 'Hello') {
        const welcome = { type: 'Welcome', value: { version: 1, agents: ['fake'], chats: [{ id: one, agent: 'fake', cwd: '/a', label: null, shelf: null }], run: 'r', may: { act: true, agents: false } } }
        const agents = { type: 'Agents', value: [{ name: 'fake', install: { type: 'Host' }, sign_in: { type: 'Unknown' }, can_sign_in: false }] }
        queueMicrotask(() => {
          for (const message of [welcome, agents, ...first]) messages.forEach((listener) => listener(JSON.stringify(message)))
        })
      }
    },
    onMessage: (listener) => void messages.push(listener),
    onClose: (listener) => void closes.push(listener),
    close() {
      this.drop()
    },
    drop: () => closes.splice(0).forEach((listener) => listener()),
    deliver: (message) => messages.forEach((listener) => listener(JSON.stringify(message))),
  }
}

describe('live', () => {
  it("doesn't show the chat before under another one chosen while the link is down", async () => {
    const link = transport()
    const live = new Live()
    await live.connect(async () => link)
    expect(live.link).toBe('open')
    live.watch(one)
    link.drop()
    expect(live.link).toBe('closed')
    live.watch(two)
    expect(live.chat).toBeNull()
  })

  it('watches the chat chosen again once connected again', async () => {
    const live = new Live()
    live.watch(one)
    const link = transport()
    await live.connect(async () => link)
    expect(link.sent.map((message) => message.type)).toEqual(['Hello', 'Watch'])
  })
})

describe('the agents', () => {
  it('follows their statuses', async () => {
    const link = transport()
    const live = new Live()
    await live.connect(async () => link)
    expect(live.agents.map((agent) => agent.install.type)).toEqual(['Host'])
    expect(live.may).toEqual({ act: true, agents: false })
    const signedIn = [{ name: 'fake', install: { type: 'Host' }, sign_in: { type: 'SignedIn', value: { type: 'Account' } }, can_sign_in: true }]
    link.deliver({ type: 'Agents', value: signedIn })
    expect(live.agents).toEqual(signedIn)
  })
})

describe('the saved chats', () => {
  it('follows them, whether CAF says them before the connection is made or after', async () => {
    const before = [saved(one)]
    const link = transport([{ type: 'Saved', value: before }])
    const live = new Live()
    await live.connect(async () => link)
    expect(live.saved).toEqual(before)
    const after = [saved(one, { type: 'Loaded' }), saved(two)]
    link.deliver({ type: 'Saved', value: after })
    expect(live.saved).toEqual(after)
  })
})

describe('what can be done', () => {
  it('only live, following, and with /viewer up', () => {
    expect(canAct({ mode: 'Live', following: true, viewer: 'open' })).toBe(true)
    expect(canAct({ mode: 'Live', following: true, viewer: 'closed' })).toBe(false)
    expect(canAct({ mode: 'Live', following: true, viewer: 'connecting' })).toBe(false)
    expect(canAct({ mode: 'Live', following: false, viewer: 'open' })).toBe(false)
    expect(canAct({ mode: 'Replay', following: false, viewer: null })).toBe(false)
    expect(canAct({ mode: null, following: true, viewer: null })).toBe(false)
  })
})
