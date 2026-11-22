<script lang="ts">
  // The ring's popover: what this chat has used and spent. How full the
  // agent's memory is, as it last said; what the chat has cost, from the
  // turns CAF has a record of; and how many tokens they processed.
  import type { DeepReadonly, Spend, Usage } from '@clackatoo/caf'
  import { cost as costOf, total } from '@clackatoo/caf'
  import { count, percent, tokens, usd } from './figures.ts'

  let {
    usage,
    spend,
    openUsage,
  }: {
    usage: DeepReadonly<Usage> | null
    spend: DeepReadonly<Spend>
    /** Opens the usage page; there's no link to it without. */
    openUsage?: (() => void) | undefined
  } = $props()

  const share = $derived(usage === null || usage.size <= 0 ? 0 : Math.min(1, usage.used / usage.size))
  const tone = $derived(share > 0.95 ? 'warning' : share > 0.8 ? 'caution' : null)
  const cost = $derived(costOf(spend))
  const processed = $derived(total(spend.tokens))
  /** How the cost was come by: as the agent reported it, estimated at API prices, or both. */
  const how = $derived.by(() => {
    if (spend.estimated === 0) return 'As the agent reported it.'
    if (spend.reported === 0) return 'An estimate, at API prices.'
    return `${usd(spend.reported)} as the agent reported it, ${usd(spend.estimated)} estimated at API prices.`
  })
</script>

<div class="chat-usage" role="dialog" aria-label="This chat's usage">
  <section>
    <h3>This chat</h3>
    <div class="rows">
      <span class="label">Context</span>
      {#if usage !== null}
        <span class="bar" title="{percent(share)} full"><span class="fill" data-tone={tone} style:width="{share * 100}%"></span></span>
        <span class="value" data-tone={tone} title="{count(usage.used)} of {count(usage.size)} tokens">
          {tokens(usage.used)} <small>/ {tokens(usage.size)}</small>
        </span>
      {:else}
        <span class="value unknown">Not said yet</span>
      {/if}
    </div>
    <dl class="facts">
      <dt>Cost so far</dt>
      <dd class="cost">
        {#if spend.turns === 0}
          <span class="unknown">Nothing yet</span>
        {:else}
          {usd(cost)}{#if spend.estimated > 0}{' '}<small class="estimate">API estimate</small>{/if}
        {/if}
      </dd>
      {#if spend.turns > 0}
        <dd class="how">{how}</dd>
      {/if}
      {#if spend.unpriced > 0}
        <dd class="how unpriced">
          {tokens(spend.unpriced)} tokens on models with no known price aren't in it.
        </dd>
      {/if}
      <dt>Tokens processed</dt>
      <dd class="processed" title="{count(processed)} tokens, over {spend.turns} {spend.turns === 1 ? 'turn' : 'turns'}">
        {spend.turns === 0 ? '0' : tokens(processed)}
      </dd>
    </dl>
  </section>
  {#if openUsage !== undefined}
    <section>
      <button type="button" class="open-usage" onclick={openUsage}>Open usage <span aria-hidden="true">→</span></button>
    </section>
  {/if}
</div>

<style>
  .chat-usage {
    width: 22em;
    background: var(--caf-bg);
    border: 1px solid var(--caf-border);
    border-radius: 10px;
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.18);
    font-size: 0.9em;
    color: var(--caf-text);
  }
  section {
    padding: 0.7em 1em;
  }
  section + section {
    border-top: 1px solid var(--caf-border);
  }
  h3 {
    margin: 0 0 0.6em;
    font-size: 0.85em;
    font-weight: normal;
    color: var(--caf-muted);
  }
  .rows {
    display: grid;
    grid-template-columns: auto minmax(4em, 1fr) auto;
    gap: 0.6em 0.8em;
    align-items: center;
  }
  .value {
    text-align: right;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .value.unknown {
    grid-column: 2 / -1;
  }
  small,
  .unknown {
    color: var(--caf-muted);
  }
  .bar {
    position: relative;
    height: 6px;
    border-radius: 3px;
    background: var(--caf-inset);
    overflow: hidden;
  }
  .fill {
    position: absolute;
    inset: 0 auto 0 0;
    border-radius: 3px;
    background: var(--caf-muted);
  }
  [data-tone='caution'] {
    color: var(--caf-warn);
  }
  .fill[data-tone='caution'] {
    background: var(--caf-warn);
  }
  [data-tone='warning'] {
    color: var(--caf-error);
  }
  .fill[data-tone='warning'] {
    background: var(--caf-error);
  }
  .facts {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 0.3em 0.8em;
    margin: 0.8em 0 0;
  }
  dt {
    color: var(--caf-muted);
  }
  dd {
    margin: 0;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .how {
    grid-column: 1 / -1;
    text-align: left;
    color: var(--caf-muted);
    font-size: 0.9em;
    margin-top: -0.15em;
  }
  .open-usage {
    display: flex;
    justify-content: space-between;
    width: 100%;
    font: inherit;
    padding: 0;
    border: 0;
    background: none;
    color: var(--caf-accent);
    cursor: pointer;
  }
  .open-usage:hover {
    text-decoration: underline;
  }
</style>
