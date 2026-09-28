<script lang="ts">
  let { checked = $bindable(false), disabled = false, label }: { checked: boolean; disabled?: boolean; label: string } = $props();
</script>

<button type="button" role="switch" aria-checked={checked} aria-label={label} class:on={checked} {disabled} onclick={() => (checked = !checked)}></button>

<style>
  button { width: 38px; height: 22px; border-radius: 999px; background: var(--line); position: relative; flex: none; cursor: pointer; border: 0; padding: 0; }
  button:disabled { opacity: 0.5; cursor: default; }
  button::after { content: ''; position: absolute; left: 3px; top: 3px; width: 16px; height: 16px; border-radius: 50%; background: #fff; box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25); transition: left 0.15s; }
  button.on { background: var(--accent); }
  button.on::after { left: 19px; }
</style>
