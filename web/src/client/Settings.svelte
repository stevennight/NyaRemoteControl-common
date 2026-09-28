<script lang="ts">
  import Seg from '../lib/Seg.svelte';
  import Switch from '../lib/Switch.svelte';
  import Icon from '../lib/Icon.svelte';
  import { call, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import type { ClientState, Defaults } from './types';

  let { cs, onsaved }: { cs: ClientState; onsaved: (s: ClientState) => void } = $props();

  // Edited copy, taken once when the page opens.
  // svelte-ignore state_referenced_locally
  let d = $state<Defaults>(structuredClone($state.snapshot(cs.defaults)));
  const dirty = $derived(JSON.stringify(d) !== JSON.stringify(cs.defaults));
  let saving = $state(false);

  // --- host display presets ---
  type Preset = 'asis' | 'extend' | 'private';
  const preset = $derived<Preset | null>(
    d.vd_count === 0 && !d.block_input
      ? 'asis'
      : d.vd_count > 0 && !d.physical_off && !d.block_input
        ? 'extend'
        : d.vd_count > 0 && d.physical_off && d.block_input
          ? 'private'
          : null,
  );
  function applyPreset(p: Preset) {
    if (p === 'asis') Object.assign(d, { vd_count: 0, physical_off: false, block_input: false });
    if (p === 'extend') Object.assign(d, { vd_count: Math.max(1, d.vd_count), physical_off: false, block_input: false });
    if (p === 'private') Object.assign(d, { vd_count: Math.max(1, d.vd_count), physical_off: true, block_input: true });
  }
  $effect(() => {
    if (d.vd_count === 0 && d.physical_off) d.physical_off = false;
  });

  // --- bitrate ---
  // Edited copy, taken once when the page opens.
  // svelte-ignore state_referenced_locally
  let bitrateMode = $state(d.unlimited_bitrate ? 'unlimited' : d.bitrate_kbps ? 'manual' : 'auto');
  $effect(() => {
    d.unlimited_bitrate = bitrateMode === 'unlimited';
    if (bitrateMode !== 'manual') d.bitrate_kbps = 0;
    else if (!d.bitrate_kbps) d.bitrate_kbps = 10000;
  });
  // Edited copy, taken once when the page opens.
  // svelte-ignore state_referenced_locally
  let fpsMode = $state(d.max_fps ? 'fixed' : 'auto');
  $effect(() => {
    if (fpsMode === 'auto') d.max_fps = 0;
    else if (!d.max_fps) d.max_fps = 60;
  });

  const policies = [
    ['auto', '自动', '办公模式用“清晰优先”，游戏模式用“均衡”'],
    ['quality', '清晰优先', '只有持续 2 秒以上严重发送不出去才降，最低保留 60%'],
    ['balanced', '均衡', '持续积压或延迟明显上涨时降到实际能发送的速率，最低 35%'],
    ['smooth', '流畅优先', '积压、延迟上涨、丢包都会触发，最低 15%，适合很差的网络'],
    ['fixed', '固定码率', '从不自动调整'],
  ];
  const policyHelp = $derived(policies.find((p) => p[0] === d.bitrate_policy)?.[2] ?? '');

  async function save() {
    saving = true;
    try {
      const s = await call<ClientState>('save_defaults', { defaults: $state.snapshot(d) });
      onsaved(s);
      toast('已保存，下次连接时生效', 'ok');
    } catch (e) {
      toast(`保存失败：${errorText(e)}`, 'error');
    } finally {
      saving = false;
    }
  }

  function revert() {
    d = structuredClone($state.snapshot(cs.defaults));
    bitrateMode = d.unlimited_bitrate ? 'unlimited' : d.bitrate_kbps ? 'manual' : 'auto';
    fpsMode = d.max_fps ? 'fixed' : 'auto';
  }
</script>

<div class="head"><h2>连接设置</h2><span class="sub">对所有设备生效，下次连接时使用</span></div>

<div class="set">
  <div class="card group">
    <h3>被控端显示器</h3>
    <div class="preset" role="radiogroup" aria-label="常用组合">
      {#each [['asis', '原样', '传输被控端现有的显示器'], ['extend', '加虚拟屏', '新建虚拟显示器，物理屏照常显示'], ['private', '隐私屏', '只留虚拟屏，物理屏黑屏，屏蔽本地键鼠']] as [key, name, text] (key)}
        <button type="button" role="radio" aria-checked={preset === key} class="opt" class:on={preset === key} onclick={() => applyPreset(key as Preset)}>
          <span class="mini" aria-hidden="true">
            {#if key === 'asis'}<i></i><i></i>{:else if key === 'extend'}<i class="v"></i><i></i>{:else}<i class="v"></i><i class="off"></i>{/if}
          </span>
          <b>{#if key === 'private'}<Icon name="eyeoff" size={15} />{/if}{name}</b>
          <span class="desc">{text}</span>
        </button>
      {/each}
    </div>
    <div class="field">
      <div class="text"><b>虚拟显示器数量</b><span>在被控端新建，第一个设为主显示器。需要被控端安装“虚拟显示器”组件，并以服务模式运行</span></div>
      <div class="ctl">
        <Seg bind:value={d.vd_count} label="虚拟显示器数量" options={[{ value: 0, label: '不用' }, { value: 1, label: '1' }, { value: 2, label: '2' }, { value: 3, label: '3' }, { value: 4, label: '4' }]} />
      </div>
    </div>
    <div class="field" class:disabled={d.vd_count === 0}>
      <div class="text"><b>被控端物理显示器</b><span>关闭后被控端屏幕黑屏，断开 15 秒后自动恢复</span></div>
      <div class="ctl">
        <Seg bind:value={d.physical_off} label="被控端物理显示器" disabled={d.vd_count === 0} options={[{ value: false, label: '保持显示' }, { value: true, label: '关闭' }]} />
      </div>
    </div>
    <div class="field">
      <div class="text"><b>屏蔽被控端本地键盘鼠标</b><span>远程操作时旁边的人无法操作（Ctrl+Alt+Del 除外）</span></div>
      <Switch bind:checked={d.block_input} label="屏蔽被控端本地键盘鼠标" />
    </div>
    <div class="field" class:disabled={d.vd_count === 0}>
      <div class="text"><b>虚拟显示器分辨率</b><span>跟随窗口：调整窗口大小或全屏后自动跟随，画面 1:1 最清晰。不影响被控端物理显示器</span></div>
      <div class="ctl">
        <Seg bind:value={d.vd_size} label="虚拟显示器分辨率" disabled={d.vd_count === 0} options={[{ value: 'window', label: '跟随窗口' }, { value: 'screen', label: '跟随本机屏幕' }, { value: 'fixed', label: '固定' }]} />
        {#if d.vd_size === 'fixed'}
          <input class="input sm num" type="number" min="640" max="7680" step="8" bind:value={d.vd_width} aria-label="宽" />
          <span class="muted">×</span>
          <input class="input sm num" type="number" min="480" max="4320" step="2" bind:value={d.vd_height} aria-label="高" />
        {/if}
      </div>
    </div>
    <div class="field" class:disabled={d.vd_count === 0}>
      <div class="text"><b>使用本机的缩放比例</b><span>本机 150% 时虚拟显示器也用 150%，文字大小一致</span></div>
      <Switch bind:checked={d.vd_scale} label="使用本机的缩放比例" disabled={d.vd_count === 0} />
    </div>
  </div>

  <div class="card group">
    <h3>画面</h3>
    <div class="field">
      <div class="text"><b>模式</b><span>办公：文字清晰（4:4:4），静止时补发清晰帧 · 游戏：高帧率、低延迟</span></div>
      <Seg bind:value={d.mode} label="模式" options={[{ value: 'office', label: '办公' }, { value: 'game', label: '游戏' }]} />
    </div>
    <div class="field">
      <div class="text"><b>码率上限</b><span>{bitrateMode === 'auto' ? '按分辨率和帧率估算，1080p60 办公约 7.5 Mbps' : bitrateMode === 'unlimited' ? '最高 80 Mbps；静止画面只占用实际需要的带宽' : '手动指定'}</span></div>
      <div class="ctl">
        <Seg bind:value={bitrateMode} label="码率上限" options={[{ value: 'auto', label: '自动' }, { value: 'unlimited', label: '不限制' }, { value: 'manual', label: '手动' }]} />
        {#if bitrateMode === 'manual'}
          <input class="input sm num" type="number" min="1000" max="80000" step="500" bind:value={d.bitrate_kbps} aria-label="码率 kbps" /><span class="muted small">kbps</span>
        {/if}
      </div>
    </div>
    <div class="field">
      <div class="text"><b>网络变差时</b><span>{policyHelp}</span></div>
      <select class="input" bind:value={d.bitrate_policy} aria-label="码率策略">
        {#each policies as [v, l] (v)}<option value={v}>{l}</option>{/each}
      </select>
    </div>
    <div class="field">
      <div class="text"><b>帧率上限</b><span>跟随本机显示器的刷新率，或手动限制</span></div>
      <div class="ctl">
        <Seg bind:value={fpsMode} label="帧率上限" options={[{ value: 'auto', label: '跟随显示器' }, { value: 'fixed', label: '限制' }]} />
        {#if fpsMode === 'fixed'}
          <input class="input sm num" type="number" min="15" max="240" bind:value={d.max_fps} aria-label="帧率" /><span class="muted small">fps</span>
        {/if}
      </div>
    </div>
  </div>

  <div class="card group">
    <h3>声音与外设</h3>
    <div class="field"><div class="text"><b>播放被控端声音</b></div><Switch bind:checked={d.audio} label="播放被控端声音" /></div>
    <div class="field"><div class="text"><b>同步剪贴板</b><span>文字和图片自动同步</span></div><Switch bind:checked={d.clipboard} label="同步剪贴板" /></div>
    <div class="field"><div class="text"><b>连接后全屏</b></div><Switch bind:checked={d.fullscreen} label="连接后全屏" /></div>
  </div>

  <div class="card group">
    <h3>高级</h3>
    <div class="field">
      <div class="text"><b>编码格式</b></div>
      <select class="input" bind:value={d.codec} aria-label="编码格式">
        <option value="auto">自动</option><option value="hevc">HEVC</option><option value="h264">H.264</option><option value="av1">AV1</option>
      </select>
    </div>
    <div class="field">
      <div class="text"><b>色度</b><span>4:4:4 文字最清晰，4:2:0 最省带宽</span></div>
      <select class="input" bind:value={d.chroma} aria-label="色度">
        <option value="auto">自动</option><option value="444">4:4:4</option><option value="420">4:2:0</option>
      </select>
    </div>
    <div class="field">
      <div class="text"><b>被控端编码器</b></div>
      <select class="input" bind:value={d.encoder} aria-label="被控端编码器">
        <option value="auto">自动</option><option value="nvenc">NVIDIA NVENC</option><option value="qsv">Intel QSV</option><option value="amf">AMD AMF</option><option value="software">软件</option>
      </select>
    </div>
    <div class="field">
      <div class="text"><b>硬件解码</b><span>本机不支持时自动用软件解码</span></div>
      <Switch bind:checked={d.hw_decode} label="硬件解码" />
    </div>
  </div>

  <div class="savebar">
    <button class="btn ghost" onclick={revert} disabled={!dirty || saving}>放弃修改</button>
    <button class="btn primary" onclick={save} disabled={!dirty || saving}>保存</button>
  </div>
</div>

<style>
  .preset { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; padding: 8px 18px 14px; }
  .opt { border: 1.5px solid var(--line); border-radius: 10px; padding: 12px; cursor: pointer; background: var(--surface); text-align: left; display: flex; flex-direction: column; }
  .opt:hover { border-color: var(--text-3); }
  .opt.on { border-color: var(--accent); background: var(--accent-soft); }
  .opt b { display: flex; align-items: center; gap: 6px; font-size: 13.5px; }
  .desc { color: var(--text-3); font-size: 12px; margin-top: 4px; }
  .mini { display: flex; gap: 3px; height: 30px; align-items: flex-end; margin-bottom: 8px; }
  .mini i { display: block; border-radius: 3px; border: 1.5px solid var(--text-3); height: 24px; width: 36px; }
  .mini i.v { border-color: var(--accent); background: var(--accent-soft); }
  .mini i.off { border-style: dashed; opacity: 0.45; }
  .num { width: 92px; }
  .savebar { position: sticky; bottom: -26px; display: flex; justify-content: flex-end; gap: 8px; padding: 14px 0 4px; background: linear-gradient(transparent, var(--bg) 35%); }
  @media (max-width: 720px) { .preset { grid-template-columns: 1fr; } }
</style>
