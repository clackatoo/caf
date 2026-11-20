// The inspector channel's client: steps and the position, the state
// cache, out-of-date Past messages, and the chats as they were built
// from Past through the SDK's ChatCopy.

import { describe, expect, it } from 'vitest'
import type { Directive, Event, ToViewer } from '@clackatoo/caf'
import type { Step, ToInspector } from '../../src/inspector/channel.ts'
import { Inspector, type Socket } from '../../src/inspector/client.svelte.ts'
import { fixtures } from '../fixtures.ts'

class FakeSocket implements Socket {
  sent: unknown[] = []
  #listeners: Record<string, ((event: { data: unknown }) => void)[]> = {}
  send(text: string) {
    this.sent.push(JSON.parse(text))
  }
  close() {
    this.emit('close')
  }
  addEventListener(type: string, listener: (event: { data: unknown }) => void) {
    ;(this.#listeners[type] ??= []).push(listener)
  }
  emit(type: string, data?: unknown) {
    for (const listener of this.#listeners[type] ?? []) listener({ data })
  }
  say(message: ToInspector) {
    this.emit('message', JSON.stringify(message))
  }
}

function step(no: number, directives: Directive[] = []): Step {
  const event: Event = { type: 'Woken' }
  return { no, now: { wall: 1_800_000_000_000 + no, mono: no * 1000 }, event, directives }
}

function open(): { socket: FakeSocket; inspector: Inspector } {
  const socket = new FakeSocket()
  const inspector = new Inspector(socket)
  socket.emit('open')
  socket.say({ type: 'Hello', value: { mode: { type: 'Live' }, file: '/rec.jsonl', run: 'r1' } })
  return { socket, inspector }
}

describe('steps and the position', () => {
  it('follows the end while live', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Steps', value: { from: 1, steps: [step(1), step(2)] } })
    socket.say({ type: 'Position', value: { live: true, at: 2, end: 2 } })
    socket.say({ type: 'Steps', value: { from: 3, steps: [step(3)] } })
    expect(inspector.count).toBe(3)
    expect(inspector.step(3)?.no).toBe(3)
    expect(inspector.position).toEqual({ live: true, at: 3, end: 3 })
    expect(inspector.live).toBe(true)
  })

  it('stays put while looking back, as steps go on arriving', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Steps', value: { from: 1, steps: [step(1), step(2)] } })
    socket.say({ type: 'Position', value: { live: false, at: 1, end: 2 } })
    socket.say({ type: 'Steps', value: { from: 3, steps: [step(3)] } })
    expect(inspector.position).toEqual({ live: false, at: 1, end: 3 })
  })

  it('says when steps come with a gap, and takes none of them', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Steps', value: { from: 2, steps: [step(2)] } })
    expect(inspector.count).toBe(0)
    expect(inspector.problem).toContain('Steps came from 2')
  })

  it('asks to seek, within the steps there are, and to follow', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Steps', value: { from: 1, steps: [step(1), step(2)] } })
    inspector.seek(9)
    inspector.seek(-3)
    inspector.follow()
    expect(socket.sent).toEqual([{ type: 'Seek', value: { step: 2 } }, { type: 'Seek', value: { step: 0 } }, { type: 'Follow' }])
  })

  it('records where the replay diverged', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Diverged', value: { at: 4, expected: [], got: null } })
    expect(inspector.diverged).toEqual({ at: 4, expected: [], got: null })
  })

  it("says when a message is one it doesn't know", () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Telegram' } as never)
    expect(inspector.problem).toContain('Telegram')
  })
})

describe('states', () => {
  it('asks once for each position, and keeps what comes', () => {
    const { socket, inspector } = open()
    inspector.stateAt(3)
    inspector.stateAt(3)
    expect(socket.sent).toEqual([{ type: 'StateAt', value: { step: 3 } }])
    socket.say({ type: 'State', value: { at: 3, core: { chats: [] } } })
    expect(inspector.state(3)).toEqual({ chats: [] })
    inspector.stateAt(3)
    expect(socket.sent).toHaveLength(1)
  })

  it('asks again once the link opens, if it was asked before', () => {
    const socket = new FakeSocket()
    const inspector = new Inspector(socket)
    inspector.stateAt(2)
    expect(socket.sent).toEqual([])
    socket.emit('open')
    inspector.stateAt(2)
    expect(socket.sent).toEqual([{ type: 'StateAt', value: { step: 2 } }])
  })

  it('keeps only the latest few', () => {
    const { socket, inspector } = open()
    for (let at = 0; at < 40; at++) socket.say({ type: 'State', value: { at, core: at } })
    expect(inspector.state(0)).toBeUndefined()
    expect(inspector.state(39)).toBe(39)
  })
})

describe('the chats as they were', () => {
  const fixture = fixtures().find((each) => each.steps.length >= 6)!
  const id = fixture.picture.chat.id
  const summary = { id, agent: fixture.picture.chat.agent, cwd: fixture.picture.chat.cwd }

  function past(socket: FakeSocket, at: number, message: ToViewer) {
    socket.say({ type: 'Past', value: { at, message } })
  }

  it('builds them from a Picture, then Changes, through ChatCopy', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Position', value: { live: false, at: 5, end: 9 } })
    past(socket, 5, { type: 'Chats', value: [summary] })
    past(socket, 5, { type: 'Picture', value: fixture.picture })
    expect(inspector.past.summaries).toEqual([summary])
    expect(inspector.past.chats.get(id)).toEqual(fixture.picture.chat)

    socket.say({ type: 'Position', value: { live: false, at: 6, end: 9 } })
    past(socket, 6, { type: 'Changes', value: { chat: id, changes: fixture.steps.map((step) => step.change) } })
    expect(inspector.past.chats.get(id)).toEqual(fixture.steps.at(-1)!.chat)
    expect(inspector.past.problem).toBeNull()
  })

  it('drops a Past on its way before a later seek', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Position', value: { live: false, at: 5, end: 9 } })
    past(socket, 4, { type: 'Picture', value: fixture.picture })
    expect(inspector.past.chats.size).toBe(0)
  })

  it('says when the copy needs a picture: a gap is a bug here, not something to hide', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Position', value: { live: false, at: 5, end: 9 } })
    past(socket, 5, { type: 'Picture', value: fixture.picture })
    socket.say({ type: 'Position', value: { live: false, at: 6, end: 9 } })
    past(socket, 6, { type: 'Changes', value: { chat: id, changes: fixture.steps.slice(2).map((step) => step.change) } })
    expect(inspector.past.problem).toContain(`Chat ${id}`)
  })

  it('drops the copy of any chat the list of chats leaves out', () => {
    const { socket, inspector } = open()
    const other = { ...fixture.picture, chat: { ...fixture.picture.chat, id: id + 100 } }
    socket.say({ type: 'Position', value: { live: false, at: 8, end: 9 } })
    past(socket, 8, { type: 'Chats', value: [summary, { ...summary, id: id + 100 }] })
    past(socket, 8, { type: 'Picture', value: fixture.picture })
    past(socket, 8, { type: 'Picture', value: other })
    expect([...inspector.past.chats.keys()]).toEqual([id, id + 100])

    socket.say({ type: 'Position', value: { live: false, at: 2, end: 9 } })
    past(socket, 2, { type: 'Chats', value: [summary] })
    expect([...inspector.past.chats.keys()]).toEqual([id])
    past(socket, 2, { type: 'Picture', value: fixture.picture })
    expect([...inspector.past.chats.keys()]).toEqual([id])
  })

  it('forgets every chat, moving back to before any', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Position', value: { live: false, at: 8, end: 9 } })
    past(socket, 8, { type: 'Chats', value: [summary] })
    past(socket, 8, { type: 'Picture', value: fixture.picture })
    socket.say({ type: 'Position', value: { live: false, at: 0, end: 9 } })
    past(socket, 0, { type: 'Chats', value: [] })
    expect(inspector.past.chats.size).toBe(0)
    expect(inspector.past.summaries).toEqual([])
  })

  it('keeps a closed chat, which the list still names', () => {
    const { socket, inspector } = open()
    const closed = { ...fixture.picture, chat: { ...fixture.picture.chat, state: { type: 'Closed' as const } } }
    socket.say({ type: 'Position', value: { live: false, at: 8, end: 9 } })
    past(socket, 8, { type: 'Chats', value: [summary] })
    past(socket, 8, { type: 'Picture', value: closed })
    socket.say({ type: 'Position', value: { live: false, at: 9, end: 9 } })
    past(socket, 9, { type: 'Chats', value: [summary] })
    expect(inspector.past.chats.get(id)?.state).toEqual({ type: 'Closed' })
  })

  it('takes nothing as Past while following', () => {
    const { socket, inspector } = open()
    socket.say({ type: 'Position', value: { live: true, at: 3, end: 3 } })
    past(socket, 3, { type: 'Picture', value: fixture.picture })
    expect(inspector.past.chats.size).toBe(0)
  })
})
