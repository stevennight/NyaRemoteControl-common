<script lang="ts">
  // Dialogs of a connection attempt (driven by the `connect` event) and of
  // host management (add / rename / delete).
  import Modal from '../lib/Modal.svelte';
  import { call, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import { isPairLink, type ClientState, type Local, type Phase } from './types';

  let {
    phase,
    local = $bindable(),
    onstate,
  }: { phase: Phase; local: Local; onstate: (s: ClientState) => void } = $props();

  let code = $state('');
  let address = $state('');
  let name = $state('');
  let error = $state('');
  let fromClipboard = $state(false);
  const link = $derived(isPairLink(address));

  $effect(() => {
    // Fresh fields whenever a dialog opens.
    if (phase.phase === 'pairing') code = '';
  });
  $effect(() => {
    error = '';
    if (local?.kind === 'add') {
      address = '';
      name = '';
      // A pairing link just copied (from the host's 本机 page, or a chat): offer it.
      call<string | null>('clipboard_link')
        .then((l) => {
          if (l && !address) {
            address = l;
            fromClipboard = true;
          }
        })
        .catch(() => {});
    } else if (local?.kind === 'rename') {
      name = local.host.custom_name ? local.host.name : '';
    }
  });

  const fail = (e: unknown) => toast(errorText(e), 'error');

  function pair(e?: SubmitEvent) {
    e?.preventDefault();
    if (code.trim()) call('pair', { code: code.trim() }).catch(fail);
  }

  async function add(connect: boolean) {
    const a = address.trim();
    if (!a) return;
    try {
      if (connect) {
        await call('connect', { address: a, name: name.trim() || null });
      } else {
        onstate(await call<ClientState>('save_host', { address: a, name: name.trim() }));
      }
      local = null;
    } catch (e) {
      error = errorText(e);
    }
  }

  async function rename(e: SubmitEvent) {
    e.preventDefault();
    if (local?.kind !== 'rename') return;
    try {
      onstate(await call<ClientState>('rename_host', { address: local.host.address, name: name.trim() }));
      local = null;
    } catch (e) {
      error = errorText(e);
    }
  }

  async function remove() {
    if (local?.kind !== 'delete') return;
    try {
      onstate(await call<ClientState>('delete_host', { address: local.host.address }));
      local = null;
    } catch (e) {
      fail(e);
    }
  }

  function autofocus(el: HTMLInputElement) {
    el.focus();
    el.select();
  }
</script>

{#if phase.phase === 'connecting'}
  <Modal title="正在连接" onclose={() => call('cancel_connect')}>
    <div class="spin-row"><span class="spinner"></span><span>正在连接 {phase.label} …</span></div>
    {#snippet footer()}<button class="btn" onclick={() => call('cancel_connect')}>取消</button>{/snippet}
  </Modal>
{:else if phase.phase === 'pairing'}
  <Modal title="首次连接：配对" onclose={() => call('pair', { code: null })}>
    <p>请输入 {phase.label} 的配对码。在被控端打开 nya-server 管理界面（概览页），或执行 <span class="mono">nya-server pair</span> 查看。</p>
    <form onsubmit={pair}>
      <input class="input code" bind:value={code} placeholder="XXXX-XXXX-XXXX-XXXX-XXXX-XXXX" spellcheck="false" use:autofocus />
    </form>
    {#snippet footer()}
      <button class="btn ghost" onclick={() => call('pair', { code: null })}>取消</button>
      <button class="btn primary" onclick={() => pair()} disabled={!code.trim()}>配对</button>
    {/snippet}
  </Modal>
{:else if phase.phase === 'pin_changed'}
  <Modal title="被控端证书已变化" onclose={() => call('pin_changed', { retry: false })}>
    <p>{phase.label} 的证书和上次保存的不一样。常见原因：被控端从开发模式改为服务模式，或重装过；也可能有人在冒充被控端。</p>
    {#snippet footer()}
      <button class="btn ghost" onclick={() => call('pin_changed', { retry: false })}>取消</button>
      <button class="btn primary" onclick={() => call('pin_changed', { retry: true })}>重新验证</button>
    {/snippet}
  </Modal>
{:else if phase.phase === 'invite'}
  <Modal title="通过配对链接连接" width={460} onclose={() => call('invite_ok', { ok: false })}>
    <p>要连接到 <b>{phase.label}</b> 并自动配对吗？</p>
    <div class="addrs">{#each phase.addresses as a (a)}<span class="chip mono">{a}</span>{/each}</div>
    <p class="hint">只打开你自己或信任的人发来的链接：连接后，你在远程窗口里的键盘输入、剪贴板和拖进去的文件会发送到那台电脑。</p>
    {#snippet footer()}
      <button class="btn ghost" onclick={() => call('invite_ok', { ok: false })}>取消</button>
      <button class="btn primary" onclick={() => call('invite_ok', { ok: true })}>连接</button>
    {/snippet}
  </Modal>
{:else if phase.phase === 'verify'}
  <Modal title="核对证书指纹" width={460}>
    <p>被控端已经认识本机，所以没有用配对码验证它的身份。请核对指纹：</p>
    <div class="fp mono selectable">{phase.fingerprint}</div>
    <p>与被控端管理界面（或 <span class="mono">nya-server pair</span>）显示的“证书指纹”一致才继续。</p>
    {#snippet footer()}
      <button class="btn ghost" onclick={() => call('fingerprint_ok', { ok: false })}>不一致，取消</button>
      <button class="btn primary" onclick={() => call('fingerprint_ok', { ok: true })}>一致，继续</button>
    {/snippet}
  </Modal>
{/if}

{#if local?.kind === 'add'}
  <Modal title="添加设备" onclose={() => (local = null)}>
    <p>被控端的地址（Tailscale / EasyTier 等组网后的 IP，可带端口），第一次连接时输入被控端显示的配对码；或者粘贴被控端“本机”页的配对链接，连接时自动配对。</p>
    <label class="lbl" for="addr">地址或配对链接</label>
    <input id="addr" class="input" bind:value={address} oninput={() => (fromClipboard = false)} placeholder="100.64.0.2、host:47100 或 nyaremote://…" spellcheck="false" use:autofocus />
    {#if link}
      <p class="hint ok">{fromClipboard ? '已从剪贴板填入配对链接。' : '这是配对链接：'}点“连接”会同时尝试链接里的地址，并自动配对。</p>
    {/if}
    <div style="height: 12px"></div>
    <label class="lbl" for="nm">名称（可选）</label>
    <input id="nm" class="input" bind:value={name} placeholder="不填则使用被控端自己设置的名称" />
    {#if error}<div class="err">{error}</div>{/if}
    {#snippet footer()}
      <button class="btn ghost" onclick={() => (local = null)}>取消</button>
      {#if !link}<button class="btn" onclick={() => add(false)} disabled={!address.trim()}>仅保存</button>{/if}
      <button class="btn primary" onclick={() => add(true)} disabled={!address.trim()}>连接</button>
    {/snippet}
  </Modal>
{:else if local?.kind === 'rename'}
  <Modal title="改名" onclose={() => (local = null)}>
    <p class="mono">{local.host.address}</p>
    <form onsubmit={rename}>
      <input class="input" bind:value={name} aria-label="名称" placeholder={local.host.server_name || local.host.address} use:autofocus />
    </form>
    <p class="hint">
      {#if local.host.server_name}被控端自己设置的名称是“{local.host.server_name}”。{/if}留空则使用被控端的名称{local.host.server_name ? '' : '（连接后获取）'}，被控端改名后这里也跟着变。
    </p>
    {#if error}<div class="err">{error}</div>{/if}
    {#snippet footer()}
      <button class="btn ghost" onclick={() => (local = null)}>取消</button>
      <button class="btn primary" onclick={() => rename(new SubmitEvent('submit'))}>保存</button>
    {/snippet}
  </Modal>
{:else if local?.kind === 'delete'}
  <Modal title="删除设备" onclose={() => (local = null)}>
    <p>删除“{local.host.name}”？之后再连接需要重新配对。</p>
    {#snippet footer()}
      <button class="btn ghost" onclick={() => (local = null)}>取消</button>
      <button class="btn danger-fill" onclick={remove}>删除</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .spin-row { display: flex; align-items: center; gap: 12px; padding: 6px 0 2px; color: var(--text-2); }
  .spinner { width: 18px; height: 18px; border-radius: 50%; border: 2px solid var(--line); border-top-color: var(--accent); animation: spin 0.8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .code { font-family: var(--mono); font-size: 16px; letter-spacing: 1px; text-transform: uppercase; }
  .fp { font-size: 18px; font-weight: 600; padding: 10px 12px; border-radius: 8px; background: var(--surface-2); border: 1px solid var(--line); margin-bottom: 12px; word-break: break-all; }
  .err { color: var(--danger); font-size: 13px; margin-top: 10px; }
  .hint { color: var(--text-3); font-size: 12.5px; margin: 10px 0 0; }
  .hint.ok { color: var(--ok); }
  .addrs { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 4px; }
</style>
