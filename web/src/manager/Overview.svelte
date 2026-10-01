<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import Modal from '../lib/Modal.svelte';
  import { call, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import { since, time, type Snapshot } from './types';

  let { snap }: { snap: Snapshot } = $props();
  let confirmReset = $state(false);

  const svcText = { running: '运行中', stopped: '已停止', pending: '正在切换…', not_installed: '未安装', unknown: '未知' };
  const kindChip: Record<string, [string, string]> = {
    connected: ['ok', '连接'],
    disconnected: ['', '断开'],
    paired: ['ok', '配对'],
    pairing_failed: ['warn', '配对失败'],
    rejected: ['err', '拒绝'],
    service: ['', '服务'],
    other: ['', '事件'],
  };

  async function run(cmd: string, args?: unknown) {
    try {
      const msg = await call<string>(cmd, args);
      if (msg) toast(msg, 'ok');
    } catch (e) {
      toast(errorText(e), 'error');
    }
  }

  function copy(text: string, what: string) {
    navigator.clipboard.writeText(text).then(() => toast(`已复制${what}`, 'ok'));
  }

  const session = $derived(snap.status?.session ?? null);
  const stream = $derived(snap.status?.host?.stream ?? '');
</script>

<div class="head"><h2>概览</h2></div>

{#if snap.points_here === false}
  <div class="banner warn">
    <Icon name="alert" /><span class="grow">已安装的服务用的不是本目录的 nya-server-svc.exe（程序被移动过，或服务由旧版本安装）。</span>
    <button class="btn sm" onclick={() => run('svc', { action: 'install' })}>重新安装服务</button>
  </div>
{:else if snap.svc === 'running' && !snap.live}
  <div class="banner warn"><Icon name="alert" /><span class="grow">服务在运行，但连不上它的控制通道（服务版本较旧？）：请在“设置”页底部重新安装服务。</span></div>
{/if}
{#if snap.status && !snap.status.listen}
  <div class="banner err"><Icon name="alert" /><span class="grow">没有在监听：{snap.status.listen_error}</span></div>
{/if}

<div class="kpis">
  <div class="card panel">
    <h4>
      服务
      <span class="r">
        {#if snap.svc === 'not_installed'}
          <button class="btn sm primary" onclick={() => run('svc', { action: 'install' })} disabled={!!snap.busy}>安装服务</button>
        {:else if snap.svc === 'stopped'}
          <button class="btn sm primary" onclick={() => run('svc', { action: 'start' })} disabled={!!snap.busy}><Icon name="play" size={14} />启动</button>
        {:else if snap.svc === 'running'}
          <button class="btn sm" onclick={() => run('svc', { action: 'restart' })} disabled={!!snap.busy}><Icon name="refresh" size={14} />重启</button>
          <button class="btn sm" onclick={() => run('svc', { action: 'stop' })} disabled={!!snap.busy}><Icon name="stop" size={14} />停止</button>
        {/if}
      </span>
    </h4>
    <div class="status">
      <div class="big {snap.svc}"><Icon name={snap.svc === 'running' ? 'check' : snap.svc === 'not_installed' ? 'power' : 'alert'} size={22} /></div>
      <div>
        <b>{svcText[snap.svc]}</b>
        <span>
          {#if snap.status}
            {snap.status.listen ? `监听 UDP ${snap.status.listen}` : '没有在监听'} ·
            {snap.status.host?.running ? `采集进程运行中（会话 ${snap.status.host.console_session}）` : '采集进程未运行'} · 版本 {snap.status.server_version}
          {:else if snap.svc === 'not_installed'}
            安装后开机自启，可以操作锁屏、登录界面和 UAC 弹窗
          {:else}
            端口 UDP {snap.config.port}
          {/if}
        </span>
      </div>
    </div>
  </div>

  <div class="card panel">
    <h4>
      配对码
      <span class="r">
        <button class="btn sm ghost" onclick={() => (confirmReset = true)}><Icon name="refresh" size={14} />重新生成</button>
      </span>
    </h4>
    <div class="pair">
      <span class="code selectable">{snap.code || '—'}</span>
      <button class="btn icon" aria-label="复制配对码" onclick={() => copy(snap.code, '配对码')} disabled={!snap.code}><Icon name="copy" /></button>
    </div>
    <div class="hint">客户端第一次连接时输入；已配对的客户端之后不再需要。</div>
  </div>
</div>

<div class="card panel">
  <h4>当前连接</h4>
  {#if session}
    <div class="conn">
      <div class="avatar">{session.client_name.slice(0, 1).toUpperCase()}</div>
      <div class="grow">
        <b>{session.client_name} <span class="chip ok">正在操作</span></b>
        <span>来自 {session.remote_addr} · {since(session.since_unix)} · 客户端 {session.client_version}</span>
      </div>
      <button class="btn danger" onclick={() => run('disconnect')}>{snap.status?.viewers?.length ? '全部断开' : '断开'}</button>
    </div>
    {#each snap.status?.viewers ?? [] as v (v.remote_addr + v.since_unix)}
      <div class="conn">
        <div class="avatar">{v.client_name.slice(0, 1).toUpperCase()}</div>
        <div class="grow">
          <b>{v.client_name} <span class="chip">正在观看</span></b>
          <span>来自 {v.remote_addr} · {since(v.since_unix)} · 客户端 {v.client_version}</span>
        </div>
      </div>
    {/each}
    {#if stream}
      <dl class="kv"><dt>画面</dt><dd>{stream}</dd></dl>
    {/if}
  {:else}
    <div class="muted small">{snap.live ? '没有客户端连接' : '服务运行后显示'}</div>
  {/if}
</div>

<div class="card panel">
  <h4>证书指纹<span class="r"><button class="btn sm ghost" onclick={() => copy(snap.fingerprint, '指纹')} disabled={!snap.fingerprint}><Icon name="copy" size={14} />复制</button></span></h4>
  <div class="mono selectable fp">{snap.fingerprint || '—'}</div>
  <div class="hint">客户端提示“证书已变化”或要求核对指纹时，用这里对照。</div>
</div>

<div class="card panel">
  <h4>最近事件</h4>
  {#if snap.status?.recent.length}
    <ul class="events">
      {#each snap.status.recent as e, i (i)}
        <li><time>{time(e.unix)}</time><span class="chip {kindChip[e.kind]?.[0] ?? ''}">{kindChip[e.kind]?.[1] ?? '事件'}</span><span class="selectable">{e.text}</span></li>
      {/each}
    </ul>
  {:else}
    <div class="muted small">{snap.live ? '暂无' : '服务运行后显示（更早的记录见“日志”）'}</div>
  {/if}
</div>

{#if confirmReset}
  <Modal title="重新生成配对码" onclose={() => (confirmReset = false)}>
    <p>旧配对码将失效。已经配对过的客户端不受影响。</p>
    {#snippet footer()}
      <button class="btn ghost" onclick={() => (confirmReset = false)}>取消</button>
      <button class="btn primary" onclick={() => { confirmReset = false; run('reset_code'); }}>重新生成</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .kpis { display: grid; grid-template-columns: 1.15fr 1fr; gap: 14px; margin-bottom: 14px; }
  .card.panel { padding: 16px 18px; margin-bottom: 14px; }
  .kpis .card.panel { margin-bottom: 0; }
  h4 { margin: 0 0 12px; font-size: 13px; color: var(--text-3); font-weight: 600; display: flex; align-items: center; gap: 8px; min-height: 28px; }
  h4 .r { margin-left: auto; display: flex; gap: 6px; }
  .status { display: flex; align-items: center; gap: 12px; }
  .big { width: 44px; height: 44px; border-radius: 12px; display: grid; place-items: center; flex: none; background: var(--surface-2); color: var(--text-3); }
  .big.running { background: var(--ok-soft); color: var(--ok); }
  .big.stopped, .big.pending { background: var(--warn-soft); color: var(--warn); }
  .status b { font-size: 16px; display: block; }
  .status span { color: var(--text-3); font-size: 12.5px; }
  .pair { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .code { font: 600 15.5px/1.2 var(--mono); letter-spacing: 0.5px; white-space: nowrap; padding: 8px 12px; background: var(--surface-2); border-radius: 8px; border: 1px solid var(--line); }
  .hint { color: var(--text-3); font-size: 12.5px; margin-top: 8px; }
  .fp { font-size: 13.5px; word-break: break-all; }
  .conn { display: flex; align-items: center; gap: 14px; }
  .avatar { width: 40px; height: 40px; border-radius: 10px; background: var(--accent-soft); color: var(--accent-text); display: grid; place-items: center; font-weight: 700; flex: none; }
  .conn .grow { flex: 1; min-width: 0; }
  .conn .grow b { display: block; }
  .conn .grow span { color: var(--text-3); font-size: 12.5px; }
  .kv { display: grid; grid-template-columns: max-content 1fr; gap: 6px 16px; font-size: 13px; margin: 12px 0 0; }
  .kv dt { color: var(--text-3); }
  .kv dd { margin: 0; }
  .events { list-style: none; margin: 0; padding: 0; }
  .events li { display: flex; gap: 12px; padding: 8px 0; border-top: 1px solid var(--line); font-size: 13px; align-items: center; }
  .events li:first-child { border-top: 0; padding-top: 0; }
  .events time { color: var(--text-3); font-variant-numeric: tabular-nums; width: 92px; flex: none; }
  @media (max-width: 820px) { .kpis { grid-template-columns: 1fr; } }
</style>
