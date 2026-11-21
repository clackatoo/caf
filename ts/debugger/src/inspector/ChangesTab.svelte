<script lang="ts">
  // Each chat's numbered log of changes, as the core keeps it at the
  // position: what viewers are sent.
  import type { Change, Numbered } from '@clackatoo/caf'
  import type { Inspector } from './client.svelte.ts'
  import JsonTree from './JsonTree.svelte'

  let { inspector }: { inspector: Inspector } = $props()

  const at = $derived(inspector.position.at)
  const state = $derived(inspector.state(at))
  const logs = $derived(readLogs(state))

  $effect(() => {
    const wanted = at
    // Again once the link opens, if it wasn't when first asked.
    if (inspector.link !== 'open') return
    const timer = setTimeout(() => inspector.stateAt(wanted), 120)
    return () => clearTimeout(timer)
  })

  /** `inspect()`'s `logs`: each chat's `{ changes, next }`. Its shape is the core's, so read it carefully. */
  function readLogs(state: unknown): { chat: string; changes: Numbered[]; next: unknown }[] | null {
    const logs = (state as { logs?: unknown } | undefined)?.logs
    if (typeof logs !== 'object' || logs === null) return null
    return Object.entries(logs as Record<string, unknown>).map(([chat, log]) => {
      const changes = (log as { changes?: unknown })?.changes
      return { chat, changes: Array.isArray(changes) ? (changes as Numbered[]) : [], next: (log as { next?: unknown })?.next }
    })
  }

  function summary(change: Change): string {
    const value = (change as { value?: unknown }).value
    switch (change.type) {
      case 'TurnAdded':
        return `turn ${change.value.id}`
      case 'TurnInserted':
        return `turn ${change.value.turn.id} before ${change.value.before}`
      case 'TurnLife':
        return `turn ${change.value.turn}: ${change.value.life.type}`
      case 'TurnModel':
        return `turn ${change.value.turn}: ${change.value.model.name ?? change.value.model.id}`
      case 'ItemAdded':
        return `item ${change.value.item.id} (${change.value.item.kind.type}) in turn ${change.value.turn}`
      case 'Appended':
        return `item ${change.value.item}: +${change.value.text.length} chars`
      case 'ToolChanged':
        return `item ${change.value.item}: ${change.value.tool.status.type}`
      case 'ItemEnded':
        return `item ${change.value.item}`
      case 'State':
        return change.value.type
      case 'Usage':
        return `${change.value.used} of ${change.value.size}`
      case 'QuestionAsked':
        return `question ${change.value.id} (${change.value.kind.type})`
      case 'QuestionGone':
        return `question ${change.value}`
      case 'Settings':
        return change.value.chosen.map((choice) => `${choice.kind.type} ${String(choice.value.value)}`).join(', ') || 'none chosen'
      default:
        return typeof value === 'object' ? '' : String(value)
    }
  }
</script>

<div class="changes">
  {#if state === undefined}
    <p class="empty">Asking for the state…</p>
  {:else if logs === null}
    <p class="empty">The core's state has no change logs.</p>
  {:else if logs.length === 0}
    <p class="empty">No chats yet.</p>
  {:else}
    {#each logs as log (log.chat)}
      <section>
        <h4>Chat {log.chat} <span class="sub">next change: {String(log.next)}, {log.changes.length} kept</span></h4>
        <ol>
          {#each log.changes as numbered (numbered.no)}
            <li>
              <details>
                <summary>
                  <span class="no">{numbered.no}</span>
                  <span class="type">{numbered.change.type}</span>
                  <span class="what">{summary(numbered.change)}</span>
                </summary>
                <JsonTree value={numbered.change} openTo={2} />
              </details>
            </li>
          {:else}
            <li class="none">None kept.</li>
          {/each}
        </ol>
      </section>
    {/each}
  {/if}
</div>

<style>
  .changes {
    padding: 0.6em 0.8em;
    overflow: auto;
    height: 100%;
    font-size: 12px;
  }
  h4 {
    margin: 0.6em 0 0.3em;
    font-size: 1.05em;
  }
  .sub {
    color: var(--caf-muted);
    font-weight: normal;
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  summary {
    cursor: pointer;
    display: flex;
    gap: 0.6em;
    padding: 0.1em 0.3em;
    border-radius: 3px;
  }
  summary:hover {
    background: var(--caf-hover);
  }
  .no {
    color: var(--caf-muted);
    font-variant-numeric: tabular-nums;
    min-width: 4em;
  }
  .type {
    font-family: var(--caf-mono);
    color: var(--caf-accent);
    min-width: 9em;
  }
  .what {
    font-family: var(--caf-mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .empty,
  .none {
    color: var(--caf-faint);
  }
</style>
