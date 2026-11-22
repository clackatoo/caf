// An agent's account limits in the Agents panel: its lowest window at a
// glance, and each window's bar, pace, reset and when CAF heard of it,
// counting down as time passes.

import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AccountLimits, AgentStatus, LimitWindow } from '@clackatoo/caf'
import AgentsPanel from '../src/AgentsPanel.svelte'
import Limits from '../src/Limits.svelte'
import { asOfText, limitStateText, resetsText, until } from '../src/limits.ts'
import { clock, dayAndTime } from '../src/chat/time.ts'
import { status, statuses } from './chat/build.ts'

afterEach(() => {
  cleanup()
  vi.useRealTimers()
})

const MINUTE = 60_000
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR
/** When the sample's limits were heard. */
const HEARD = 1_795_000_000_000

const text = (element: Element) => (element.textContent ?? '').replace(/\s+/g, ' ').trim()

/** The sample's agent with limits: every kind of window, Opus's refused. */
function sampled(): AccountLimits {
  const agent = statuses().find((each) => each.limits.windows.length > 0)
  if (agent === undefined) throw new Error('the sample has no agent with limits')
  return agent.limits
}

function window(fields: Partial<LimitWindow> = {}): LimitWindow {
  return { kind: { type: 'FiveHours' }, used: 0, resets: HEARD + HOUR, state: { type: 'Fine' }, heard: HEARD, ...fields }
}

function draw(limits: AccountLimits, now = HEARD) {
  return render(Limits, { name: 'claude', limits, now })
}

/** The limits' one line, which opens them. */
function glance(): HTMLElement {
  return document.querySelector('summary') as HTMLElement
}

function shown(name: string): HTMLElement {
  return document.querySelector(`[data-window="${name}"]`) as HTMLElement
}

describe('the words for limits', () => {
  it('says roughly how long until a reset', () => {
    expect(until(20_000)).toBe('under a minute')
    expect(until(42 * MINUTE + 59_000)).toBe('42 min')
    expect(until(2 * HOUR + 5 * MINUTE)).toBe('2 h 5 min')
    expect(until(4 * DAY + 3 * HOUR + 59 * MINUTE)).toBe('4 d 3 h')
    // No part that's nothing.
    expect(until(2 * HOUR + 59_000)).toBe('2 h')
    expect(until(4 * DAY + 59 * MINUTE)).toBe('4 d')
    expect(resetsText(HEARD + 2 * HOUR, HEARD)).toBe('resets in 2 h')
    // Due, and not yet forgotten by the core.
    expect(resetsText(HEARD, HEARD)).toBe('resetting')
    expect(resetsText(HEARD - 1, HEARD)).toBe('resetting')
  })

  it('says when CAF heard, with the day only if it was another', () => {
    expect(asOfText(HEARD, HEARD + MINUTE)).toBe(`as of ${clock(HEARD)}`)
    expect(asOfText(HEARD, HEARD + 2 * DAY)).toBe(`as of ${dayAndTime(HEARD)}`)
  })

  it('says each state', () => {
    expect(limitStateText({ type: 'Fine' })).toBe('Allowed')
    expect(limitStateText({ type: 'Warning' })).toBe('Allowed, but near the limit')
    expect(limitStateText({ type: 'Refused' })).toBe('Refused until it resets')
  })
})

describe('an agent’s limits', () => {
  it('shows the lowest window at a glance, and every window opened', async () => {
    draw(sampled())
    const summary = glance()
    // Opus's week is refused: nothing left, so out, said in words too, as
    // colour alone isn't enough. Whose they are is said to a screen
    // reader, and the line is named by what it says.
    expect(text(summary)).toBe("claude's limits: Weekly · Opus: 0% left, out")
    expect(summary.hasAttribute('aria-label')).toBe(false)
    expect(summary.querySelector('.hidden')!.textContent).toBe("claude's limits: ")
    expect(summary.closest('details')!.dataset.lowness).toBe('out')
    await fireEvent.click(summary)
    const names = [...document.querySelectorAll<HTMLElement>('[data-window]')].map((each) => each.dataset.window)
    expect(names).toEqual(['5-hour', 'Weekly', 'Weekly · Opus', 'Weekly · extra included', 'thirty_day'])
    expect(screen.getAllByRole('meter').map((bar) => bar.getAttribute('aria-valuenow'))).toEqual(['58', '19', '0', '88', '97'])
  })

  it("says each window's pace to a screen reader, as well as its tick", () => {
    draw(sampled())
    const said = (name: string) => within(shown(name)).getByRole('meter').getAttribute('aria-valuetext')
    expect(said('5-hour')).toBe('58% left; at an even pace, 56% would be left')
    // No pace for a window CAF doesn't know.
    expect(said('thirty_day')).toBe('97% left')
  })

  it('says how low the lowest window is in words, and nothing when it is fine', () => {
    for (const [used, said] of [[60, '5-hour: 40% left'], [85, '5-hour: 15% left, low'], [95, '5-hour: 5% left, out']] as const) {
      draw({ windows: [window({ used })], extra_usage: null, cleared: null })
      expect(text(glance())).toBe(`claude's limits: ${said}`)
      cleanup()
    }
  })

  it('shows what is left of each window, how low, its state, its reset and when it was heard', () => {
    draw(sampled())
    const fiveHours = shown('5-hour')
    expect(text(fiveHours)).toBe(`5-hour 58% left Allowed, resets in 2 h 46 min, as of ${clock(HEARD)}`)
    expect(fiveHours.dataset.lowness).toBe('fine')
    expect(within(fiveHours).getByRole('meter', { name: '5-hour left' }).querySelector('.fill')!.getAttribute('style')).toContain('width: 58%')
    const week = shown('Weekly')
    expect(text(week)).toContain('19% left Allowed, but near the limit, resets in 2 h 46 min')
    expect(week.dataset.lowness).toBe('low')
    expect(week.dataset.state).toBe('Warning')
    const opus = shown('Weekly · Opus')
    expect(text(opus)).toContain('0% left Refused until it resets')
    expect(opus.dataset.lowness).toBe('out')
    // A window CAF doesn't know, with no reset: neither pace nor reset.
    const other = shown('thirty_day')
    expect(text(other)).toBe(`thirty_day 97% left Allowed, as of ${clock(HEARD)}`)
    expect(other.querySelector('.pace')).toBeNull()
  })

  it('marks where an even pace would have what is left', () => {
    // Two of five hours gone: at an even pace, 60% would be left.
    draw({ windows: [window({ used: 50, resets: HEARD + 3 * HOUR })], extra_usage: null, cleared: null })
    const tick = shown('5-hour').querySelector<HTMLElement>('.pace')!
    expect(tick.dataset.left).toBe('60')
    expect(tick.getAttribute('style')).toContain('left: 60%')
    expect(tick.title).toBe('At an even pace, 60% would be left')
  })

  it('says when extra usage is in use, and only then', () => {
    const limits = (inUse: boolean): AccountLimits => ({ windows: [window({ used: 100, state: { type: 'Refused' } })], extra_usage: { in_use: inUse, heard: HEARD }, cleared: null })
    draw(limits(true))
    // Told apart from the glance to a screen reader, as the eye has the gap.
    expect(text(glance())).toBe("claude's limits: 5-hour: 0% left, out, Extra usage in use")
    expect(glance().querySelector('.glance .hidden')!.textContent).toBe(', ')
    cleanup()
    draw(limits(false))
    expect(text(glance())).toBe("claude's limits: 5-hour: 0% left, out")
  })

  it('shows extra usage in use with no windows', () => {
    draw({ windows: [], extra_usage: { in_use: true, heard: HEARD }, cleared: null })
    const summary = glance()
    expect(text(summary)).toBe("claude's limits: Extra usage in use")
    // No window, so no dot to say how low one is.
    expect(summary.querySelector('.dot')).toBeNull()
  })

  it('shows nothing for an agent that has said nothing of them', () => {
    const { container } = draw({ windows: [], extra_usage: { in_use: false, heard: HEARD }, cleared: HEARD })
    expect(container.querySelector('.limits')).toBeNull()
  })

  it('is low at 20% left, and out at 10%', () => {
    for (const [used, lowness] of [[79, 'fine'], [80, 'low'], [90, 'out']] as const) {
      draw({ windows: [window({ used })], extra_usage: null, cleared: null })
      expect(document.querySelector<HTMLElement>('.limits')!.dataset.lowness).toBe(lowness)
      cleanup()
    }
  })
})

describe('the Agents panel’s limits', () => {
  it('shows each agent’s own, and none for those that have none', () => {
    vi.useFakeTimers({ now: HEARD, toFake: ['Date', 'setInterval', 'clearInterval'] })
    const agents = statuses()
    render(AgentsPanel, { agents, actions: { install() {}, stopInstall() {}, signIn() {}, paste() {}, stopSignIn() {}, signOut() {} } })
    const withLimits = agents.filter((agent) => agent.limits.windows.length > 0).map((agent) => agent.name)
    expect(withLimits.length).toBeGreaterThan(0)
    for (const agent of agents) {
      const row = document.querySelector(`[data-agent="${agent.name}"]`)!
      expect(row.querySelector('.limits') !== null, agent.name).toBe(withLimits.includes(agent.name))
    }
  })

  it('counts down to each reset as time passes', async () => {
    vi.useFakeTimers({ now: HEARD, toFake: ['Date', 'setInterval', 'clearInterval'] })
    const agent: AgentStatus = { ...status({ type: 'Host' }), limits: { windows: [window({ resets: HEARD + HOUR })], extra_usage: null, cleared: null } }
    render(AgentsPanel, { agents: [agent], actions: { install() {}, stopInstall() {}, signIn() {}, paste() {}, stopSignIn() {}, signOut() {} } })
    expect(text(shown('5-hour'))).toContain('resets in 1 h,')
    await vi.advanceTimersByTimeAsync(20 * MINUTE)
    expect(text(shown('5-hour'))).toContain('resets in 40 min')
    await vi.advanceTimersByTimeAsync(40 * MINUTE)
    expect(text(shown('5-hour'))).toContain('resetting')
  })
})
