<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import { call, errorText } from './ipc';
  import { toast } from '../lib/notify.svelte';

  let report = $state('');
  let running = $state(false);

  async function run() {
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

<div class="head">
  <h2>诊断</h2>
  <div class="right">
    {#if report}
      <button class="btn" onclick={() => navigator.clipboard.writeText(report).then(() => toast('已复制', 'ok'))}><Icon name="copy" size={16} />复制结果</button>
      <button class="btn" onclick={() => call('open_logs')}><Icon name="folder" size={16} />打开所在目录</button>
    {/if}
    <button class="btn primary" onclick={run} disabled={running}><Icon name="activity" size={16} />{running ? '检测中…' : '运行诊断'}</button>
  </div>
</div>
<p class="lead">检测显卡、显示器（含 HDR）、各编码器、截屏、跨显卡传输、音频和虚拟显示器驱动，大约需要 10 秒。结果保存在日志目录的 nya-diag.txt，排查问题时请一起提供。</p>

{#if running}
  <div class="card waiting"><span class="spinner"></span>正在检测，请稍候…</div>
{:else if report}
  <div class="card out"><pre class="log">{report}</pre></div>
{/if}

<style>
  .lead { color: var(--text-2); margin: -8px 0 16px; font-size: 13.5px; }
  .out { max-height: calc(100vh - 190px); overflow: auto; }
  .waiting { display: flex; align-items: center; gap: 10px; padding: 24px 18px; color: var(--text-3); }
  .spinner { width: 15px; height: 15px; border-radius: 50%; border: 2px solid var(--line); border-top-color: var(--accent); animation: spin 0.8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
