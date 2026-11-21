<script lang="ts">
  // One chat, whole: its header, its state, its turns, the questions
  // waiting, and the composer.
  //
  // A pure renderer: it draws the chat it's given and calls `actions`
  // when the user does something. It keeps nothing of the chat's itself,
  // so the same view draws a live chat or one played back from a
  // recording. With `readOnly` (such as while looking back in time), it
  // draws the chat as it was, and nothing can be done in it.
  import type { AgentStatus, Chat, DeepReadonly } from '@clackatoo/caf'
  import { untrack } from 'svelte'
  import { attempt, type ChatActions } from './actions.ts'
  import AgentBanner from './AgentBanner.svelte'
  import Composer from './Composer.svelte'
  import Questions from './Questions.svelte'
  import StateBanner from './StateBanner.svelte'
  import Turn from './Turn.svelte'
  import Usage from './Usage.svelte'
  import { errorText, stateLabel } from './words.ts'
  import './theme.css'

  let {
    chat,
    agent,
    actions,
    readOnly = false,
    readOnlyWhy = 'As it was then: nothing can be done here.',
    appSummary = () => "An item of the app's own",
  }: {
    chat: DeepReadonly<Chat>
    /**
     * The status of the chat's agent, from `Connection.agents`: whether
     * it's installed and signed in. Without it, a chat waiting for its
     * agent says only that it waits.
     */
    agent?: DeepReadonly<AgentStatus> | undefined
    actions: ChatActions
    readOnly?: boolean
    /** Why nothing can be done, said while `readOnly`. */
    readOnlyWhy?: string
    /** The one-line summary of one of the app's own items, which only the app can write. */
    appSummary?: (app: unknown) => string
  } = $props()

  /** The status of the chat's agent, if the host gave it, and it's this chat's agent's. */
  const status = $derived(agent?.name === chat.agent ? agent : undefined)
  const running = $derived(chat.turns.find((turn) => turn.life.type === 'Running' || turn.life.type === 'Stopping'))
  const closed = $derived(chat.state.type === 'Closed')
  let problem = $state<string | null>(null)
  // Follow the end of the chat as it grows, unless the user has scrolled up.
  let scroller = $state<HTMLElement>()
  let following = true

  // Another chat: what this view said and where it was scrolled were the
  // last chat's. (The rest is inside the {#key} below, drawn afresh.)
  let shownId = untrack(() => chat.id)
  $effect.pre(() => {
    if (chat.id === shownId) return
    shownId = chat.id
    problem = null
    following = true
  })

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

<!-- Drawn afresh for another chat, so nothing typed or shown for one (a
     draft, why something failed) carries over to the next. -->
{#key chat.id}
<section class="caf-chat" aria-label="Chat {chat.id}">
  <header>
    <div class="who">
      <span class="agent">{chat.agent}</span>
      <!-- Cut from the left, so the end of the path shows. -->
      <span class="cwd" title={chat.cwd}><bdi>{chat.cwd}</bdi></span>
    </div>
    {#if chat.usage !== null}<Usage usage={chat.usage} />{/if}
    <span class="state" data-state={chat.state.type}>{stateLabel(chat.state)}</span>
    {#if !closed}
      <button type="button" class="close" disabled={readOnly} onclick={close}>Close</button>
    {/if}
  </header>
  {#if problem !== null}<div class="problem" role="alert">{problem}</div>{/if}
  {#if readOnly}<div class="looking-back" role="status">{readOnlyWhy}</div>{/if}
  <StateBanner state={chat.state} />
  {#if !closed}
    <AgentBanner
      name={chat.agent}
      agent={status}
      waiting={chat.state.type === 'WaitingForAgent'}
      {readOnly}
      install={actions.install}
      stopInstall={actions.stopInstall}
      notNow={actions.stop}
      paste={actions.paste}
      stopSignIn={actions.stopSignIn}
    />
  {/if}

  <div class="turns" bind:this={scroller} {onscroll}>
    {#each chat.turns as turn (turn.id)}
      <Turn {turn} {readOnly} sendAgain={actions.sendAgain} signIn={actions.signIn}
        canSignIn={status?.can_sign_in ?? false}
        {appSummary}
      />
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
{/key}

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
