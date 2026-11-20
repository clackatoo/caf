<script lang="ts">
  // Questions waiting for the user, pinned above the composer, oldest
  // first.
  //
  // - A permission question offers the agent's options as it gave them,
  //   with none chosen beforehand: nothing is focused, and Enter answers
  //   nothing, so an option of kind Other is never the default either.
  // - A session slow to open offers keep waiting, retry or stop. Escape
  //   means keep waiting, as dismissing it does.
  import type { Answer, DeepReadonly, Question, QuestionId } from '@clackatoo/caf'
  import { attempt } from './actions.ts'
  import { errorText, optionTone, tagOf } from './words.ts'

  let {
    questions,
    readOnly,
    answer,
  }: {
    questions: DeepReadonly<Question[]>
    readOnly: boolean
    answer: (question: QuestionId, answer: Answer) => Promise<unknown> | void
  } = $props()

  /** Questions being answered now, and why an answer failed. */
  let busy = $state<ReadonlySet<QuestionId>>(new Set())
  let problems = $state<ReadonlyMap<QuestionId, string>>(new Map())

  async function reply(question: QuestionId, given: Answer) {
    if (readOnly || busy.has(question)) return
    busy = new Set([...busy, question])
    const error = await attempt(() => answer(question, given))
    busy = new Set([...busy].filter((id) => id !== question))
    const next = new Map(problems)
    if (error === null) next.delete(question)
    else next.set(question, errorText(error))
    problems = next
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key !== 'Escape' || readOnly || event.defaultPrevented) return
    // Only Escape meant for the chat: pressed in it, or with nothing else focused.
    const target = event.target
    if (target !== document.body && !(target instanceof Element && target.closest('.caf-chat') !== null)) return
    const slow = questions.find((question) => question.kind.type === 'SlowToOpen')
    if (slow === undefined) return
    event.preventDefault()
    void reply(slow.id, { type: 'SlowToOpen', value: { type: 'KeepWaiting' } })
  }
</script>

<svelte:window {onkeydown} />

{#if questions.length > 0}
  <section class="questions" aria-label="Questions">
    <!-- Not keyed by id: a chat may hold two questions with one id, and the
         agent chooses its options' ids. -->
    {#each questions as question, i (i)}
      {@const kind = question.kind}
      <div class="question" data-kind={kind.type}>
        {#if kind.type === 'Permission'}
          <div class="ask">The agent asks permission: <strong>{kind.value.title}</strong></div>
          <div class="options">
            {#each kind.value.options as option, j (j)}
              <button
                type="button"
                class={optionTone(option.kind)}
                disabled={readOnly || busy.has(question.id)}
                onclick={() => reply(question.id, { type: 'Permission', value: option.id })}>{option.label}</button
              >
            {/each}
          </div>
        {:else if kind.type === 'SlowToOpen'}
          <div class="ask">
            {#if kind.value.adapter_answered}
              The session is taking long to open.
            {:else}
              The agent is taking long to start: it hasn't answered at all yet.
            {/if}
          </div>
          <div class="options">
            <button
              type="button"
              disabled={readOnly || busy.has(question.id)}
              onclick={() => reply(question.id, { type: 'SlowToOpen', value: { type: 'KeepWaiting' } })}
              >Keep waiting</button
            >
            <button
              type="button"
              disabled={readOnly || busy.has(question.id)}
              onclick={() => reply(question.id, { type: 'SlowToOpen', value: { type: 'Retry' } })}>Retry</button
            >
            <button
              type="button"
              class="reject"
              disabled={readOnly || busy.has(question.id)}
              onclick={() => reply(question.id, { type: 'SlowToOpen', value: { type: 'Stop' } })}>Stop</button
            >
          </div>
        {:else}
          <div class="ask">A question this viewer doesn't know how to ask ({tagOf(kind)}).</div>
        {/if}
        {#if problems.has(question.id)}<div class="problem">{problems.get(question.id)}</div>{/if}
      </div>
    {/each}
  </section>
{/if}

<style>
  .questions {
    display: flex;
    flex-direction: column;
    gap: 0.5em;
    padding: 0.6em 0.9em;
    border-top: 1px solid var(--caf-border);
    background: var(--caf-panel);
  }
  .question {
    border: 1px solid var(--caf-accent);
    border-radius: 8px;
    padding: 0.6em 0.8em;
    background: var(--caf-bg);
  }
  .ask {
    margin-bottom: 0.5em;
    overflow-wrap: anywhere;
  }
  .options {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em;
  }
  button {
    font: inherit;
    font-size: 0.92em;
    padding: 0.3em 0.9em;
    border-radius: 6px;
    border: 1px solid var(--caf-border);
    background: var(--caf-bg);
    color: var(--caf-text);
    cursor: pointer;
  }
  button:hover:not(:disabled) {
    background: var(--caf-hover);
  }
  button.allow {
    border-color: var(--caf-ok);
    color: var(--caf-ok);
  }
  button.reject {
    border-color: var(--caf-error);
    color: var(--caf-error);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .problem {
    margin-top: 0.4em;
    color: var(--caf-error);
    font-size: 0.9em;
  }
</style>
