<script lang="ts" generics="T">
  // Segmented control: one of a few options.
  let {
    options,
    value = $bindable(),
    disabled = false,
    label,
  }: { options: { value: T; label: string; title?: string }[]; value: T; disabled?: boolean; label?: string } = $props();
</script>

<div class="seg" role="radiogroup" aria-label={label} class:disabled>
  {#each options as o (o.label)}
    <button
      type="button"
      role="radio"
      aria-checked={o.value === value}
      class:on={o.value === value}
      title={o.title}
      {disabled}
      onclick={() => (value = o.value)}>{o.label}</button
    >
  {/each}
</div>

<style>
  .seg { display: inline-flex; background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; padding: 2px; gap: 2px; flex-wrap: wrap; }
  .seg.disabled { opacity: 0.5; }
  button { border: 0; background: transparent; padding: 4px 12px; border-radius: 6px; cursor: pointer; color: var(--text-2); font-size: 13px; white-space: nowrap; }
  button:hover:not(:disabled) { color: var(--text); }
  button.on { background: var(--surface); color: var(--text); box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12); font-weight: 600; }
</style>
