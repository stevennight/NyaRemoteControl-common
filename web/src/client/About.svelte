<script lang="ts">
  import Icon from '../lib/Icon.svelte';
  import Switch from '../lib/Switch.svelte';
  import UpdateCard from '../lib/UpdateCard.svelte';
  import { call, errorText } from '../lib/ipc';
  import { toast } from '../lib/notify.svelte';
  import type { ClientState } from './types';

  let { cs, onsaved }: { cs: ClientState; onsaved: (s: ClientState) => void } = $props();
  let report = $state('');
  let running = $state(false);
  // svelte-ignore state_referenced_locally
  let myName = $state(cs.client_name);
  let savingName = $state(false);

  async function setCheck(on: boolean) {
    try {
      onsaved(await call<ClientState>('set_check_updates', { on }));
    } catch (e) {
      toast(errorText(e), 'error');
    }
  }

  async function saveName(e: SubmitEvent) {
    e.preventDefault();
    savingName = true;
    try {
      const s = await call<ClientState>('set_client_name', { name: myName.trim() });
      onsaved(s);
      myName = s.client_name;
      toast('已保存，下次连接时被控端显示新名称', 'ok');
    } catch (e) {
      toast(errorText(e), 'error');
    } finally {
      savingName = false;
    }
  }

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
    <div class="field">
      <div class="text"><b>本机名称</b><span>被控端的“已配对客户端”和连接记录里显示这个名字。留空则用计算机名 {cs.computer_name}</span></div>
      <form class="ctl" onsubmit={saveName}>
        <input class="input" bind:value={myName} placeholder={cs.computer_name} maxlength="64" aria-label="本机名称" />
        <button class="btn" type="submit" disabled={savingName || myName.trim() === cs.client_name}>保存</button>
      </form>
    </div>
    <div class="field"><div class="text"><b>硬件解码</b><span>{cs.decode}</span></div></div>
  </div>

  <UpdateCard
    info={cs.update}
    current={cs.version}
    note="下载后会请求一次管理员权限，然后客户端关闭、安装并自动重新打开；正在进行的远程连接会断开。"
    oncheck={() => call('update_check').catch((e) => toast(errorText(e), 'error'))}
    oninstall={() => call('update_apply').catch((e) => toast(errorText(e), 'error'))}
  />
  <div class="card group">
    <div class="field">
      <div class="text"><b>启动时检查新版本</b><span>从 GitHub 查看是否有新版本；是否安装由你决定</span></div>
      <Switch bind:checked={() => cs.check_updates, (v) => setCheck(v)} label="启动时检查新版本" />
    </div>
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
