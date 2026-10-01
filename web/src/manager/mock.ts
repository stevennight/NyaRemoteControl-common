// Sample backend for working on the page in a normal browser.
import type { Mock } from '../lib/ipc';
import type { Component, Snapshot } from './types';

const now = Math.floor(Date.now() / 1000);
const snap: Snapshot = {
  elevated: true,
  computer: 'DESKTOP-C5E71I8',
  version: '0.2.0',
  svc: 'running',
  live: true,
  points_here: true,
  status: {
    server_version: '0.2.0',
    listen: '[::]:47100',
    listen_error: '',
    host: { running: true, console_session: 1, stream: '1920x1080@60 · hevc 4:4:4 · nvenc' },
    session: { client_name: 'DESKTOP-DEV', client_version: '0.2.0', remote_addr: '100.64.0.3:52011', since_unix: now - 42 * 60 },
    viewers: [{ client_name: '笔记本', client_version: '0.2.0', remote_addr: '100.64.0.9:50122', since_unix: now - 5 * 60 }],
    recent: [
      { unix: now - 42 * 60, kind: 'connected', text: 'DESKTOP-DEV（100.64.0.3）已连接' },
      { unix: now - 95 * 60, kind: 'disconnected', text: 'DESKTOP-DEV 断开：网络中断' },
      { unix: now - 26 * 3600, kind: 'pairing_failed', text: '100.64.0.9 配对失败：配对码错误' },
      { unix: now - 27 * 3600, kind: 'service', text: '服务已启动' },
    ],
  },
  code: 'K7QM-2XRA-9PLE-4TCD-HW8N-3JFV',
  fingerprint: '3205 4309 b911 ee42 2458 2fe7 9fc1 1c44',
  config: { port: 47100, bind: '::', name: '', encoder: 'auto', office_bitrate_kbps: 0, game_bitrate_kbps: 0, max_fps: 144, audio: true, log_level: 'info' },
  encoders: ['auto', 'nvenc', 'qsv', 'amf', 'software'],
  clients: [
    { fingerprint: 'a81c2f0d9e6b4471c0de55aa', name: 'DESKTOP-DEV', paired_at: '2026-09-27 17:05' },
    { fingerprint: '5e09bb1273ac4f0e991234cd', name: 'LAPTOP-HOME', paired_at: '2026-09-20 21:40' },
  ],
  load_error: null,
  busy: null,
  log_dir: 'C:\\ProgramData\\NyaRemoteControl\\logs',
};

const components: Component[] = [
  { id: 'vdd', name: '虚拟显示器', product: 'Virtual Display Driver 25.7.23', purpose: '在被控端新建显示器：分辨率跟随客户端窗口、多屏、隐私屏（本机显示器黑屏、本机键鼠屏蔽）。需要服务模式', status: '平时停用，有客户端需要时自动启用', installed: true, url: 'https://github.com/VirtualDrivers/Virtual-Display-Driver/releases', note: '免费开源；平时保持停用，不影响本机显示器' },
  { id: 'cable', name: '虚拟声卡', product: 'VB-Cable', purpose: '把客户端麦克风送进被控端：客户端工具条打开“麦克风”，被控端软件选择“CABLE Output”作为麦克风', status: null, installed: false, url: 'https://vb-audio.com/Cable/', note: '捐赠软件（安装即表示同意 VB-Audio 许可），需联网从官网下载；安装后需要重启一次' },
  { id: 'vigem', name: '手柄', product: 'ViGEmBus 1.22.0', purpose: '客户端的手柄在被控端显示为 Xbox 手柄', status: '驱动已加载', installed: true, url: 'https://github.com/nefarius/ViGEmBus/releases', note: '免费；作者已停止维护，但仍可用' },
  { id: 'usbip', name: 'USB 透传', product: 'usbip-win2 0.9.8.1', purpose: 'U 盾、加密狗等 USB 设备从客户端透传到被控端', status: null, installed: false, url: 'https://github.com/vadimgrn/usbip-win2/releases', note: '客户端另需 usbipd-win' },
];

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

export const mock: Mock = async (cmd, args, emit) => {
  await wait(100);
  switch (cmd) {
    case 'snapshot':
      return structuredClone(snap);
    case 'svc': {
      const label = { install: '安装服务', uninstall: '卸载服务', start: '启动服务', stop: '停止服务', restart: '重启服务' }[args.action as string] ?? '';
      snap.busy = label;
      emit('snapshot', structuredClone(snap));
      await wait(1200);
      snap.busy = null;
      if (args.action === 'stop') { snap.svc = 'stopped'; snap.live = false; }
      if (args.action === 'start' || args.action === 'restart' || args.action === 'install') { snap.svc = 'running'; snap.live = true; }
      if (args.action === 'uninstall') { snap.svc = 'not_installed'; snap.live = false; }
      emit('snapshot', structuredClone(snap));
      return `${label}完成`;
    }
    case 'reset_code':
      snap.code = 'Q2WE-8RTY-4UIO-PL9K-3JHG-7FDS';
      emit('snapshot', structuredClone(snap));
      return '已生成新配对码，已生效';
    case 'set_config':
      snap.config = args.config;
      return '设置已保存并生效';
    case 'remove_client':
      snap.clients = snap.clients.filter((c) => c.fingerprint !== args.fingerprint);
      emit('snapshot', structuredClone(snap));
      return '已移除';
    case 'disconnect':
      snap.status!.session = null;
      snap.status!.viewers = [];
      emit('snapshot', structuredClone(snap));
      return '已断开';
    case 'components':
      await wait(500);
      return structuredClone(components);
    case 'install':
      emit('install', { current: '虚拟声卡', status: '正在下载…', log: [], reboot: false, done: false });
      setTimeout(() => emit('install', { current: null, status: '', log: [{ error: false, text: '虚拟声卡 安装完成（需要重启）' }], reboot: true, done: true }), 1500);
      return { current: '虚拟声卡', status: '准备中…', log: [], reboot: false, done: false };
    case 'diag':
      await wait(1200);
      return '== NyaRemoteControl 被控端诊断 ==\n\n-- 显卡 --\n[0] NVIDIA GeForce GTX 1650 vendor=10de\n\n-- 显示器 --\nid=1 \\\\.\\DISPLAY1 1920x1080 @(0,0) 60Hz 主显示器=true\n虚拟显示器驱动：已安装（未启用，连接时按需启用）';
    case 'log':
      return Array.from({ length: 40 }, (_, i) => `2026-09-29T02:${String(i).padStart(2, '0')}:00Z  INFO nya_server::${args.name}: 示例日志 ${i}`).join('\n');
    default:
      return null;
  }
};
