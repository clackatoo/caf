<script lang="ts">
  // An agent's account limits, in the Agents panel: one line with its
  // lowest window, how low it is at a glance, and, opened, each window
  // with a bar of what's left, where an even pace would have it, when it
  // resets, and when CAF heard. Nothing for an agent that hasn't said.
  import { label, left, lowest, lowness, pace, type AccountLimits, type DeepReadonly, type Wall } from '@clackatoo/caf'
  import { asOfText, limitStateText, meterText, resetsText } from './limits.ts'

  let {
    name,
    limits,
    now,
  }: {
    /** The agent's, for a screen reader. */
    name: string
    limits: DeepReadonly<AccountLimits>
    /** The time to measure resets and pace from. */
    now: Wall
  } = $props()

  const least = $derived(lowest(limits))
  const extra = $derived(limits.extra_usage?.in_use === true)
</script>

{#if least !== null || extra}
  <details class="limits" data-lowness={least === null ? 'fine' : lowness(least)}>
    <!-- Named by what it says: whose they are to a screen reader, and how
         low in words as well as the dot's colour. -->
    <summary>
      <span class="hidden">{`${name}'s limits: `}</span>
      {#if least !== null}
        {@const low = lowness(least)}
        <span class="dot" aria-hidden="true"></span>
        <span class="glance">{label(least.kind)}: {left(least)}% left{#if low !== 'fine'}<span class="low">, {low}</span>{/if}</span>
      {/if}
      {#if extra}<span class="extra">Extra usage in use</span>{/if}
    </summary>
    {#each limits.windows as window (JSON.stringify(window.kind))}
      {@const called = label(window.kind)}
      {@const remaining = left(window)}
      {@const even = pace(window, now)}
      <div class="window" data-window={called} data-lowness={lowness(window)} data-state={window.state.type}>
        <div class="line">
          <span>{called}</span>
          <span>{remaining}% left</span>
        </div>
        <div
          class="bar"
          role="meter"
          aria-label="{called} left"
          aria-valuemin="0"
          aria-valuemax="100"
          aria-valuenow={remaining}
          aria-valuetext={meterText(remaining, even)}
        >
          <div class="fill" style:width="{remaining}%"></div>
          {#if even !== null}
            <div class="pace" style:left="{100 - even}%" data-left={100 - even} title="At an even pace, {100 - even}% would be left"></div>
          {/if}
        </div>
        <div class="detail">
          {limitStateText(window.state)}{#if window.resets !== null}, {resetsText(window.resets, now)}{/if}, {asOfText(window.heard, now)}
        </div>
      </div>
    {/each}
  </details>
{/if}

<style>
  .limits {
    font-size: 0.92em;
  }
  summary {
    cursor: pointer;
    color: var(--caf-muted);
  }
  .dot {
    display: inline-block;
    width: 0.6em;
    height: 0.6em;
    border-radius: 50%;
    margin-right: 0.2em;
    background: var(--caf-ok);
  }
  /* Said to screen readers: the panel's row says whose to the eye. */
  .hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .limits[data-lowness='low'] .low {
    color: var(--caf-warn);
  }
  .limits[data-lowness='out'] .low {
    color: var(--caf-error);
  }
  .extra {
    margin-left: 0.4em;
    color: var(--caf-warn);
  }
  .limits[data-lowness='low'] .dot,
  .window[data-lowness='low'] .fill {
    background: var(--caf-warn);
  }
  .limits[data-lowness='out'] .dot,
  .window[data-lowness='out'] .fill {
    background: var(--caf-error);
  }
  .window {
    display: flex;
    flex-direction: column;
    gap: 0.15em;
    margin-top: 0.4em;
  }
  .line {
    display: flex;
    justify-content: space-between;
  }
  .bar {
    position: relative;
    height: 0.45em;
    border-radius: 3px;
    background: var(--caf-inset);
    border: 1px solid var(--caf-border);
  }
  .fill {
    height: 100%;
    border-radius: 2px;
    background: var(--caf-ok);
  }
  .pace {
    position: absolute;
    top: -0.2em;
    bottom: -0.2em;
    width: 2px;
    margin-left: -1px;
    background: var(--caf-text);
  }
  .detail {
    color: var(--caf-muted);
  }
</style>
