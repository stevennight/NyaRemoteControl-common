// Sample backend for working on the page in a normal browser.
import type { Mock } from '../lib/ipc';
import type { ClientState } from './types';

const now = Math.floor(Date.now() / 1000);
const state: ClientState = {
  version: '0.2.0 (预览)',
  computer: 'DESKTOP-DEV',
  client_name: '',
  computer_name: 'DESKTOP-DEV',
  check_updates: true,
  update: { state: 'available', current: '0.2.0', latest: '0.2.1', notes: '- 修复：……\n- 新增：自动更新', page: 'https://github.com/stevennight/NyaRemoteControl-client/releases/tag/v0.2.1', progress: 0, message: '', checked_unix: now - 60 },
  decode: '硬件解码：不可用（软件解码）',
  hosts: [
    { name: '公司台式机', address: 'frp.dev.nyatori.com', paired: true, last_connected: now - 600, server_name: 'DESKTOP-GTX1650', custom_name: true, settings: null },
    { name: 'GAMING-PC', address: '100.64.0.7', paired: true, last_connected: now - 3 * 86400, server_name: 'GAMING-PC', custom_name: false, settings: null },
    { name: '100.64.0.12:47101', address: '100.64.0.12:47101', paired: false, last_connected: 0, server_name: '', custom_name: false, settings: null },
  ],
  defaults: {
    mode: 'office', fullscreen: false, display: 0, bitrate_kbps: 0, unlimited_bitrate: false, bitrate_policy: 'auto', video_transport: 'auto',
    max_fps: 0, encoder: 'auto', codec: 'auto', chroma: 'auto', audio: true, clipboard: true, hw_decode: true,
    vd_count: 1, physical_off: false, block_input: false, vd_size: 'window', vd_width: 1920, vd_height: 1080, vd_scale: true, multi_window: false,
    mic: false, grab_keyboard: false,
  },
};

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
const clone = () => structuredClone(state);

export const mock: Mock = async (cmd, args, emit) => {
  await wait(120);
  switch (cmd) {
    case 'state':
      return clone();
    case 'connect': {
      const label = args.name ?? args.address;
      emit('connect', { phase: 'connecting', label });
      await wait(900);
      const known = state.hosts.find((h) => h.address === args.address);
      if (known?.paired) {
        emit('connect', { phase: 'idle' });
        emit('notice', { kind: 'error', text: `无法连接 ${label}：连接超时（示例数据）` });
      } else {
        emit('connect', { phase: 'pairing', label });
      }
      return null;
    }
    case 'pair':
    case 'cancel_connect':
    case 'pin_changed':
    case 'fingerprint_ok':
      emit('connect', { phase: 'idle' });
      return null;
    case 'save_host':
      state.hosts.push({ name: args.name || args.address, address: args.address, paired: false, last_connected: 0, server_name: '', custom_name: !!args.name, settings: null });
      return clone();
    case 'rename_host': {
      const h = state.hosts.find((h) => h.address === args.address)!;
      if (!args.name) {
        Object.assign(h, { name: h.server_name || h.address, custom_name: false });
        return clone();
      }
      if (state.hosts.some((h) => h.name === args.name && h.address !== args.address)) throw new Error('已有同名的被控端');
      Object.assign(h, { name: args.name, custom_name: true });
      return clone();
    }
    case 'delete_host':
      state.hosts = state.hosts.filter((h) => h.address !== args.address);
      return clone();
    case 'save_defaults':
      if (args.address) state.hosts.find((h) => h.address === args.address)!.settings = args.defaults;
      else state.defaults = args.defaults;
      return clone();
    case 'reset_host_settings':
      state.hosts.find((h) => h.address === args.address)!.settings = null;
      return clone();
    case 'update_check':
      state.update = { ...state.update, state: 'checking' };
      emit('state', clone());
      await wait(800);
      state.update = { ...state.update, state: 'available', checked_unix: Math.floor(Date.now() / 1000) };
      emit('state', clone());
      return null;
    case 'update_apply':
      for (let p = 0; p <= 100; p += 20) {
        state.update = { ...state.update, state: 'downloading', progress: p };
        emit('state', clone());
        await wait(300);
      }
      state.update = { ...state.update, state: 'installing', message: '正在安装，完成后会自动重新打开' };
      emit('state', clone());
      return null;
    case 'set_check_updates':
      state.check_updates = args.on;
      return clone();
    case 'set_client_name':
      state.client_name = args.name.trim();
      state.computer = state.client_name || state.computer_name;
      return clone();
    case 'diag':
      await wait(600);
      return '== NyaRemoteControl 客户端诊断 ==\n版本 0.2.0\n\n[0] Red Hat QXL controller vendor=1b36\n    显示器 \\\\.\\DISPLAY1 1920x1080 60Hz\n    硬件解码：[]\n\n音频输出：OK';
    case 'open_logs':
      return null;
  }
  throw new Error(`未知命令 ${cmd}`);
};
