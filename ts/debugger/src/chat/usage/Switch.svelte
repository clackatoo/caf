<script lang="ts" generics="T extends string">
  // A few buttons, one pressed: the page's choices of period, measure and
  // grouping.
  let {
    options,
    value = $bindable(),
    label,
    small = false,
  }: {
    options: readonly { value: T; label: string }[]
    value: T
    /** What it chooses, for a screen reader. */
    label: string
    small?: boolean
  } = $props()
</script>

<div class="switch" class:small role="group" aria-label={label}>
  {#each options as option (option.value)}
    <button type="button" aria-pressed={value === option.value} onclick={() => (value = option.value)}>{option.label}</button>
  {/each}
</div>

<style>
  .switch {
    display: inline-flex;
    background: var(--caf-inset);
    border: 1px solid var(--caf-border);
    border-radius: 8px;
    padding: 2px;
  }
  button {
    font: inherit;
    font-size: 0.9em;
    padding: 0.2em 0.75em;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--caf-muted);
    white-space: nowrap;
    cursor: pointer;
  }
  .small button {
    font-size: 0.85em;
    padding: 0.15em 0.6em;
  }
  button[aria-pressed='true'] {
    background: var(--caf-bg);
    color: var(--caf-text);
    box-shadow: 0 1px 2px rgb(0 0 0 / 0.12);
  }
</style>
