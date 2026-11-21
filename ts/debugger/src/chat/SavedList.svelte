<script lang="ts">
  // The saved chats on the shelves the host lists, from
  // `Connection.saved`: open or not, each with Restore while it isn't.
  // The host decides each restore, as it does each open, so it's offered
  // whatever `Connection.may` says, and a refusal is shown under the chat.
  import { CafError, type CannotRestore, type DeepReadonly, type SavedChat } from '@clackatoo/caf'
  import { attempt, type SavedActions } from './actions.ts'
  import { dayAndTime, fullTime } from './time.ts'
  import { cannotRestoreText, errorText, folderName, mayRestoreAgain, savedStateLabel, turnCount } from './words.ts'
  import './theme.css'

  let {
    saved,
    actions,
    readOnly = false,
  }: {
    saved: DeepReadonly<SavedChat[]>
    actions: SavedActions
    /** Nothing can be restored now: the link is down, or this is the past. */
    readOnly?: boolean
  } = $props()

  /** A saved chat's key: its id is only its own on its shelf. */
  const keyOf = (chat: DeepReadonly<SavedChat>) => `${chat.shelf}\u0000${chat.header.chat}`

  /** What a saved chat is called: the app's line for it, or else its first message. */
  const nameOf = (chat: DeepReadonly<SavedChat>) => chat.header.label ?? chat.header.first ?? 'Untitled'

  /**
   * The chat being restored from here. Once CAF has said it's open, it
   * stays so until the list says it's loaded, or says anything new of it,
   * so Restore doesn't come back for a moment between the two.
   */
  let restoring = $state.raw<{ key: string; answered: DeepReadonly<SavedChat[]> | null } | null>(null)

  /** Why restoring a chat from here failed, while its state is still what it was then. */
  interface Problem {
    text: string
    /** Why it couldn't be restored, if CAF said. */
    why: DeepReadonly<CannotRestore> | null
    /** The chat's state when it failed. */
    state: string
  }
  let problems = $state<Record<string, Problem>>({})

  $effect(() => {
    const rows = new Map(saved.map((chat) => [keyOf(chat), chat]))
    // A problem goes once the chat's state moves on: someone else may have
    // restored it, or CAF says why it failed itself.
    for (const [key, problem] of Object.entries(problems)) {
      if (rows.get(key)?.state.type !== problem.state) delete problems[key]
    }
    if (restoring === null) return
    const row = rows.get(restoring.key)
    if (readOnly || row === undefined || row.state.type === 'Loaded') restoring = null
    else if (restoring.answered !== null && saved !== restoring.answered) restoring = null
  })

  async function restore(chat: DeepReadonly<SavedChat>) {
    const key = keyOf(chat)
    if (readOnly || restoring !== null) return
    restoring = { key, answered: null }
    delete problems[key]
    const error = await attempt(() => actions.restore(chat.header.chat, chat.shelf))
    if (restoring?.key !== key) return
    if (error === null) {
      restoring = { key, answered: saved }
      return
    }
    restoring = null
    const why = error instanceof CafError && error.why.type === 'CannotRestore' ? error.why.value : null
    const now = saved.find((each) => keyOf(each) === key) ?? chat
    problems[key] = { text: errorText(error), why, state: now.state.type }
  }

  /** Whether to offer Restore: not after a failure that trying again can't help. */
  function offered(chat: DeepReadonly<SavedChat>, problem: Problem | undefined): boolean {
    if (chat.state.type === 'Failed') return mayRestoreAgain(chat.state.value)
    if (chat.state.type !== 'Saved') return false
    return problem?.why == null || mayRestoreAgain(problem.why)
  }
</script>

<section class="caf-saved-list" aria-label="Saved chats">
  <div class="top"><span class="heading">Saved</span></div>
  <ul>
    {#each saved as chat (keyOf(chat))}
      {@const { header, state } = chat}
      {@const key = keyOf(chat)}
      {@const name = nameOf(chat)}
      {@const problem = problems[key]}
      {@const again = state.type === 'Failed' || problem !== undefined}
      <li data-state={state.type}>
        <span class="name" title={name}>{name}</span>
        <span class="meta" title={header.cwd}>{header.agent} · {folderName(header.cwd)}</span>
        <span class="meta">
          <time datetime={new Date(header.changed).toISOString()} title="Last changed {fullTime(header.changed)}">
            {dayAndTime(header.changed)}
          </time>
          · {turnCount(header.turns)}{#if header.closed}{' '}· closed{/if}
        </span>
        <div class="state">
          {#if state.type === 'Loaded'}
            <span class="loaded">{savedStateLabel(state)}</span>
            <button type="button" aria-label="Show {name}" onclick={() => actions.select(header.chat)}>Show</button>
          {:else if state.type === 'Restoring' || restoring?.key === key}
            <span class="restoring" role="status">{savedStateLabel({ type: 'Restoring' })}</span>
          {:else}
            {#if state.type !== 'Saved' && state.type !== 'Failed'}<span>{savedStateLabel(state)}</span>{/if}
            {#if offered(chat, problem)}
              <button
                type="button"
                aria-label={again ? `Try again to restore ${name}` : `Restore ${name}`}
                disabled={readOnly || restoring !== null}
                onclick={() => restore(chat)}
              >
                {again ? 'Try again' : 'Restore'}
              </button>
            {/if}
          {/if}
        </div>
        {#if problem !== undefined}
          <div class="problem" role="alert">{problem.text}</div>
        {:else if state.type === 'Failed'}
          <div class="problem">It can't be restored: {cannotRestoreText(state.value)}</div>
        {/if}
      </li>
    {:else}
      <li class="none">No chats saved.</li>
    {/each}
  </ul>
</section>

<style>
  .caf-saved-list {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--caf-panel);
    color: var(--caf-text);
    font-family: var(--caf-sans);
    font-size: 0.92em;
    border-top: 1px solid var(--caf-border);
  }
  .top {
    padding: 0.55em 0.7em;
    border-bottom: 1px solid var(--caf-border);
  }
  .heading {
    font-weight: 600;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0.3em;
    overflow-y: auto;
  }
  li {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.1em;
    padding: 0.4em 0.6em;
  }
  li + li {
    border-top: 1px solid var(--caf-border);
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
  .state {
    display: flex;
    align-items: center;
    gap: 0.5em;
    margin-top: 0.2em;
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  .loaded::before {
    content: '● ';
    color: var(--caf-ok);
  }
  button {
    font: inherit;
    padding: 0.15em 0.7em;
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
  .problem {
    color: var(--caf-error);
    font-size: 0.85em;
    overflow-wrap: anywhere;
  }
  .none {
    color: var(--caf-faint);
    padding: 0.6em;
  }
</style>
