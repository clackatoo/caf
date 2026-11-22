// Words for an agent's account limits, beside the Agents panel's. The
// maths is the SDK's (`pace`, `left`, `lowness`), so the debugger says
// what any viewer does.

import type { DeepReadonly, LimitState, Wall } from '@clackatoo/caf'
import { clock, dayAndTime } from './chat/time.ts'

const MINUTE = 60_000
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

/** How long until something, roughly: `under a minute`, `42 min`, `2 h 5 min`, `2 h`, `4 d 3 h`, `4 d`. */
export function until(ms: number): string {
  if (ms < MINUTE) return 'under a minute'
  const min = Math.floor(ms / MINUTE)
  if (ms < HOUR) return `${min} min`
  if (ms < DAY) return parts(Math.floor(min / 60), 'h', min % 60, 'min')
  return parts(Math.floor(ms / DAY), 'd', Math.floor((ms % DAY) / HOUR), 'h')
}

/** Two parts of a time, the second left out if it's nothing. */
function parts(big: number, bigUnit: string, small: number, smallUnit: string): string {
  return small === 0 ? `${big} ${bigUnit}` : `${big} ${bigUnit} ${small} ${smallUnit}`
}

/** What a window's bar says to a screen reader: what's left, and where an even pace would have it, if known. */
export function meterText(left: number, pace: number | null): string {
  return pace === null ? `${left}% left` : `${left}% left; at an even pace, ${100 - pace}% would be left`
}

/** When a window resets, from `now`: `resets in 2 h 5 min`, or `resetting` once it's due and not yet forgotten. */
export function resetsText(resets: Wall, now: Wall): string {
  return resets <= now ? 'resetting' : `resets in ${until(resets - now)}`
}

/** When CAF heard of a window: its time, if today, else its day and time too. */
export function asOfText(heard: Wall, now: Wall): string {
  const today = new Date(heard).toDateString() === new Date(now).toDateString()
  return `as of ${today ? clock(heard) : dayAndTime(heard)}`
}

/** Whether the agent still takes turns in a window. */
export function limitStateText(state: DeepReadonly<LimitState>): string {
  switch (state.type) {
    case 'Fine':
      return 'Allowed'
    case 'Warning':
      return 'Allowed, but near the limit'
    case 'Refused':
      return 'Refused until it resets'
  }
}
