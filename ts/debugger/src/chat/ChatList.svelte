<script lang="ts">
  // The open chats, and "New chat": an agent from the ones CAF offers,
  // and a folder for it to work in.
  import type { AgentName, ChatId, ChatSummary, DeepReadonly } from '@clackatoo/caf'
  import { attempt, type ListActions } from './actions.ts'
  import { errorText } from './words.ts'
  import './theme.css'

  let {
    chats,
    agents,
    selected,
    actions,
    readOnly = false,
  }: {
    chats: DeepReadonly<ChatSummary[]>
    agents: readonly AgentName[]
    selected: ChatId | null
    actions: ListActions
    /** Nothing can be opened: looking back, or a recording. */
    readOnly?: boolean
  } = $props()

  let adding = $state(false)
  let agent = $state<AgentName>('')
  let cwd = $state('')
  let opening = $state(false)
  let problem = $state<string | null>(null)

  const absolute = $derived(cwd.startsWith('/') || /^[A-Za-z]:[\\/]/.test(cwd))

  function start() {
    adding = true
    problem = null
    if (!agents.includes(agent)) agent = agents[0] ?? ''
  }

  async function open(event: SubmitEvent) {
    event.preventDefault()
    if (opening || agent === '' || !absolute) return
    opening = true
    problem = null
    const error = await attempt(() => actions.open(agent, cwd.trim()))
    opening = false
    if (error === null) adding = false
    else problem = errorText(error)
  }

  function folder(path: string): string {
    const parts = path.split(/[\\/]/).filter((part) => part !== '')
    return parts.at(-1) ?? path
  }
</script>

<nav class="caf-chat-list" aria-label="Chats">
  <div class="top">
    <span class="heading">Chats</span>
    {#if !adding}
      <button type="button" disabled={readOnly || agents.length === 0} onclick={start}>New chat</button>
    {/if}
  </div>

  {#if adding}
    <form class="new" onsubmit={open}>
      <label>
        Agent
        <select bind:value={agent}>
          {#each agents as name, i (i)}<option value={name}>{name}</option>{/each}
        </select>
      </label>
      <label>
        Folder
        <input bind:value={cwd} placeholder="/home/me/project" spellcheck="false" autocomplete="off" />
      </label>
      {#if cwd !== '' && !absolute}<div class="hint">A full path, starting with /.</div>{/if}
      {#if problem !== null}<div class="problem" role="alert">{problem}</div>{/if}
      <div class="buttons">
        <button type="submit" class="primary" disabled={readOnly || opening || agent === '' || !absolute}>
          {opening ? 'Opening…' : 'Open'}
        </button>
        <button type="button" onclick={() => (adding = false)}>Cancel</button>
      </div>
    </form>
  {/if}

  <ul>
    {#each chats as chat (chat.id)}
      <li>
        <button
          type="button"
          class="chat"
          class:selected={chat.id === selected}
          aria-current={chat.id === selected ? 'true' : undefined}
          title={chat.cwd}
          onclick={() => actions.select(chat.id)}
        >
          <span class="name">{folder(chat.cwd)}</span>
          <span class="meta">{chat.agent} · #{chat.id}</span>
        </button>
      </li>
    {:else}
      <li class="none">No chats open.</li>
    {/each}
  </ul>
</nav>

<style>
  .caf-chat-list {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
    background: var(--caf-panel);
    color: var(--caf-text);
    font-family: var(--caf-sans);
    font-size: 0.92em;
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.55em 0.7em;
    border-bottom: 1px solid var(--caf-border);
  }
  .heading {
    font-weight: 600;
  }
  button {
    font: inherit;
    padding: 0.25em 0.7em;
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
  .new {
    display: flex;
    flex-direction: column;
    gap: 0.5em;
    padding: 0.7em;
    border-bottom: 1px solid var(--caf-border);
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.2em;
    color: var(--caf-muted);
    font-size: 0.9em;
  }
  select,
  input {
    font: inherit;
    color: var(--caf-text);
    background: var(--caf-bg);
    border: 1px solid var(--caf-border);
    border-radius: 6px;
    padding: 0.3em 0.5em;
  }
  input {
    font-family: var(--caf-mono);
  }
  .buttons {
    display: flex;
    gap: 0.4em;
  }
  .hint {
    color: var(--caf-muted);
    font-size: 0.85em;
  }
  .problem {
    color: var(--caf-error);
    font-size: 0.85em;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0.3em;
    overflow-y: auto;
    flex: 1;
  }
  .chat {
    width: 100%;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    border: none;
    background: none;
    padding: 0.4em 0.6em;
    text-align: left;
  }
  .chat:hover {
    background: var(--caf-hover);
  }
  .chat.selected {
    background: var(--caf-bg);
    box-shadow: inset 3px 0 0 var(--caf-accent);
  }
  .name {
    font-weight: 500;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    color: var(--caf-muted);
    font-size: 0.85em;
  }
  .none {
    color: var(--caf-faint);
    padding: 0.6em;
  }
</style>
