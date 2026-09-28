<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import Modal from '../lib/Modal.svelte';
  import { call, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import type { Snapshot } from './types';

  let { snap }: { snap: Snapshot } = $props();
  let removing = $state<{ name: string; fingerprint: string } | null>(null);

  async function remove() {
    const c = removing;
    removing = null;
    if (!c) return;
    try {
      toast(await call<string>('remove_client', { fingerprint: c.fingerprint }), 'ok');
    } catch (e) {
      toast(errorText(e), 'error');
    }
  }
</script>

<div class="head"><h2>已配对客户端</h2><span class="sub">{snap.clients.length} 台</span></div>
<p class="lead">已配对的客户端可以免配对码直接连接。移除后需要重新配对；正在连接的会被断开。</p>

<div class="card">
  {#each snap.clients as c (c.fingerprint)}
    <div class="row">
      <div class="avatar">{c.name.slice(0, 1).toUpperCase()}</div>
      <div class="grow">
        <b>{c.name}</b>
        <span class="mono">{c.fingerprint.slice(0, 16)}…</span>
      </div>
      <span class="muted small">配对于 {c.paired_at}</span>
      <button class="btn sm danger" onclick={() => (removing = c)}><Icon name="trash" size={14} />移除</button>
    </div>
  {:else}
    <div class="empty"><Icon name="users" size={28} /><span>还没有已配对的客户端</span><small>客户端第一次连接时输入“概览”页上的配对码即可</small></div>
  {/each}
</div>

{#if removing}
  <Modal title="移除客户端" onclose={() => (removing = null)}>
    <p>移除“{removing.name}”？它下次连接需要重新输入配对码；如果正在连接，会被断开。</p>
    {#snippet footer()}
      <button class="btn ghost" onclick={() => (removing = null)}>取消</button>
      <button class="btn danger-fill" onclick={remove}>移除</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .lead { color: var(--text-2); margin: -8px 0 16px; font-size: 13.5px; }
  .row { display: flex; align-items: center; gap: 14px; padding: 14px 18px; }
  .row + .row { border-top: 1px solid var(--line); }
  .avatar { width: 36px; height: 36px; border-radius: 10px; background: var(--accent-soft); color: var(--accent-text); display: grid; place-items: center; font-weight: 700; flex: none; }
  .grow { flex: 1; min-width: 0; }
  .grow b { display: block; }
  .grow span { color: var(--text-3); }
  .empty { display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 36px 16px; color: var(--text-3); text-align: center; }
  .empty small { font-size: 12.5px; }
</style>
