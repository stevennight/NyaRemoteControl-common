<script lang="ts">
  // "⋯" button with a small popup menu.
  import Icon from './Icon.svelte';
  let {
    items,
    label = '更多',
    up = false,
  }: { items: { label: string; icon?: string; danger?: boolean; onclick: () => void }[]; label?: string; up?: boolean } = $props();
  let open = $state(false);
</script>

<svelte:window onclick={() => (open = false)} />
<div class="wrap">
  <button
    type="button"
    class="btn icon"
    aria-label={label}
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={(e) => {
      e.stopPropagation();
      open = !open;
    }}
  >
    <Icon name="more" />
  </button>
  {#if open}
    <div class="menu" class:up role="menu">
      {#each items as it (it.label)}
        <button
          type="button"
          role="menuitem"
          class="mi"
          class:danger={it.danger}
          onclick={() => {
            open = false;
            it.onclick();
          }}
        >
          {#if it.icon}<Icon name={it.icon} size={15} />{/if}{it.label}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .wrap { position: relative; }
  .menu { position: absolute; right: 0; top: calc(100% + 6px); background: var(--surface); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow-pop); padding: 5px; min-width: 150px; z-index: 20; }
  .menu.up { top: auto; bottom: calc(100% + 6px); }
  .mi { display: flex; align-items: center; gap: 10px; width: 100%; padding: 7px 10px; border-radius: 6px; border: 0; background: none; color: var(--text); cursor: pointer; font-size: 13.5px; text-align: left; white-space: nowrap; }
  .mi:hover { background: var(--surface-2); }
  .mi.danger { color: var(--danger); }
</style>
