<script lang="ts">
  // A tool call as a row: its kind, title, status and how long it ran,
  // unfolding to what it was given and what it gave back.
  import type { DeepReadonly, ToolCall, Window } from '@clackatoo/caf'
  import { took } from './time.ts'
  import { rawJson, unhidden } from './unhidden.ts'
  import { toolKind, toolStatus } from './words.ts'

  let { tool, window }: { tool: DeepReadonly<ToolCall>; window: DeepReadonly<Window> } = $props()

  const kind = $derived(toolKind(tool.kind))
  const time = $derived(took(window))
  const input = $derived(tool.input === null ? null : rawJson(tool.input))
  const ended = $derived(
    ['Done', 'Failed', 'Refused', 'Cancelled'].includes(tool.status.type),
  )
  // Until the debugger draws every piece (feature 10): their text.
  const output = $derived(
    tool.result
      .map((piece) =>
        piece.type === 'Text'
          ? piece.value.text
          : piece.type === 'Output'
            ? piece.value.lines.join('\n')
            : '',
      )
      .filter((text) => text !== '')
      .join('\n'),
  )
</script>

<details class="tool" data-status={tool.status.type}>
  <summary>
    <span class="icon" title={kind.name} aria-hidden="true">{kind.icon}</span>
    <span class="kind">{kind.name}</span>
    <span class="title">{tool.title}</span>
    <span class="status">{toolStatus(tool.status)}</span>
    {#if time !== null}<span class="time">{time}</span>{/if}
  </summary>
  <div class="body">
    <div class="label">Input</div>
    {#if input === null}
      <div class="none">None</div>
    {:else}
      <pre>{input}</pre>
    {/if}
    <div class="label">Output</div>
    {#if output === ''}
      <div class="none">{ended ? 'None' : 'None yet'}</div>
    {:else}
      <pre>{unhidden(output)}</pre>
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
  .status {
    color: var(--caf-muted);
    font-size: 0.9em;
  }
  [data-status='Running'] .status,
  [data-status='Pending'] .status {
    color: var(--caf-accent);
  }
  [data-status='Done'] .status {
    color: var(--caf-ok);
  }
  [data-status='Failed'] .status {
    color: var(--caf-error);
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
  .label {
    color: var(--caf-muted);
    font-size: 0.85em;
    margin: 0.4em 0 0.2em;
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
  .none {
    color: var(--caf-faint);
    font-style: italic;
  }
</style>
