<script lang="ts">
  // The debugger: the chat view on the left, the inspector on the right,
  // and the timeline across the bottom.
  //
  // Live and following, the chat view shows `/viewer`'s chats, as any
  // host's does. Looking back, or replaying, it shows the chats as they
  // were, from the inspector channel's `Past` messages, read-only.
  import type { AgentName, Answer, Chat, ChatId, DeepReadonly, QuestionId, UserMessage } from '@clackatoo/caf'
  import AgentsPanel, { type AgentActions } from './AgentsPanel.svelte'
  import { ChatList, ChatView, showsAgent, type ChatActions, type ListActions } from './chat/index.ts'
  import { PROTOCOL } from './inspector/channel.ts'
  import { Inspector } from './inspector/client.svelte.ts'
  import InspectorView from './inspector/Inspector.svelte'
  import Timeline from './inspector/Timeline.svelte'
  import { webSocket } from '@clackatoo/caf'
  import { hasToken, httpUrl, socketUrl, type LinkState } from './link.ts'
  import { Live } from './live.svelte.ts'
  import { canAct } from './view.ts'

  let inspector = $state.raw(connectInspector())
  let live = $state.raw<Live | null>(null)
  let selected = $state<ChatId | null>(null)
  let showInspector = $state(true)
  /** The chat view's share of the width. */
  let split = $state(0.55)
  let saving = $state<HTMLDialogElement>()

  function connectInspector(): Inspector {
    return new Inspector(new WebSocket(socketUrl('/inspect')))
  }

  const mode = $derived(inspector.hello?.mode.type ?? null)
  // Live, once the server says so: `/viewer`.
  $effect(() => {
    if (mode !== 'Live' || live !== null || inspector.mismatch !== null) return
    const made = new Live()
    live = made
    void made.connect(() => webSocket(socketUrl('/viewer')))
  })

  const lookingBack = $derived(mode === 'Replay' || !inspector.position.live)
  const summaries = $derived(lookingBack ? inspector.past.summaries : (live?.chats ?? []))
  const chat = $derived(
    selected === null ? null : lookingBack ? (inspector.past.chats.get(selected) ?? null) : (live?.chat ?? null),
  )

  // The live watch follows the chat chosen, link up or not: while it's
  // down, the chat chosen is remembered, and watched once it's back.
  $effect(() => {
    live?.watch(selected)
  })

  const actable = $derived(canAct({ mode, following: inspector.position.live, viewer: live?.link ?? null }))
  const readOnlyWhy = $derived(
    lookingBack ? 'As it was then: nothing can be done here.' : 'The link to CAF is down: nothing can be done until it’s back.',
  )
  /** The page has no token, or the server refused it: Reconnect can't help. */
  const locked = $derived(!hasToken() || inspector.refused)

  // With nothing chosen, show the newest chat there is. A chat chosen
  // stays chosen: moving on the timeline never changes which chat is
  // shown, even to a step before it existed.
  $effect(() => {
    if (selected === null && summaries.length > 0) selected = summaries.at(-1)!.id
  })

  const link = $derived.by((): LinkState => {
    if (inspector.link !== 'open') return inspector.link
    if (mode === 'Live') return live?.link ?? 'connecting'
    return 'open'
  })

  function reconnect() {
    inspector.close()
    live?.close()
    live = null
    inspector = connectInspector()
  }

  const chatActions: ChatActions = {
    send: (message: UserMessage) => live?.caf.send(need(selected), message),
    stop: () => live?.caf.stop(need(selected)),
    answer: (question: QuestionId, answer: Answer) => live?.caf.answer(question, answer),
    sendAgain: (message: DeepReadonly<UserMessage>) => live?.caf.send(need(selected), structuredClone(message) as UserMessage),
    close: () => live?.caf.close(need(selected)),
    install: () => live?.caf.install(agentOf(chat)),
    stopInstall: () => live?.caf.stopInstall(agentOf(chat)),
    signIn: () => live?.caf.signIn(agentOf(chat)),
    paste: (text: string) => live?.caf.paste(agentOf(chat), text),
    stopSignIn: () => live?.caf.stopSignIn(agentOf(chat)),
  }

  const agentActions: AgentActions = {
    install: (agent: AgentName) => live?.caf.install(agent),
    stopInstall: (agent: AgentName) => live?.caf.stopInstall(agent),
    signIn: (agent: AgentName) => live?.caf.signIn(agent),
    paste: (agent: AgentName, text: string) => live?.caf.paste(agent, text),
    stopSignIn: (agent: AgentName) => live?.caf.stopSignIn(agent),
    signOut: (agent: AgentName) => live?.caf.signOut(agent),
  }

  /** The agents on offer, live. Looking back, or replaying, there are none to show: a recording holds no statuses. */
  const agents = $derived(lookingBack ? [] : (live?.agents ?? []))
  /** The status of the chat's agent, live. */
  const agentStatus = $derived(chat === null ? undefined : agents.find((agent) => agent.name === chat.agent))

  const listActions: ListActions = {
    select: (chat: ChatId) => (selected = chat),
    open: async (agent: AgentName, cwd: string) => {
      if (live === null) throw new Error('Not live.')
      selected = await live.caf.open({ agent, cwd, session: { type: 'New' } })
    },
  }

  function agentOf(chat: DeepReadonly<Chat> | null): AgentName {
    if (chat === null) throw new Error('No chat is chosen.')
    return chat.agent
  }

  function need(chat: ChatId | null): ChatId {
    if (chat === null) throw new Error('No chat is chosen.')
    return chat
  }

  let saveProblem = $state<string | null>(null)

  /**
   * Downloads the recording. It's fetched, then saved from a blob: a link
   * straight to `/recording?token=…` would leave the token in the
   * browser's download history.
   */
  async function save() {
    saveProblem = null
    try {
      const response = await fetch(httpUrl('/recording'))
      if (!response.ok) throw new Error(`the server said ${response.status} ${response.statusText}`)
      const url = URL.createObjectURL(await response.blob())
      const a = document.createElement('a')
      a.href = url
      a.download = inspector.hello?.file?.split(/[\\/]/).at(-1) || 'caf-recording.jsonl'
      a.click()
      setTimeout(() => URL.revokeObjectURL(url), 60_000)
      saving?.close()
    } catch (error) {
      saveProblem = `It couldn't be downloaded: ${error instanceof Error ? error.message : String(error)}.`
    }
  }

  // Dragging the divider.
  let main = $state<HTMLElement>()
  function ondividerdown(event: PointerEvent) {
    const target = event.currentTarget as HTMLElement
    target.setPointerCapture(event.pointerId)
    const move = (moved: PointerEvent) => {
      if (main === undefined) return
      const box = main.getBoundingClientRect()
      split = Math.min(0.85, Math.max(0.25, (moved.clientX - box.left) / box.width))
    }
    const up = () => {
      target.removeEventListener('pointermove', move)
      target.removeEventListener('pointerup', up)
    }
    target.addEventListener('pointermove', move)
    target.addEventListener('pointerup', up)
  }
  function ondividerkey(event: KeyboardEvent) {
    if (event.key === 'ArrowLeft') split = Math.max(0.25, split - 0.05)
    else if (event.key === 'ArrowRight') split = Math.min(0.85, split + 0.05)
  }
</script>

<div class="app">
  <header class="bar">
    <span class="brand">CAF debugger</span>
    {#if mode !== null}
      <span class="mode" data-mode={mode}>{mode}</span>
      {#if inspector.hello?.file}<span class="file" title={inspector.hello.file}><bdi>{inspector.hello.file}</bdi></span>{/if}
    {/if}
    <span class="link" data-link={link}>
      {link === 'open' ? 'Connected' : link === 'connecting' ? 'Connecting…' : 'Disconnected'}
    </span>
    {#if inspector.mismatch !== null}
      <!-- Shown below instead of the app. -->
    {:else if locked}
      <span class="problem">This page can't reach caf-debugger: open the address it printed, with its <code>#token=</code>, again.</span>
    {:else if link === 'closed'}<button type="button" onclick={reconnect}>Reconnect</button>{/if}
    {#if live?.problem}<span class="problem">{live.problem}</span>{/if}
    <span class="spacer"></span>
    {#if mode === 'Live'}
      <button type="button" onclick={() => saving?.showModal()} disabled={!inspector.hello?.file}>Save bug report</button>
    {/if}
    <button type="button" aria-pressed={showInspector} onclick={() => (showInspector = !showInspector)}>
      {showInspector ? 'Hide' : 'Show'} inspector
    </button>
  </header>

  {#if inspector.mismatch !== null}
    <div class="mismatch" role="alert">
      <h2>This page is from a different build of caf-debugger</h2>
      <p>
        The server speaks version {String(inspector.mismatch.server ?? 'none')} of the inspector channel, and this page was built for
        version {PROTOCOL}. Rebuild the app (<code>npm run build</code> in <code>ts/debugger</code>) and reload.
      </p>
    </div>
  {:else}
  <main bind:this={main} style:grid-template-columns={showInspector ? `${split}fr 6px ${1 - split}fr` : '1fr'}>
    <div class="left">
      <div class="list">
        <div class="chats">
          <ChatList chats={summaries} agents={agents.map((agent) => agent.name)} {selected} actions={listActions} readOnly={!actable} />
        </div>
        {#if mode === 'Live' && !lookingBack}
          <AgentsPanel
            {agents}
            actions={agentActions}
            readOnly={!actable}
            may={live?.may.agents ?? false}
            inChat={chat !== null && showsAgent(chat) ? chat.agent : null}
          />
        {/if}
      </div>
      <div class="chat">
        {#if chat !== null}
          <ChatView
            {chat}
            agent={agentStatus}
            may={lookingBack ? undefined : live?.may}
            actions={chatActions}
            readOnly={!actable}
            {readOnlyWhy}
          />
        {:else}
          <p class="empty">
            {#if selected === null}
              {lookingBack ? 'No chats at this step.' : 'No chat yet: open one with New chat.'}
            {:else if lookingBack && !summaries.some((summary) => summary.id === selected)}
              Chat {selected} didn't exist at this step.
            {:else}
              Loading chat {selected}…
            {/if}
          </p>
        {/if}
      </div>
    </div>
    {#if showInspector}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
      <div
        class="divider"
        role="separator"
        aria-label="Resize the chat and the inspector"
        aria-orientation="vertical"
        aria-valuenow={Math.round(split * 100)}
        tabindex="0"
        onpointerdown={ondividerdown}
        onkeydown={ondividerkey}
      ></div>
      <div class="right"><InspectorView {inspector} /></div>
    {/if}
  </main>

  <Timeline {inspector} />
  {/if}
</div>

<dialog bind:this={saving} class="save" aria-labelledby="save-title">
  <h2 id="save-title">Save a bug report</h2>
  <p>
    The bug report is this run's recording: every event CAF's core was given, so anyone can replay it step by step. It holds
    <strong>everything said</strong> in this run: your messages, the agents' answers and thinking, the files they read, the commands
    they ran and what those printed, and the folders they worked in. Sign-in emails and the values of MCP servers' secrets are left
    out.
  </p>
  <p>Share it only with someone you'd show all of that to.</p>
  {#if saveProblem !== null}<p class="problem" role="alert">{saveProblem}</p>{/if}
  <div class="buttons">
    <button type="button" onclick={() => saving?.close()}>Cancel</button>
    <button type="button" class="primary" onclick={save}>Download it</button>
  </div>
</dialog>

<style>
  :global(html),
  :global(body) {
    margin: 0;
    height: 100%;
    background: var(--caf-bg);
    color: var(--caf-text);
    font-family: var(--caf-sans);
    font-size: 14px;
  }
  :global(#app) {
    height: 100%;
  }
  .app {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 0.7em;
    padding: 0.45em 0.8em;
    border-bottom: 1px solid var(--caf-border);
    background: var(--caf-panel);
  }
  .brand {
    font-weight: 700;
  }
  .mode {
    padding: 0.05em 0.6em;
    border-radius: 999px;
    font-size: 0.85em;
    background: var(--caf-inset);
  }
  .mode[data-mode='Live'] {
    background: var(--caf-error-bg);
    color: var(--caf-error);
  }
  .file {
    font-family: var(--caf-mono);
    font-size: 0.85em;
    color: var(--caf-muted);
    max-width: 28em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
  }
  .link {
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  .link[data-link='open']::before {
    content: '● ';
    color: var(--caf-ok);
  }
  .link[data-link='closed'] {
    color: var(--caf-error);
  }
  .problem {
    color: var(--caf-error);
    font-size: 0.85em;
  }
  .spacer {
    flex: 1;
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
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .primary {
    background: var(--caf-accent);
    border-color: var(--caf-accent);
    color: var(--caf-accent-text);
  }
  main {
    flex: 1;
    min-height: 0;
    display: grid;
  }
  .left {
    display: grid;
    grid-template-columns: minmax(10em, 15em) 1fr;
    min-width: 0;
    min-height: 0;
  }
  .list {
    border-right: 1px solid var(--caf-border);
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .chats {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .chat {
    min-width: 0;
    min-height: 0;
  }
  .empty {
    color: var(--caf-faint);
    text-align: center;
    margin-top: 4em;
  }
  .divider {
    cursor: col-resize;
    background: var(--caf-border);
    touch-action: none;
  }
  .divider:hover,
  .divider:focus-visible {
    background: var(--caf-accent);
    outline: none;
  }
  .right {
    min-width: 0;
    min-height: 0;
  }
  .mismatch {
    max-width: 40em;
    margin: 4em auto;
    padding: 1.2em 1.4em;
    border: 1px solid var(--caf-error);
    border-radius: 10px;
    background: var(--caf-error-bg);
  }
  .mismatch h2 {
    margin: 0 0 0.5em;
    font-size: 1.15em;
  }
  .mismatch p {
    line-height: 1.5;
    margin: 0;
  }
  .save {
    max-width: 34em;
    border: 1px solid var(--caf-border);
    border-radius: 10px;
    background: var(--caf-bg);
    color: var(--caf-text);
    padding: 1.2em 1.4em;
  }
  .save::backdrop {
    background: rgb(0 0 0 / 0.35);
  }
  .save h2 {
    margin: 0 0 0.6em;
    font-size: 1.15em;
  }
  .save p {
    line-height: 1.5;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 0.5em;
  }
</style>
