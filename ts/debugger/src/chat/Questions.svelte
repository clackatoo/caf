<script lang="ts">
  // Questions waiting for the user, pinned above the composer, oldest
  // first.
  //
  // - A permission question offers the agent's options as it gave them,
  //   with none chosen beforehand: nothing is focused, and Enter answers
  //   nothing, so an option of kind Other is never the default either.
  //   Once CAF has seen the call it's about, it shows what the call
  //   would do: the command it runs, or the change it makes. A question
  //   about a call to one of CAF's own tools is CAF's own, not the
  //   agent's: it's marked as CAF's, says in CAF's words what the agent
  //   wants to do, and what it's about (the file, or the command), never
  //   the title, which would say it twice, and offers CAF's Allow and
  //   Refuse.
  // - A session slow to open offers keep waiting, retry or stop. Escape
  //   means keep waiting, as dismissing it does.
  import type { Answer, DeepReadonly, ItemId, Question, QuestionId, ToolCall } from '@clackatoo/caf'
  import { attempt } from './actions.ts'
  import DiffView from './DiffView.svelte'
  import { cafAbout, cafAsks } from './tools.ts'
  import { unhidden } from './unhidden.ts'
  import { errorText, optionTone, tagOf } from './words.ts'

  let {
    questions,
    toolOf = () => null,
    readOnly,
    answer,
  }: {
    questions: DeepReadonly<Question[]>
    /** The tool call an item is, if it's one: what a permission question is about. */
    toolOf?: (id: ItemId) => DeepReadonly<ToolCall> | null
    readOnly: boolean
    answer: (question: QuestionId, answer: Answer) => Promise<unknown> | void
  } = $props()

  /**
   * Each question with its key: its id, and how many questions before it
   * share that id (a chat may hold two with one id). Keyed so, an answered
   * question's buttons go with it: were they reused for the next question,
   * the focus left on one would answer that question unseen.
   */
  const keyed = $derived.by(() => {
    const seen = new Map<QuestionId, number>()
    return questions.map((question) => {
      const n = seen.get(question.id) ?? 0
      seen.set(question.id, n + 1)
      return { question, key: `${question.id}#${n}` }
    })
  })

  let section = $state<HTMLElement>()
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
    // Only Escape meant for this chat: pressed in this view, or with
    // nothing focused on a page that shows only this one.
    const root = section?.closest('.caf-chat')
    if (root == null) return
    const target = event.target
    const inside = target instanceof Node && root.contains(target)
    const alone = target === document.body && document.querySelectorAll('.caf-chat').length === 1
    if (!inside && !alone) return
    const slow = questions.find((question) => question.kind.type === 'SlowToOpen')
    if (slow === undefined) return
    event.preventDefault()
    void reply(slow.id, { type: 'SlowToOpen', value: { type: 'KeepWaiting' } })
  }
</script>

<svelte:window {onkeydown} />

{#if questions.length > 0}
  <section class="questions" aria-label="Questions" bind:this={section}>
    {#each keyed as { question, key } (key)}
      {@const kind = question.kind}
      <div
        class="question"
        data-kind={kind.type}
        data-caf={kind.type === 'Permission' && kind.value.tool !== null && toolOf(kind.value.tool)?.origin.type === 'Caf'}
      >
        {#if kind.type === 'Permission'}
          {@const tool = kind.value.tool === null ? null : toolOf(kind.value.tool)}
          {@const action = tool?.action ?? null}
          {@const diffs = tool?.result.flatMap((piece) => (piece.type === 'Diff' ? [piece.value] : [])) ?? []}
          {#if tool?.origin.type === 'Caf'}
            <div class="ask"><span class="who">CAF</span> {cafAsks(action, 'The agent')}: <strong
                >{unhidden(cafAbout(tool) ?? kind.value.title)}</strong
              ></div>
          {:else}
            <div class="ask">The agent asks permission: <strong>{kind.value.title}</strong></div>
          {/if}
          {#if action?.type === 'Run' || diffs.length > 0}
            <div class="about">
              {#if action?.type === 'Run'}
                <pre class="command">{unhidden(action.value.command)}</pre>
                <div class="folder">In {unhidden(action.value.folder)}{action.value.background ? ', in the background' : ''}</div>
              {/if}
              {#each diffs as diff, d (d)}<DiffView {diff} />{/each}
            </div>
          {/if}
          <div class="options">
            <!-- By position: the agent chooses its options' ids, or CAF does for its own. -->
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
  .who {
    color: var(--caf-caf);
    font-weight: 600;
    font-size: 0.85em;
    letter-spacing: 0.04em;
    margin-right: 0.6em;
  }
  .about {
    display: flex;
    flex-direction: column;
    gap: 0.4em;
    margin-bottom: 0.6em;
    max-height: 40vh;
    overflow: auto;
  }
  .command {
    margin: 0;
    background: var(--caf-inset);
    border-radius: 4px;
    padding: 0.5em 0.6em;
    font-family: var(--caf-mono);
    font-size: 0.9em;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .folder {
    color: var(--caf-muted);
    font-size: 0.85em;
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
