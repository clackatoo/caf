// The app's end of the inspector channel (`/inspect`): the steps so far,
// where the app is looking, the core's state at positions it asked for,
// and the chats as they were.

import type { JsonValue } from '@clackatoo/caf'
import type { LinkState } from '../link.ts'
import type { FromInspector, Mode, Step, ToInspector } from './channel.ts'
import { PastChats } from './past.svelte.ts'

/** A WebSocket, or anything that behaves like one for this. */
export interface Socket {
  send(text: string): void
  close(): void
  addEventListener(type: 'open' | 'close', listener: () => void): void
  addEventListener(type: 'message', listener: (event: { data: unknown }) => void): void
}

/** How many positions' states are kept: the one shown, the one before, and a few around. */
const KEPT_STATES = 16

export class Inspector {
  link = $state<LinkState>('connecting')
  /** What the server is doing, once it has said. */
  hello = $state.raw<{ mode: Mode; file: string | null; run: string | null } | null>(null)
  /** Where the app is looking. Steps are numbered from 1; position `at` is just after step `at`, and 0 is before any. */
  position = $state.raw({ live: true, at: 0, end: 0 })
  /** The first step where the replay check failed, if one has. */
  diverged = $state.raw<{ at: number; expected: unknown[]; got: unknown[] | null } | null>(null)
  /** The chats as they were at the position, while looking back. */
  readonly past = new PastChats()
  /** What the server sent that this app couldn't read. */
  problem = $state<string | null>(null)

  /** Steps by number, from 1. Not reactive itself: `count` is. */
  readonly #steps: Step[] = []
  count = $state(0)
  /** The core's state at positions asked for, newest last. */
  #states = $state.raw<ReadonlyMap<number, JsonValue>>(new Map())
  #asked = new Set<number>()
  #socket: Socket

  constructor(socket: Socket) {
    this.#socket = socket
    socket.addEventListener('open', () => (this.link = 'open'))
    socket.addEventListener('close', () => (this.link = 'closed'))
    socket.addEventListener('message', (event) => {
      if (typeof event.data !== 'string') return
      let message: ToInspector
      try {
        message = JSON.parse(event.data) as ToInspector
      } catch {
        this.problem = 'The server sent something that isn’t JSON.'
        return
      }
      this.#hear(message)
    })
  }

  /** Step `no`, if it has come. */
  step(no: number): Step | undefined {
    return this.#steps[no - 1]
  }

  /** The core's state at a position, if it has come. Ask for it with `stateAt`. */
  state(at: number): JsonValue | undefined {
    return this.#states.get(at)
  }

  get live(): boolean {
    return this.hello?.mode.type === 'Live'
  }

  /** Look at a position: live, this stops following. */
  seek(step: number): void {
    this.#send({ type: 'Seek', value: { step: Math.max(0, Math.min(step, this.position.end)) } })
  }

  /** Follow live again. */
  follow(): void {
    this.#send({ type: 'Follow' })
  }

  /** Asks for the core's state at a position, unless it has it or has asked. */
  stateAt(step: number): void {
    if (step < 0 || this.#states.has(step) || this.#asked.has(step)) return
    // Not sent until the link is open: whoever asks, asks again then.
    if (this.#send({ type: 'StateAt', value: { step } })) this.#asked.add(step)
  }

  close(): void {
    this.#socket.close()
  }

  /** Sends a message, if the link is open: whether it was sent. */
  #send(message: FromInspector): boolean {
    if (this.link !== 'open') return false
    this.#socket.send(JSON.stringify(message))
    return true
  }

  #hear(message: ToInspector): void {
    switch (message.type) {
      case 'Hello':
        this.hello = message.value
        break
      case 'Steps': {
        const { from, steps } = message.value
        if (from !== this.#steps.length + 1) {
          this.problem = `Steps came from ${from}, but the last one here is ${this.#steps.length}.`
          return
        }
        this.#steps.push(...steps)
        this.count = this.#steps.length
        const end = this.#steps.length
        this.position = { ...this.position, end, at: this.position.live ? end : this.position.at }
        break
      }
      case 'Position': {
        const was = this.position
        const now = message.value
        if (!now.live) {
          // Moving back, or the first move since following, every chat comes whole.
          this.past.begin(was.live || now.at < was.at)
        }
        this.position = now
        break
      }
      case 'State': {
        const states = new Map(this.#states)
        states.delete(message.value.at)
        states.set(message.value.at, message.value.core)
        while (states.size > KEPT_STATES) states.delete(states.keys().next().value!)
        this.#states = states
        this.#asked.delete(message.value.at)
        break
      }
      case 'Past':
        // One on its way before a later seek is out of date.
        if (message.value.at === this.position.at && !this.position.live) this.past.take(message.value.message)
        break
      case 'Diverged':
        this.diverged = message.value
        break
      default:
        // A message of a later server's: the channel is the app's own, so
        // this is a mismatch of versions, not something to act on.
        this.problem = `The server sent a message this app doesn't know (${(message as { type?: unknown }).type}).`
    }
  }
}
