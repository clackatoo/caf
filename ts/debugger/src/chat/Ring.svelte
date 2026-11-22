<script lang="ts">
  // The context ring, by Send: how full the agent's memory is, as it last
  // said, amber past 80% and red past 95%. Clicking it opens what the
  // chat has used and spent. Before the agent says, it's an empty ring.
  import type { DeepReadonly, Spend, Usage } from '@clackatoo/caf'
  import ChatUsage from './ChatUsage.svelte'

  let {
    usage,
    spend,
    openUsage,
  }: {
    usage: DeepReadonly<Usage> | null
    spend: DeepReadonly<Spend>
    openUsage?: (() => void) | undefined
  } = $props()

  let open = $state(false)
  let wrap = $state<HTMLElement>()

  const share = $derived(usage === null || usage.size <= 0 ? null : Math.min(1, usage.used / usage.size))
  const percent = $derived(share === null ? null : Math.round(share * 100))
  const tone = $derived(share === null ? null : share > 0.95 ? 'warning' : share > 0.8 ? 'caution' : null)
  const RADIUS = 15.5
  const ROUND = 2 * Math.PI * RADIUS

  // Closed by Escape, or a click anywhere else.
  $effect(() => {
    if (!open) return
    const away = (event: PointerEvent) => {
      if (wrap !== undefined && !wrap.contains(event.target as Node)) open = false
    }
    const key = (event: KeyboardEvent) => {
      if (event.key === 'Escape') open = false
    }
    window.addEventListener('pointerdown', away)
    window.addEventListener('keydown', key)
    return () => {
      window.removeEventListener('pointerdown', away)
      window.removeEventListener('keydown', key)
    }
  })

  function opened() {
    open = false
    openUsage?.()
  }
</script>

<div class="ring-wrap" bind:this={wrap}>
  <button
    type="button"
    class="ring"
    data-tone={tone}
    aria-expanded={open}
    aria-label={percent === null ? "This chat's usage" : `This chat's usage: context ${percent}% full`}
    title={percent === null ? 'Usage' : `Context: ${percent}% used`}
    onclick={() => (open = !open)}
  >
    <svg viewBox="0 0 37 37" aria-hidden="true">
      <circle class="track" cx="18.5" cy="18.5" r={RADIUS} />
      {#if share !== null && share > 0}
        <circle class="used" cx="18.5" cy="18.5" r={RADIUS} stroke-dasharray="{ROUND * share} {ROUND}" />
      {/if}
    </svg>
    <span class="percent">{percent === null ? '' : `${percent}%`}</span>
  </button>
  {#if open}
    <div class="pop">
      <ChatUsage {usage} {spend} openUsage={openUsage === undefined ? undefined : opened} />
    </div>
  {/if}
</div>

<style>
  .ring-wrap {
    position: relative;
    flex: none;
  }
  .ring {
    position: relative;
    width: 2.6em;
    height: 2.6em;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: none;
    color: var(--caf-muted);
    font: inherit;
    font-size: 0.75em;
    font-variant-numeric: tabular-nums;
    cursor: pointer;
    display: grid;
    place-items: center;
  }
  .ring:hover,
  .ring[aria-expanded='true'] {
    background: var(--caf-hover);
  }
  .ring[data-tone='caution'] {
    color: var(--caf-warn);
  }
  .ring[data-tone='warning'] {
    color: var(--caf-error);
  }
  svg {
    position: absolute;
    inset: 0;
    transform: rotate(-90deg);
  }
  circle {
    fill: none;
    stroke-width: 2.5;
  }
  .track {
    stroke: var(--caf-border);
  }
  .used {
    stroke: currentColor;
    stroke-linecap: round;
  }
  .pop {
    position: absolute;
    right: 0;
    bottom: calc(100% + 0.5em);
    z-index: 3;
  }
</style>
