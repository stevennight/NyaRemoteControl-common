<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import Toasts from '../lib/Toasts.svelte';
  import { call, on, errorText } from '../lib/ipc';
  import Overview from './Overview.svelte';
  import Clients from './Clients.svelte';
  import Settings from './Settings.svelte';
  import Components from './Components.svelte';
  import Diagnostics from './Diagnostics.svelte';
  import Logs from './Logs.svelte';
  import type { Snapshot } from './types';

  let snap = $state<Snapshot | null>(null);
  let loadError = $state('');
  let page = $state('overview');

  $effect(() => {
    call<Snapshot>('snapshot').then((s) => (snap = s)).catch((e) => (loadError = errorText(e)));
    return on<Snapshot>('snapshot', (s) => (snap = s));
  });

  const nav = [
    { id: 'overview', icon: 'home', label: '概览' },
    { id: 'clients', icon: 'users', label: '已配对客户端' },
    { id: 'settings', icon: 'sliders', label: '设置' },
    { id: 'components', icon: 'puzzle', label: '可选组件' },
    { id: 'diag', icon: 'activity', label: '诊断' },
    { id: 'logs', icon: 'doc', label: '日志' },
  ];
  const svcShort = { running: '服务运行中', stopped: '服务已停止', pending: '服务切换中', not_installed: '服务未安装', unknown: '服务状态未知' };
</script>

<div class="shell">
  <nav class="side">
    <div class="brand"><span class="logo">N</span><span class="name">NyaRemoteControl<small>被控端</small></span></div>
    {#each nav as n (n.id)}
      <button class="nav" class:on={page === n.id} onclick={() => (page = n.id)} disabled={!snap?.elevated && n.id !== 'overview'}>
        <Icon name={n.icon} /><span class="label">{n.label}</span>
      </button>
    {/each}
    <div class="grow"></div>
    {#if snap}
      <div class="me">
        <b>{snap.computer}</b>
        <span class="svc"><span class="dot" class:ok={snap.svc === 'running'} class:warn={snap.svc === 'stopped'}></span>{svcShort[snap.svc]} · v{snap.version}</span>
      </div>
    {/if}
  </nav>
  <main class="main">
    {#if snap?.busy}
      <div class="banner warn busy"><span class="spinner"></span>正在{snap.busy}…</div>
    {/if}
    {#if snap && !snap.elevated}
      <div class="banner warn">
        <Icon name="shield" /><span class="grow">需要管理员权限才能管理服务、查看配对码。</span>
        <button class="btn sm" onclick={() => call('relaunch_elevated').catch((e) => (loadError = errorText(e)))}>以管理员身份重新打开</button>
      </div>
    {/if}
    {#if snap?.load_error}
      <div class="banner err"><Icon name="alert" /><span class="grow">{snap.load_error}</span></div>
    {/if}
    {#if loadError}
      <div class="banner err"><Icon name="alert" /><span class="grow">{loadError}</span></div>
    {/if}
    {#if snap && snap.elevated}
      {#if page === 'overview'}<Overview {snap} />
      {:else if page === 'clients'}<Clients {snap} />
      {:else if page === 'settings'}<Settings {snap} />
      {:else if page === 'components'}<Components />
      {:else if page === 'diag'}<Diagnostics />
      {:else}<Logs />{/if}
    {/if}
  </main>
</div>
<Toasts />

<style>
  .svc { display: flex; align-items: center; gap: 6px; }
  .busy { position: sticky; top: -22px; z-index: 5; }
  .spinner { width: 15px; height: 15px; border-radius: 50%; border: 2px solid var(--line); border-top-color: var(--accent); animation: spin 0.8s linear infinite; flex: none; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .nav:disabled { opacity: 0.45; cursor: default; }
</style>
