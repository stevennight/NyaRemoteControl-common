<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import Toasts from '../lib/Toasts.svelte';
  import { call, on, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import Devices from './Devices.svelte';
  import Settings from './Settings.svelte';
  import About from './About.svelte';
  import Dialogs from './Dialogs.svelte';
  import type { ClientState, Local, Phase } from './types';

  let cs = $state<ClientState | null>(null);
  let loadError = $state('');
  let page = $state<'devices' | 'settings' | 'about'>('devices');
  let phase = $state<Phase>({ phase: 'idle' });
  let local = $state<Local>(null);
  /** Host whose settings the settings page edits; null = the defaults. */
  let scope = $state<string | null>(null);

  $effect(() => {
    call<ClientState>('state').then((s) => (cs = s)).catch((e) => (loadError = errorText(e)));
    const offs = [
      on<ClientState>('state', (s) => (cs = s)),
      on<Phase>('connect', (p) => (phase = p)),
      on<{ kind: 'info' | 'ok' | 'error'; text: string }>('notice', (n) => toast(n.text, n.kind)),
    ];
    return () => offs.forEach((f) => f());
  });

  const nav = [
    { id: 'devices', icon: 'monitor', label: '设备' },
    { id: 'settings', icon: 'sliders', label: '连接设置' },
    { id: 'about', icon: 'info', label: '关于与诊断' },
  ] as const;
</script>

<div class="shell">
  <nav class="side">
    <div class="brand"><span class="logo">N</span><span class="name">NyaRemoteControl<small>远程桌面</small></span></div>
    {#each nav as n (n.id)}
      <button class="nav" class:on={page === n.id} onclick={() => ((page = n.id), n.id === 'settings' && (scope = null))}><Icon name={n.icon} /><span class="label">{n.label}</span></button>
    {/each}
    <div class="grow"></div>
    {#if cs}<div class="me"><b>本机 {cs.computer}</b>{cs.decode}</div>{/if}
  </nav>
  <main class="main">
    {#if cs}
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
