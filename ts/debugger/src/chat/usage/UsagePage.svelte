<script lang="ts">
  // The usage page, after CAF SI's: what every CAF app on this computer
  // has used and spent over a period, by agent, app, model and day. Cost
  // is what agents reported, and an estimate at API prices where they
  // didn't; models with no price are marked.
  //
  // The app gives it `load`, such as `Connection.usage`, and `setPrice`
  // if the viewer may set prices. Give it only to a viewer that `may.usage`.
  import type { ModelPrice, Summary, UsageQuery, Wall } from '@clackatoo/caf'
  import { group } from '@clackatoo/caf'
  import { untrack } from 'svelte'
  import { errorText } from '../words.ts'
  import Breakdown from './Breakdown.svelte'
  import Overview from './Overview.svelte'
  import Switch from './Switch.svelte'
  import Totals from './Totals.svelte'
  import { colour, ownZone, PERIODS, queryFor, type Colours, type Metric, type Period } from './page.ts'

  let {
    load,
    setPrice,
    close,
    zone = ownZone(),
    now = Date.now,
  }: {
    /** Asks CAF for a summary. */
    load: (query: UsageQuery) => Promise<Summary>
    /** Sets a model's price, or clears the user's own; absent if the viewer may not. */
    setPrice?: ((model: string, price: ModelPrice | null) => Promise<unknown>) | undefined
    /** Closes the page, if the app shows it over something. */
    close?: (() => void) | undefined
    /** The time zone days begin in: the user's own by default. */
    zone?: string
    /** The time now, which periods end at. */
    now?: () => Wall
  } = $props()

  let period = $state<Period>('7d')
  let metric = $state<Metric>('cost')
  let by = $state<'agent' | 'app'>('agent')
  let summary = $state.raw<Summary | null>(null)
  let loading = $state(false)
  let problem = $state<string | null>(null)
  /** The last request: an answer to any earlier one is dropped. */
  let asked = 0

  async function refresh() {
    const mine = ++asked
    loading = true
    problem = null
    try {
      const got = await load(queryFor(period, now(), zone))
      if (mine === asked) summary = got
    } catch (error) {
      if (mine === asked) problem = errorText(error)
    } finally {
      if (mine === asked) loading = false
    }
  }

  // Asked again whenever the period changes, and once to begin with.
  $effect(() => {
    void period
    untrack(() => void refresh())
  })

  /** Each agent's and app's colour, by its place among the period's by cost. */
  const colours = $derived.by((): Colours => {
    const of = (key: 'agent' | 'app') => new Map(summary === null ? [] : group(summary, key).map((each, n) => [each.key, colour(n)]))
    return { agent: of('agent'), app: of('app') }
  })

  async function priced(model: string, price: ModelPrice | null) {
    await setPrice?.(model, price)
    // Costs are estimated with the new price: ask again.
    await refresh()
  }

  const ofPeriod = $derived(PERIODS.find((each) => each.value === period)!.label)
</script>

<section class="usage-page" aria-label="Usage">
  <header>
    <h2>Usage</h2>
    <span class="spacer"></span>
    <Switch options={PERIODS} bind:value={period} label="Period" />
    <Switch options={[{ value: 'cost', label: 'Cost' }, { value: 'tokens', label: 'Tokens' }]} bind:value={metric} label="Measure" />
    <button type="button" class="icon" title="Refresh" aria-label="Refresh" disabled={loading} onclick={refresh}>
      <span class:spin={loading} aria-hidden="true">↻</span>
    </button>
    {#if close !== undefined}
      <button type="button" class="icon" title="Close" aria-label="Close usage" onclick={close}><span aria-hidden="true">✕</span></button>
    {/if}
  </header>
  <div class="body">
    {#if problem !== null}
      <p class="problem" role="alert">Usage couldn't be read: {problem}</p>
    {/if}
    {#if loading}
      <p class="notice" role="status">
        {summary === null ? 'Reading usage…' : 'Reading again… the figures may still change.'}
      </p>
    {/if}
    {#if summary !== null}
      {#if summary.unreadable.length > 0}
        <p class="partial" role="alert">
          Some of the usage record couldn't be read, so these figures are short:
          {#each summary.unreadable as file, n (file)}<code>{file}</code>{n < summary.unreadable.length - 1 ? ', ' : '.'}{/each}
        </p>
      {/if}
      {#if summary.buckets.length === 0}
        <p class="notice">No chats in this period ({ofPeriod.toLowerCase()}).</p>
      {/if}
      <Overview {summary} {metric} bind:by {colours} />
      {#if summary.buckets.length > 0}
        <Totals {summary} />
        <Breakdown {summary} {metric} {colours} setPrice={setPrice === undefined ? undefined : priced} />
      {/if}
    {/if}
  </div>
</section>

<style>
  .usage-page {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--caf-bg);
    color: var(--caf-text);
    font-family: var(--caf-sans);
  }
  header {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.6em;
    padding: 0.6em 0.8em 0.6em 1.3em;
    border-bottom: 1px solid var(--caf-border);
  }
  h2 {
    font-size: 1.05em;
    margin: 0;
  }
  .spacer {
    flex: 1;
  }
  .icon {
    width: 2em;
    height: 2em;
    display: grid;
    place-items: center;
    font: inherit;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--caf-muted);
    cursor: pointer;
  }
  .icon:hover {
    background: var(--caf-hover);
  }
  .icon:disabled {
    cursor: default;
  }
  .spin {
    display: inline-block;
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 1.3em 1.6em 2em;
  }
  .notice {
    color: var(--caf-muted);
    font-size: 0.9em;
    margin: 0 0 1em;
  }
  .problem {
    color: var(--caf-error);
    margin: 0 0 1em;
  }
  .partial {
    color: var(--caf-warn);
    background: var(--caf-warn-bg);
    border-radius: 6px;
    padding: 0.4em 0.7em;
    margin: 0 0 1em;
    font-size: 0.9em;
  }
  code {
    font-family: var(--caf-mono);
    font-size: 0.9em;
  }
</style>
