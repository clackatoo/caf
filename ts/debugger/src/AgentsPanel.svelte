<script lang="ts">
  // The agents CAF offers, beside the list of chats: whether each is
  // installed and signed in, with Install or Stop, Sign in or Sign out,
  // and, while one is being signed in to, its link, code and paste box;
  // and its account's limits, as it last said them.
  import type { AgentName, AgentStatus, DeepReadonly } from '@clackatoo/caf'
  import { attempt } from './chat/actions.ts'
  import SignIn from './chat/SignIn.svelte'
  import Limits from './Limits.svelte'
  import { bytes, cannotText, canRetry, errorText, installed, installLabel, signInLabel } from './chat/words.ts'

  /** What the user can do about an agent. */
  export interface AgentActions {
    install(agent: AgentName): Promise<unknown> | void
    stopInstall(agent: AgentName): Promise<unknown> | void
    signIn(agent: AgentName): Promise<unknown> | void
    paste(agent: AgentName, text: string): Promise<unknown> | void
    stopSignIn(agent: AgentName): Promise<unknown> | void
    signOut(agent: AgentName): Promise<unknown> | void
  }

  let {
    agents,
    actions,
    readOnly = false,
    may = true,
    inChat = null,
  }: {
    agents: readonly DeepReadonly<AgentStatus>[]
    actions: AgentActions
    readOnly?: boolean
    /** Whether the host lets this viewer install agents and sign in and out: without it, it only shows where each stands. */
    may?: boolean
    /** The agent of the chat shown beside, whose sign-in, link and paste box and all, the chat already shows. */
    inChat?: AgentName | null
  } = $props()

  /** The time limits are measured from: ticked, so what's left of a window until it resets counts down. */
  let now = $state(Date.now())
  $effect(() => {
    const tick = setInterval(() => (now = Date.now()), 15_000)
    return () => clearInterval(tick)
  })

  /** Agents with something being done to them now, and why something failed. */
  let busy = $state<ReadonlySet<AgentName>>(new Set())
  let problems = $state<ReadonlyMap<AgentName, string>>(new Map())

  async function run(agent: AgentName, action: (agent: AgentName) => Promise<unknown> | void) {
    if (readOnly || busy.has(agent)) return
    busy = new Set([...busy, agent])
    const error = await attempt(() => action(agent))
    busy = new Set([...busy].filter((name) => name !== agent))
    const next = new Map(problems)
    if (error === null) next.delete(agent)
    else next.set(agent, errorText(error))
    problems = next
  }

  /** Whether an agent can be started now, so it can be signed in to or out of. */
  function ready(agent: DeepReadonly<AgentStatus>): boolean {
    return agent.install.type === 'Installed' || agent.install.type === 'Host'
  }
</script>

<section class="agents" aria-label="Agents">
  <div class="heading">Agents</div>
  <ul>
    {#each agents as agent (agent.name)}
      {@const install = agent.install}
      {@const signIn = agent.sign_in}
      {@const working = readOnly || busy.has(agent.name)}
      <li class="agent" data-agent={agent.name} data-install={install.type} data-sign-in={signIn.type}>
        <div class="name">{agent.name}</div>
        <div class="state">{installLabel(install)}</div>
        {#if install.type === 'Missing'}
          <div class="detail">About {bytes(install.value.size)} to download.</div>
        {:else if install.type === 'Installing'}
          {@const share = installed(install.value.done, install.value.size)}
          <progress max="1" value={share} aria-label="Installing {agent.name}">{Math.round(share * 100)}%</progress>
          <div class="detail">{bytes(install.value.done)} of about {bytes(install.value.size)}</div>
        {:else if install.type === 'Cannot'}
          <div class="detail">It can't be installed: {cannotText(install.value)}</div>
        {/if}
        {#if ready(agent)}<div class="state">{signInLabel(signIn)}</div>{/if}
        <Limits name={agent.name} limits={agent.limits} {now} />
        {#if may}
          <div class="buttons">
            {#if install.type === 'Missing'}
              <button
                type="button"
                aria-label="Install {agent.name}"
                disabled={working}
                onclick={() => run(agent.name, actions.install)}>Install</button
              >
            {:else if canRetry(install)}
              <button
                type="button"
                aria-label="Try again to install {agent.name}"
                disabled={working}
                onclick={() => run(agent.name, actions.install)}>Try again</button
              >
            {:else if install.type === 'Installing'}
              <button
                type="button"
                aria-label="Stop installing {agent.name}"
                disabled={working}
                onclick={() => run(agent.name, actions.stopInstall)}>Stop installing</button
              >
            {/if}
            <!-- Only where CAF can sign in now, as the agent's status says; again
                 while signed in, to switch accounts. -->
            {#if agent.can_sign_in && signIn.type !== 'SigningIn'}
              <button
                type="button"
                aria-label="Sign in to {agent.name}"
                disabled={working}
                onclick={() => run(agent.name, actions.signIn)}>Sign in</button
              >
            {/if}
            {#if ready(agent) && signIn.type === 'SignedIn'}
              <button
                type="button"
                aria-label="Sign out of {agent.name}"
                disabled={working}
                onclick={() => run(agent.name, actions.signOut)}>Sign out</button
              >
            {/if}
          </div>
          {#if signIn.type === 'SigningIn'}
            {#if agent.name === inChat}
              <div class="detail">Signing in: see the chat.</div>
            {:else}
              <SignIn
                name={agent.name}
                link={signIn.value}
                {readOnly}
                paste={(text) => actions.paste(agent.name, text)}
                stop={() => actions.stopSignIn(agent.name)}
              />
            {/if}
          {/if}
        {/if}
        {#if problems.has(agent.name)}<div class="problem" role="alert">{problems.get(agent.name)}</div>{/if}
      </li>
    {:else}
      <li class="none">No agents offered.</li>
    {/each}
  </ul>
</section>

<style>
  .agents {
    border-top: 1px solid var(--caf-border);
    background: var(--caf-panel);
    font-size: 0.9em;
    overflow-y: auto;
    max-height: 45%;
  }
  .heading {
    font-weight: 600;
    padding: 0.5em 0.8em 0.2em;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .agent {
    display: flex;
    flex-direction: column;
    gap: 0.3em;
    padding: 0.5em 0.8em;
    border-bottom: 1px solid var(--caf-border);
  }
  .name {
    font-weight: 600;
  }
  .state {
    color: var(--caf-muted);
  }
  .detail {
    font-size: 0.92em;
    color: var(--caf-muted);
    overflow-wrap: anywhere;
  }
  progress {
    width: 100%;
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em;
  }
  .buttons:empty {
    display: none;
  }
  button {
    font: inherit;
    font-size: 0.92em;
    padding: 0.2em 0.7em;
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
  }
  .none {
    padding: 0.5em 0.8em;
    color: var(--caf-faint);
  }
</style>
