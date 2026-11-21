<script lang="ts">
  // One turn: what started it, its life, its items, and if it failed, why.
  import type { DeepReadonly, Turn, UserMessage } from '@clackatoo/caf'
  import { attempt } from './actions.ts'
  import Item from './Item.svelte'
  import { clock, fullTime, isoTime, took } from './time.ts'
  import { errorText, failureText, lifeHint, lifeLabel, remedyText, shortId, tagOf } from './words.ts'

  let {
    turn,
    readOnly,
    act: acts,
    sendAgain,
    signIn,
    canSignIn,
    appSummary,
  }: {
    turn: DeepReadonly<Turn>
    readOnly: boolean
    /** Whether the host lets this viewer act: Send again sends. */
    act: boolean
    sendAgain: (message: DeepReadonly<UserMessage>) => Promise<unknown> | void
    /** The Sign in remedy: start signing in to the chat's agent. */
    signIn: () => Promise<unknown> | void
    /** Whether CAF can sign in to the chat's agent now, as its status says. */
    canSignIn: boolean
    appSummary: (app: unknown) => string
  } = $props()

  /** The message that started it, if a message did. */
  const message = $derived.by((): DeepReadonly<UserMessage> | null => {
    switch (turn.starter.type) {
      case 'User':
        return turn.starter.value
      case 'OtherChat':
        return turn.starter.value.message
      default:
        return null
    }
  })

  const time = $derived(took(turn.window))
  let sending = $state(false)
  let problem = $state<string | null>(null)

  async function act(action: () => Promise<unknown> | void) {
    if (readOnly || sending) return
    sending = true
    problem = null
    const error = await attempt(action)
    sending = false
    if (error !== null) problem = errorText(error)
  }

  function text(message: DeepReadonly<UserMessage>): string {
    return message.parts
      .map((part) => (part.type === 'Text' ? part.value : `[a part this viewer doesn't know: ${tagOf(part)}]`))
      .join('\n')
  }
</script>

<article class="turn" class:imported={turn.imported} data-life={turn.life.type} data-starter={turn.starter.type}>
  <header>
    {#if turn.starter.type === 'User'}
      <span class="starter">You</span>
    {:else if turn.starter.type === 'Agent'}
      <span class="starter agent">The agent, by itself</span>
    {:else if turn.starter.type === 'OtherChat'}
      <span class="starter other" title={turn.starter.value.from}>From chat {shortId(turn.starter.value.from)}</span>
    {:else if turn.starter.type === 'Notice'}
      <span class="starter notice">A notice from the app</span>
    {:else if turn.starter.type === 'Caf'}
      <span class="starter caf">CAF</span>
    {:else}
      <span class="starter">Started by something this viewer doesn't know ({tagOf(turn.starter)})</span>
    {/if}
    {#if turn.imported}<span class="history" title="Imported from the agent's own record: CAF didn't see it happen">History</span>{/if}
    <span class="life" title={lifeHint(turn.life)}>{lifeLabel(turn.life)}</span>
    {#if turn.model}
      <span class="model" title="The model that answered, as the agent said when it was sent"
        ><span class="hidden">{'Model: '}</span>{turn.model.name ?? turn.model.id}</span
      >
    {/if}
    {#if turn.window.start !== null}
      <time class="when" datetime={isoTime(turn.window.start)} title={fullTime(turn.window.start)}>
        {clock(turn.window.start)}
      </time>
    {/if}
    {#if time !== null}<span class="took">{time}</span>{/if}
  </header>

  {#if message !== null}
    <div class="message">{text(message)}</div>
  {:else if turn.starter.type === 'Notice'}
    <div class="message notice">{turn.starter.value}</div>
  {/if}

  {#if turn.items.length > 0}
    <div class="items">
      {#each turn.items as item (item.id)}
        <Item {item} {appSummary} />
      {/each}
    </div>
  {/if}

  {#if turn.life.type === 'Failed'}
    {@const failure = turn.life.value}
    <!-- Not an alert: a page of history would announce every old failure. -->
    <div class="failure">
      <div class="why"><span class="who">CAF</span>{failureText(failure.reason)}</div>
      {#if failure.agent_said !== null}
        <figure class="said">
          <figcaption>The agent said:</figcaption>
          <blockquote>{failure.agent_said}</blockquote>
        </figure>
      {/if}
      {#if failure.remedy !== null}
        {#if failure.remedy.type === 'SendAgain'}
          {#if message !== null}
            {#if acts}
              <button type="button" disabled={readOnly || sending} onclick={() => act(() => sendAgain(message))}>Send again</button>
            {/if}
          {/if}
        {:else if failure.remedy.type === 'SignIn'}
          <!-- Signing in doesn't send it again: once signed in, the user does. -->
          {#if !canSignIn}<div class="remedy">{remedyText(failure.remedy)}</div>{/if}
          <div class="remedies">
            {#if canSignIn}
              <button type="button" disabled={readOnly || sending} onclick={() => act(signIn)}>Sign in</button>
            {/if}
            {#if message !== null && acts}
              <button type="button" class="quiet" disabled={readOnly || sending} onclick={() => act(() => sendAgain(message))}>
                Send again
              </button>
            {/if}
          </div>
        {:else}
          <div class="remedy">{remedyText(failure.remedy)}</div>
        {/if}
      {/if}
      {#if problem !== null}<div class="problem">{problem}</div>{/if}
    </div>
  {/if}
</article>

<style>
  .turn {
    display: flex;
    flex-direction: column;
    gap: 0.5em;
    padding: 0.8em 0;
    border-bottom: 1px solid var(--caf-border);
  }
  /* History is drawn quieter, in muted text rather than see-through, which
     would fall below readable contrast. */
  .turn.imported {
    color: var(--caf-muted);
  }
  .turn.imported .message {
    background: var(--caf-inset);
  }
  header {
    display: flex;
    gap: 0.6em;
    align-items: baseline;
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  .starter {
    font-weight: 600;
    color: var(--caf-text);
  }
  .starter.agent {
    color: var(--caf-accent);
  }
  .starter.caf,
  .starter.notice {
    color: var(--caf-caf);
  }
  .history {
    border: 1px solid var(--caf-border);
    border-radius: 999px;
    padding: 0 0.5em;
    font-size: 0.9em;
  }
  .life {
    border-radius: 999px;
    padding: 0.05em 0.55em;
    background: var(--caf-inset);
    font-size: 0.9em;
  }
  [data-life='Waiting'] .life {
    color: var(--caf-muted);
    border: 1px dashed var(--caf-faint);
  }
  [data-life='Running'] .life,
  [data-life='Stopping'] .life {
    background: var(--caf-accent);
    color: var(--caf-accent-text);
  }
  [data-life='Finished'] .life {
    color: var(--caf-ok);
  }
  [data-life='Stopped'] .life,
  [data-life='CutOff'] .life {
    color: var(--caf-warn);
    background: var(--caf-warn-bg);
  }
  [data-life='Failed'] .life {
    color: var(--caf-error);
    background: var(--caf-error-bg);
  }
  .model {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Said to screen readers: the badge's place says it to the eye. */
  .hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .when,
  .took {
    margin-left: auto;
    font-variant-numeric: tabular-nums;
  }
  .when + .took {
    margin-left: 0;
  }
  .message {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    background: var(--caf-user);
    border-radius: 8px;
    padding: 0.55em 0.8em;
  }
  .message.notice {
    background: var(--caf-caf-bg);
  }
  .items {
    display: flex;
    flex-direction: column;
    gap: 0.5em;
  }
  .failure {
    border: 1px solid var(--caf-error);
    background: var(--caf-error-bg);
    border-radius: 6px;
    padding: 0.6em 0.8em;
    display: flex;
    flex-direction: column;
    gap: 0.5em;
    align-items: flex-start;
  }
  .who {
    color: var(--caf-caf);
    font-weight: 600;
    font-size: 0.85em;
    letter-spacing: 0.04em;
    margin-right: 0.6em;
  }
  .said {
    margin: 0;
    width: 100%;
  }
  figcaption {
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  blockquote {
    margin: 0.2em 0 0;
    padding: 0.3em 0.7em;
    border-left: 3px solid var(--caf-faint);
    color: var(--caf-muted);
    font-family: var(--caf-mono);
    font-size: 0.88em;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .remedy {
    font-size: 0.92em;
  }
  .problem {
    color: var(--caf-error);
    font-size: 0.9em;
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
  .remedies {
    display: flex;
    gap: 0.5em;
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
</style>
