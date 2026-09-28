<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import Menu from '../lib/Menu.svelte';
  import { call, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import { ago, hue, type ClientState, type Host } from './types';

  let {
    cs,
    onadd,
    onrename,
    ondelete,
  }: { cs: ClientState; onadd: () => void; onrename: (h: Host) => void; ondelete: (h: Host) => void } = $props();

  let address = $state('');

  function connect(address: string, name?: string) {
    call('connect', { address, name: name ?? null }).catch((e) => toast(errorText(e), 'error'));
  }

  function quick(e: SubmitEvent) {
    e.preventDefault();
    const a = address.trim();
    if (a) connect(a);
  }

  function copy(text: string) {
    navigator.clipboard.writeText(text).then(() => toast('已复制地址', 'ok'));
  }
</script>

<div class="head">
  <h2>设备</h2>
  <span class="sub">{cs.hosts.length} 台</span>
  <div class="right">
    <button class="btn" onclick={onadd}><Icon name="plus" size={16} />添加设备</button>
  </div>
</div>

<form class="quick card" onsubmit={quick}>
  <Icon name="link" />
  <input bind:value={address} placeholder="输入地址直接连接，例如 100.64.0.2 或 host:47100" aria-label="地址" spellcheck="false" />
  <button class="btn primary" type="submit" disabled={!address.trim()}>连接</button>
</form>

{#if cs.hosts.length}
  <div class="section-title">已保存的设备</div>
{/if}
<div class="grid">
  {#each cs.hosts as h (h.address)}
    <div class="card dev">
      <button class="thumb" style:--h={hue(h.name)} onclick={() => connect(h.address, h.name)} aria-label="连接 {h.name}">
        <span class="initial">{h.name.slice(0, 1).toUpperCase()}</span>
        <span class="go"><Icon name="play" size={16} />连接</span>
      </button>
      <div class="info">
        <div class="name" title={h.name}>{h.name}</div>
        <div class="meta">
          <span class="mono addr" title={h.address}>{h.address}</span>
          {#if h.paired}<span class="chip ok">已配对</span>{:else}<span class="chip warn">未配对</span>{/if}
        </div>
        <div class="meta">{h.paired ? `上次连接：${ago(h.last_connected)}` : '第一次连接需要配对码'}</div>
      </div>
      <div class="actions">
        <button class="btn primary" onclick={() => connect(h.address, h.name)}>连接</button>
        <Menu
          up
          items={[
            { label: '改名', icon: 'edit', onclick: () => onrename(h) },
            { label: '复制地址', icon: 'copy', onclick: () => copy(h.address) },
            { label: '删除', icon: 'trash', danger: true, onclick: () => ondelete(h) },
          ]}
        />
      </div>
    </div>
  {/each}
  <button class="add" onclick={onadd}>
    <Icon name="plus" size={26} />
    <span>添加设备</span>
    {#if !cs.hosts.length}<small>输入被控端的地址（Tailscale / EasyTier 等组网后的 IP）</small>{/if}
  </button>
</div>

<style>
  .quick { display: flex; gap: 10px; align-items: center; padding: 8px 8px 8px 16px; margin-bottom: 22px; color: var(--text-3); }
  .quick input { flex: 1; border: 0; outline: 0; background: transparent; color: var(--text); min-width: 0; padding: 4px 0; }
  .quick input::placeholder { color: var(--text-3); }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 14px; }
  .dev { padding: 12px; display: flex; flex-direction: column; gap: 12px; min-width: 0; }
  .thumb {
    height: 104px; border-radius: 8px; border: 0; cursor: pointer; position: relative; overflow: hidden;
    background: linear-gradient(135deg, hsl(var(--h) 45% 42%), hsl(calc(var(--h) + 40) 40% 22%));
    display: grid; place-items: center; color: #fff;
  }
  .initial { font-size: 40px; font-weight: 700; opacity: 0.9; letter-spacing: 1px; }
  .go { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; gap: 6px; background: rgba(0, 0, 0, 0.45); opacity: 0; transition: opacity 0.12s; font-weight: 600; }
  .thumb:hover .go, .thumb:focus-visible .go { opacity: 1; }
  .info { min-width: 0; }
  .name { font-weight: 650; font-size: 15px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meta { color: var(--text-3); font-size: 12.5px; display: flex; gap: 6px; align-items: center; min-width: 0; }
  .addr { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .actions { display: flex; gap: 8px; margin-top: auto; }
  .actions .primary { flex: 1; }
  .add { border: 1.5px dashed var(--line); background: transparent; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 6px; min-height: 236px; color: var(--text-3); cursor: pointer; border-radius: 12px; padding: 16px; text-align: center; }
  .add:hover { color: var(--accent-text); border-color: var(--accent); background: var(--accent-soft); }
  .add small { font-size: 12px; max-width: 200px; }
</style>
