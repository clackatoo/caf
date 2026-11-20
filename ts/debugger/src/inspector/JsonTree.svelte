<script lang="ts">
  // A JSON value as a folding tree. With a diff, what changed is marked,
  // folded or not, and what was removed is shown struck through.
  import JsonTree from './JsonTree.svelte'
  import { key, type Diff } from './diff.ts'

  let {
    value,
    name = null,
    path = '',
    diff = null,
    depth = 0,
    openTo = 1,
  }: {
    value: unknown
    /** Its key or index in its parent: none at the root. */
    name?: string | null
    path?: string
    diff?: Diff | null
    depth?: number
    /** Nodes shallower than this start open. */
    openTo?: number
  } = $props()

  const isArray = $derived(Array.isArray(value))
  const isObject = $derived(typeof value === 'object' && value !== null)
  const entries = $derived(
    isObject
      ? isArray
        ? (value as unknown[]).map((child, i) => [String(i), child] as const)
        : Object.entries(value as Record<string, unknown>)
      : [],
  )
  const changed = $derived(diff?.changed.has(path) ?? false)
  const removed = $derived(
    diff === null ? [] : [...diff.removed].filter(([at]) => at.slice(0, at.lastIndexOf('/')) === path),
  )
  // Open is the user's to change; until then, it follows the depth.
  let opened = $state<boolean | null>(null)
  const open = $derived(opened ?? depth < openTo)

  /** The last key of a path, unescaped. */
  function last(at: string): string {
    return at.slice(at.lastIndexOf('/') + 1).replace(/~1/g, '/').replace(/~0/g, '~')
  }

  function scalar(value: unknown): string {
    return value === undefined ? 'undefined' : JSON.stringify(value)
  }

  function brief(value: unknown): string {
    if (Array.isArray(value)) return value.length === 0 ? '[]' : `[ ${value.length} ]`
    const keys = Object.keys(value as object)
    if (keys.length === 0) return '{}'
    // A CAF enum reads best by its tag.
    const type = (value as { type?: unknown }).type
    if (typeof type === 'string') return `{ ${type}${keys.length > 1 ? ' …' : ''} }`
    return `{ ${keys.length} }`
  }
</script>

<div class="node" class:changed>
  {#if isObject && entries.length > 0}
    <button type="button" class="row" aria-expanded={open} onclick={() => (opened = !open)}>
      <span class="twisty">{open ? '▾' : '▸'}</span>
      {#if name !== null}<span class="key">{name}</span>:{/if}
      <span class="brief">{brief(value)}</span>
    </button>
    {#if open}
      <div class="children">
        {#each entries as [child, inner] (child)}
          <JsonTree value={inner} name={child} path="{path}/{key(child)}" {diff} depth={depth + 1} {openTo} />
        {/each}
        {#each removed as [at, gone] (at)}
          <div class="row removed" title="Removed since the step before">
            <span class="key">{last(at)}</span>:
            <span class="scalar">{typeof gone === 'object' && gone !== null ? brief(gone) : scalar(gone)}</span>
          </div>
        {/each}
      </div>
    {/if}
  {:else}
    <div class="row leaf">
      {#if name !== null}<span class="key">{name}</span>:{/if}
      <span class="scalar" data-kind={value === null ? 'null' : typeof value}>{isObject ? brief(value) : scalar(value)}</span>
    </div>
    {#if removed.length > 0}
      <!-- An emptied object or array: what it held. -->
      <div class="children">
        {#each removed as [at, gone] (at)}
          <div class="row removed" title="Removed since the step before">
            <span class="key">{last(at)}</span>:
            <span class="scalar">{typeof gone === 'object' && gone !== null ? brief(gone) : scalar(gone)}</span>
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .node {
    font-family: var(--caf-mono);
    font-size: 12px;
    line-height: 1.55;
  }
  .row {
    display: flex;
    gap: 0.35em;
    align-items: baseline;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    padding: 0 0.3em;
    font: inherit;
    color: inherit;
    border-radius: 3px;
    white-space: nowrap;
  }
  button.row {
    cursor: pointer;
  }
  button.row:hover {
    background: var(--caf-hover);
  }
  .leaf {
    padding-left: calc(0.3em + 1ch + 0.35em);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .twisty {
    color: var(--caf-faint);
    width: 1ch;
  }
  .children {
    padding-left: 1.4em;
    border-left: 1px solid var(--caf-border);
    margin-left: 0.55em;
  }
  .key {
    color: var(--caf-accent);
  }
  .brief {
    color: var(--caf-muted);
  }
  .scalar[data-kind='string'] {
    color: var(--caf-ok);
  }
  .scalar[data-kind='number'],
  .scalar[data-kind='boolean'],
  .scalar[data-kind='null'] {
    color: var(--caf-caf);
  }
  .changed > .row {
    background: var(--caf-warn-bg);
    box-shadow: inset 2px 0 0 var(--caf-warn);
  }
  .removed {
    text-decoration: line-through;
    color: var(--caf-error);
    background: var(--caf-error-bg);
    padding-left: calc(0.3em + 1ch + 0.35em);
  }
  .removed .key {
    color: inherit;
  }
</style>
