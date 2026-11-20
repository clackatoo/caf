<script lang="ts">
  // Every step: its number, time, what happened, and how many directives
  // the core gave out. Choosing one looks at it (a seek). Below, the step
  // looked at: its event, its directives, and the agent traffic in it as
  // JSON both ways.
  import type { Inspector } from './client.svelte.ts'
  import JsonTree from './JsonTree.svelte'
  import { traffic } from './decode.ts'
  import { directiveSummary, eventSummary, frameSummary, worstReport } from './summary.ts'

  let { inspector }: { inspector: Inspector } = $props()

  const ROW = 24
  let table = $state<HTMLElement>()
  let top = $state(0)
  let height = $state(300)

  const count = $derived(inspector.count)
  const at = $derived(inspector.position.at)
  const first = $derived(Math.max(1, Math.floor(top / ROW) + 1 - 5))
  const last = $derived(Math.min(count, Math.ceil((top + height) / ROW) + 5))
  const rows = $derived(Array.from({ length: Math.max(0, last - first + 1) }, (_, i) => first + i))
  const step = $derived(at >= 1 ? inspector.step(at) : undefined)
  const moves = $derived(step === undefined ? [] : traffic(step.event, step.directives))

  // Keep the step looked at in view as it moves, such as while following.
  $effect(() => {
    if (table === undefined || at < 1) return
    const y = (at - 1) * ROW
    if (y < table.scrollTop || y + ROW > table.scrollTop + table.clientHeight) {
      table.scrollTop = Math.max(0, y - table.clientHeight / 2)
    }
  })

  function time(wall: number): string {
    const date = new Date(wall)
    return `${date.toLocaleTimeString(undefined, { hour12: false })}.${String(date.getMilliseconds()).padStart(3, '0')}`
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === 'ArrowDown' || event.key === 'j') inspector.seek(at + 1)
    else if (event.key === 'ArrowUp' || event.key === 'k') inspector.seek(at - 1)
    else return
    event.preventDefault()
  }
</script>

<div class="steps">
  <div class="head" role="row">
    <span class="no">#</span><span class="time">Time</span><span class="what">Event</span><span class="count" title="Directives">Dir.</span>
  </div>
  <div
    class="table"
    role="grid"
    aria-label="Steps"
    tabindex="0"
    bind:this={table}
    bind:clientHeight={height}
    onscroll={() => (top = table?.scrollTop ?? 0)}
    {onkeydown}
  >
    <div class="spacer" style:height="{count * ROW}px">
      {#each rows as no (no)}
        {@const row = inspector.step(no)}
        {#if row !== undefined}
          {@const report = worstReport(row.directives)}
          <button
            type="button"
            class="row"
            class:selected={no === at}
            class:diverged={inspector.diverged?.at === no}
            data-report={report}
            style:top="{(no - 1) * ROW}px"
            onclick={() => inspector.seek(no)}
          >
            <span class="no">{no}</span>
            <span class="time">{time(row.now.wall)}</span>
            <span class="what">{eventSummary(row.event)}</span>
            <span class="count">{row.directives.length}</span>
          </button>
        {/if}
      {/each}
    </div>
    {#if count === 0}<p class="empty">No steps yet.</p>{/if}
  </div>

  <div class="detail">
    {#if step === undefined}
      <p class="empty">{at === 0 ? 'Before the first step: the core is fresh.' : 'This step hasn’t come yet.'}</p>
    {:else}
      <h3>Step {step.no} <span class="sub">at {time(step.now.wall)}, steady clock {String(step.now.mono)} ns</span></h3>
      <section>
        <h4>Event</h4>
        <div class="summary">{eventSummary(step.event)}</div>
        <JsonTree value={step.event} openTo={2} />
      </section>
      <section>
        <h4>Directives ({step.directives.length})</h4>
        {#each step.directives as directive, i (i)}
          <div class="directive" data-type={directive.type} data-level={directive.type === 'Report' ? directive.value.level.type : null}>
            <div class="summary">{directiveSummary(directive)}</div>
            <JsonTree value={directive} openTo={0} />
          </div>
        {:else}
          <p class="none">None.</p>
        {/each}
      </section>
      {#if moves.length > 0}
        <section>
          <h4>Agent traffic</h4>
          {#each moves as move, i (i)}
            {#if move.from === 'agent'}
              <div class="traffic from-agent">
                <div class="summary">Agent {move.process} said: {frameSummary(move.frame)}</div>
                <JsonTree value={move.frame.type === 'Message' ? move.frame.value : move.frame} openTo={3} />
              </div>
            {:else}
              <div class="traffic from-caf">
                <div class="summary">CAF wrote to agent {move.process}:</div>
                {#each move.lines as line, j (j)}
                  {#if 'json' in line}
                    <JsonTree value={line.json} openTo={3} />
                  {:else}
                    <pre>{line.text}</pre>
                  {/if}
                {/each}
              </div>
            {/if}
          {/each}
        </section>
      {/if}
    {/if}
  </div>
</div>

<style>
  .steps {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .head,
  .row {
    display: grid;
    grid-template-columns: 4.5em 7.5em 1fr 3em;
    gap: 0.5em;
    align-items: center;
    padding: 0 0.6em;
    font-size: 12px;
  }
  .head {
    color: var(--caf-muted);
    border-bottom: 1px solid var(--caf-border);
    padding-top: 0.3em;
    padding-bottom: 0.3em;
  }
  .table {
    position: relative;
    flex: 0 0 40%;
    min-height: 6em;
    overflow-y: auto;
    border-bottom: 1px solid var(--caf-border);
  }
  .table:focus-visible {
    outline: 2px solid var(--caf-accent);
    outline-offset: -2px;
  }
  .spacer {
    position: relative;
  }
  .row {
    position: absolute;
    left: 0;
    right: 0;
    height: 24px;
    border: none;
    background: none;
    color: var(--caf-text);
    text-align: left;
    cursor: pointer;
    font-family: inherit;
  }
  .row:hover {
    background: var(--caf-hover);
  }
  .row.selected {
    background: var(--caf-user);
    box-shadow: inset 3px 0 0 var(--caf-accent);
  }
  .row[data-report='Warn'] .what {
    color: var(--caf-warn);
  }
  .row.diverged {
    background: var(--caf-error-bg);
  }
  .no,
  .time,
  .count {
    font-variant-numeric: tabular-nums;
    color: var(--caf-muted);
  }
  .count {
    text-align: right;
  }
  .what {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--caf-mono);
  }
  .detail {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 0.6em 0.8em;
  }
  h3 {
    margin: 0 0 0.5em;
    font-size: 1em;
  }
  .sub {
    color: var(--caf-muted);
    font-weight: normal;
    font-size: 0.85em;
  }
  h4 {
    margin: 0.8em 0 0.3em;
    font-size: 0.85em;
    color: var(--caf-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .summary {
    font-family: var(--caf-mono);
    font-size: 12px;
    margin-bottom: 0.2em;
    overflow-wrap: anywhere;
  }
  .directive,
  .traffic {
    border: 1px solid var(--caf-border);
    border-radius: 6px;
    padding: 0.35em 0.5em;
    margin-bottom: 0.4em;
  }
  .directive[data-level='Warn'] {
    border-color: var(--caf-warn);
    background: var(--caf-warn-bg);
  }
  .from-agent {
    border-left: 3px solid var(--caf-accent);
  }
  .from-caf {
    border-left: 3px solid var(--caf-caf);
  }
  pre {
    margin: 0;
    font-family: var(--caf-mono);
    font-size: 12px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .empty,
  .none {
    color: var(--caf-faint);
    padding: 0.5em;
    margin: 0;
  }
</style>
