// The app's end of the inspector channel (`/inspect`): the steps so far,
// where the app is looking, the core's state at positions it asked for,
// and the chats as they were.

import type { JsonValue } from '@clackatoo/caf'
import type { LinkState } from '../link.ts'
import { PROTOCOL, type Check, type FromInspector, type Mode, type Step, type ToInspector } from './channel.ts'
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
/** How many states can wait to be asked for: while dragging, the oldest go. */
const WANTED = 4

export class Inspector {
  link = $state<LinkState>('connecting')
  /**
   * The link closed before it ever opened: the server refused it, most
   * likely for its token (caf-debugger restarted, or the page was opened
   * again without the address it printed), or it's not running.
   */
  refused = $state(false)
  #opened = false
  /** What the server is doing, once it has said. */
  hello = $state.raw<{ mode: Mode; file: string | null; run: string | null; tools: boolean } | null>(null)
  /** Where the app is looking. Steps are numbered from 1; position `at` is just after step `at`, and 0 is before any. */
  position = $state.raw({ live: true, at: 0, end: 0 })
  /** The first step where the replay check failed, if one has. */
  diverged = $state.raw<{ check: Check; at: number; expected: unknown[]; got: unknown[] | null } | null>(null)
  /**
   * The server speaks another version of the channel than this app was
   * built for: the page is from another build of caf-debugger. The link is
   * closed, and nothing more is taken from it or sent.
   */
  mismatch = $state.raw<{ server: unknown } | null>(null)
  /** The chats as they were at the position, while looking back. */
  readonly past = new PastChats()
  /** What the server sent that this app couldn't read. */
  problem = $state<string | null>(null)

  /** Steps by number, from 1. Not reactive itself: `count` is. */
  readonly #steps: Step[] = []
  count = $state(0)
  /** The core's state at positions asked for, newest last. */
  #states = $state.raw<ReadonlyMap<number, JsonValue>>(new Map())
  /**
   * The state asked for and not yet come. Only one is asked at a time:
   * the server answers only the last StateAt waiting when it gets to
   * them, so a second sent at once would leave the first unanswered.
   */
  #asking: number | null = null
  /** States wanted next, oldest first, at most `WANTED`. */
  #wanted: number[] = []
  #socket: Socket

  constructor(socket: Socket) {
    this.#socket = socket
    socket.addEventListener('open', () => {
      this.#opened = true
      this.link = 'open'
      this.#ask()
    })
    socket.addEventListener('close', () => {
      this.refused = !this.#opened
      this.link = 'closed'
    })
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

  /**
   * Look at a position, as the timeline and the Steps table do: the last
   * step, live, means following again.
   */
  look(step: number): void {
    if (this.live && step >= this.position.end) {
      if (!this.position.live) this.follow()
    } else if (step !== this.position.at || this.position.live) {
      this.seek(step)
    }
  }

  /** Follow live again. */
  follow(): void {
    this.#send({ type: 'Follow' })
  }

  /** Asks for the core's state at a position, unless it has it or has asked. */
  stateAt(asked: number): void {
    // The server takes a position past the end as the end: ask for that.
    const step = Math.min(asked, this.position.end)
    if (step < 0 || this.#states.has(step) || this.#asking === step) return
    if (!this.#wanted.includes(step)) this.#wanted.push(step)
    if (this.#wanted.length > WANTED) this.#wanted.shift()
    this.#ask()
  }

  /** Asks for the next state wanted, if none is on its way. */
  #ask(): void {
    while (this.#asking === null && this.#wanted.length > 0) {
      const step = this.#wanted[0]!
      if (this.#states.has(step)) {
        this.#wanted.shift()
        continue
      }
      // Not sent until the link is open: it stays wanted, and whoever
      // asked asks again then.
      if (!this.#send({ type: 'StateAt', value: { step } })) return
      this.#wanted.shift()
      this.#asking = step
    }
  }

  close(): void {
    this.#socket.close()
  }

  /** Sends a message, if the link is open: whether it was sent. */
  #send(message: FromInspector): boolean {
    if (this.link !== 'open' || this.mismatch !== null) return false
    this.#socket.send(JSON.stringify(message))
    return true
  }

  #hear(message: ToInspector): void {
    if (this.mismatch !== null) return
    switch (message.type) {
      case 'Hello': {
        const protocol: unknown = (message.value as { protocol?: unknown }).protocol
        if (protocol !== PROTOCOL) {
          this.mismatch = { server: protocol ?? null }
          this.#socket.close()
          this.link = 'closed'
          return
        }
        this.hello = message.value
        break
      }
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
        if (!message.value.live) this.past.begin()
        this.position = message.value
        break
      }
      case 'State': {
        const states = new Map(this.#states)
        states.delete(message.value.at)
        states.set(message.value.at, message.value.core)
        while (states.size > KEPT_STATES) states.delete(states.keys().next().value!)
        this.#states = states
        // Only one is ever on its way, so this answers it, whatever
        // position the server answered for.
        this.#asking = null
        this.#ask()
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
