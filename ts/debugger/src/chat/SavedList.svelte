<script lang="ts">
  // The saved chats on the shelves the host lists, from
  // `Connection.saved`: open or not, each with Restore while it isn't,
  // where the viewer may act.
  import type { DeepReadonly, SavedChat } from '@clackatoo/caf'
  import { attempt, type SavedActions } from './actions.ts'
  import { dayAndTime, fullTime } from './time.ts'
  import { cannotRestoreText, errorText, folderName, savedStateLabel, turnCount } from './words.ts'
  import './theme.css'

  let {
    saved,
    actions,
    act = true,
    readOnly = false,
  }: {
    saved: DeepReadonly<SavedChat[]>
    actions: SavedActions
    /** The host lets this viewer act, from `Connection.may`: without it, nothing is offered. */
    act?: boolean
    /** Nothing can be restored now: the link is down. */
    readOnly?: boolean
  } = $props()

  /** A saved chat's key: its id is only its own on its shelf. */
  const keyOf = (chat: DeepReadonly<SavedChat>) => `${chat.shelf}\u0000${chat.header.chat}`

  /** The chat being restored from here, while CAF answers. */
  let restoring = $state<string | null>(null)
  /** Why restoring a chat from here failed, by its key: shown under it until it's tried again. */
  let problems = $state<Record<string, string>>({})

  async function restore(chat: DeepReadonly<SavedChat>) {
    const key = keyOf(chat)
    if (readOnly || !act || restoring !== null) return
    restoring = key
    delete problems[key]
    const error = await attempt(() => actions.restore(chat.header.chat, chat.shelf))
    restoring = null
    if (error !== null) problems[key] = errorText(error)
  }
</script>

<section class="caf-saved-list" aria-label="Saved chats">
  <div class="top"><span class="heading">Saved</span></div>
  <ul>
    {#each saved as chat (keyOf(chat))}
      {@const { header, state } = chat}
      {@const key = keyOf(chat)}
      <li data-state={state.type}>
        <!-- The app's line for it if it has one, or else its first message. -->
        <span class="name" title={header.label ?? header.first ?? undefined}>
          {header.label ?? header.first ?? 'Nothing said yet'}
        </span>
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
            <button type="button" onclick={() => actions.select(header.chat)}>Show</button>
          {:else if state.type === 'Restoring' || restoring === key}
            <span class="restoring" role="status">{savedStateLabel({ type: 'Restoring' })}</span>
          {:else}
            {#if state.type !== 'Saved' && state.type !== 'Failed'}<span>{savedStateLabel(state)}</span>{/if}
            {#if act && (state.type === 'Saved' || state.type === 'Failed')}
              <button type="button" disabled={readOnly || restoring !== null} onclick={() => restore(chat)}>
                {state.type === 'Failed' ? 'Try again' : 'Restore'}
              </button>
            {/if}
          {/if}
        </div>
        {#if problems[key] !== undefined}
          <div class="problem" role="alert">{problems[key]}</div>
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
