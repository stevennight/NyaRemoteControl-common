<script lang="ts">
  import Seg from '../lib/Seg.svelte';
  import Switch from '../lib/Switch.svelte';
  import Modal from '../lib/Modal.svelte';
  import { call, errorText } from './ipc';
  import { toast } from '../lib/notify.svelte';
  import type { Config, Snapshot } from './types';

  let { snap }: { snap: Snapshot } = $props();

  // Edited copy, taken once when the page opens.
  // svelte-ignore state_referenced_locally
  let c = $state<Config>(structuredClone($state.snapshot(snap.config)));
  const dirty = $derived(JSON.stringify(c) !== JSON.stringify(snap.config));
  let saving = $state(false);
  let confirmUninstall = $state(false);

  const encoderLabel: Record<string, string> = { auto: '自动', nvenc: 'NVIDIA NVENC', qsv: 'Intel QSV', amf: 'AMD AMF', software: '软件' };

  async function save() {
    saving = true;
    try {
      toast(await call<string>('set_config', { config: $state.snapshot(c) }), 'ok');
    } catch (e) {
      toast(errorText(e), 'error');
    } finally {
      saving = false;
    }
  }

  async function svc(action: string) {
    try {
      toast(await call<string>('svc', { action }), 'ok');
    } catch (e) {
      toast(errorText(e), 'error');
    }
  }
</script>

<div class="head"><h2>被控设置</h2><span class="sub">{snap.live ? '保存后立即生效' : '服务没有运行：保存到配置文件，服务启动时生效'}</span></div>

<div class="set">
  <div class="card group">
    <h3>网络</h3>
    <div class="field">
      <div class="text"><b>端口（UDP 和 TCP）</b><span>画面等走 UDP，文件走 TCP。改端口会断开当前连接，并自动更新防火墙规则</span></div>
      <input class="input sm num" type="number" min="1024" max="65535" bind:value={c.port} aria-label="端口" />
    </div>
    <div class="field">
      <div class="text"><b>监听地址</b><span>:: 表示所有网卡；填组网 IP（如 100.x.y.z）则只接受该网卡的连接</span></div>
      <input class="input sm addr" bind:value={c.bind} spellcheck="false" aria-label="监听地址" />
    </div>
    <div class="field">
      <div class="text"><b>显示名称</b><span>客户端看到的名字，留空 = 计算机名（{snap.computer}）</span></div>
      <input class="input sm addr" bind:value={c.name} placeholder={snap.computer} aria-label="显示名称" />
    </div>
  </div>

  <div class="card group">
    <h3>画面与声音</h3>
    <div class="field">
      <div class="text"><b>编码器</b><span>自动：优先用显示器所在的显卡编码</span></div>
      <select class="input" bind:value={c.encoder} aria-label="编码器">
        {#each snap.encoders as e (e)}<option value={e}>{encoderLabel[e] ?? e}</option>{/each}
      </select>
    </div>
    {#each [['office_bitrate_kbps', '办公模式码率'], ['game_bitrate_kbps', '游戏模式码率']] as [key, label] (key)}
      {@const k = key as 'office_bitrate_kbps' | 'game_bitrate_kbps'}
      <div class="field">
        <div class="text"><b>{label}</b><span>客户端没有指定时使用；自动 = 按分辨率和帧率估算</span></div>
        <div class="ctl">
          <Seg
            {label}
            bind:value={() => (c[k] === 0 ? 'auto' : 'manual'), (v) => (c[k] = v === 'auto' ? 0 : c[k] || 10000)}
            options={[{ value: 'auto', label: '自动' }, { value: 'manual', label: '手动' }]}
          />
          {#if c[k] !== 0}
            <input class="input sm num" type="number" min="500" max="150000" step="500" bind:value={c[k]} aria-label="{label} kbps" /><span class="muted small">kbps</span>
          {/if}
        </div>
      </div>
    {/each}
    <div class="field">
      <div class="text"><b>最高帧率</b><span>客户端和显示器刷新率更低时按更低的来</span></div>
      <div class="ctl"><input class="input sm num" type="number" min="30" max="240" bind:value={c.max_fps} aria-label="最高帧率" /><span class="muted small">fps</span></div>
    </div>
    <div class="field">
      <div class="text"><b>传输系统声音</b></div>
      <Switch bind:checked={c.audio} label="传输系统声音" />
    </div>
  </div>

  <div class="card group">
    <h3>更新</h3>
    <div class="field">
      <div class="text"><b>服务自动检查新版本</b><span>服务每 12 小时查看一次 GitHub 上的新版本（nya-server status 可见）；安装请到“关于与诊断”</span></div>
      <Switch bind:checked={c.check_updates} label="自动检查新版本" />
    </div>
  </div>

  <div class="card group">
    <h3>日志</h3>
    <div class="field">
      <div class="text"><b>日志级别</b><span>排查问题时改成 debug，服务重启后生效</span></div>
      <select class="input" bind:value={c.log_level} aria-label="日志级别">
        {#each ['error', 'warn', 'info', 'debug'] as l (l)}<option value={l}>{l}</option>{/each}
      </select>
    </div>
  </div>

  <div class="savebar">
    <button class="btn ghost" onclick={() => (c = structuredClone($state.snapshot(snap.config)))} disabled={!dirty || saving}>放弃修改</button>
    <button class="btn primary" onclick={save} disabled={!dirty || saving}>保存</button>
  </div>

  <div class="card group">
    <h3>服务</h3>
    <div class="field">
      <div class="text"><b>重新安装服务</b><span>移动过程序目录，或从旧版本升级后使用；配对信息会保留</span></div>
      <button class="btn" onclick={() => svc('install')} disabled={!!snap.busy}>重新安装</button>
    </div>
    <div class="field">
      <div class="text"><b>关闭远程控制</b><span>卸载服务，这台电脑不再能被控制；证书和配对信息会保留（彻底清除请用命令行 nya-server uninstall --purge）</span></div>
      <button class="btn danger" onclick={() => (confirmUninstall = true)} disabled={snap.svc === 'not_installed' || !!snap.busy}>关闭</button>
    </div>
  </div>
</div>

{#if confirmUninstall}
  <Modal title="关闭远程控制" onclose={() => (confirmUninstall = false)}>
    <p>将卸载服务，这台电脑不能再被远程控制，直到在“本机 → 概览”里重新开启。证书和配对信息会保留。</p>
    {#snippet footer()}
      <button class="btn ghost" onclick={() => (confirmUninstall = false)}>取消</button>
      <button class="btn danger-fill" onclick={() => { confirmUninstall = false; svc('uninstall'); }}>关闭</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .num { width: 100px; }
  .addr { width: 220px; }
  .savebar { position: sticky; bottom: -26px; display: flex; justify-content: flex-end; gap: 8px; padding: 14px 0 4px; background: linear-gradient(transparent, var(--bg) 35%); z-index: 2; }
</style>
