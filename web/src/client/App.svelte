<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import Toasts from '../lib/Toasts.svelte';
  import { call, on, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import Devices from './Devices.svelte';
  import Settings from './Settings.svelte';
  import About from './About.svelte';
  import Dialogs from './Dialogs.svelte';
  import Host from '../host/Host.svelte';
  import * as host from '../host/ipc';
  import type { Snapshot } from '../host/types';
  import type { ClientState, Local, Phase } from './types';

  let cs = $state<ClientState | null>(null);
  let loadError = $state('');
  /** Remote control: devices, settings, about; this computer: host, host-<sub-page>. */
  let page = $state('devices');
  let phase = $state<Phase>({ phase: 'idle' });
  let local = $state<Local>(null);
  /** Host whose settings the settings page edits; null = the defaults. */
  let scope = $state<string | null>(null);
  /** This computer as a host ("本机"). */
  let snap = $state<Snapshot | null>(null);
  let hostError = $state('');

  $effect(() => {
    call<ClientState>('state').then((s) => (cs = s)).catch((e) => (loadError = errorText(e)));
    call<string | null>('start_page').then((p) => p && (page = p)).catch(() => {});
    host.call<Snapshot>('snapshot').then((s) => (snap = s)).catch((e) => (hostError = errorText(e)));
    const offs = [
      on<ClientState>('state', (s) => (cs = s)),
      on<Phase>('connect', (p) => (phase = p)),
      on<{ kind: 'info' | 'ok' | 'error'; text: string }>('notice', (n) => toast(n.text, n.kind)),
      host.on<Snapshot>('snapshot', (s) => (snap = s)),
    ];
    return () => offs.forEach((f) => f());
  });

  const remote = [
    { id: 'devices', icon: 'monitor', label: '设备' },
    { id: 'settings', icon: 'sliders', label: '连接设置' },
  ] as const;
  const mine = [
    { id: 'host', icon: 'home', label: '概览' },
    { id: 'host-clients', icon: 'users', label: '已配对客户端' },
    { id: 'host-settings', icon: 'sliders', label: '被控设置' },
    { id: 'host-components', icon: 'puzzle', label: '可选组件' },
    { id: 'host-diag', icon: 'activity', label: '被控诊断' },
    { id: 'host-logs', icon: 'doc', label: '服务日志' },
  ] as const;
  const svcShort = { running: '允许远程控制', stopped: '服务已停止', pending: '服务切换中', not_installed: '未开启远程控制', unknown: '服务状态未知' };

  /** Sub-pages of 本机 need admin rights and the service installed. */
  const hostLocked = (id: string) => id !== 'host' && (!snap?.elevated || snap.svc === 'not_installed');
  const hostPage = $derived(page === 'host' ? 'overview' : page.slice('host-'.length));
  // Remote control turned off (or not elevated): back to the overview.
  $effect(() => {
    if (snap && page.startsWith('host-') && hostLocked(page)) page = 'host';
  });
</script>

<div class="shell">
  <nav class="side">
    <div class="brand"><span class="logo" aria-hidden="true"><svg viewBox="20 20 68 68" fill="#fff"><path d="M36 38 41 27 47 38zM61 38 67 27 72 38zM48 68h12l2 8H46zM42 76h24v3H42zM50 44v14l3.5-3.5L56 60l2.5-1-2.5-5.5H61z"/><path fill-rule="evenodd" d="M32 36h44a4 4 0 0 1 4 4v24a4 4 0 0 1-4 4H32a4 4 0 0 1-4-4V40a4 4 0 0 1 4-4zM34 40v22h40V40z"/></svg></span><span class="name">NyaRemoteControl<small>远程桌面</small></span></div>
    <div class="group">远程控制</div>
    {#each remote as n (n.id)}
      <button class="nav" class:on={page === n.id} onclick={() => ((page = n.id), n.id === 'settings' && (scope = null))}><Icon name={n.icon} /><span class="label">{n.label}</span></button>
    {/each}
    <div class="group">本机</div>
    {#each mine as n (n.id)}
      <button class="nav" class:on={page === n.id} onclick={() => (page = n.id)} disabled={hostLocked(n.id)}><Icon name={n.icon} /><span class="label">{n.label}</span></button>
    {/each}
    <div class="grow"></div>
    <button class="nav" class:on={page === 'about'} onclick={() => (page = 'about')}><Icon name="info" /><span class="label">关于与诊断</span></button>
    {#if cs}
      <div class="me">
        <b>本机 {cs.computer}</b>
        {#if snap}<span class="svc"><span class="dot" class:ok={snap.svc === 'running'} class:warn={snap.svc === 'stopped'}></span>{svcShort[snap.svc]}</span>{/if}
        {cs.decode}
      </div>
    {/if}
  </nav>
  <main class="main">
    {#if page.startsWith('host')}
      {#if snap}
        <Host {snap} page={hostPage} />
      {:else if hostError}
        <div class="banner err"><Icon name="alert" />{hostError}</div>
      {/if}
    {:else if cs}
      {#if page === 'devices'}
        <Devices
          {cs}
          onadd={() => (local = { kind: 'add' })}
          onrename={(h) => (local = { kind: 'rename', host: h })}
          ondelete={(h) => (local = { kind: 'delete', host: h })}
          onsettings={(h) => ((scope = h.address), (page = 'settings'))}
        />
      {:else if page === 'settings'}
        {#key scope}
          <Settings {cs} bind:scope onsaved={(s) => (cs = s)} />
        {/key}
      {:else}
        <About {cs} onsaved={(s) => (cs = s)} />
      {/if}
    {:else if loadError}
      <div class="banner err"><Icon name="alert" />{loadError}</div>
    {/if}
  </main>
</div>

<Dialogs {phase} bind:local onstate={(s) => (cs = s)} />
<Toasts />

<style>
  .group { padding: 12px 10px 4px; font-size: 11.5px; color: var(--text-3); font-weight: 600; letter-spacing: 0.5px; }
  .brand + .group { padding-top: 0; }
  .svc { display: flex; align-items: center; gap: 6px; margin-bottom: 2px; }
  .nav:disabled { opacity: 0.45; cursor: default; }
  @media (max-width: 720px) { .group { display: none; } }
</style>
