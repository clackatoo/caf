<script lang="ts">
  // Every step from the start to now: drag to any, and at the end, live,
  // it follows. Looking back shows the chats and the core as they were.
  import type { Inspector } from './client.svelte.ts'

  let { inspector }: { inspector: Inspector } = $props()

  const position = $derived(inspector.position)
  const live = $derived(inspector.live)
  // While dragging, the handle shows where the user has it, not where the
  // server has got to.
  let dragging = $state<number | null>(null)
  const shown = $derived(dragging ?? position.at)

  // One seek in flight at a time while dragging, the latest one winning.
  let timer: ReturnType<typeof setTimeout> | null = null
  function oninput(event: Event & { currentTarget: HTMLInputElement }) {
    dragging = Number(event.currentTarget.value)
    if (timer !== null) return
    timer = setTimeout(() => {
      timer = null
      if (dragging !== null) go(dragging)
    }, 50)
  }
  function onchange(event: Event & { currentTarget: HTMLInputElement }) {
    if (timer !== null) clearTimeout(timer)
    timer = null
    go(Number(event.currentTarget.value))
    dragging = null
  }

  /** Looks at a position: the end, live, means follow. */
  function go(at: number) {
    if (live && at >= position.end) {
      if (!position.live) inspector.follow()
    } else if (at !== position.at || position.live) {
      inspector.seek(at)
    }
  }
</script>

<div class="timeline" class:back={!position.live}>
  <button type="button" disabled={shown <= 0} onclick={() => inspector.seek(shown - 1)} aria-label="Step back">◀</button>
  <button type="button" disabled={shown >= position.end} onclick={() => go(shown + 1)} aria-label="Step forward">▶</button>
  <input
    type="range"
    min="0"
    max={position.end}
    step="1"
    value={shown}
    aria-label="Position"
    {oninput}
    {onchange}
  />
  <span class="where">
    {#if position.live}
      <span class="live">● Live</span>, step {position.end}
    {:else}
      Step {shown} of {position.end}
    {/if}
  </span>
  {#if live}
    <button type="button" class="follow" disabled={position.live} onclick={() => inspector.follow()}>Back to live</button>
  {/if}
</div>

<style>
  .timeline {
    display: flex;
    align-items: center;
    gap: 0.5em;
    padding: 0.45em 0.8em;
    border-top: 1px solid var(--caf-border);
    background: var(--caf-panel);
    font-size: 0.88em;
  }
  .timeline.back {
    background: var(--caf-warn-bg);
  }
  input {
    flex: 1;
    accent-color: var(--caf-accent);
  }
  .where {
    min-width: 10em;
    text-align: right;
    color: var(--caf-muted);
    font-variant-numeric: tabular-nums;
  }
  .live {
    color: var(--caf-error);
    font-weight: 600;
  }
  button {
    font: inherit;
    padding: 0.2em 0.6em;
    border-radius: 6px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .follow:not(:disabled) {
    border-color: var(--caf-accent);
    color: var(--caf-accent);
  }
</style>
