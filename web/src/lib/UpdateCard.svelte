<script lang="ts" module>
  /** Update state, as the client (client/src/app/update.rs) and the host (control.proto UpdateStatus) report it. */
  export type UpdateInfo = {
    state: 'idle' | 'checking' | 'up_to_date' | 'available' | 'downloading' | 'installing' | 'failed' | 'unavailable';
    current: string;
    latest: string;
    notes: string;
    page: string;
    progress: number;
    message: string;
    checked_unix: number;
  };
</script>

<script lang="ts">
  import Icon from './Icon.svelte';
  import { call } from './ipc';

  let {
    info,
    current,
    note = '',
    oncheck,
    oninstall,
  }: {
    info: UpdateInfo | null;
    /** Shown before the first check. */
    current: string;
    /** What installing does here (restarts, reconnects…). */
    note?: string;
    oncheck: () => void;
    oninstall: () => void;
  } = $props();

  let showNotes = $state(false);
  const s = $derived(info?.state ?? 'idle');
  const busy = $derived(s === 'checking' || s === 'downloading' || s === 'installing');

  function checked(unix: number): string {
    if (!unix) return '';
    const d = new Date(unix * 1000);
    return `，检查于 ${d.toLocaleString('zh-CN', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' })}`;
  }
</script>

<div class="card panel upd">
  <h4>
    版本与更新
    <span class="r">
      {#if s === 'available'}
        <button class="btn sm primary" onclick={oninstall}><Icon name="download" size={14} />更新到 {info?.latest}</button>
      {:else}
        <button class="btn sm" onclick={oncheck} disabled={busy}><Icon name="refresh" size={14} />{s === 'checking' ? '检查中…' : '检查更新'}</button>
      {/if}
    </span>
  </h4>
  <div class="line">
    <span>当前版本 <b class="mono">{info?.current || current}</b></span>
    {#if s === 'up_to_date'}<span class="chip ok">已是最新{checked(info?.checked_unix ?? 0)}</span>
    {:else if s === 'available'}<span class="chip warn">有新版本 {info?.latest}</span>
    {:else if s === 'downloading'}<span class="chip">正在下载 {info?.progress ?? 0}%</span>
    {:else if s === 'installing'}<span class="chip">正在安装…</span>
    {:else if s === 'failed'}<span class="chip err">出错</span>{/if}
  </div>
  {#if s === 'downloading'}
    <div class="bar"><i style:width="{info?.progress ?? 0}%"></i></div>
  {/if}
  {#if info?.message && s !== 'up_to_date'}<div class="msg" class:err={s === 'failed'}>{info.message}</div>{/if}
  {#if s === 'available'}
    {#if note}<div class="hint">{note}</div>{/if}
    <div class="links">
      {#if info?.notes}<button class="link" onclick={() => (showNotes = !showNotes)}>{showNotes ? '收起更新内容' : '查看更新内容'}</button>{/if}
      {#if info?.page}<button class="link" onclick={() => call('open_url', { url: info?.page })}>发布页</button>{/if}
    </div>
    {#if showNotes}<pre class="notes">{info?.notes}</pre>{/if}
  {/if}
</div>

<style>
  .upd { padding: 16px 18px; margin-bottom: 14px; }
  h4 { margin: 0 0 8px; font-size: 13px; color: var(--text-3); font-weight: 600; display: flex; align-items: center; gap: 8px; min-height: 28px; }
  h4 .r { margin-left: auto; display: flex; gap: 6px; }
  .line { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; padding: 4px 0; color: var(--text-2); }
  .bar { height: 6px; border-radius: 3px; background: var(--surface-2); overflow: hidden; margin: 8px 0 2px; }
  .bar i { display: block; height: 100%; background: var(--accent); transition: width 0.3s; }
  .msg { font-size: 13px; color: var(--text-2); margin-top: 6px; }
  .msg.err { color: var(--danger); }
  .hint { font-size: 12.5px; color: var(--text-3); margin-top: 6px; }
  .links { display: flex; gap: 14px; margin-top: 6px; }
  .link { border: 0; background: none; color: var(--accent-text); cursor: pointer; padding: 0; font: inherit; font-size: 13px; }
  .notes { white-space: pre-wrap; font-size: 12.5px; max-height: 220px; overflow: auto; background: var(--surface-2); border-radius: 8px; padding: 10px; margin: 8px 0 0; }
</style>
