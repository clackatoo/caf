<script lang="ts">
  // An agent's sign-in, under way: the link to open, the code to type, a
  // box to paste a code back into if the agent takes one, and Stop.
  //
  // The link opens in a new tab, cut off from this page
  // (`noopener noreferrer`), and only if it's https: CAF has already
  // checked its site against the agent's profile, and this is the view's
  // own check.
  import type { DeepReadonly, SignInLink } from '@clackatoo/caf'
  import { attempt } from './actions.ts'
  import { errorText, signInUrl } from './words.ts'

  let {
    name,
    link,
    readOnly,
    paste,
    stop,
  }: {
    /** The agent's name. */
    name: string
    link: DeepReadonly<SignInLink>
    readOnly: boolean
    paste: (text: string) => Promise<unknown> | void
    stop: () => Promise<unknown> | void
  } = $props()

  const url = $derived(signInUrl(link.url))

  let busy = $state(false)
  let problem = $state<string | null>(null)
  let code = $state('')

  async function run(action: () => Promise<unknown> | void): Promise<boolean> {
    if (readOnly || busy) return false
    busy = true
    problem = null
    const error = await attempt(action)
    busy = false
    if (error !== null) problem = errorText(error)
    return error === null
  }

  async function onsubmit(event: SubmitEvent) {
    event.preventDefault()
    const text = code.trim()
    if (text === '') return
    if (await run(() => paste(text))) code = ''
  }
</script>

<div class="sign-in">
  <div>Signing in to <strong>{name}</strong>.</div>
  {#if link.url === null}
    <div class="step">Waiting for the sign-in page's address…</div>
  {:else if url !== null}
    <div class="step">
      Open <a href={url.href} target="_blank" rel="noopener noreferrer">the sign-in page on {url.host}</a>, and approve it
      there.
    </div>
  {:else}
    <!-- Never a link: only an https address is opened. -->
    <div class="step">
      The sign-in page's address isn't one this viewer opens (it must start with https://):
      <code>{link.url}</code>
    </div>
  {/if}
  {#if link.code !== null}
    <div class="step">When it asks for a code, type <code class="code">{link.code}</code></div>
  {/if}
  {#if link.paste}
    <form class="paste" {onsubmit}>
      <label>
        If the page gives you a code instead, paste it here:
        <input type="text" autocomplete="off" spellcheck="false" bind:value={code} disabled={readOnly || busy} />
      </label>
      <button type="submit" disabled={readOnly || busy || code.trim() === ''}>Send the code</button>
    </form>
  {/if}
  <div>
    <button type="button" class="quiet" disabled={readOnly || busy} onclick={() => run(stop)}>Stop signing in</button>
  </div>
  {#if problem !== null}<div class="problem" role="alert">{problem}</div>{/if}
</div>

<style>
  .sign-in {
    display: flex;
    flex-direction: column;
    gap: 0.45em;
  }
  .step {
    overflow-wrap: anywhere;
  }
  a {
    color: var(--caf-accent);
    font-weight: 600;
  }
  code {
    font-family: var(--caf-mono);
    background: var(--caf-inset);
    border-radius: 4px;
    padding: 0.05em 0.35em;
  }
  .code {
    font-size: 1.1em;
    letter-spacing: 0.05em;
    user-select: all;
  }
  .paste {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5em;
    align-items: flex-end;
  }
  .paste label {
    display: flex;
    flex-direction: column;
    gap: 0.25em;
    flex: 1;
    min-width: 14em;
  }
  input {
    font: inherit;
    font-family: var(--caf-mono);
    padding: 0.3em 0.5em;
    border-radius: 6px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
  }
  button {
    font: inherit;
    font-size: 0.92em;
    padding: 0.3em 0.9em;
    border-radius: 6px;
    border: 1px solid var(--caf-accent);
    background: var(--caf-accent);
    color: var(--caf-accent-text);
    cursor: pointer;
  }
  button.quiet {
    background: var(--caf-bg);
    color: var(--caf-text);
    border-color: var(--caf-border);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .problem {
    color: var(--caf-error);
  }
</style>
