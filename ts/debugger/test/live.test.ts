// The live chats over /viewer: while the link is down, a chat chosen
// isn't shown as the one before it, and nothing can be done.

import { describe, expect, it, vi } from 'vitest'
import { CafError, type ChatId, type Transport } from '@clackatoo/caf'
import { oneAtATime } from '../src/chat/actions.ts'
import { Live } from '../src/live.svelte.ts'
import { canAct, chatActor, shownChat } from '../src/view.ts'

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
        const welcome = { type: 'Welcome', value: { version: 1, agents: ['fake'], chats: [{ id: one, agent: 'fake', cwd: '/a', label: null, shelf: null, archived: null }], run: 'r', may: { act: true, agents: false } } }
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

  it('keeps a chat whose watch ended as it was, and watches it afresh when it is chosen again', async () => {
    const link = transport()
    const live = new Live()
    await live.connect(async () => link)
    live.watch(one)
    const chat = { id: one, agent: 'fake', cwd: '/a', state: { type: 'Closed' }, turns: [], usage: null, questions: [], settings: { offered: [], chosen: [], own: [] }, archived: 1 }
    link.deliver({ type: 'Picture', value: { chat, latest: 0 } })
    expect(live.chat?.archived).toBe(1)
    link.deliver({ type: 'Ended', value: { chat: one } })
    expect(live.chat?.archived).toBe(1)
    live.watch(one)
    expect(link.sent.map((message) => message.type)).toEqual(['Hello', 'Watch', 'Watch'])
    // Shown as it was until it comes again: a view drawing it, and what's
    // being written in it, stay.
    expect(live.chat?.archived).toBe(1)
    link.deliver({ type: 'Picture', value: { chat: { ...chat, archived: null }, latest: 1 } })
    expect(live.chat?.archived).toBeNull()
    // Another chat shows nothing of this one.
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
    expect(live.may).toEqual({ act: true, agents: false, usage: false })
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

describe('the chat shown', () => {
  it('is the live chat only while it is the one chosen', () => {
    const chat = { id: one }
    expect(shownChat(one, chat)).toBe(chat)
    expect(shownChat(two, chat)).toBeNull()
    expect(shownChat(null, chat)).toBeNull()
    expect(shownChat(one, null)).toBeNull()
  })
})

describe('acting on the chat chosen', () => {
  const gone = () => new CafError({ type: 'Refused', value: { type: 'NoSuchChat' } })

  /** An actor on `chosen`, its restores held until `finish`, saved on a shelf unless `saved` is false. */
  function actor(chosen: { now: ChatId | null }, saved = true) {
    let finish = () => {}
    const restore = vi.fn(() => new Promise<ChatId>((done) => (finish = () => done(one))))
    const watch = vi.fn()
    const act = chatActor({
      chosen: () => chosen.now,
      shelf: () => (saved ? 'work' : undefined),
      restore,
      watch,
      restoring: oneAtATime<ChatId>(),
    })
    return { act, restore, watch, finish: () => finish() }
  }

  it('watches the chat again once restored and acted on, while it is still the one chosen', async () => {
    const chosen = { now: one as ChatId | null }
    const { act, restore, watch, finish } = actor(chosen)
    const send = vi.fn().mockRejectedValueOnce(gone()).mockResolvedValue(1200)
    const sending = act(send)
    await vi.waitFor(() => expect(restore).toHaveBeenCalledWith(one, 'work'))
    finish()
    await expect(sending).resolves.toBe(1200)
    expect(send).toHaveBeenCalledWith(one)
    expect(watch).toHaveBeenCalledWith(one)
  })

  it("never watches it again once another chat is chosen meanwhile", async () => {
    const chosen = { now: one as ChatId | null }
    const { act, restore, watch, finish } = actor(chosen)
    const send = vi.fn().mockRejectedValueOnce(gone()).mockResolvedValue(1200)
    const sending = act(send)
    await vi.waitFor(() => expect(restore).toHaveBeenCalled())
    chosen.now = two
    finish()
    await expect(sending).resolves.toBe(1200)
    expect(send).toHaveBeenLastCalledWith(one)
    expect(watch).not.toHaveBeenCalled()
  })

  it('watches it again after an act that needed no restore, as its watch may have ended', async () => {
    const chosen = { now: one as ChatId | null }
    const { act, restore, watch } = actor(chosen)
    await expect(act(async () => 1200)).resolves.toBe(1200)
    expect(restore).not.toHaveBeenCalled()
    expect(watch).toHaveBeenCalledWith(one)
  })

  it('watches it again after a restore whose act then fails, and leaves the failure to the caller', async () => {
    const chosen = { now: one as ChatId | null }
    const { act, restore, watch, finish } = actor(chosen)
    const refused = new CafError({ type: 'Refused', value: { type: 'NoSuchAgent' } })
    const send = vi.fn().mockRejectedValueOnce(gone()).mockRejectedValue(refused)
    const sending = act(send)
    await vi.waitFor(() => expect(restore).toHaveBeenCalled())
    finish()
    await expect(sending).rejects.toBe(refused)
    expect(watch).toHaveBeenCalledWith(one)
  })

  it("restores nothing for a chat that isn't saved", async () => {
    const chosen = { now: one as ChatId | null }
    const { act, restore } = actor(chosen, false)
    await expect(act(vi.fn().mockRejectedValue(gone()))).rejects.toEqual(gone())
    expect(restore).not.toHaveBeenCalled()
  })
})
