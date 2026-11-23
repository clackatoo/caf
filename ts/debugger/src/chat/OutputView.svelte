<script lang="ts">
  // A command's output, as its terminal showed it: its lines in colour,
  // how many earlier ones weren't kept, a caret while it runs, and how it
  // ended. Lines come and change as CAF sends them (`OutputChanged`).
  //
  // Only the last MOST_DRAWN lines are drawn until the user asks for the
  // rest, so a command printing many short lines doesn't hold the page
  // up as each change redraws it. Colours are still worked out from the
  // first line kept, as a colour set early carries on down.
  import type { DeepReadonly, Output } from '@clackatoo/caf'
  import { colour } from './ansi.ts'
  import { count } from './figures.ts'
  import { droppedText, endedBadly, endingText } from './tools.ts'

  let { output, running }: { output: DeepReadonly<Output>; running: boolean } = $props()

  /** The most lines drawn before the user asks for all of them. */
  const MOST_DRAWN = 2000

  let all = $state(false)
  const coloured = $derived(colour(output.lines))
  const hidden = $derived(all ? 0 : Math.max(0, coloured.length - MOST_DRAWN))
  const lines = $derived(hidden === 0 ? coloured : coloured.slice(hidden))
</script>

<div class="output">
  {#if output.dropped > 0}<div class="dropped">{droppedText(output.dropped)}</div>{/if}
  {#if hidden > 0}
    <button type="button" class="all" onclick={() => (all = true)}
      >Show {count(hidden)} earlier {hidden === 1 ? 'line' : 'lines'}</button
    >
  {/if}
  <pre class="lines">{#each lines as runs, n (n)}<span class="line"
        >{#each runs as run, r (r)}{#if run.style === ''}{run.text}{:else}<span style={run.style}>{run.text}</span
            >{/if}{/each}</span
      >{#if n < lines.length - 1}{'\n'}{/if}{/each}{#if running && output.ending === null}<span
        class="caret"
        aria-label="Still running">▍</span
      >{/if}</pre>
  {#if output.ending !== null}
    <div class="ending" class:bad={endedBadly(output.ending)}>{endingText(output.ending)}</div>
  {/if}
</div>

<style>
  /* The terminal's 16 colours, readable on the inset in each theme. */
  .output {
    --caf-ansi-0: #1c2430;
    --caf-ansi-1: #b8392b;
    --caf-ansi-2: #1f7a48;
    --caf-ansi-3: #865a00;
    --caf-ansi-4: #2f6db3;
    --caf-ansi-5: #8b3fa8;
    --caf-ansi-6: #1d7a87;
    --caf-ansi-7: #5d6877;
    --caf-ansi-8: #626b79;
    --caf-ansi-9: #c94f3f;
    --caf-ansi-10: #2e9c6a;
    --caf-ansi-11: #a07000;
    --caf-ansi-12: #3a7fd0;
    --caf-ansi-13: #a356c2;
    --caf-ansi-14: #2591a0;
    --caf-ansi-15: #1c2430;
  }
  @media (prefers-color-scheme: dark) {
    .output {
      --caf-ansi-0: #5c6570;
      --caf-ansi-1: #ec7a68;
      --caf-ansi-2: #5cc184;
      --caf-ansi-3: #e0b452;
      --caf-ansi-4: #6aa5e8;
      --caf-ansi-5: #c991e1;
      --caf-ansi-6: #62c4d6;
      --caf-ansi-7: #c4cbd4;
      --caf-ansi-8: #8a94a3;
      --caf-ansi-9: #f39a8b;
      --caf-ansi-10: #7fd8a1;
      --caf-ansi-11: #f0cc78;
      --caf-ansi-12: #8fbdf0;
      --caf-ansi-13: #dcb0ee;
      --caf-ansi-14: #8bd8e5;
      --caf-ansi-15: #e4e8ee;
    }
  }
  .lines {
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
  .dropped,
  .ending {
    color: var(--caf-faint);
    font-size: 0.85em;
    margin: 0.2em 0;
  }
  .all {
    font: inherit;
    font-size: 0.85em;
    margin: 0.2em 0;
    padding: 0.1em 0.6em;
    border-radius: 4px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
    cursor: pointer;
  }
  .ending.bad {
    color: var(--caf-error);
  }
  .caret {
    color: var(--caf-accent);
    animation: blink 1s steps(1) infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .caret {
      animation: none;
    }
  }
</style>
