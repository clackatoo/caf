<script lang="ts">
  // The open chats, and "New chat": an agent from the ones CAF offers,
  // a folder for it to work in, and its model, where CAF knows the
  // agent's models. Where it doesn't, the chat view offers them once the
  // chat is open, before anything is sent.
  import type { AgentName, AgentStatus, ChatId, ChatSummary, DeepReadonly, SettingChoice } from '@clackatoo/caf'
  import { untrack } from 'svelte'
  import { attempt, type ListActions } from './actions.ts'
  import { runs } from './settings.ts'
  import { errorText, folderName, shortId } from './words.ts'
  import './theme.css'

  let {
    chats,
    agents,
    statuses = [],
    selected,
    actions,
    readOnly = false,
  }: {
    chats: DeepReadonly<ChatSummary[]>
    agents: readonly AgentName[]
    /** The agents' statuses, from `Connection.agents`: where one's models are known, New chat offers choosing one. */
    statuses?: readonly DeepReadonly<AgentStatus>[]
    selected: ChatId | null
    actions: ListActions
    /** Nothing can be opened: looking back, or a recording. */
    readOnly?: boolean
  } = $props()

  let adding = $state(false)
  let agent = $state<AgentName>('')
  let cwd = $state('')
  /** The model chosen, or '' for the one the agent would use. */
  let model = $state('')
  /** Whether the user picked `model`, rather than it being what the agent would use. */
  let picked = false
  /** The agent `model` was picked for. */
  let pickedFor: AgentName | null = null
  let opening = $state(false)
  let problem = $state<string | null>(null)

  const absolute = $derived(cwd.startsWith('/') || /^[A-Za-z]:[\\/]/.test(cwd))
  /** The agent's models, if CAF knows them, as their setting. */
  const models = $derived.by(() => {
    const known = statuses.find((status) => status.name === agent)?.settings
    if (known?.type !== 'Known') return null
    const setting = known.value.find((setting) => setting.kind.type === 'Model')
    return setting?.form.type === 'Choices' ? { current: setting.current, choices: setting.form.value } : null
  })
  /** What the agent would use: its model as it last said. */
  const theirs = $derived(models?.current?.type === 'Choice' ? models.current.value : '')

  // Until the user picks one, the model is what the agent would use, as
  // its status says. A pick stays through the agents' statuses changing,
  // until another agent is chosen, or the agent no longer offers it.
  $effect.pre(() => {
    const now = agent
    const choices = models?.choices ?? []
    const own = theirs
    untrack(() => {
      if (now !== pickedFor) picked = false
      pickedFor = now
      if (picked && choices.some((choice) => choice.id === model)) return
      picked = false
      model = own
    })
  })

  function start() {
    adding = true
    problem = null
    picked = false
    model = theirs
    if (!agents.includes(agent)) agent = agents[0] ?? ''
  }

  async function open(event: SubmitEvent) {
    event.preventDefault()
    if (readOnly || opening || agent === '' || !absolute) return
    opening = true
    problem = null
    // Only a model the user changed is chosen: otherwise the agent's own,
    // or the host's default for it, applies.
    const settings: SettingChoice[] = picked && model !== theirs ? [{ kind: { type: 'Model' }, value: { type: 'Choice', value: model } }] : []
    const error = await attempt(() => actions.open(agent, cwd.trim(), settings))
    opening = false
    if (error === null) adding = false
    else problem = errorText(error)
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
      {#if models !== null}
        <label>
          Model
          <select bind:value={model} onchange={() => (picked = true)}>
            {#if theirs === ''}<option value="">The agent's own</option>{/if}
            {#each runs(models.choices) as run, r (r)}
              {#if run.group === null}
                {#each run.choices as choice (choice.id)}<option value={choice.id} title={choice.description}>{choice.name}</option>{/each}
              {:else}
                <optgroup label={run.group}>
                  {#each run.choices as choice (choice.id)}<option value={choice.id} title={choice.description}>{choice.name}</option>{/each}
                </optgroup>
              {/if}
            {/each}
          </select>
        </label>
        {@const about = models.choices.find((choice) => choice.id === model)?.description ?? null}
        {#if about !== null}<div class="hint">{about}</div>{/if}
      {/if}
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
          <!-- The app's line for it if it has one, or else its folder. -->
          <span class="name">{chat.label ?? folderName(chat.cwd)}</span>
          <span class="meta">{chat.agent} · {chat.label == null ? shortId(chat.id) : folderName(chat.cwd)}</span>
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
