<script lang="ts">
  // Where the chat stands with its agent, when that's worth saying: while
  // it opens, once its session is lost, and once it's closed.
  import type { ChatState, DeepReadonly } from '@clackatoo/caf'
  import { lostText, tagOf } from './words.ts'

  let { state }: { state: DeepReadonly<ChatState> } = $props()
</script>

{#if state.type === 'Opening'}
  <div class="banner opening" role="status">Opening the session…</div>
{:else if state.type === 'Lost'}
  {@const reason = state.value.reason}
  <div class="banner lost" role="status" data-reason={reason.type}>
    <div>{lostText(reason)} Your next message opens it again.</div>
    {#if reason.type === 'CouldNotStart'}
      <figure>
        <figcaption>The system said:</figcaption>
        <blockquote>{reason.value.error}</blockquote>
      </figure>
    {:else if reason.type === 'AgentStopped' && reason.value.stderr.length > 0}
      <figure>
        <figcaption>Its last lines of error output:</figcaption>
        <pre>{reason.value.stderr.join('\n')}</pre>
      </figure>
    {:else if reason.type === 'Refused'}
      <figure>
        <figcaption>The agent said:</figcaption>
        <blockquote>{reason.value.message}</blockquote>
      </figure>
    {/if}
  </div>
{:else if state.type === 'Closed'}
  <div class="banner closed" role="status">This chat is closed.</div>
{:else if state.type !== 'Open'}
  <div class="banner lost" role="status">The chat is in a state this viewer doesn't know ({tagOf(state)}).</div>
{/if}

<style>
  .banner {
    padding: 0.55em 0.9em;
    font-size: 0.92em;
    border-bottom: 1px solid var(--caf-border);
  }
  .opening {
    background: var(--caf-inset);
    color: var(--caf-muted);
  }
  .lost {
    background: var(--caf-warn-bg);
    color: var(--caf-text);
    border-left: 3px solid var(--caf-warn);
  }
  .closed {
    background: var(--caf-inset);
    color: var(--caf-muted);
  }
  figure {
    margin: 0.4em 0 0;
  }
  figcaption {
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  blockquote,
  pre {
    margin: 0.2em 0 0;
    padding: 0.3em 0.7em;
    border-left: 3px solid var(--caf-faint);
    color: var(--caf-muted);
    font-family: var(--caf-mono);
    font-size: 0.88em;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
