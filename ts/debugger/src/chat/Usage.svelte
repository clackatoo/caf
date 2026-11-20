<script lang="ts">
  // How full the agent's memory is, and what the session has cost, as the
  // agent last said.
  import type { DeepReadonly, Usage } from '@clackatoo/caf'

  let { usage }: { usage: DeepReadonly<Usage> } = $props()

  const share = $derived(usage.size > 0 ? Math.min(1, usage.used / usage.size) : 0)
  const percent = $derived(Math.round(share * 100))
  const cost = $derived(usage.cost === null ? null : money(usage.cost.amount, usage.cost.currency))

  function money(amount: number, currency: string): string {
    try {
      return new Intl.NumberFormat(undefined, { style: 'currency', currency }).format(amount)
    } catch {
      // Not a currency code the browser knows.
      return `${amount} ${currency}`
    }
  }

  function tokens(n: number): string {
    return new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 }).format(n)
  }
</script>

<div class="usage" title="{usage.used.toLocaleString()} of {usage.size.toLocaleString()} tokens">
  <meter min="0" max="1" low="0.7" high="0.9" optimum="0" value={share} aria-label="Memory used"></meter>
  <span class="figures">{percent}% of {tokens(usage.size)}</span>
  {#if cost !== null}<span class="cost">{cost}</span>{/if}
</div>

<style>
  .usage {
    display: flex;
    align-items: center;
    gap: 0.5em;
    font-size: 0.85em;
    color: var(--caf-muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  meter {
    width: 5em;
    height: 0.6em;
  }
  .cost {
    color: var(--caf-text);
  }
</style>
