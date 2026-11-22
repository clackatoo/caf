<script lang="ts">
  // Each period's cost or tokens as a bar, stacked by agent or app, every
  // period present so the chart has no gaps.
  import type { Group, Resolution } from '@clackatoo/caf'
  import { tokens, usd } from '../figures.ts'
  import { measure, periodName, type Metric } from './page.ts'

  let {
    periods,
    keys,
    colours,
    metric,
    by,
    zone,
  }: {
    /** Each period's groups, from `split`. */
    periods: { start: number; groups: Group[] }[]
    /** The groups in stacking order, bottom first. */
    keys: string[]
    colours: Map<string, string>
    metric: Metric
    by: Resolution
    zone: string
  } = $props()

  const W = 640
  const H = 220
  const LEFT = 48
  const BOTTOM = 22
  const TOP = 8

  const show = (value: number) => (metric === 'cost' ? usd(value) : tokens(value))
  const heights = $derived(periods.map(({ groups }) => groups.reduce((all, each) => all + measure(each.totals, metric), 0)))
  /** The top of the scale: a little over the tallest bar, or 1 for an empty chart. */
  const top = $derived(Math.max(...heights, 0) * 1.12 || 1)
  const width = $derived((W - LEFT) / Math.max(1, periods.length))
  /** Every how many bars a label goes, so they don't crowd. */
  const every = $derived(Math.max(1, Math.ceil(periods.length / 10)))
  const ticks = [0, 1, 2, 3, 4]

  /** A period's bar, as rectangles stacked from the bottom in `keys`' order. */
  function stack(groups: Group[], at: number) {
    let y = H - BOTTOM
    const rects = []
    for (const key of keys) {
      const group = groups.find((each) => each.key === key)
      if (group === undefined) continue
      const value = measure(group.totals, metric)
      const h = ((H - BOTTOM - TOP) * value) / top
      if (h <= 0) continue
      y -= h
      rects.push({ key, value, x: LEFT + at * width + width * 0.18, y, w: width * 0.64, h: Math.max(h - 1, 0.5) })
    }
    return rects
  }
</script>

<svg class="chart" viewBox="0 0 {W} {H}" role="img" aria-label="{metric === 'cost' ? 'Cost' : 'Tokens'} by {by.type === 'Hour' ? 'hour' : 'day'}">
  {#each ticks as n (n)}
    {@const y = H - BOTTOM - ((H - BOTTOM - TOP) * n) / 4}
    <line class="grid" x1={LEFT} x2={W} y1={y} y2={y} />
    <text x={LEFT - 6} y={y + 4} text-anchor="end">{show((top * n) / 4)}</text>
  {/each}
  {#each periods as period, at (period.start)}
    {#each stack(period.groups, at) as rect (rect.key)}
      <rect x={rect.x} y={rect.y} width={rect.w} height={rect.h} rx="2" style:fill={colours.get(rect.key)}>
        <title>{periodName(period.start, by, zone, true)} · {rect.key}: {show(rect.value)}</title>
      </rect>
    {/each}
    {#if at % every === 0}
      <text x={LEFT + at * width + width / 2} y={H - 5} text-anchor="middle">{periodName(period.start, by, zone)}</text>
    {/if}
  {/each}
</svg>

<style>
  .chart {
    width: 100%;
    height: auto;
    display: block;
  }
  text {
    fill: var(--caf-muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .grid {
    stroke: var(--caf-border);
  }
</style>
