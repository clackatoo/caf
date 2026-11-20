<script lang="ts">
  // One thing the agent did in a turn, by its kind.
  import type { DeepReadonly, Item } from '@clackatoo/caf'
  import Markdown from './Markdown.svelte'
  import Tool from './Tool.svelte'
  import { rawJson, unhidden } from './unhidden.ts'
  import { noteText, tagOf } from './words.ts'

  let {
    item,
    appSummary,
  }: {
    item: DeepReadonly<Item>
    /** The one-line summary of one of the app's own items: the app knows its type, the view doesn't. */
    appSummary: (app: unknown) => string
  } = $props()

  const kind = $derived(item.kind)
</script>

{#if kind.type === 'Words'}
  <div class="words"><Markdown text={kind.value} /></div>
{:else if kind.type === 'Thinking'}
  <details class="thinking">
    <summary>Thinking</summary>
    <div class="thought"><Markdown text={kind.value} /></div>
  </details>
{:else if kind.type === 'Tool'}
  <Tool tool={kind.value} window={item.window} />
{:else if kind.type === 'Note'}
  <div class="note" role="note"><span class="who">CAF</span>{noteText(kind.value)}</div>
{:else if kind.type === 'Compacted'}
  <div class="note" role="note"><span class="who">CAF</span>The agent compacted its memory here.</div>
{:else if kind.type === 'App'}
  <div class="app">{appSummary(kind.value)}</div>
{:else if kind.type === 'Unknown'}
  <details class="unknown">
    <summary>Something the agent sent that CAF couldn't read: <code>{unhidden(kind.value.what)}</code></summary>
    <pre>{rawJson(kind.value.raw)}</pre>
  </details>
{:else}
  <details class="unknown">
    <summary>An item of a kind this viewer doesn't know: <code>{tagOf(kind)}</code></summary>
    <pre>{rawJson(kind)}</pre>
  </details>
{/if}

<style>
  .words {
    padding: 0.1em 0;
  }
  details > summary {
    cursor: pointer;
    color: var(--caf-muted);
    font-size: 0.92em;
  }
  .thinking .thought {
    margin-top: 0.3em;
    padding-left: 0.8em;
    border-left: 2px solid var(--caf-border);
    color: var(--caf-muted);
    font-style: italic;
  }
  .note {
    background: var(--caf-caf-bg);
    color: var(--caf-text);
    border-left: 3px solid var(--caf-caf);
    border-radius: 4px;
    padding: 0.45em 0.7em;
    font-size: 0.92em;
  }
  .who {
    color: var(--caf-caf);
    font-weight: 600;
    font-size: 0.85em;
    letter-spacing: 0.04em;
    margin-right: 0.6em;
  }
  .app {
    color: var(--caf-muted);
    font-size: 0.92em;
    border: 1px dashed var(--caf-border);
    border-radius: 6px;
    padding: 0.35em 0.6em;
  }
  .unknown pre {
    margin: 0.3em 0 0;
    max-height: 20em;
    overflow: auto;
    background: var(--caf-inset);
    border-radius: 4px;
    padding: 0.5em 0.6em;
    font-family: var(--caf-mono);
    font-size: 0.85em;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  code {
    font-family: var(--caf-mono);
  }
</style>
