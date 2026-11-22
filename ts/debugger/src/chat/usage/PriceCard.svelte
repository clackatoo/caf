<script lang="ts">
  // One model's price, as the summary used it, and where it came from;
  // and, if the viewer may, a price of the user's own for it, or the
  // table's again. A price set is every CAF app's on the machine.
  import type { ModelPrice, PricedModel, Wall } from '@clackatoo/caf'
  import { CafError } from '@clackatoo/caf'
  import { attempt } from '../actions.ts'
  import { rate } from '../figures.ts'
  import { dayAndTime } from '../time.ts'
  import { errorText, tagOf } from '../words.ts'
  import { KINDS } from './page.ts'

  let {
    model,
    name = model,
    priced,
    fetched,
    setPrice,
  }: {
    /** The model, by the agent's id for it. */
    model: string
    /** Its name to show, from the summary's `names`; its id, if it has none. */
    name?: string
    /** Its price, if the summary had one. */
    priced: PricedModel | null
    /** When LiteLLM's table was last fetched, if ever. */
    fetched: Wall | null
    /** Sets the model's price, or with null clears the user's own; absent if the viewer may not. */
    setPrice?: ((model: string, price: ModelPrice | null) => Promise<unknown>) | undefined
  } = $props()

  let editing = $state(false)
  let busy = $state(false)
  let problem = $state<string | null>(null)
  /** The form's rates, as typed: null (or undefined) where a box is empty. */
  type Draft = Record<'input' | 'cache_read' | 'cache_write' | 'output' | 'fast', number | null | undefined>
  let draft = $state<Draft>({ input: null, cache_read: null, cache_write: null, output: null, fast: null })

  const source = $derived.by(() => {
    if (priced === null) return null
    switch (priced.layer.type) {
      case 'Own':
        return 'Your own price'
      case 'Fetched':
        return fetched === null ? "From LiteLLM's table" : `From LiteLLM's table, fetched ${dayAndTime(fetched)}`
      case 'Bundled':
        return "From the table that came with CAF"
      default:
        return `From ${tagOf(priced.layer)}`
    }
  })

  function edit() {
    const now = priced?.price
    draft = now === undefined ? { input: null, cache_read: null, cache_write: null, output: null, fast: null } : { ...now }
    problem = null
    editing = true
  }

  /** The draft as a price, or why it isn't one. */
  function read(): ModelPrice | string {
    const rates = KINDS.map(({ key }) => draft[key])
    if (rates.some((each) => typeof each !== 'number' || !Number.isFinite(each) || each < 0)) {
      return 'Give every rate, in US dollars per million tokens, as a number of 0 or more.'
    }
    const fast = draft.fast ?? null
    if (fast !== null && (!Number.isFinite(fast) || fast <= 0)) return "Fast mode's multiplier is a number above 0, or left empty."
    const [input, cache_read, cache_write, output] = rates as number[]
    return { input: input!, cache_read: cache_read!, cache_write: cache_write!, output: output!, fast }
  }

  /**
   * The price to set or clear: the user's own entry that priced it, if
   * that's what did, as it may be named otherwise than the agent's id;
   * else a new one, by the agent's id.
   */
  const target = $derived(priced?.layer.type === 'Own' ? priced.model : model)

  async function save(price: ModelPrice | null) {
    if (setPrice === undefined || busy) return
    busy = true
    problem = null
    const error = await attempt(() => setPrice(target, price))
    busy = false
    if (error instanceof CafError && error.why.type === 'NoLedger') {
      problem = 'The app keeps no record of usage, so prices of your own can’t be set here.'
    } else if (error !== null) problem = errorText(error)
    else editing = false
  }

  function submit(event: SubmitEvent) {
    event.preventDefault()
    const price = read()
    if (typeof price === 'string') problem = price
    else void save(price)
  }
</script>

<!-- A model is shown by its name, if it has one, then by the id its
     price goes by. -->
{#snippet named(id: string)}
  {#if name !== model}{name} (<code>{id}</code>){:else}<code>{id}</code>{/if}
{/snippet}

<div class="price" role="group" aria-label="The price of {name}">
  {#if priced === null}
    <p>CAF knows no price for {@render named(model)}, so turns on it that didn't report a cost aren't counted in the cost.</p>
  {:else}
    <p>
      {source}{#if priced.model !== model}, as <code>{priced.model}</code>{/if}. In US dollars per million tokens:
    </p>
    <dl>
      {#each KINDS as kind (kind.key)}
        <div><dt>{kind.label}</dt><dd>{rate(priced.price[kind.key])}</dd></div>
      {/each}
      {#if priced.price.fast !== null}<div><dt>Fast mode</dt><dd>×{priced.price.fast}</dd></div>{/if}
    </dl>
  {/if}

  {#if setPrice !== undefined}
    {#if editing}
      <form onsubmit={submit}>
        <p class="hint">Your own price for {@render named(target)}, in US dollars per million tokens. Every CAF app on this computer uses it.</p>
        <div class="fields">
          {#each KINDS as kind (kind.key)}
            <label>{kind.label}<input type="number" min="0" step="any" bind:value={draft[kind.key]} /></label>
          {/each}
          <label>Fast mode ×<input type="number" min="0" step="any" placeholder="None" bind:value={draft.fast} /></label>
        </div>
        <div class="buttons">
          <button type="button" onclick={() => (editing = false)} disabled={busy}>Cancel</button>
          <button type="submit" class="primary" disabled={busy}>Save</button>
        </div>
      </form>
    {:else}
      <div class="buttons">
        {#if priced?.layer.type === 'Own'}
          <button type="button" onclick={() => save(null)} disabled={busy}>Use the table's price</button>
        {/if}
        <button type="button" onclick={edit} disabled={busy}>{priced?.layer.type === 'Own' ? 'Change your price' : 'Set your own price'}</button>
      </div>
    {/if}
  {/if}
  {#if problem !== null}<p class="problem" role="alert">{problem}</p>{/if}
</div>

<style>
  .price {
    padding: 0.6em 0.4em 0.8em;
    font-size: 0.95em;
    white-space: normal;
    text-align: left;
  }
  p {
    margin: 0 0 0.5em;
  }
  code {
    font-family: var(--caf-mono);
    font-size: 0.9em;
  }
  dl {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em 1.6em;
    margin: 0 0 0.6em;
  }
  dt {
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  dd {
    margin: 0;
    font-variant-numeric: tabular-nums;
  }
  .hint {
    color: var(--caf-muted);
  }
  .fields {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5em 1em;
    margin-bottom: 0.6em;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.2em;
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  input {
    width: 7em;
    font: inherit;
    font-size: 1.1em;
    padding: 0.25em 0.4em;
    border-radius: 6px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
  }
  .buttons {
    display: flex;
    gap: 0.5em;
  }
  button {
    font: inherit;
    font-size: 0.9em;
    padding: 0.25em 0.8em;
    border-radius: 6px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
    cursor: pointer;
  }
  .primary {
    background: var(--caf-accent);
    border-color: var(--caf-accent);
    color: var(--caf-accent-text);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .problem {
    color: var(--caf-error);
    margin-top: 0.5em;
  }
</style>
