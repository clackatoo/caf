<script lang="ts">
  // A tool call as a row: its kind, title, status and how long it ran,
  // unfolding, as CAF SI lays it out, to who allowed it, what it gave back,
  // the files it's about, and what it was given, folded away. A call with
  // a diff unfolds by itself, once, as an edit is what the user most
  // wants to check, and so does a command running with output, so it can
  // be watched as it comes.
  import type { DeepReadonly, ToolCall, Window } from '@clackatoo/caf'
  import PieceView from './PieceView.svelte'
  import { took } from './time.ts'
  import { count } from './figures.ts'
  import { cutText, decisionText, ended as hasEnded, locationText } from './tools.ts'
  import { rawJson, unhidden } from './unhidden.ts'
  import { toolKind, toolStatus } from './words.ts'

  let { tool, window }: { tool: DeepReadonly<ToolCall>; window: DeepReadonly<Window> } = $props()

  const kind = $derived(toolKind(tool.kind))
  const time = $derived(took(window))
  const ended = $derived(hasEnded(tool.status))
  const decision = $derived(decisionText(tool.decision))
  // Cut, the input is the start of its JSON, as a string: shown as it is.
  const input = $derived(
    tool.input === null ? null : tool.input_cut !== null && typeof tool.input === 'string' ? unhidden(tool.input) : rawJson(tool.input),
  )
  const live = $derived(!ended && tool.result.some((piece) => piece.type === 'Output'))
  const diff = $derived(tool.result.some((piece) => piece.type === 'Diff'))
  // A refused call never ran: what it gave back is what it would have
  // done, such as the diff CAF worked out before asking.
  const resultLabel = $derived(tool.status.type === 'Refused' ? 'What it would have done (never ran)' : 'Output')

  let open = $state(false)
  let unfolded = false
  $effect(() => {
    if ((live || diff) && !unfolded) {
      unfolded = true
      open = true
    }
  })
</script>

<details class="tool" data-status={tool.status.type} bind:open>
  <summary>
    <span class="icon" title={kind.name} aria-hidden="true">{kind.icon}</span>
    <span class="kind">{kind.name}</span>
    <span class="title">{tool.title}</span>
    <span class="status">{toolStatus(tool.status)}</span>
    {#if time !== null}<span class="time">{time}</span>{/if}
  </summary>
  <div class="body">
    {#if decision !== null}
      <div class="decision" data-decision={tool.decision.type}>{decision}</div>
    {/if}
    <div class="label">{resultLabel}</div>
    {#if tool.result.length === 0}
      <div class="none">{ended ? 'None' : 'None yet'}</div>
    {:else}
      <div class="result">
        {#each tool.result as piece, n (n)}<PieceView {piece} running={!ended} />{/each}
      </div>
    {/if}
    {#if tool.locations.length > 0}
      <div class="label">Files</div>
      <ul class="locations">
        {#each tool.locations as location, n (n)}<li>{unhidden(locationText(location))}</li>{/each}
        {#if tool.locations_cut > 0}<li class="more">and {count(tool.locations_cut)} more</li>{/if}
      </ul>
    {/if}
    {#if input === null}
      <div class="label">Input</div>
      <div class="none">None</div>
    {:else}
      <details class="input">
        <summary class="label">Input</summary>
        <pre>{input}</pre>
        {#if tool.input_cut !== null}<div class="cut">{cutText(tool.input_cut)}</div>{/if}
      </details>
    {/if}
  </div>
</details>

<style>
  .tool {
    border: 1px solid var(--caf-border);
    border-radius: 6px;
    background: var(--caf-panel);
    font-size: 0.92em;
  }
  summary {
    display: flex;
    gap: 0.5em;
    align-items: baseline;
    padding: 0.35em 0.6em;
    cursor: pointer;
    list-style: none;
  }
  summary::-webkit-details-marker {
    display: none;
  }
  summary::before {
    content: '▸';
    color: var(--caf-faint);
  }
  details[open] > summary::before {
    content: '▾';
  }
  /* The kind's name, for screen readers: the icon says it to the eye. */
  .kind {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--caf-mono);
  }
  /* CAF SI's roles: the accent for what's happening now or waits on the
     user, the warning for what went wrong or was refused, muted for
     what's done or was stopped, and calm for what hasn't started. */
  .status {
    font-size: 0.9em;
  }
  [data-status='Running'] .status,
  [data-status='Asking'] .status {
    color: var(--caf-accent);
  }
  [data-status='Done'] .status,
  [data-status='Cancelled'] .status {
    color: var(--caf-muted);
  }
  [data-status='Failed'] .status,
  [data-status='Refused'] .status {
    color: var(--caf-error);
  }
  [data-status='Asking'] {
    border-color: var(--caf-accent);
  }
  .time {
    color: var(--caf-faint);
    font-size: 0.9em;
    font-variant-numeric: tabular-nums;
  }
  .body {
    border-top: 1px solid var(--caf-border);
    padding: 0.4em 0.6em 0.6em;
  }
  .decision {
    color: var(--caf-muted);
    font-size: 0.9em;
    overflow-wrap: anywhere;
  }
  .label {
    color: var(--caf-muted);
    font-size: 0.85em;
    margin: 0.4em 0 0.2em;
  }
  .input > summary {
    cursor: pointer;
  }
  .locations {
    margin: 0;
    padding-left: 1.2em;
    font-family: var(--caf-mono);
    font-size: 0.9em;
    overflow-wrap: anywhere;
  }
  .locations .more {
    color: var(--caf-faint);
    font-family: var(--caf-sans);
    list-style: none;
  }
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
  .result {
    display: flex;
    flex-direction: column;
    gap: 0.4em;
  }
  .cut {
    color: var(--caf-faint);
    font-size: 0.85em;
    margin-top: 0.2em;
  }
  .none {
    color: var(--caf-faint);
    font-style: italic;
  }
</style>
