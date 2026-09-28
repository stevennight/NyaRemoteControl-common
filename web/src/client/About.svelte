<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import { call, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import type { ClientState } from './types';

  let { cs }: { cs: ClientState } = $props();
  let report = $state('');
  let running = $state(false);

  async function diag() {
    running = true;
    try {
      report = await call<string>('diag');
    } catch (e) {
      toast(errorText(e), 'error');
    } finally {
      running = false;
    }
  }
</script>

<div class="head"><h2>关于与诊断</h2></div>

<div class="set">
  <div class="card group">
    <h3>本机</h3>
    <div class="field"><div class="text"><b>NyaRemoteControl 客户端</b><span>版本 {cs.version}</span></div></div>
    <div class="field"><div class="text"><b>计算机名</b><span>被控端的“已配对客户端”里显示这个名字</span></div><span class="mono selectable">{cs.computer}</span></div>
    <div class="field"><div class="text"><b>硬件解码</b><span>{cs.decode}</span></div></div>
  </div>

  <div class="card group">
    <h3>诊断</h3>
    <div class="field">
      <div class="text"><b>检测本机显卡、硬件解码和声音输出</b><span>连接有问题时，把结果和日志一起发给开发者</span></div>
      <div class="ctl">
        <button class="btn" onclick={() => call('open_logs')}><Icon name="folder" size={16} />打开日志目录</button>
        <button class="btn primary" onclick={diag} disabled={running}><Icon name="activity" size={16} />{running ? '检测中…' : '运行诊断'}</button>
      </div>
    </div>
    {#if report}
      <div class="report">
        <button class="btn sm copy" onclick={() => navigator.clipboard.writeText(report).then(() => toast('已复制', 'ok'))}><Icon name="copy" size={14} />复制</button>
        <pre class="log">{report}</pre>
      </div>
    {/if}
  </div>
</div>

<style>
  .report { position: relative; border-top: 1px solid var(--line); max-height: 360px; overflow: auto; }
  .copy { position: sticky; float: right; top: 8px; margin: 8px 12px 0 0; }
</style>
