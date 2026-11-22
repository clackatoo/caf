<script lang="ts">
  // The period's tokens by kind, what the cache saved, and how the cost
  // divides: by kind of token, and by speed (fast mode or not).
  import type { Summary } from '@clackatoo/caf'
  import { cost, group, total, totals } from '@clackatoo/caf'
  import { count, percent, tokens, usd } from '../figures.ts'
  import { cacheSavings, colour, costByKind, KINDS } from './page.ts'

  let { summary }: { summary: Summary } = $props()

  const all = $derived(totals(summary))
  const processed = $derived(total(all.tokens))
  const saved = $derived(cacheSavings(summary))
  const byKind = $derived(costByKind(summary))
  const speeds = $derived(group(summary, 'fast'))

  const metrics = $derived([
    { label: 'Processed tokens', value: processed },
    { label: 'Cached input', value: all.tokens.cache_read },
    { label: 'Uncached input', value: all.tokens.input },
    { label: 'Cache writes', value: all.tokens.cache_write },
    { label: 'Output', value: all.tokens.output },
  ])

  /** Shares of a whole, as a bar's parts: each a label, a share and a colour. */
  type Part = { label: string; share: number; colour: string }
  function parts(values: { label: string; value: number }[]): Part[] {
    const whole = values.reduce((sum, each) => sum + each.value, 0)
    return values.map((each, n) => ({ label: each.label, share: whole === 0 ? 0 : each.value / whole, colour: colour(n) }))
  }

  const shares = $derived(
    [
      { label: 'Tokens by type', parts: parts(KINDS.map(({ key, label }) => ({ label, value: all.tokens[key] }))) },
      byKind === null
        ? null
        : {
            label: 'Cost by type',
            note: 'At the prices the models have: an agent’s reported cost isn’t split by type.',
            parts: parts(KINDS.map(({ key, label }) => ({ label, value: byKind[key] }))),
          },
      {
        label: 'Cost by speed',
        parts: parts(
          ['standard', 'fast'].map((key) => {
            const speed = speeds.find((each) => each.key === key)
            return { label: key === 'fast' ? 'Fast mode' : 'Standard', value: speed === undefined ? 0 : cost(speed.totals) }
          }),
        ),
      },
    ].filter((each) => each !== null),
  )
</script>

<h3>Totals</h3>
<dl class="metrics">
  {#each metrics as metric (metric.label)}
    <div>
      <dt>{metric.label}</dt>
      <dd title="{count(metric.value)} tokens">{tokens(metric.value)}</dd>
    </div>
  {/each}
  {#if saved !== null}
    <div>
      <dt>Cache savings</dt>
      <dd title="What reading from the cache saved, against reading the same input fresh, at the models’ prices">{usd(saved)}</dd>
    </div>
  {/if}
</dl>

<section class="shares">
  {#each shares as share (share.label)}
    <div class="share">
      <div class="label" title={'note' in share ? share.note : undefined}>{share.label}</div>
      <div class="bar">
        {#each share.parts as part (part.label)}
          {#if part.share > 0}<i style:width="{part.share * 100}%" style:background={part.colour}></i>{/if}
        {/each}
      </div>
      <div class="keys">
        {#each share.parts as part (part.label)}
          <span><i style:background={part.colour}></i>{part.label} <b>{percent(part.share)}</b></span>
        {/each}
      </div>
    </div>
  {/each}
</section>

<style>
  h3 {
    font-size: 0.9em;
    font-weight: 600;
    margin: 0 0 0.6em;
  }
  .metrics {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(9em, 1fr));
    gap: 1em 1.5em;
    margin: 0 0 1.8em;
  }
  dt {
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  dd {
    margin: 0.15em 0 0;
    font-size: 1.3em;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .shares {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(16em, 1fr));
    gap: 1.3em 1.8em;
    margin-bottom: 1.8em;
  }
  .label {
    font-size: 0.85em;
    color: var(--caf-muted);
    margin-bottom: 0.4em;
  }
  .bar {
    display: flex;
    gap: 2px;
    height: 8px;
    border-radius: 4px;
    overflow: hidden;
    background: var(--caf-inset);
    margin-bottom: 0.5em;
  }
  .bar i {
    display: block;
    height: 100%;
  }
  .keys {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3em 1em;
    font-size: 0.85em;
  }
  .keys span {
    display: inline-flex;
    align-items: center;
    gap: 0.4em;
  }
  .keys i {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    display: inline-block;
  }
  b {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
</style>
