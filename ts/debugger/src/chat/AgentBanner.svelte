<script lang="ts">
  // What the chat's agent needs before the chat can go on, when that's
  // its install or its sign-in. It's the agent's status, shared by every
  // chat with it, so the same banner shows in each.
  //
  // - While the agent is being signed in to, whatever the chat's state:
  //   SignIn's link, code, paste box and Stop.
  // - Otherwise, while the chat waits for its agent: Install (with its
  //   size) and Not now, progress while it installs, or why it can't be
  //   installed.
  //
  // It offers only what the host lets this viewer do: without the right
  // to act on agents, it says what's happening and offers no Install or
  // sign-in; without the right to act, no Not now.
  import type { AgentStatus, DeepReadonly } from '@clackatoo/caf'
  import { attempt } from './actions.ts'
  import SignIn from './SignIn.svelte'
  import { bytes, canCheckAgain, canRetry, cannotText, errorText, installed, tagOf } from './words.ts'

  let {
    name,
    agent,
    waiting,
    readOnly,
    act,
    agents,
    install,
    stopInstall,
    notNow,
    paste,
    stopSignIn,
    checkAgain,
  }: {
    /** The agent's name, as the chat has it. */
    name: string
    /** The agent's status, if the host gave it. */
    agent: DeepReadonly<AgentStatus> | undefined
    /** Whether the chat is waiting for its agent. */
    waiting: boolean
    /** Looking back: nothing can be done. */
    readOnly: boolean
    /** Whether the host lets this viewer act on chats: Not now stops the chat's messages. */
    act: boolean
    /** Whether the host lets this viewer install agents and sign in to them. */
    agents: boolean
    install: () => Promise<unknown> | void
    stopInstall: () => Promise<unknown> | void
    /** Not now: stop the chat's waiting message. */
    notNow: () => Promise<unknown> | void
    paste: (text: string) => Promise<unknown> | void
    stopSignIn: () => Promise<unknown> | void
    /** Have CAF look at the machine again, where the agent can't be installed for want of something on it. Without it, no Check again. */
    checkAgain?: (() => Promise<unknown> | void) | undefined
  } = $props()

  let busy = $state(false)
  let problem = $state<string | null>(null)

  async function run(action: () => Promise<unknown> | void) {
    if (readOnly || busy) return
    busy = true
    problem = null
    const error = await attempt(action)
    busy = false
    if (error !== null) problem = errorText(error)
  }
</script>

<!-- Only the first line of each is a live region: it says what changed,
     without reading out progress, codes or buttons each time. -->
{#if agent?.sign_in.type === 'SigningIn'}
  <div class="banner" data-agent-state="SigningIn">
    {#if agents}
      <SignIn {name} link={agent.sign_in.value} {readOnly} {paste} stop={stopSignIn} />
    {:else}
      <div role="status">Signing in to <strong>{name}</strong>, from the app.</div>
    {/if}
  </div>
{:else if waiting}
  {@const standing = agent?.install}
  <div class="banner" data-agent-state={standing?.type ?? 'None'}>
    {#if standing === undefined}
      <div role="status">Waiting for <strong>{name}</strong>. Messages you send wait until it's ready.</div>
    {:else if standing.type === 'Missing'}
      <div role="status"><strong>{name}</strong> isn't installed.</div>
      <div>
        {agents ? 'Installing it' : 'The app can install it, which'} downloads about {bytes(standing.value.size)}. Messages
        you send wait until it's installed.
      </div>
      <div class="buttons">
        {#if agents}
          <button type="button" disabled={readOnly || busy} onclick={() => run(install)}>Install</button>
        {/if}
        {#if act}
          <button type="button" class="quiet" disabled={readOnly || busy} onclick={() => run(notNow)}>Not now</button>
        {/if}
      </div>
    {:else if standing.type === 'Installing'}
      {@const share = installed(standing.value.done, standing.value.size)}
      <div role="status">Installing <strong>{name}</strong>.</div>
      <div>
        {bytes(standing.value.done)} of about {bytes(standing.value.size)}. Messages you send wait until it's done.
      </div>
      <progress max="1" value={share} aria-label="Installing {name}">{Math.round(share * 100)}%</progress>
      {#if agents}
        <div class="buttons">
          <button type="button" class="quiet" disabled={readOnly || busy} onclick={() => run(stopInstall)}>
            Stop installing
          </button>
        </div>
      {/if}
    {:else if standing.type === 'Cannot'}
      <div role="status"><strong>{name}</strong> can't be installed: {cannotText(standing.value)}</div>
      <div class="buttons">
        {#if agents && canRetry(standing)}
          <button type="button" disabled={readOnly || busy} onclick={() => run(install)}>Try again</button>
        {:else if agents && checkAgain !== undefined && canCheckAgain(standing)}
          <button type="button" disabled={readOnly || busy} onclick={() => run(checkAgain)}>Check again</button>
        {/if}
        {#if act}
          <button type="button" class="quiet" disabled={readOnly || busy} onclick={() => run(notNow)}>Not now</button>
        {/if}
      </div>
    {:else if standing.type === 'Unknown'}
      <div role="status">Checking whether <strong>{name}</strong> is installed…</div>
    {:else if standing.type === 'Installed' || standing.type === 'Host'}
      <div role="status">Starting <strong>{name}</strong>…</div>
    {:else}
      <div role="status">
        Waiting for <strong>{name}</strong>, which is in a state this viewer doesn't know ({tagOf(standing)}).
      </div>
    {/if}
    {#if problem !== null}<div class="problem" role="alert">{problem}</div>{/if}
  </div>
{/if}

<style>
  .banner {
    padding: 0.55em 0.9em;
    font-size: 0.92em;
    border-bottom: 1px solid var(--caf-border);
    background: var(--caf-caf-bg);
    color: var(--caf-text);
    border-left: 3px solid var(--caf-caf);
    display: flex;
    flex-direction: column;
    gap: 0.45em;
  }
  progress {
    width: 100%;
    max-width: 24em;
  }
  .buttons {
    display: flex;
    gap: 0.5em;
  }
  button {
    font: inherit;
    font-size: 0.92em;
    padding: 0.3em 0.9em;
    border-radius: 6px;
    border: 1px solid var(--caf-accent);
    background: var(--caf-accent);
    color: var(--caf-accent-text);
    cursor: pointer;
  }
  button.quiet {
    background: var(--caf-bg);
    color: var(--caf-text);
    border-color: var(--caf-border);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .problem {
    color: var(--caf-error);
  }
</style>
