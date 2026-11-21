<script lang="ts">
  // The agent's settings the user may choose, beside the composer: its
  // model and effort as menus of its choices, fast mode as a switch.
  //
  // What's on offer is the agent's to say (see settings.ts). Before the
  // chat's session first opens, it offers what CAF knows the agent
  // offers, if it knows, or, if the viewer may act on agents, to learn
  // it: a choice made then is applied as the session opens. A change
  // shows as waiting until the agent takes it: from the next message
  // while a turn runs or the session isn't open, as CAF holds it until
  // then, and otherwise at once.
  //
  // While a change is being sent, the controls stay as they are, focus
  // and all, and the bar says it's busy: what's chosen meanwhile is put
  // back, not sent.
  import type { DeepReadonly, KnownSettings, Setting, SettingChoice, Settings, SettingValue } from '@clackatoo/caf'
  import { attempt } from './actions.ts'
  import { offered, runs, standing, valueName } from './settings.ts'
  import { errorText, tagOf } from './words.ts'

  let {
    settings,
    known,
    held,
    disabled,
    canLearn,
    set,
    learn,
  }: {
    /** The chat's settings: what its agent offers, and what was chosen. */
    settings: DeepReadonly<Settings>
    /** What CAF knows of the settings the chat's agent offers, from its status, if the host gave it. */
    known: DeepReadonly<KnownSettings> | undefined
    /** A change made now waits for the next message: a turn runs, or the session isn't open. */
    held: boolean
    /** Nothing can be chosen: looking back, the chat is closed, or the host doesn't let this viewer act. */
    disabled: boolean
    /** Whether the user may ask CAF to learn what the agent offers: the host lets this viewer act, on chats and agents. */
    canLearn: boolean
    set: (choice: SettingChoice) => Promise<unknown> | void
    learn: () => Promise<unknown> | void
  } = $props()

  /** This bar's own, for its elements' ids: two chats may show on one page. */
  const uid = $props.id()
  const shown = $derived(offered(settings, known))
  /** The change being sent, if one is: its control shows it meanwhile. */
  let sending = $state.raw<{ kind: string; value: SettingValue } | null>(null)
  let learning = $state(false)
  const busy = $derived(sending !== null || learning)
  let problem = $state<string | null>(null)

  /**
   * Chooses `value` for a setting, its control showing `was` before.
   * `show` puts a value in the control: what it showed, if the choice
   * isn't sent, because another is being sent, or isn't taken.
   */
  async function choose(
    setting: DeepReadonly<Setting>,
    value: SettingValue,
    was: DeepReadonly<SettingValue> | null,
    show: (value: DeepReadonly<SettingValue> | null) => void,
  ) {
    if (disabled || busy) {
      show(sending?.kind === setting.kind.type ? sending.value : was)
      return
    }
    sending = { kind: setting.kind.type, value }
    problem = null
    const error = await attempt(() => set({ kind: setting.kind, value }))
    sending = null
    if (error !== null) {
      problem = errorText(error)
      show(was)
    }
  }

  async function learnNow() {
    if (!canLearn || busy) return
    learning = true
    problem = null
    const error = await attempt(learn)
    learning = false
    if (error !== null) problem = errorText(error)
  }

  /**
   * A change waiting, as the user reads it: "Sonnet, from your next
   * message", "Changing to Sonnet…", "Turning fast mode on…". Empty with
   * none waiting.
   */
  function waitingText(setting: DeepReadonly<Setting>, value: DeepReadonly<SettingValue> | null): string {
    if (value === null) return ''
    const on = valueName(setting, value).toLowerCase()
    if (setting.form.type === 'Switch') {
      return held ? `${setting.name} ${on}, from your next message` : `Turning ${lowerFirst(setting.name)} ${on}…`
    }
    const name = valueName(setting, value)
    return held ? `${name}, from your next message` : `Changing to ${name}…`
  }

  /** "Fast mode" as it reads mid-sentence: "fast mode". */
  function lowerFirst(text: string): string {
    return text.charAt(0).toLowerCase() + text.slice(1)
  }

  /** The ids of what describes a setting's control: what it says of itself, and its change waiting. */
  function describedBy(kind: string, about: boolean): string {
    return about ? `${uid}-${kind}-about ${uid}-${kind}-waiting` : `${uid}-${kind}-waiting`
  }
</script>

{#if shown.length > 0}
  <div class="caf-settings" role="group" aria-label="The agent's settings" aria-busy={busy}>
    <div class="row">
      {#each shown as setting (setting.kind.type)}
        {@const { value, waiting } = standing(setting, settings.chosen)}
        {@const kind = setting.kind.type}
        <div class="setting" data-kind={kind}>
          {#if setting.form.type === 'Choices'}
            {@const choices = setting.form.value}
            {@const id = value?.type === 'Choice' ? value.value : ''}
            {@const about = choices.find((choice) => choice.id === id)?.description ?? null}
            <label>
              <span class="name">{setting.name}</span>
              <select
                value={id}
                {disabled}
                aria-describedby={describedBy(kind, about !== null)}
                onchange={(event) => {
                  const select = event.currentTarget
                  const put = (shown: DeepReadonly<SettingValue> | null) =>
                    (select.value = shown?.type === 'Choice' ? shown.value : '')
                  void choose(setting, { type: 'Choice', value: select.value }, value, put)
                }}
              >
                {#if id === ''}<option value="" disabled>Not said</option>{/if}
                <!-- Chosen before the agent said what it offers, or no longer offered: still shown. -->
                {#if id !== '' && !choices.some((choice) => choice.id === id)}<option value={id}>{id}</option>{/if}
                {#each runs(choices) as run, r (r)}
                  {#if run.group === null}
                    {#each run.choices as choice (choice.id)}
                      <option value={choice.id} title={choice.description}>{choice.name}</option>
                    {/each}
                  {:else}
                    <optgroup label={run.group}>
                      {#each run.choices as choice (choice.id)}
                        <option value={choice.id} title={choice.description}>{choice.name}</option>
                      {/each}
                    </optgroup>
                  {/if}
                {/each}
              </select>
            </label>
            {#if about !== null}<span class="about" id="{uid}-{kind}-about">{about}</span>{/if}
          {:else if setting.form.type === 'Switch'}
            <label class="switch">
              <input
                type="checkbox"
                checked={value?.type === 'Switch' && value.value}
                {disabled}
                aria-describedby={describedBy(kind, kind === 'Fast')}
                onchange={(event) => {
                  const box = event.currentTarget
                  const put = (shown: DeepReadonly<SettingValue> | null) => (box.checked = shown?.type === 'Switch' && shown.value)
                  void choose(setting, { type: 'Switch', value: box.checked }, value, put)
                }}
              />
              <span class="name">{setting.name}</span>
            </label>
            {#if kind === 'Fast'}<span class="about" id="{uid}-{kind}-about">May cost more.</span>{/if}
          {:else}
            <span class="name">{setting.name}</span>
            <span class="about">A kind of setting this viewer can't show ({tagOf(setting.form)}).</span>
          {/if}
          <!-- Always there, so a change that comes to wait is read out as its text changes. -->
          <span class="waiting" id="{uid}-{kind}-waiting" role="status">{waitingText(setting, waiting)}</span>
        </div>
      {/each}
    </div>
    {#if problem !== null}<div class="problem" role="alert">{problem}</div>{/if}
  </div>
{:else if known?.type === 'Learning'}
  <div class="caf-settings"><span role="status">Learning what the agent offers, such as its models…</span></div>
{:else if (known?.type === 'Unknown' || known?.type === 'Failed') && canLearn}
  <div class="caf-settings">
    <div class="row">
      {#if known.type === 'Failed'}
        <span>What the agent offers couldn't be learned: {known.value}</span>
      {:else}
        <span>What the agent offers, such as its models, shows once its session opens.</span>
      {/if}
      <button type="button" aria-busy={learning} onclick={learnNow}>{known.type === 'Failed' ? 'Try again' : 'Learn it now'}</button>
    </div>
    {#if problem !== null}<div class="problem" role="alert">{problem}</div>{/if}
  </div>
{/if}

<style>
  .caf-settings {
    padding: 0.45em 0.9em;
    border-top: 1px solid var(--caf-border);
    background: var(--caf-bg);
    font-size: 0.85em;
    color: var(--caf-muted);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4em 1.2em;
  }
  .setting {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.2em 0.5em;
    min-width: 0;
  }
  label {
    display: flex;
    align-items: center;
    gap: 0.4em;
  }
  .name {
    color: var(--caf-text);
  }
  select,
  button {
    font: inherit;
    color: var(--caf-text);
    background: var(--caf-bg);
    border: 1px solid var(--caf-border);
    border-radius: 6px;
    padding: 0.15em 0.4em;
  }
  button {
    padding: 0.15em 0.7em;
    cursor: pointer;
  }
  select:disabled,
  input:disabled,
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .about {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .waiting {
    color: var(--caf-accent);
  }
  .problem {
    color: var(--caf-error);
    margin-top: 0.3em;
  }
</style>
