<script lang="ts">
  import type { Snippet } from 'svelte';
  let {
    title,
    onclose,
    width = 420,
    children,
    footer,
  }: { title: string; onclose?: () => void; width?: number; children: Snippet; footer?: Snippet } = $props();

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape' && onclose) onclose();
  }
</script>

<svelte:window onkeydown={key} />
<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose?.()}>
  <div class="modal" role="dialog" aria-modal="true" aria-label={title} style:width="{width}px">
    <h3>{title}</h3>
    {@render children()}
    {#if footer}<div class="foot">{@render footer()}</div>{/if}
  </div>
</div>

<style>
  .scrim { position: fixed; inset: 0; background: rgba(10, 12, 16, 0.38); display: grid; place-items: center; z-index: 50; backdrop-filter: blur(2px); animation: fade 0.12s; }
  .modal { max-width: calc(100vw - 32px); max-height: calc(100vh - 32px); overflow: auto; background: var(--surface); border-radius: 14px; box-shadow: var(--shadow-pop); padding: 22px; border: 1px solid var(--line); animation: pop 0.14s; }
  h3 { margin: 0 0 6px; font-size: 17px; }
  .modal :global(p) { margin: 0 0 14px; color: var(--text-2); font-size: 13.5px; }
  .foot { display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px; flex-wrap: wrap; }
  @keyframes fade { from { opacity: 0; } }
  @keyframes pop { from { transform: translateY(6px) scale(0.98); opacity: 0; } }
</style>
