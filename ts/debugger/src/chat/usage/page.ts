// What the usage page asks for and works out from a summary, beyond the
// SDK's sums: the query for each period it offers, how periods are named,
// and what prices say about the tokens (cost by kind, cache savings).

import type { DeepReadonly, Resolution, Summary, Totals, UsageQuery, Wall } from '@clackatoo/caf'
import { cost, group, price, total } from '@clackatoo/caf'

/** The periods the page offers, each ending now. */
export type Period = '24h' | '7d' | '30d' | '90d'

export const PERIODS: readonly { value: Period; label: string }[] = [
  { value: '24h', label: 'Past 24h' },
  { value: '7d', label: '7 days' },
  { value: '30d', label: '30 days' },
  { value: '90d', label: '90 days' },
]

/** What the page measures: money, or tokens. */
export type Metric = 'cost' | 'tokens'

/** The user's own time zone, which days begin in. */
export function ownZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone
}

/**
 * The query for a period ending `now`: the past 24 hours by hour, or the
 * last so many days by day, today included. It starts where a period
 * starts, so the first bar is a whole one.
 */
export function queryFor(period: Period, now: Wall, zone: string): UsageQuery {
  const by: Resolution = period === '24h' ? { type: 'Hour' } : { type: 'Day' }
  const back = period === '24h' ? 23 : period === '7d' ? 6 : period === '30d' ? 29 : 89
  let since = startOf(now, zone, by)
  // Each step back is the start of the period before, however long it was.
  for (let n = 0; n < back; n++) since = startOf(since - 1, zone, by)
  return { since, until: now, zone, by, apps: [], agents: [] }
}

/**
 * The start of the hour or day `t` is in, in `zone`: as the SDK finds a
 * summary's periods, so the page and its bars agree.
 */
function startOf(t: Wall, zone: string, by: Resolution): Wall {
  const query: UsageQuery = { since: t, until: t + 1, zone, by, apps: [], agents: [] }
  const one: Summary = { query, buckets: [], chats: { all: 0, by_app: [], by_agent: [] }, prices: [], matched: [], fetched: null, unreadable: [] }
  return Number(group(one, 'period')[0]!.key)
}

/** A period's name, short for a chart's axis (`Sat 21`, `14:00`), or long for a table (`Sat, 21 Nov`). */
export function periodName(start: Wall, by: DeepReadonly<Resolution>, zone: string, long = false): string {
  const options: Intl.DateTimeFormatOptions =
    by.type === 'Hour'
      ? { hour: '2-digit', minute: '2-digit' }
      : long
        ? { weekday: 'short', day: 'numeric', month: 'short' }
        : { weekday: 'short', day: 'numeric' }
  return new Intl.DateTimeFormat(undefined, { ...options, timeZone: zone }).format(start)
}

/** A group's measure: its cost, or its tokens. */
export function measure(totals: Totals, metric: Metric): number {
  return metric === 'cost' ? cost(totals) : total(totals.tokens)
}

/** The kinds of token, as the page names them. */
export const KINDS = [
  { key: 'input', label: 'Input' },
  { key: 'cache_read', label: 'Cache read' },
  { key: 'cache_write', label: 'Cache write' },
  { key: 'output', label: 'Output' },
] as const

export type Kind = (typeof KINDS)[number]['key']

/**
 * What each kind of token cost at the prices the summary used, in US
 * dollars, fast mode's multiplier included: how the cost divides, as an
 * estimate. A cost the agent reported isn't split by kind, so its tokens
 * are priced here too. Models with no price are left out. Null if no
 * model had one.
 */
export function costByKind(summary: Summary): Record<Kind, number> | null {
  const sums: Record<Kind, number> = { input: 0, cache_read: 0, cache_write: 0, output: 0 }
  let priced = false
  for (const bucket of summary.buckets) {
    const found = price(summary, bucket.model)
    if (found === null) continue
    priced = true
    const times = bucket.fast ? (found.price.fast ?? 1) : 1
    for (const { key } of KINDS) sums[key] += (bucket.tokens[key] * found.price[key] * times) / 1e6
  }
  return priced ? sums : null
}

/**
 * What reading input from the cache saved, at the prices the summary
 * used: each cached token at the model's input rate, less what it cost
 * read from the cache. Null if no model read from the cache at a price.
 */
export function cacheSavings(summary: Summary): number | null {
  let saved: number | null = null
  for (const bucket of summary.buckets) {
    const found = price(summary, bucket.model)
    if (found === null || bucket.tokens.cache_read === 0) continue
    const times = bucket.fast ? (found.price.fast ?? 1) : 1
    saved = (saved ?? 0) + (bucket.tokens.cache_read * (found.price.input - found.price.cache_read) * times) / 1e6
  }
  return saved
}

/** How many colours a chart's groups take in turn. */
export const COLOURS = 8

/** Each agent's colour, and each app's, as CSS colours. */
export type Colours = Record<'agent' | 'app', Map<string, string>>

/**
 * The colour of the `n`th group, as a CSS colour. Past the eighth, each
 * colour comes again paler, then paler still, so the first 24 groups are
 * each a colour of their own.
 */
export function colour(n: number): string {
  const base = `var(--caf-series-${(n % COLOURS) + 1})`
  const round = Math.floor(n / COLOURS)
  if (round === 0) return base
  return `color-mix(in srgb, ${base} ${round === 1 ? 60 : 30}%, var(--caf-bg))`
}

/** The colour of the `n`th part of one whole, such as a kind of token: tones of one colour, never a group's. */
export function tone(n: number): string {
  return `var(--caf-tone-${(n % 4) + 1})`
}

/** What a summary covers, after "in": `the past 24 hours`, `the last 7 days`. */
export function covering(summary: Summary): string {
  const periods = group(summary, 'period').length
  if (summary.query.by.type === 'Hour') return periods === 1 ? 'the past hour' : `the past ${periods} hours`
  return periods === 1 ? 'one day' : `the last ${periods} days`
}
