// The usage page: it asks for the period chosen, in the user's zone, and
// draws the summary's sums, by agent or app, model and day; it shows a
// model's price, and sets the user's own; and it says plainly what went
// wrong, or that the figures are short.

import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CafError, type Bucket, type PricedModel, type Summary, type Tokens, type UsageQuery } from '@clackatoo/caf'
import UsagePage from '../../src/chat/usage/UsagePage.svelte'
import { cacheSavings, colour, costByKind, queryFor } from '../../src/chat/usage/page.ts'
import { percent } from '../../src/chat/figures.ts'

afterEach(cleanup)

// The chart follows its width, as laid out, which jsdom doesn't do: it
// draws at its own width meanwhile.
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const HOUR = 3_600_000
/** 09:30 UTC, Sunday 22 November 2026. */
const NOW = Date.UTC(2026, 10, 22, 9, 30)

function tokens(fields: Partial<Tokens> = {}): Tokens {
  return { input: 0, cache_read: 0, cache_write: 0, output: 0, thinking: null, ...fields }
}

function bucket(fields: Partial<Bucket> = {}): Bucket {
  return {
    start: Date.UTC(2026, 10, 21),
    app: 'clackatoo',
    agent: 'claude-agent-acp',
    model: 'claude-opus-5-5',
    fast: false,
    tokens: tokens({ input: 1_000_000, cache_read: 4_000_000, output: 100_000 }),
    reported: 2,
    estimated: 0,
    unpriced: 0,
    turns: 2,
    ...fields,
  }
}

const OPUS: PricedModel = {
  model: 'anthropic/claude-opus-5-5',
  price: { input: 5, cache_read: 0.5, cache_write: 6.25, output: 25, fast: 2 },
  layer: { type: 'Fetched' },
}

function summary(query: UsageQuery, fields: Partial<Summary> = {}): Summary {
  return {
    query,
    buckets: [
      bucket(),
      bucket({ start: Date.UTC(2026, 10, 22), app: 'eyespy', agent: 'codex', model: 'gpt-6', reported: 0, estimated: 0, unpriced: 500_000, tokens: tokens({ input: 500_000 }), turns: 1 }),
      bucket({ start: Date.UTC(2026, 10, 22), app: 'eyespy', fast: true, reported: 0, estimated: 1, tokens: tokens({ input: 100_000, cache_read: 1_000_000 }), turns: 1 }),
    ],
    chats: {
      all: 3,
      by_app: [
        ['clackatoo', 1],
        ['eyespy', 2],
      ],
      by_agent: [
        ['claude-agent-acp', 2],
        ['codex', 1],
      ],
    },
    prices: [OPUS],
    fetched: Date.UTC(2026, 10, 21, 8),
    unreadable: [],
    ...fields,
  }
}

/** The page, over a `load` that answers each query with `answer(query)`. */
function draw(answer: (query: UsageQuery) => Promise<Summary> = async (query) => summary(query), withPrice = true) {
  const load = vi.fn(answer)
  const setPrice = vi.fn(async () => {})
  const view = render(UsagePage, { load, setPrice: withPrice ? setPrice : undefined, zone: 'UTC', now: () => NOW })
  return { ...view, load, setPrice }
}

const text = (element: Element) => (element.textContent ?? '').replace(/\s+/g, ' ').trim()
/** An element's children's text, a space between each. */
const parts = (element: Element) => [...element.children].map((each) => text(each)).join(' ')
const press = (name: string) => fireEvent.click(screen.getByRole('button', { name }))
const big = (container: HTMLElement) => text(container.querySelector('.big')!)
const shares = () => [...screen.getByRole('list', { name: /^By / }).querySelectorAll('li')].map((li) => text(li))

describe('the query', () => {
  it('asks for the last seven days by day, today included, from midnight in the zone', async () => {
    const { load } = draw()
    await waitFor(() => expect(load).toHaveBeenCalledOnce())
    expect(load.mock.calls[0]![0]).toStrictEqual({
      since: Date.UTC(2026, 10, 16),
      until: NOW,
      zone: 'UTC',
      by: { type: 'Day' },
      apps: [],
      agents: [],
    })
  })

  it('asks again for another period: the past 24 hours by hour, from the start of an hour', async () => {
    const { load } = draw()
    await waitFor(() => expect(load).toHaveBeenCalledOnce())
    await press('Past 24h')
    await waitFor(() => expect(load).toHaveBeenCalledTimes(2))
    expect(load.mock.calls[1]![0]).toMatchObject({ since: Date.UTC(2026, 10, 21, 10), until: NOW, by: { type: 'Hour' } })
    await press('90 days')
    await waitFor(() => expect(load).toHaveBeenCalledTimes(3))
    expect(load.mock.calls[2]![0]).toMatchObject({ since: Date.UTC(2026, 7, 25), by: { type: 'Day' } })
  })

  it("starts days at the zone's midnight, and steps back over a change of clocks", () => {
    // London's clocks went back on 25 October 2026.
    const query = queryFor('7d', Date.UTC(2026, 9, 28, 12), 'Europe/London')
    expect(query.since).toBe(Date.UTC(2026, 9, 21, 23))
    const day = queryFor('24h', Date.UTC(2026, 9, 25, 12, 10), 'Asia/Kolkata')
    // 17:40 there: its hour began at 17:00, 11:30 UTC.
    expect(day.since).toBe(Date.UTC(2026, 9, 25, 11, 30) - 23 * HOUR)
  })

  it('refreshes on asking, keeping the figures shown meanwhile', async () => {
    let answer: (summary: Summary) => void = () => {}
    let asked = 0
    const { container, load } = draw(async (query) => (++asked === 1 ? summary(query) : new Promise((done) => (answer = done))))
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('Refresh')
    expect(load).toHaveBeenCalledTimes(2)
    expect(screen.getByRole('status').textContent).toContain('Reading again')
    expect(big(container)).toBe('$3.00')
    answer(summary(load.mock.calls[1]![0], { buckets: [bucket({ reported: 7 })] }))
    await waitFor(() => expect(big(container)).toBe('$7.00'))
    expect(screen.queryByRole('status')).toBeNull()
  })
})

describe('the overview', () => {
  it('shows the cost, the chats, and that it is an estimate', async () => {
    const { container } = draw()
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    expect(text(container.querySelector('.sub')!)).toBe("3 chats · API estimate ⓘ · 7.5% of tokens had no price, and aren't in it")
    // What the estimate is, a click away, not only on hover.
    expect(screen.queryByRole('note')).toBeNull()
    await press('About the estimate')
    expect(screen.getByRole('note').textContent).toContain("an estimate at the models' API prices")
    expect(shares()).toStrictEqual(['claude-agent-acp 2 chats $3.00 100% of cost · 6.2M tokens', 'codex 1 chat $0.00 0% of cost · 500K tokens'])
  })

  it("says it's as agents reported it when nothing was estimated", async () => {
    const { container } = draw(async (query) =>
      summary(query, { buckets: [bucket()], chats: { all: 1, by_app: [['clackatoo', 1]], by_agent: [['claude-agent-acp', 1]] } }),
    )
    await waitFor(() => expect(big(container)).toBe('$2.00'))
    expect(text(container.querySelector('.sub')!)).toBe('1 chat · as agents reported it')
    expect(screen.queryByRole('button', { name: 'About the estimate' })).toBeNull()
  })

  it('splits by app instead', async () => {
    draw()
    await waitFor(() => expect(shares()).toHaveLength(2))
    await press('By app')
    expect(shares()).toStrictEqual(['clackatoo 1 chat $2.00 67% of cost · 5.1M tokens', 'eyespy 2 chats $1.00 33% of cost · 1.6M tokens'])
    expect(screen.getByRole('img', { name: 'Cost by day' })).toBeTruthy()
  })

  it('measures in tokens instead', async () => {
    const { container } = draw()
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('Tokens')
    expect(big(container)).toBe('6.7M')
    expect(shares()[0]).toBe('claude-agent-acp 2 chats 6.2M 93% of tokens · $3.00')
  })

  it('charts every day of the period, stacked by agent', async () => {
    const { container } = draw()
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    const chart = screen.getByRole('img', { name: 'Cost by day' })
    // A label under each day's bar.
    expect(chart.querySelectorAll('text[text-anchor="middle"]')).toHaveLength(7)
    expect([...chart.querySelectorAll('rect title')].map((each) => each.textContent)).toStrictEqual([
      `${titleDay(Date.UTC(2026, 10, 21))} · claude-agent-acp: $2.00`,
      `${titleDay(Date.UTC(2026, 10, 22))} · claude-agent-acp: $1.00`,
    ])
  })

  it('totals tokens by kind, and what the cache saved at the prices used', async () => {
    const { container } = draw()
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    const totals = [...container.querySelectorAll('.metrics > div')].map((each) => parts(each))
    expect(totals).toStrictEqual([
      'Processed tokens 6.7M',
      'Cached input 5M',
      'Uncached input 1.6M',
      'Cache writes 0',
      'Output 100K',
      // 4M cached at $4.50 a million less, and 1M at twice that in fast mode.
      'Cache savings ⓘ $27.00',
    ])
    await press('About cache savings')
    expect(screen.getByRole('note').textContent).toContain('against reading the same input fresh')
    expect(text(container)).toContain('At the prices the models have: an agent’s reported cost isn’t split by type.')
    // Kinds of token are tones of one colour, never an agent's.
    const swatches = [...container.querySelectorAll('.share .keys i')].map((each) => (each as HTMLElement).style.background)
    expect(swatches.every((each) => each.startsWith('var(--caf-tone-'))).toBe(true)
    const labels = [...container.querySelectorAll('.share .label')].map((each) => text(each))
    expect(labels).toStrictEqual(['Tokens by type', 'Cost by type', 'Cost by speed'])
    expect(parts(container.querySelectorAll('.share .keys')[2]!)).toBe('Standard 67% Fast mode 33%')
  })
})

/** A day as the chart's tooltips name it. */
function titleDay(start: number): string {
  return new Intl.DateTimeFormat(undefined, { weekday: 'short', day: 'numeric', month: 'short', timeZone: 'UTC' }).format(start)
}

describe('the breakdown', () => {
  const rows = (container: HTMLElement) => [...container.querySelectorAll('table tbody tr')].map((row) => [...row.querySelectorAll('td')].map((td) => text(td)))

  it('is by model, biggest cost first, a model with no price marked Unpriced', async () => {
    const { container } = draw()
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    expect(rows(container)).toStrictEqual([
      ['1', 'claude-opus-5-5', '$3.00', '100%', '6.2M'],
      ['2', 'gpt-6', 'Unpriced', '–', '500K'],
    ])
  })

  it("shows a model's price and where it came from, and sets the user's own", async () => {
    const { container, load, setPrice } = draw()
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('claude-opus-5-5')
    const card = within(container.querySelector('.price') as HTMLElement)
    expect(text(container.querySelector('.price p')!)).toContain("From LiteLLM's table, fetched")
    expect(text(container.querySelector('.price p')!)).toContain('as anthropic/claude-opus-5-5')
    expect([...container.querySelectorAll('.price dl > div')].map((each) => parts(each))).toStrictEqual([
      'Input $5',
      'Cache read $0.50',
      'Cache write $6.25',
      'Output $25',
      'Fast mode ×2',
    ])
    await fireEvent.click(card.getByRole('button', { name: 'Set your own price' }))
    const input = card.getByRole('spinbutton', { name: 'Input' })
    expect((input as HTMLInputElement).value).toBe('5')
    await fireEvent.input(input, { target: { value: '4' } })
    await fireEvent.click(card.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(setPrice).toHaveBeenCalledOnce())
    expect(setPrice).toHaveBeenCalledWith('claude-opus-5-5', { input: 4, cache_read: 0.5, cache_write: 6.25, output: 25, fast: 2 })
    // Asked again, as costs are estimated with it now.
    await waitFor(() => expect(load).toHaveBeenCalledTimes(2))
  })

  it('refuses a price with a rate missing, and says why', async () => {
    const { container, setPrice } = draw()
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('gpt-6')
    expect(text(container.querySelector('.price p')!)).toContain('CAF knows no price for gpt-6')
    const card = within(container.querySelector('.price') as HTMLElement)
    await fireEvent.click(card.getByRole('button', { name: 'Set your own price' }))
    await fireEvent.input(card.getByRole('spinbutton', { name: 'Input' }), { target: { value: '1' } })
    await fireEvent.click(card.getByRole('button', { name: 'Save' }))
    expect(card.getByRole('alert').textContent).toContain('Give every rate')
    expect(setPrice).not.toHaveBeenCalled()
  })

  it("clears and changes the user's own price by the entry that priced it", async () => {
    const own = { ...OPUS, layer: { type: 'Own' as const } }
    const { container, setPrice } = draw(async (query) => summary(query, { prices: [own] }))
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('claude-opus-5-5')
    expect(text(container.querySelector('.price p')!)).toContain('Your own price')
    await press("Use the table's price")
    expect(setPrice).toHaveBeenCalledWith('anthropic/claude-opus-5-5', null)
    await waitFor(() => expect(screen.getByRole('button', { name: 'Change your price' })).toBeTruthy())
    await press('Change your price')
    await press('Save')
    expect(setPrice).toHaveBeenLastCalledWith('anthropic/claude-opus-5-5', OPUS.price)
  })

  it("says why a price couldn't be set", async () => {
    const { container, setPrice } = draw()
    setPrice.mockRejectedValueOnce(new CafError({ type: 'PriceNotSet', value: 'Permission denied' }))
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('claude-opus-5-5')
    await press('Set your own price')
    await press('Save')
    await waitFor(() =>
      expect(screen.getByRole('group', { name: 'The price of claude-opus-5-5' }).querySelector('[role=alert]')?.textContent).toBe(
        "The price couldn't be set, and prices are as they were. The system said: Permission denied",
      ),
    )
  })

  it('offers no price of your own without leave to set one', async () => {
    const { container } = draw(undefined, false)
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('claude-opus-5-5')
    expect(screen.queryByRole('button', { name: 'Set your own price' })).toBeNull()
    expect(text([...container.querySelectorAll('.footnote')].at(-1)!)).toBe('Click a model for its price.')
    expect(text(container.querySelector('.footnote')!)).toContain("Unpriced: tokens on a model CAF knows no price for")
  })

  it('is by app, with its chats', async () => {
    const { container } = draw()
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('App')
    expect(rows(container)).toStrictEqual([
      ['1', 'clackatoo', '1', '$2.00', '67%', '5.1M'],
      ['2', 'eyespy', '2', '$1.00 500K unpriced', '33%', '1.6M'],
    ])
  })

  it('is by day, newest first, every day present, split by agent or by app', async () => {
    const { container } = draw()
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('Day')
    const head = () => [...container.querySelectorAll('table thead th')].map((th) => text(th))
    expect(head()).toStrictEqual(['Day', 'claude-agent-acp', 'codex', 'Total', 'Tokens'])
    const days = rows(container)
    expect(days).toHaveLength(7)
    expect(days[0]!.slice(1)).toStrictEqual(['$1.00', '$0.00', '$1.00', '1.6M'])
    expect(days[1]!.slice(1)).toStrictEqual(['$2.00', '–', '$2.00', '5.1M'])
    expect(days[2]!.slice(1)).toStrictEqual(['–', '–', '$0.00', '0'])
    await press('Split by app')
    expect(head()).toStrictEqual(['Day', 'clackatoo', 'eyespy', 'Total', 'Tokens'])
  })
})

describe('what goes wrong', () => {
  it.each([
    [new CafError({ type: 'NotAllowed' }), "The host doesn't let this viewer do that."],
    [new CafError({ type: 'NoLedger' }), 'The app keeps no record of usage'],
    [new CafError({ type: 'BadQuery', value: { type: 'Zone', value: 'Mars/Olympus' } }), "CAF doesn't know the time zone Mars/Olympus"],
    [new CafError({ type: 'Disconnected' }), 'The link to CAF dropped'],
  ])('says so plainly: %s', async (error, says) => {
    draw(async () => {
      throw error
    })
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain(says))
    expect(screen.getByRole('alert').textContent).toContain("Usage couldn't be read")
  })

  it('drops what it showed when asking again fails', async () => {
    let asked = 0
    const { container } = draw(async (query) => {
      if (++asked === 1) return summary(query)
      throw new CafError({ type: 'NoLedger' })
    })
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    await press('30 days')
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('The app keeps no record of usage'))
    expect(container.querySelector('.big')).toBeNull()
    expect(container.querySelector('table')).toBeNull()
  })

  it('says the figures are short when some of the record could not be read', async () => {
    const { container } = draw(async (query) => summary(query, { unreadable: ['eyespy/2026-10.jsonl.zst', 'clackatoo/2026-11.jsonl'] }))
    await waitFor(() => expect(big(container)).toBe('$3.00'))
    expect(text(screen.getByRole('alert'))).toBe(
      "Some of the usage record couldn't be read, so these figures are short: eyespy/2026-10.jsonl.zst, clackatoo/2026-11.jsonl.",
    )
  })

  it('says when there was nothing in the period', async () => {
    const { container } = draw(async (query) => summary(query, { buckets: [], chats: { all: 0, by_app: [], by_agent: [] } }))
    await waitFor(() => expect(big(container)).toBe('$0.00'))
    expect(text(container)).toContain('No chats in the last 7 days.')
    expect(container.querySelector('table')).toBeNull()
  })
})

describe('prices of the tokens', () => {
  const query = queryFor('7d', NOW, 'UTC')

  it('divide the cost by kind, at each model’s rates, fast mode’s multiplier included', () => {
    const byKind = costByKind(summary(query))!
    // 1M input at $5 and 4M cached at $0.50, then 0.1M input and 1M cached at twice that.
    expect(byKind.input).toBeCloseTo(5 + 1)
    expect(byKind.cache_read).toBeCloseTo(2 + 1)
    expect(byKind.output).toBeCloseTo(2.5)
    expect(byKind.cache_write).toBe(0)
  })

  it('are none with no price', () => {
    expect(costByKind(summary(query, { prices: [] }))).toBeNull()
    expect(cacheSavings(summary(query, { prices: [] }))).toBeNull()
  })
})

describe('colours and shares', () => {
  it('gives the first 24 groups a colour each', () => {
    const colours = Array.from({ length: 24 }, (_, n) => colour(n))
    expect(new Set(colours).size).toBe(24)
  })

  it('rounds a share the same however close it comes to a tenth', () => {
    expect(percent(0.0999)).toBe('10%')
    expect(percent(0.1)).toBe('10%')
    expect(percent(0.0994)).toBe('9.9%')
    expect(percent(0.0004)).toBe('<0.1%')
    expect(percent(0)).toBe('0%')
    expect(percent(1)).toBe('100%')
  })
})
