<script lang="ts">
  // A note too long to sit beside what it's about: an ⓘ button that shows
  // it, and hides it again, for a mouse, a keyboard and a screen reader
  // alike, where a tooltip reaches only a mouse.
  import type { Snippet } from 'svelte'

  let { label, children }: { label: string; children: Snippet } = $props()
  let open = $state(false)
</script>

<span class="info">
  <button type="button" aria-label={label} aria-expanded={open} onclick={() => (open = !open)}>
    <span aria-hidden="true">ⓘ</span>
  </button>
  {#if open}<span class="note" role="note">{@render children()}</span>{/if}
</span>

<style>
  .info {
    position: relative;
    display: inline;
  }
  button {
    font: inherit;
    padding: 0 0.15em;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--caf-muted);
    cursor: pointer;
  }
  button:hover,
  button[aria-expanded='true'] {
    color: var(--caf-text);
    background: var(--caf-hover);
  }
  .note {
    display: block;
    position: absolute;
    left: 0;
    top: calc(100% + 0.3em);
    z-index: 2;
    width: 20em;
    padding: 0.5em 0.7em;
    border: 1px solid var(--caf-border);
    border-radius: 8px;
    background: var(--caf-bg);
    color: var(--caf-text);
    font-size: 0.95em;
    font-weight: normal;
    line-height: 1.4;
    box-shadow: 0 6px 18px rgb(0 0 0 / 0.15);
  }
</style>
