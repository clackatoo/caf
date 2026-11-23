<script lang="ts">
  // One file's change, as CAF worked its hunks out: drawn as they are,
  // each line with its numbers before and after, never compared again
  // here.
  import type { DeepReadonly, Diff } from '@clackatoo/caf'
  import { count } from './figures.ts'
  import { moreHunks } from './tools.ts'
  import { unhidden } from './unhidden.ts'

  let { diff }: { diff: DeepReadonly<Diff> } = $props()

  const SIGN = { Added: '+', Removed: '−', Kept: ' ' } as Record<string, string>
</script>

<div class="diff">
  <div class="head">
    <span class="path">{unhidden(diff.path)}</span>
    {#if diff.new_file}<span class="made">New file</span>{/if}
    <span class="counts"
      ><span class="added">+{count(diff.added)}</span> <span class="removed">−{count(diff.removed)}</span></span
    >
  </div>
  {#if diff.hunks.length > 0}
    <div class="hunks">
      {#each diff.hunks as hunk, h (h)}
        {#if h > 0}<div class="gap" aria-hidden="true">⋯</div>{/if}
        <table class="hunk">
          <tbody>
            {#each hunk.lines as line, l (l)}
              <tr class="line" data-change={line.change.type}>
                <td class="old">{line.old ?? ''}</td>
                <td class="new">{line.new ?? ''}</td>
                <td class="sign">{SIGN[line.change.type] ?? '?'}</td>
                <td class="text">{unhidden(line.text)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/each}
    </div>
  {/if}
  {#if diff.more > 0}<div class="more">{moreHunks(diff.more)}</div>{/if}
</div>

<style>
  .diff {
    border: 1px solid var(--caf-border);
    border-radius: 4px;
    overflow: hidden;
  }
  .head {
    display: flex;
    gap: 0.6em;
    align-items: baseline;
    padding: 0.3em 0.6em;
    background: var(--caf-panel);
    border-bottom: 1px solid var(--caf-border);
    font-size: 0.9em;
  }
  .path {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
    font-family: var(--caf-mono);
  }
  .made {
    color: var(--caf-muted);
  }
  .counts {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .added {
    color: var(--caf-ok);
  }
  .removed {
    color: var(--caf-error);
  }
  .hunks {
    max-height: 24em;
    overflow: auto;
    background: var(--caf-inset);
  }
  .hunk {
    border-collapse: collapse;
    width: 100%;
    font-family: var(--caf-mono);
    font-size: 0.85em;
  }
  td {
    padding: 0 0.4em;
    vertical-align: top;
  }
  .old,
  .new,
  .sign {
    color: var(--caf-faint);
    text-align: right;
    user-select: none;
    white-space: nowrap;
    width: 1px;
    font-variant-numeric: tabular-nums;
  }
  .text {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  [data-change='Added'] {
    background: color-mix(in srgb, var(--caf-ok) 14%, transparent);
  }
  [data-change='Removed'] {
    background: color-mix(in srgb, var(--caf-error) 14%, transparent);
  }
  .gap,
  .more {
    color: var(--caf-faint);
    font-size: 0.85em;
    padding: 0.1em 0.6em;
  }
  .more {
    border-top: 1px solid var(--caf-border);
  }
</style>
