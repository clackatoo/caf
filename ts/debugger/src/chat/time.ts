// Times from windows. A window's times are when CAF heard of things; the
// view has no clock of its own, so while something runs it shows when it
// started, and how long it took only once it has ended.

import type { DeepReadonly, Wall, Window } from '@clackatoo/caf'

/** A length of time in milliseconds, short: `850 ms`, `4.2 s`, `3 min 5 s`, `2 h 4 min`. */
export function duration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)} s`
  const s = Math.round(ms / 1000)
  if (s < 60) return `${s} s`
  const min = Math.floor(s / 60)
  if (min < 60) return `${min} min ${s % 60} s`
  return `${Math.floor(min / 60)} h ${min % 60} min`
}

/** How long a window lasted, if it has ended and both times are known. */
export function took(window: DeepReadonly<Window>): string | null {
  if (window.start === null || window.end.type !== 'At') return null
  return duration(window.end.value - window.start)
}

/** A calendar time as the user's clock shows it. */
export function clock(wall: Wall): string {
  return new Date(wall).toLocaleTimeString()
}

/** A day and time, short: when a saved chat last changed. */
export function dayAndTime(wall: Wall): string {
  return new Date(wall).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })
}

/** A calendar time in full, for a tooltip. */
export function fullTime(wall: Wall): string {
  return new Date(wall).toLocaleString()
}
