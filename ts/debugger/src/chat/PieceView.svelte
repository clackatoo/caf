<script lang="ts">
  // One piece of what a tool call gave back, by its kind.
  import type { DeepReadonly, Piece } from '@clackatoo/caf'
  import DiffView from './DiffView.svelte'
  import OutputView from './OutputView.svelte'
  import { bytes, cutText, followable, imageSource } from './tools.ts'
  import { rawJson, unhidden } from './unhidden.ts'
  import { tagOf } from './words.ts'

  let { piece, running }: { piece: DeepReadonly<Piece>; running: boolean } = $props()
</script>

{#if piece.type === 'Text'}
  <div class="piece text">
    <pre>{unhidden(piece.value.text)}</pre>
    {#if piece.value.cut !== null}<div class="cut">{cutText(piece.value.cut)}</div>{/if}
  </div>
{:else if piece.type === 'Diff'}
  <div class="piece"><DiffView diff={piece.value} /></div>
{:else if piece.type === 'Output'}
  <div class="piece"><OutputView output={piece.value} {running} /></div>
{:else if piece.type === 'Image'}
  {@const image = piece.value}
  {@const source = imageSource(image.mime, image.data)}
  <div class="piece image">
    {#if source !== null}
      <img src={source} alt={`An image (${unhidden(image.mime)}, ${bytes(image.size)})`} />
    {:else if image.data === null}
      <div class="none">An image too big to keep ({bytes(image.size)})</div>
    {:else}
      <div class="none">An image of a type this viewer doesn't show ({unhidden(image.mime)})</div>
    {/if}
  </div>
{:else if piece.type === 'Link'}
  {@const link = piece.value}
  {@const href = followable(link.uri)}
  <div class="piece link">
    {#if href !== null}
      <a {href} target="_blank" rel="noopener noreferrer">{unhidden(link.name || link.uri)}</a>
    {:else}
      <span class="name">{unhidden(link.name)}</span>
    {/if}
    {#if href === null || (link.name !== '' && link.name !== link.uri)}<code class="uri">{unhidden(link.uri)}</code>{/if}
  </div>
{:else if piece.type === 'Unknown'}
  <details class="piece unknown">
    <summary>A piece this viewer can't show: <code>{unhidden(piece.value.what)}</code></summary>
    <pre>{rawJson(piece.value.raw)}</pre>
  </details>
{:else}
  <details class="piece unknown">
    <summary>A piece of a kind this viewer doesn't know: <code>{tagOf(piece)}</code></summary>
    <pre>{rawJson(piece)}</pre>
  </details>
{/if}

<style>
  pre {
    margin: 0;
    max-height: 20em;
    overflow: auto;
    background: var(--caf-inset);
    border-radius: 4px;
    padding: 0.5em 0.6em;
    font-family: var(--caf-mono);
    font-size: 0.9em;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .cut {
    color: var(--caf-faint);
    font-size: 0.85em;
    margin-top: 0.2em;
  }
  img {
    display: block;
    max-width: 100%;
    max-height: 24em;
    border-radius: 4px;
  }
  .none {
    color: var(--caf-faint);
    font-style: italic;
  }
  .link {
    display: flex;
    gap: 0.6em;
    align-items: baseline;
    flex-wrap: wrap;
    overflow-wrap: anywhere;
  }
  a {
    color: var(--caf-link);
  }
  .uri {
    color: var(--caf-muted);
    font-family: var(--caf-mono);
    font-size: 0.9em;
  }
  .unknown > summary {
    cursor: pointer;
    color: var(--caf-muted);
    font-size: 0.92em;
  }
</style>
