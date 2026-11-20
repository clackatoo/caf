// When a step happened, for the Steps table: the time since the run's
// first step, measured on the steady clock (`Now.mono`, in nanoseconds),
// which doesn't jump as a wall clock can.
//
// The steady clock comes through `JSON.parse`, so past 2^53 ns (about 104
// days after the host's clock began) a reading is already rounded, by up
// to a few hundred nanoseconds. Shown in milliseconds, that's invisible.

/** `+2.500 s`: from the first step's steady reading to this one's. */
export function sinceFirst(first: number, mono: number): string {
  return `+${((mono - first) / 1e9).toFixed(3)} s`
}

/** A calendar time to the millisecond, on a 24-hour clock. */
export function wallTime(wall: number): string {
  const date = new Date(wall)
  return `${date.toLocaleTimeString(undefined, { hour12: false })}.${String(date.getMilliseconds()).padStart(3, '0')}`
}
