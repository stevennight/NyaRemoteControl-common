<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import { call as appCall, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import Overview from './Overview.svelte';
  import Clients from './Clients.svelte';
  import Settings from './Settings.svelte';
  import Components from './Components.svelte';
  import Diagnostics from './Diagnostics.svelte';
  import Logs from './Logs.svelte';
  import type { Snapshot } from './types';

  /** The "本机" section: this computer as a host. `page` is its sub-page. */
  let { snap, page }: { snap: Snapshot; page: string } = $props();

  function elevate() {
    appCall('relaunch_elevated', { page: page === 'overview' ? 'host' : `host-${page}` }).catch((e) => toast(errorText(e), 'error'));
  }
</script>

{#if snap.busy}
  <div class="banner warn busy"><span class="spinner"></span>正在{snap.busy}…</div>
{/if}
{#if !snap.elevated && snap.svc !== 'not_installed'}
  <div class="banner warn">
    <Icon name="shield" /><span class="grow">查看配对码、修改设置、管理已配对的客户端需要管理员权限。</span>
    <button class="btn sm" onclick={elevate}>以管理员身份重新打开</button>
  </div>
{/if}
{#if snap.load_error}
  <div class="banner err"><Icon name="alert" /><span class="grow">{snap.load_error}</span></div>
{/if}

{#if page === 'overview'}<Overview {snap} onelevate={elevate} />
{:else if snap.elevated}
  {#if page === 'clients'}<Clients {snap} />
  {:else if page === 'settings'}<Settings {snap} />
  {:else if page === 'components'}<Components />
  {:else if page === 'diag'}<Diagnostics />
  {:else}<Logs />{/if}
{/if}

<style>
  .busy { position: sticky; top: -22px; z-index: 5; }
  .spinner { width: 15px; height: 15px; border-radius: 50%; border: 2px solid var(--line); border-top-color: var(--accent); animation: spin 0.8s linear infinite; flex: none; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
