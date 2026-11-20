<script lang="ts">
  // The core's whole state at the position, with what changed since the
  // position before marked.
  import type { Inspector } from './client.svelte.ts'
  import { diff } from './diff.ts'
  import JsonTree from './JsonTree.svelte'

  let { inspector }: { inspector: Inspector } = $props()

  const at = $derived(inspector.position.at)
  const state = $derived(inspector.state(at))
  const before = $derived(at > 0 ? inspector.state(at - 1) : undefined)
  const changes = $derived(state !== undefined && before !== undefined ? diff(before, state) : null)

  // Ask for what's shown, once the position settles (it moves on every
  // step while following, and on every pixel while dragging).
  $effect(() => {
    const wanted = at
    // Again once the link opens, if it wasn't when first asked.
    if (inspector.link !== 'open') return
    const timer = setTimeout(() => {
      inspector.stateAt(wanted)
      if (wanted > 0) inspector.stateAt(wanted - 1)
    }, 120)
    return () => clearTimeout(timer)
  })
</script>

<div class="state">
  <div class="note">
    The core's state just after step {at}{at === 0 ? ' (fresh)' : ''}.
    {#if changes !== null}
      {changes.changed.size === 0 && changes.removed.size === 0 ? 'Nothing changed in this step.' : 'What this step changed is marked.'}
    {/if}
    It holds what a recording hides, such as secrets: don't paste it anywhere public.
  </div>
  {#if state === undefined}
    <p class="loading">Asking for it…</p>
  {:else}
    <JsonTree value={state} diff={changes} openTo={1} />
  {/if}
</div>

<style>
  .state {
    padding: 0.6em 0.8em;
    overflow: auto;
    height: 100%;
  }
  .note {
    color: var(--caf-muted);
    font-size: 0.85em;
    margin-bottom: 0.6em;
  }
  .loading {
    color: var(--caf-faint);
  }
</style>
