<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import Seg from '../lib/Seg.svelte';
  import { call, errorText } from './ipc';
  import { tick } from 'svelte';

  let name = $state('service');
  let text = $state('');
  let box = $state<HTMLPreElement>();

  async function load(n: string) {
    try {
      text = await call<string>('log', { name: n });
    } catch (e) {
      text = errorText(e);
    }
    await tick();
    if (box) box.scrollTop = box.scrollHeight;
  }

  $effect(() => {
    load(name);
  });
</script>

<div class="head">
  <h2>日志</h2>
  <div class="right">
    <button class="btn" onclick={() => load(name)}><Icon name="refresh" size={16} />刷新</button>
    <button class="btn" onclick={() => call('open_logs')}><Icon name="folder" size={16} />打开日志目录</button>
  </div>
</div>
<div class="bar">
  <Seg
    bind:value={name}
    label="日志"
    options={[
      { value: 'service', label: '服务' },
      { value: 'helper', label: '采集进程' },
      { value: 'gui', label: '管理界面' },
      { value: 'standalone', label: '开发模式' },
      { value: 'vdd-test', label: '虚拟显示器测试' },
    ]}
  />
  <span class="muted small">最近 400 行，保留 7 天</span>
</div>
<div class="card out"><pre class="log" bind:this={box}>{text || '（空）'}</pre></div>

<style>
  .bar { display: flex; align-items: center; gap: 12px; margin-bottom: 12px; flex-wrap: wrap; }
  .out { height: calc(100vh - 170px); min-height: 200px; display: flex; }
  .out pre { flex: 1; height: 100%; }
</style>
