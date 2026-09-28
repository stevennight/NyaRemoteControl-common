<script lang="ts">
  import Icon from './Icon.svelte';
  import { toasts, dismiss } from './notify.svelte';
</script>

<div class="toasts" aria-live="polite">
  {#each toasts as t (t.id)}
    <div class="toast {t.kind}">
      <Icon name={t.kind === 'error' ? 'alert' : t.kind === 'ok' ? 'check' : 'info'} size={16} />
      <span class="selectable">{t.text}</span>
      <button type="button" aria-label="关闭" onclick={() => dismiss(t.id)}><Icon name="x" size={14} /></button>
    </div>
  {/each}
</div>

<style>
  .toasts { position: fixed; right: 16px; bottom: 16px; display: flex; flex-direction: column; gap: 8px; z-index: 60; max-width: min(420px, calc(100vw - 32px)); }
  .toast { display: flex; align-items: flex-start; gap: 10px; background: var(--surface); border: 1px solid var(--line); box-shadow: var(--shadow-pop); border-radius: 10px; padding: 10px 12px; font-size: 13.5px; animation: in 0.15s; }
  .toast.ok :global(svg) { color: var(--ok); }
  .toast.error { border-color: var(--danger); }
  .toast.error :global(svg) { color: var(--danger); }
  .toast span { flex: 1; word-break: break-word; }
  button { border: 0; background: none; color: var(--text-3); cursor: pointer; padding: 2px; }
  @keyframes in { from { transform: translateY(8px); opacity: 0; } }
</style>
