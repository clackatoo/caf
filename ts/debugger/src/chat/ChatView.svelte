<script lang="ts">
  // One chat, whole: its header, its state, its turns, the questions
  // waiting, and the composer.
  //
  // A pure renderer: it draws the chat it's given and calls `actions`
  // when the user does something. It keeps nothing of the chat's itself,
  // so the same view draws a live chat or one played back from a
  // recording. With `readOnly` (such as while looking back in time), it
  // draws the chat as it was, and nothing can be done in it.
  import type { Chat, DeepReadonly } from '@clackatoo/caf'
  import { attempt, type ChatActions } from './actions.ts'
  import Composer from './Composer.svelte'
  import Questions from './Questions.svelte'
  import StateBanner from './StateBanner.svelte'
  import Turn from './Turn.svelte'
  import Usage from './Usage.svelte'
  import { errorText, stateLabel } from './words.ts'
  import './theme.css'

  let {
    chat,
    actions,
    readOnly = false,
    appSummary = () => "An item of the app's own",
  }: {
    chat: DeepReadonly<Chat>
    actions: ChatActions
    readOnly?: boolean
    /** The one-line summary of one of the app's own items, which only the app can write. */
    appSummary?: (app: unknown) => string
  } = $props()

  const running = $derived(chat.turns.find((turn) => turn.life.type === 'Running' || turn.life.type === 'Stopping'))
  const closed = $derived(chat.state.type === 'Closed')
  let problem = $state<string | null>(null)

  // Follow the end of the chat as it grows, unless the user has scrolled up.
  let scroller = $state<HTMLElement>()
  let following = true
  function onscroll() {
    if (scroller === undefined) return
    following = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 48
  }
  $effect(() => {
    void chat
    if (following && scroller !== undefined) scroller.scrollTop = scroller.scrollHeight
  })

  async function close() {
    if (readOnly) return
    problem = null
    const error = await attempt(actions.close)
    if (error !== null) problem = errorText(error)
  }
</script>

<section class="caf-chat" aria-label="Chat {chat.id}">
  <header>
    <div class="who">
      <span class="agent">{chat.agent}</span>
      <span class="cwd" title={chat.cwd}>{chat.cwd}</span>
    </div>
    {#if chat.usage !== null}<Usage usage={chat.usage} />{/if}
    <span class="state" data-state={chat.state.type}>{stateLabel(chat.state)}</span>
    {#if !closed}
      <button type="button" class="close" disabled={readOnly} onclick={close}>Close</button>
    {/if}
  </header>
  {#if problem !== null}<div class="problem" role="alert">{problem}</div>{/if}
  {#if readOnly}<div class="looking-back" role="status">As it was then: nothing can be done here.</div>{/if}
  <StateBanner state={chat.state} />

  <div class="turns" bind:this={scroller} {onscroll}>
    {#each chat.turns as turn (turn.id)}
      <Turn {turn} {readOnly} sendAgain={actions.sendAgain} {appSummary} />
    {:else}
      <p class="empty">Nothing said yet.</p>
    {/each}
  </div>

  <Questions questions={chat.questions} {readOnly} answer={actions.answer} />
  <Composer
    running={running !== undefined}
    stopping={running?.life.type === 'Stopping'}
    disabled={readOnly || closed}
    send={(text) => actions.send({ parts: [{ type: 'Text', value: text }] })}
    stop={actions.stop}
  />
</section>

<style>
  .caf-chat {
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
    gap: 0.8em;
    padding: 0.55em 0.9em;
    border-bottom: 1px solid var(--caf-border);
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    gap: 0.6em;
    align-items: baseline;
  }
  .agent {
    font-weight: 600;
  }
  .cwd {
    color: var(--caf-muted);
    font-family: var(--caf-mono);
    font-size: 0.85em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .state {
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  .state[data-state='Open'] {
    color: var(--caf-ok);
  }
  .state[data-state='Lost'] {
    color: var(--caf-warn);
  }
  .close {
    font: inherit;
    font-size: 0.85em;
    padding: 0.2em 0.7em;
    border-radius: 6px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
    cursor: pointer;
  }
  .close:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .looking-back {
    padding: 0.4em 0.9em;
    font-size: 0.85em;
    background: var(--caf-inset);
    color: var(--caf-muted);
    border-bottom: 1px solid var(--caf-border);
  }
  .problem {
    padding: 0.4em 0.9em;
    font-size: 0.9em;
    color: var(--caf-error);
  }
  .turns {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 0.9em;
  }
  .empty {
    color: var(--caf-faint);
    text-align: center;
    margin-top: 3em;
  }
</style>
