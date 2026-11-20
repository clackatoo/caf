<script lang="ts">
  // The one place the chat view puts HTML in the page as it is: Markdown
  // rendered with HTML off, and only http(s) links (see markdown.ts).
  //
  // While words stream in, the whole text is rendered again, so it's done
  // at most once every `EVERY` ms rather than on every chunk. And while
  // the user has text selected in it, it waits: putting in new HTML would
  // drop their selection, so they couldn't copy from a reply as it comes.
  import { untrack } from 'svelte'
  import { render } from './markdown.ts'

  let { text }: { text: string } = $props()

  const EVERY = 50
  let element = $state<HTMLElement>()
  let html = $state(untrack(() => render(text)))
  let shown = untrack(() => text)
  let timer: ReturnType<typeof setTimeout> | null = null

  $effect(() => {
    if (text !== shown && timer === null) timer = setTimeout(flush, EVERY)
  })

  $effect(() => () => {
    if (timer !== null) clearTimeout(timer)
    document.removeEventListener('selectionchange', onselection)
  })

  function flush() {
    timer = null
    if (selectedInside()) {
      document.addEventListener('selectionchange', onselection)
      return
    }
    shown = text
    html = render(text)
  }

  function onselection() {
    if (selectedInside()) return
    document.removeEventListener('selectionchange', onselection)
    if (text !== shown && timer === null) timer = setTimeout(flush, 0)
  }

  function selectedInside(): boolean {
    const selection = document.getSelection()
    if (element === undefined || selection === null || selection.isCollapsed || selection.rangeCount === 0) return false
    return element.contains(selection.getRangeAt(0).commonAncestorContainer)
  }
</script>

<div class="markdown" bind:this={element}>{@html html}</div>

<style>
  .markdown {
    line-height: 1.55;
    overflow-wrap: anywhere;
  }
  .markdown :global(:first-child) {
    margin-top: 0;
  }
  .markdown :global(:last-child) {
    margin-bottom: 0;
  }
  .markdown :global(p),
  .markdown :global(ul),
  .markdown :global(ol),
  .markdown :global(pre),
  .markdown :global(blockquote),
  .markdown :global(table) {
    margin: 0.5em 0;
  }
  .markdown :global(code) {
    font-family: var(--caf-mono);
    font-size: 0.9em;
    background: var(--caf-inset);
    border-radius: 4px;
    padding: 0.1em 0.3em;
  }
  .markdown :global(pre) {
    background: var(--caf-inset);
    border: 1px solid var(--caf-border);
    border-radius: 6px;
    padding: 0.6em 0.8em;
    overflow-x: auto;
  }
  .markdown :global(pre code) {
    background: none;
    padding: 0;
  }
  .markdown :global(blockquote) {
    border-left: 3px solid var(--caf-border);
    padding-left: 0.8em;
    color: var(--caf-muted);
  }
  .markdown :global(a) {
    color: var(--caf-accent);
  }
  .markdown :global(table) {
    border-collapse: collapse;
  }
  .markdown :global(th),
  .markdown :global(td) {
    border: 1px solid var(--caf-border);
    padding: 0.25em 0.6em;
  }
</style>
