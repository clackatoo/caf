<script lang="ts">
  // The top of the usage page: the period's total, each agent's or app's
  // share of it, and a chart of it by day (or hour).
  import type { Summary } from '@clackatoo/caf'
  import { cost, group, split, total, totals } from '@clackatoo/caf'
  import { count, percent, tokens, usd } from '../figures.ts'
  import StackChart from './StackChart.svelte'
  import Switch from './Switch.svelte'
  import { measure, type Colours, type Metric } from './page.ts'

  let {
    summary,
    metric,
    by = $bindable(),
    colours,
  }: {
    summary: Summary
    metric: Metric
    /** What the shares and the chart are split by. */
    by: 'agent' | 'app'
    colours: Colours
  } = $props()

  const all = $derived(totals(summary))
  const whole = $derived(measure(all, metric))
  const groups = $derived(group(summary, by, metric))
  const chats = $derived(new Map(by === 'app' ? summary.chats.by_app : summary.chats.by_agent))
  const priced = $derived(total(all.tokens) === 0 ? 1 : 1 - all.unpriced / total(all.tokens))
</script>

<section class="overview">
  <div>
    <div class="big" title={metric === 'cost' ? undefined : `${count(total(all.tokens))} tokens`}>
      {metric === 'cost' ? usd(cost(all)) : tokens(total(all.tokens))}
    </div>
    <div class="sub">
      {summary.chats.all}
      {summary.chats.all === 1 ? 'chat' : 'chats'}
      {#if metric === 'cost'}
        · <span
          class="estimate"
          title="What agents reported, and an estimate at API prices for turns they didn't.{all.unpriced > 0
            ? ` ${percent(1 - priced)} of tokens are on models with no known price, and aren't in it.`
            : ''}">API estimate</span
        >
      {/if}
    </div>
    <div class="split"><Switch options={[{ value: 'agent', label: 'By agent' }, { value: 'app', label: 'By app' }]} bind:value={by} label="Split by" small /></div>
    <ul class="shares" aria-label={by === 'app' ? 'By app' : 'By agent'}>
      {#each groups as each (each.key)}
        {@const value = measure(each.totals, metric)}
        <li>
          <div class="line">
            <span class="name"><i style:background={colours[by].get(each.key)}></i>{each.key} <small>{chats.get(each.key) ?? 0} {chats.get(each.key) === 1 ? 'chat' : 'chats'}</small></span>
            <b>{metric === 'cost' ? usd(value) : tokens(value)}</b>
          </div>
          <p>
            {percent(whole === 0 ? 0 : value / whole)} of {metric === 'cost' ? 'cost' : 'tokens'} ·
            {metric === 'cost' ? `${tokens(total(each.totals.tokens))} tokens` : usd(cost(each.totals))}
          </p>
        </li>
      {/each}
    </ul>
  </div>
  <div class="chart">
    <h3>{summary.query.by.type === 'Hour' ? 'Hourly' : 'Daily'} {metric === 'cost' ? 'cost' : 'processed tokens'}, by {by}</h3>
    <StackChart
      periods={split(summary, by)}
      keys={groups.map((each) => each.key)}
      colours={colours[by]}
      {metric}
      by={summary.query.by}
      zone={summary.query.zone}
    />
  </div>
</section>

<style>
  .overview {
    display: grid;
    grid-template-columns: minmax(0, 17rem) minmax(0, 1fr);
    gap: 1.8em;
    margin-bottom: 1.8em;
  }
  .big {
    font-size: 2.6em;
    font-weight: 600;
    line-height: 1.1;
    font-variant-numeric: tabular-nums;
  }
  .sub {
    font-size: 0.85em;
    color: var(--caf-muted);
    margin: 0.3em 0 1.1em;
  }
  .estimate {
    text-decoration: underline dotted;
    cursor: help;
  }
  .split {
    margin-bottom: 0.9em;
  }
  .shares {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    margin-bottom: 0.8em;
  }
  .line {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 0.6em;
  }
  .name {
    display: flex;
    align-items: center;
    gap: 0.5em;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .name small {
    color: var(--caf-muted);
    font-size: 0.82em;
    white-space: nowrap;
  }
  i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex: none;
  }
  b {
    font-variant-numeric: tabular-nums;
  }
  p {
    margin: 0.15em 0 0;
    color: var(--caf-muted);
    font-size: 0.85em;
  }
  h3 {
    font-size: 0.9em;
    font-weight: 600;
    margin: 0 0 0.6em;
  }
</style>
