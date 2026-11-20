<script lang="ts">
  // The one place the chat view puts HTML in the page as it is: Markdown
  // rendered with HTML off, and only http(s) links (see markdown.ts).
  import { render } from './markdown.ts'

  let { text }: { text: string } = $props()

  const html = $derived(render(text))
</script>

<div class="markdown">{@html html}</div>

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
