<script lang="ts">
  // Where the user writes. Enter sends, Shift+Enter starts a new line.
  // While a turn runs, a message sent waits behind it (CAF queues it), so
  // Send stays on; Stop stops the running turn. Beside the buttons, the
  // app may put something of the chat's: the context ring. A chat nothing
  // can be written in, such as an archived one, keeps the box, saying why,
  // with what would let it be written in again, such as Unarchive.
  import type { Snippet } from 'svelte'
  import { attempt } from './actions.ts'
  import { errorText } from './words.ts'

  let {
    running,
    stopping,
    disabled,
    locked = null,
    send,
    stop,
    beside,
    unlock,
  }: {
    /** A turn is running, so a message would wait, and Stop is on. */
    running: boolean
    /** The running turn has been asked to stop. */
    stopping: boolean
    /** Nothing can be sent: looking back, or the chat is archived. */
    disabled: boolean
    /** Why nothing can be written, said in the box in place of the message, such as "This chat is archived.". */
    locked?: string | null
    send: (text: string) => Promise<unknown> | void
    stop: () => Promise<unknown> | void
    /** Drawn before the buttons, and never disabled: it only shows. */
    beside?: Snippet | undefined
    /** Drawn after why it's locked, while it is: what would unlock it, such as Unarchive. */
    unlock?: Snippet | undefined
  } = $props()

  let text = $state('')
  let sending = $state(false)
  let problem = $state<string | null>(null)

  async function submit() {
    const message = text
    if (disabled || sending || message.trim() === '') return
    sending = true
    problem = null
    text = ''
    const error = await attempt(() => send(message))
    sending = false
    if (error !== null) {
      problem = errorText(error)
      // Give it back, unless the user has started another.
      if (text === '') text = message
    }
  }

  async function halt() {
    if (disabled || stopping) return
    problem = null
    const error = await attempt(stop)
    if (error !== null) problem = errorText(error)
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault()
      void submit()
    }
  }
</script>

<form
  class="composer"
  onsubmit={(event) => {
    event.preventDefault()
    void submit()
  }}
>
  {#if problem !== null}<div class="problem" role="alert">{problem}</div>{/if}
  <div class="row">
    {#if locked !== null}
      <div class="locked" role="status"><span class="lock" aria-hidden="true">🔒</span>{locked}</div>
      {@render beside?.()}
      {@render unlock?.()}
    {:else}
      <textarea
        bind:value={text}
        {onkeydown}
        {disabled}
        rows="2"
        aria-label="Message"
        placeholder={disabled ? '' : running ? 'Message: it waits for the running turn' : 'Message'}
      ></textarea>
      {@render beside?.()}
      <div class="buttons">
        <button type="submit" class="send" disabled={disabled || sending || text.trim() === ''}>Send</button>
        {#if running}
          <button type="button" class="stop" disabled={disabled || stopping} onclick={halt}>
            {stopping ? 'Stopping…' : 'Stop'}
          </button>
        {/if}
      </div>
    {/if}
  </div>
</form>

<style>
  .composer {
    padding: 0.6em 0.9em 0.8em;
    border-top: 1px solid var(--caf-border);
    background: var(--caf-bg);
  }
  .row {
    display: flex;
    gap: 0.5em;
    align-items: flex-end;
  }
  textarea {
    flex: 1;
    resize: vertical;
    min-height: 2.6em;
    max-height: 16em;
    font: inherit;
    padding: 0.5em 0.7em;
    border-radius: 8px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
  }
  textarea:focus {
    outline: 2px solid var(--caf-accent);
    outline-offset: -1px;
  }
  .buttons {
    display: flex;
    flex-direction: column;
    gap: 0.3em;
  }
  button {
    font: inherit;
    padding: 0.4em 1em;
    border-radius: 6px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
    cursor: pointer;
  }
  .send {
    background: var(--caf-accent);
    border-color: var(--caf-accent);
    color: var(--caf-accent-text);
  }
  .stop {
    border-color: var(--caf-error);
    color: var(--caf-error);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .problem {
    color: var(--caf-error);
    font-size: 0.9em;
    margin-bottom: 0.4em;
  }
  .locked {
    flex: 1;
    align-self: center;
    padding: 0.5em 0.7em;
    color: var(--caf-muted);
  }
  .lock {
    margin-right: 0.4em;
  }
</style>
