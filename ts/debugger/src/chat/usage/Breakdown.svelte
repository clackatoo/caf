<script lang="ts">
  // The period broken down in a table: by model, with each model's price
  // a click away; by app; or by day (hour, for the past 24h), split by
  // agent or by app.
  import type { ModelPrice, Summary } from '@clackatoo/caf'
  import { cost, group, price, split, total } from '@clackatoo/caf'
  import { count, percent, tokens, usd } from '../figures.ts'
  import PriceCard from './PriceCard.svelte'
  import Switch from './Switch.svelte'
  import { measure, periodName, type Colours, type Metric } from './page.ts'

  let {
    summary,
    metric,
    colours,
    setPrice,
  }: {
    summary: Summary
    metric: Metric
    colours: Colours
    setPrice?: ((model: string, price: ModelPrice | null) => Promise<unknown>) | undefined
  } = $props()

  let by = $state<'model' | 'app' | 'day'>('model')
  let daySplit = $state<'agent' | 'app'>('agent')
  /** The model whose price is shown, if any. */
  let chosen = $state<string | null>(null)

  const show = (value: number) => (metric === 'cost' ? usd(value) : tokens(value))
  const rows = $derived(by === 'day' ? [] : group(summary, by, metric))
  const whole = $derived(rows.reduce((sum, row) => sum + measure(row.totals, metric), 0))
  const peak = $derived(Math.max(0, ...rows.map((row) => measure(row.totals, metric))))
  const chats = $derived(new Map(summary.chats.by_app))
  const columns = $derived(by === 'day' ? group(summary, daySplit, metric).map((each) => each.key) : [])
  const days = $derived(by === 'day' ? split(summary, daySplit).reverse() : [])
  const hourly = $derived(summary.query.by.type === 'Hour')
</script>

<div class="head">
  <h3>Breakdown</h3>
  <span class="choices">
    {#if by === 'day'}
      <Switch options={[{ value: 'agent', label: 'Split by agent' }, { value: 'app', label: 'Split by app' }]} bind:value={daySplit} label="Split by" small />
    {/if}
    <Switch
      options={[
        { value: 'model', label: 'Model' },
        { value: 'app', label: 'App' },
        { value: 'day', label: hourly ? 'Hour' : 'Day' },
      ]}
      bind:value={by}
      label="Break down by"
    />
  </span>
</div>

{#if by === 'day'}
  <table>
    <thead>
      <tr>
        <th>{hourly ? 'Hour' : 'Day'}</th>
        {#each columns as key (key)}<th><span class="key"><i style:background={colours[daySplit].get(key)}></i>{key}</span></th>{/each}
        <th>Total</th>
        <th>{metric === 'cost' ? 'Tokens' : 'Cost'}</th>
      </tr>
    </thead>
    <tbody>
      {#each days as day (day.start)}
        {@const sum = day.groups.reduce((all, each) => ({ cost: all.cost + cost(each.totals), tokens: all.tokens + total(each.totals.tokens) }), { cost: 0, tokens: 0 })}
        <tr>
          <td>{periodName(day.start, summary.query.by, summary.query.zone, true)}</td>
          {#each columns as key (key)}
            {@const cell = day.groups.find((each) => each.key === key)}
            <td>{cell === undefined ? '–' : show(measure(cell.totals, metric))}</td>
          {/each}
          <td><b>{metric === 'cost' ? usd(sum.cost) : tokens(sum.tokens)}</b></td>
          <td>{metric === 'cost' ? tokens(sum.tokens) : usd(sum.cost)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{:else}
  <table>
    <thead>
      <tr>
        <th>#</th>
        <th class="name">{by === 'model' ? 'Model' : 'App'}</th>
        {#if by === 'app'}<th>Chats</th>{/if}
        <th>Cost</th>
        <th>Share</th>
        <th>Tokens</th>
      </tr>
    </thead>
    <tbody>
      {#each rows as row, n (row.key)}
        {@const value = measure(row.totals, metric)}
        {@const all = total(row.totals.tokens)}
        <tr class:chosen={by === 'model' && chosen === row.key}>
          <td>{n + 1}</td>
          <td class="name">
            {#if by === 'model'}
              <button
                type="button"
                class="model"
                aria-expanded={chosen === row.key}
                onclick={() => (chosen = chosen === row.key ? null : row.key)}>{row.key}</button
              >
            {:else}
              <span class="key"><i style:background={colours.app.get(row.key)}></i>{row.key}</span>
            {/if}
            {#if value > 0}<div class="mbar" style:width="{(value / peak) * 100}%"></div>{/if}
          </td>
          {#if by === 'app'}<td>{chats.get(row.key) ?? 0}</td>{/if}
          <td>
            {#if row.totals.unpriced > 0 && row.totals.unpriced >= all && cost(row.totals) === 0}
              <span class="unpriced">Unpriced</span>
            {:else}
              {usd(cost(row.totals))}
              {#if row.totals.unpriced > 0}
                <span class="unpriced">{tokens(row.totals.unpriced)} unpriced</span>
              {/if}
            {/if}
          </td>
          <td>{value > 0 ? percent(value / whole) : '–'}</td>
          <td title="{count(all)} tokens">{tokens(all)}</td>
        </tr>
        {#if by === 'model' && chosen === row.key}
          <tr class="detail">
            <td></td>
            <td colspan="4">
              <PriceCard model={row.key} priced={price(summary, row.key)} fetched={summary.fetched} {setPrice} />
            </td>
          </tr>
        {/if}
      {/each}
    </tbody>
  </table>
  {#if rows.some((row) => row.totals.unpriced > 0)}
    <p class="footnote">
      Unpriced: tokens on a model CAF knows no price for, of turns whose agent reported no cost. They aren't in the
      cost.
    </p>
  {/if}
  {#if by === 'model'}<p class="footnote">Click a model for its price{setPrice === undefined ? '' : ', or to set your own'}.</p>{/if}
{/if}

<style>
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1em;
    margin-bottom: 0.5em;
  }
  h3 {
    font-size: 0.9em;
    font-weight: 600;
    margin: 0;
  }
  .choices {
    display: flex;
    gap: 0.5em;
    align-items: center;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.95em;
    font-variant-numeric: tabular-nums;
  }
  th {
    font-weight: normal;
    color: var(--caf-muted);
    font-size: 0.9em;
    text-align: right;
    padding: 0.5em 0 0.5em 1.4em;
    border-bottom: 1px solid var(--caf-border);
    white-space: nowrap;
  }
  td {
    text-align: right;
    padding: 0.45em 0 0.45em 1.4em;
    border-bottom: 1px solid var(--caf-inset);
    white-space: nowrap;
  }
  th:first-child,
  td:first-child {
    text-align: left;
    padding-left: 0.5em;
  }
  .name {
    text-align: left;
    width: 100%;
  }
  tbody tr:not(.detail):hover td,
  tr.chosen td {
    background: var(--caf-hover);
  }
  .model {
    font: inherit;
    padding: 0;
    border: 0;
    background: none;
    color: var(--caf-text);
    cursor: pointer;
    text-align: left;
  }
  .model:hover {
    text-decoration: underline;
  }
  .mbar {
    height: 4px;
    border-radius: 2px;
    margin-top: 0.3em;
    background: var(--caf-accent);
    opacity: 0.6;
  }
  .key {
    display: inline-flex;
    align-items: center;
    gap: 0.4em;
  }
  i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    display: inline-block;
    flex: none;
  }
  .unpriced {
    color: var(--caf-muted);
    font-size: 0.85em;
    border: 1px dashed var(--caf-border);
    border-radius: 4px;
    padding: 0 0.35em;
    margin-left: 0.3em;
  }
  .footnote {
    color: var(--caf-muted);
    font-size: 0.85em;
    margin: 0.5em 0 0;
  }
</style>
