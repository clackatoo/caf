<script lang="ts">
  // One chat, whole: its header, its state, its turns, the questions
  // waiting, and the composer.
  //
  // A pure renderer: it draws the chat it's given and calls `actions`
  // when the user does something. It keeps nothing of the chat's itself,
  // so the same view draws a live chat or one played back from a
  // recording. With `readOnly` (such as while looking back in time), it
  // draws the chat as it was, and nothing can be done in it.
  import type { AgentStatus, Chat, DeepReadonly, Rights } from '@clackatoo/caf'
  import { untrack } from 'svelte'
  import { attempt, type ChatActions } from './actions.ts'
  import AgentBanner from './AgentBanner.svelte'
  import { showsAgent } from './shows.ts'
  import Composer from './Composer.svelte'
  import Questions from './Questions.svelte'
  import SettingsBar from './SettingsBar.svelte'
  import StateBanner from './StateBanner.svelte'
  import Ring from './Ring.svelte'
  import Turn from './Turn.svelte'
  import { errorText, shortId, stateLabel } from './words.ts'
  import './theme.css'

  let {
    chat,
    label = null,
    agent,
    may = { act: true, agents: true, usage: true },
    actions,
    readOnly = false,
    readOnlyWhy = 'As it was then: nothing can be done here.',
    appSummary = () => "An item of the app's own",
    notice = null,
    openUsage,
  }: {
    chat: DeepReadonly<Chat>
    /** The app's line for the chat, such as its title, from its `ChatSummary`, if it has one. */
    label?: string | null
    /**
     * The status of the chat's agent, from `Connection.agents`: whether
     * it's installed and signed in. Without it, a chat waiting for its
     * agent says only that it waits.
     */
    agent?: DeepReadonly<AgentStatus> | undefined
    /**
     * What the host lets this viewer do, from `Connection.may`: without
     * `act`, the chat can be watched, not acted in; without `agents`, it
     * offers no Install or sign-in. Everything, if not given.
     */
    may?: Readonly<Rights> | undefined
    actions: ChatActions
    readOnly?: boolean
    /** Why nothing can be done, said while `readOnly`. */
    readOnlyWhy?: string
    /** The one-line summary of one of the app's own items, which only the app can write. */
    appSummary?: (app: unknown) => string
    /**
     * Something the host has to say about the chat, shown as a problem:
     * such as why its settings couldn't be chosen as it opened.
     */
    notice?: string | null
    /**
     * Opens the app's usage page, from the ring's popover. Give it only if
     * the viewer `may.usage`: without it, the popover links nowhere.
     */
    openUsage?: (() => void) | undefined
  } = $props()

  /** The status of the chat's agent, if the host gave it, and it's this chat's agent's. */
  const status = $derived(agent?.name === chat.agent ? agent : undefined)
  const running = $derived(chat.turns.find((turn) => turn.life.type === 'Running' || turn.life.type === 'Stopping'))
  /** Archived, as it stays, restored or not, until it's unarchived or sent to. */
  const archived = $derived(chat.archived !== null)
  /** Whether anything can be done in the chat: it isn't read-only, and the host lets this viewer act. */
  const acting = $derived(!readOnly && may.act)
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
    if (!acting) return
    problem = null
    const error = await attempt(actions.close)
    if (error !== null) problem = errorText(error)
  }

  async function unarchive() {
    if (!acting || actions.unarchive === undefined) return
    problem = null
    const error = await attempt(actions.unarchive)
    if (error !== null) problem = errorText(error)
  }
</script>

<!-- Drawn afresh for another chat, so nothing typed or shown for one (a
     draft, why something failed) carries over to the next. -->
{#key chat.id}
<section class="caf-chat" aria-label={label ?? `Chat ${shortId(chat.id)}`}>
  <header>
    <div class="who">
      {#if label !== null}<span class="label" title={label}>{label}</span>{/if}
      <span class="agent">{chat.agent}</span>
      <!-- Cut from the left, so the end of the path shows. -->
      <span class="cwd" title={chat.cwd}><bdi>{chat.cwd}</bdi></span>
    </div>
    <span class="state" data-state={chat.state.type} data-archived={archived}>
      {archived ? 'Archived' : stateLabel(chat.state)}
    </span>
    {#if !archived && chat.state.type !== 'Closed' && may.act}
      <button type="button" class="close" disabled={readOnly} onclick={close}>Close</button>
    {/if}
  </header>
  {#if notice !== null}<div class="problem" role="alert">{notice}</div>{/if}
  {#if problem !== null}<div class="problem" role="alert">{problem}</div>{/if}
  {#if readOnly}
    <div class="looking-back" role="status">{readOnlyWhy}</div>
  {:else if !may.act}
    <div class="looking-back" role="status">The app lets this viewer watch this chat, not act in it.</div>
  {/if}
  <!-- An archived chat's composer says so, and nothing else about its
       state matters until it's unarchived. -->
  {#if !archived}<StateBanner state={chat.state} />{/if}
  {#if showsAgent(chat)}
    <AgentBanner
      name={chat.agent}
      agent={status}
      waiting={chat.state.type === 'WaitingForAgent'}
      {readOnly}
      act={may.act}
      agents={may.agents}
      install={actions.install}
      stopInstall={actions.stopInstall}
      notNow={actions.stop}
      paste={actions.paste}
      stopSignIn={actions.stopSignIn}
      checkAgain={actions.checkAgain}
    />
  {/if}

  <div class="turns" bind:this={scroller} {onscroll}>
    {#each chat.turns as turn (turn.id)}
      <Turn
        {turn}
        {readOnly}
        act={may.act}
        sendAgain={actions.sendAgain}
        signIn={actions.signIn}
        canSignIn={may.agents && (status?.can_sign_in ?? false)}
        {appSummary}
      />
    {:else}
      <p class="empty">Nothing said yet.</p>
    {/each}
  </div>

  <Questions questions={chat.questions} readOnly={!acting} answer={actions.answer} />
  <SettingsBar
    settings={chat.settings}
    agent={chat.agent}
    known={archived ? undefined : status?.settings}
    held={running !== undefined || chat.state.type !== 'Open'}
    disabled={!acting || archived}
    canLearn={acting && may.agents}
    set={actions.setSetting}
    learn={actions.learnSettings}
  />
  <Composer
    running={running !== undefined}
    stopping={running?.life.type === 'Stopping'}
    disabled={!acting || archived}
    locked={archived ? 'This chat is archived.' : null}
    send={(text) => actions.send({ parts: [{ type: 'Text', value: text }] })}
    stop={actions.stop}
  >
    {#snippet beside()}
      <Ring usage={chat.usage} spend={chat.spend} {openUsage} />
    {/snippet}
    {#snippet unlock()}
      {#if acting && actions.unarchive !== undefined}
        <button type="button" class="unarchive" onclick={unarchive}>Unarchive</button>
      {/if}
    {/snippet}
  </Composer>
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
  .label {
    font-weight: 600;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .label + .agent {
    font-weight: normal;
    color: var(--caf-muted);
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
  .close,
  .unarchive {
    font: inherit;
    font-size: 0.85em;
    padding: 0.2em 0.7em;
    border-radius: 6px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
    cursor: pointer;
  }
  .unarchive {
    align-self: center;
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
