<script lang="ts">
  // The inspector: CAF's steps, its core's state, and each chat's changes,
  // at the position the timeline is at.
  import ChangesTab from './ChangesTab.svelte'
  import type { Inspector } from './client.svelte.ts'
  import JsonTree from './JsonTree.svelte'
  import StateTab from './StateTab.svelte'
  import StepsTab from './StepsTab.svelte'
  import { divergedText } from './summary.ts'

  let { inspector }: { inspector: Inspector } = $props()

  const tabs = ['Steps', 'State', 'Changes'] as const
  let tab = $state<(typeof tabs)[number]>('Steps')
  let showDivergence = $state(false)

  /** Arrow keys move between the tabs, as tabs do. */
  function ontabkey(event: KeyboardEvent) {
    const i = tabs.indexOf(tab)
    const next = event.key === 'ArrowRight' ? i + 1 : event.key === 'ArrowLeft' ? i - 1 : event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : null
    if (next === null) return
    event.preventDefault()
    tab = tabs[(next + tabs.length) % tabs.length]!
    document.getElementById(`inspector-tab-${tab}`)?.focus()
  }
</script>

<section class="inspector" aria-label="Inspector">
  {#if inspector.diverged !== null}
    {@const diverged = inspector.diverged}
    <div class="diverged" role="alert">
      <div class="line">
        <strong>{divergedText(diverged.check, diverged.at)}</strong>
        Replaying it didn't give what CAF did: that's a bug in CAF. What's shown after it may be wrong.
        <button type="button" onclick={() => inspector.seek(diverged.at)}>Go to it</button>
        <button type="button" onclick={() => (showDivergence = !showDivergence)}>{showDivergence ? 'Hide' : 'Show'} the difference</button>
      </div>
      {#if showDivergence}
        <div class="both">
          <div><h4>CAF gave out</h4><JsonTree value={diverged.expected} openTo={2} /></div>
          <div>
            <h4>The replay gave out</h4>
            {#if diverged.got === null}
              <p>Nothing: the step's event didn't read back from its JSON.</p>
            {:else}
              <JsonTree value={diverged.got} openTo={2} />
            {/if}
          </div>
        </div>
      {/if}
    </div>
  {/if}
  {#if inspector.past.problem !== null}
    <div class="diverged" role="alert"><strong>The chats as they were are wrong:</strong> {inspector.past.problem}</div>
  {/if}
  {#if inspector.problem !== null}
    <div class="diverged" role="alert"><strong>The server sent something wrong:</strong> {inspector.problem}</div>
  {/if}

  <div class="tabs" role="tablist" aria-label="Inspector" tabindex="-1" onkeydown={ontabkey}>
    {#each tabs as name (name)}
      <button
        type="button"
        role="tab"
        id="inspector-tab-{name}"
        aria-selected={tab === name}
        aria-controls="inspector-panel"
        tabindex={tab === name ? 0 : -1}
        class:active={tab === name}
        onclick={() => (tab = name)}>{name}</button
      >
    {/each}
  </div>
  <div class="panel" role="tabpanel" id="inspector-panel" aria-labelledby="inspector-tab-{tab}">
    {#if tab === 'Steps'}
      <StepsTab {inspector} />
    {:else if tab === 'State'}
      <StateTab {inspector} />
    {:else}
      <ChangesTab {inspector} />
    {/if}
  </div>
</section>

<style>
  .inspector {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--caf-bg);
  }
  .diverged {
    background: var(--caf-error-bg);
    border-bottom: 1px solid var(--caf-error);
    color: var(--caf-text);
    padding: 0.5em 0.8em;
    font-size: 0.88em;
  }
  .diverged .line {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5em;
    align-items: baseline;
  }
  .diverged strong {
    color: var(--caf-error);
  }
  .both {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.8em;
    margin-top: 0.5em;
    max-height: 18em;
    overflow: auto;
  }
  h4 {
    margin: 0 0 0.3em;
    font-size: 0.9em;
  }
  .tabs {
    display: flex;
    gap: 0.2em;
    padding: 0.35em 0.6em 0;
    border-bottom: 1px solid var(--caf-border);
    background: var(--caf-panel);
  }
  .tabs button {
    font: inherit;
    font-size: 0.9em;
    padding: 0.35em 0.9em;
    border: 1px solid transparent;
    border-bottom: none;
    border-radius: 6px 6px 0 0;
    background: none;
    color: var(--caf-muted);
    cursor: pointer;
    margin-bottom: -1px;
  }
  .tabs button.active {
    background: var(--caf-bg);
    border-color: var(--caf-border);
    color: var(--caf-text);
  }
  .diverged button {
    font: inherit;
    font-size: 0.95em;
    padding: 0.1em 0.6em;
    border-radius: 5px;
    border: 1px solid var(--caf-error);
    background: var(--caf-bg);
    color: var(--caf-error);
    cursor: pointer;
  }
  .panel {
    flex: 1;
    min-height: 0;
  }
</style>
