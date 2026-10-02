<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import { call, on, errorText } from './ipc';
  import { toast } from '../lib/notify.svelte';
  import type { Component, Install } from './types';

  let list = $state<Component[] | null>(null);
  let install = $state<Install>(null);
  const running = $derived(!!install && !install.done);
  const missing = $derived((list ?? []).filter((c) => !c.installed));
  const icons: Record<string, string> = { vdd: 'monitor', cable: 'mic', vigem: 'game', usbip: 'usb', winfsp: 'folder', printer: 'doc' };

  async function detect() {
    list = null;
    try {
      list = await call<Component[]>('components');
    } catch (e) {
      toast(errorText(e), 'error');
      list = [];
    }
  }

  async function start(ids: string[]) {
    try {
      install = await call<Install>('install', { ids });
    } catch (e) {
      toast(errorText(e), 'error');
    }
  }

  $effect(() => {
    detect();
    return on<Install>('install', (j) => {
      const finished = j?.done && !install?.done;
      install = j;
      if (finished) detect();
    });
  });
</script>

<div class="head">
  <h2>可选组件</h2>
  <span class="sub">不装不影响其他功能</span>
  <div class="right">
    <button class="btn" onclick={detect} disabled={running || !list}><Icon name="refresh" size={16} />重新检测</button>
    {#if missing.length}
      <button class="btn primary" onclick={() => start(missing.map((c) => c.id))} disabled={running} title={missing.map((c) => c.name).join('、')}>
        <Icon name="download" size={16} />全部安装（{missing.length}）
      </button>
    {/if}
  </div>
</div>
<p class="lead">“安装”会下载固定版本并校验 SHA-256 后静默安装；程序目录下的 drivers 文件夹里有离线安装包时优先使用。</p>

{#if install}
  <div class="card job">
    {#each install.log as l, i (i)}
      <div class="line" class:err={l.error}><Icon name={l.error ? 'alert' : 'check'} size={15} />{l.text}</div>
    {/each}
    {#if install.current}
      <div class="line"><span class="spinner"></span>{install.current}：{install.status || '准备中…'}</div>
    {:else if install.done && install.reboot}
      <div class="line warn"><Icon name="alert" size={15} />部分组件需要重启电脑后才能生效。</div>
    {/if}
  </div>
{/if}

<div class="card">
  {#if !list}
    <div class="loading"><span class="spinner"></span>正在检测…</div>
  {:else}
    {#each list as c (c.id)}
      <div class="comp">
        <div class="ic"><Icon name={icons[c.id] ?? 'puzzle'} size={20} /></div>
        <div class="grow">
          <div class="title"><b>{c.name}</b>{#if c.installed}<span class="chip ok">已安装</span>{:else}<span class="chip">未安装</span>{/if}</div>
          <p>{c.purpose}</p>
          {#if c.status}<small class="state">{c.status}</small>{/if}
          <small>{c.product} · {c.note}</small>
        </div>
        <div class="acts">
          {#if !c.installed}
            <button class="btn primary sm" onclick={() => start([c.id])} disabled={running}>安装</button>
          {/if}
          {#if c.id === 'cable' && c.installed}
            <button class="btn sm" onclick={() => call('open_sound_settings')}>声音设置</button>
          {/if}
          <button class="btn sm ghost" onclick={() => call('open_url', { url: c.url })}><Icon name="external" size={14} />官网</button>
        </div>
      </div>
    {/each}
  {/if}
</div>

<style>
  .lead { color: var(--text-2); margin: -8px 0 16px; font-size: 13.5px; }
  .job { padding: 12px 16px; margin-bottom: 14px; display: flex; flex-direction: column; gap: 6px; }
  .line { display: flex; align-items: center; gap: 8px; font-size: 13.5px; }
  .line :global(svg) { color: var(--ok); }
  .line.err, .line.err :global(svg) { color: var(--danger); }
  .line.warn, .line.warn :global(svg) { color: var(--warn); }
  .comp { display: flex; gap: 14px; padding: 16px 18px; align-items: flex-start; }
  .comp + .comp { border-top: 1px solid var(--line); }
  .ic { width: 40px; height: 40px; border-radius: 10px; background: var(--surface-2); display: grid; place-items: center; color: var(--text-2); flex: none; }
  .grow { flex: 1; min-width: 0; }
  .title { display: flex; align-items: center; gap: 8px; }
  .title b { font-size: 14.5px; }
  p { margin: 3px 0 0; color: var(--text-2); font-size: 13px; }
  small { display: block; color: var(--text-3); font-size: 12px; margin-top: 4px; }
  small.state { color: var(--text-2); }
  .acts { display: flex; gap: 6px; flex: none; }
  .loading { display: flex; align-items: center; gap: 10px; padding: 24px 18px; color: var(--text-3); }
  .spinner { width: 15px; height: 15px; border-radius: 50%; border: 2px solid var(--line); border-top-color: var(--accent); animation: spin 0.8s linear infinite; flex: none; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
