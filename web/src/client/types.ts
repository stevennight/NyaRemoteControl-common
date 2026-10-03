// Shapes exchanged with nya-client (client/src/app/launcher.rs).
import type { UpdateInfo } from '../lib/UpdateCard.svelte';

export type SharedFolder = { path: string; name: string; read_only: boolean };

export type Defaults = {
  mode: string; // office | game
  fullscreen: boolean;
  display: number;
  bitrate_kbps: number;
  unlimited_bitrate: boolean;
  bitrate_policy: string; // auto | quality | balanced | smooth | fixed
  video_transport: string; // auto | stream | datagram
  max_fps: number;
  encoder: string;
  codec: string;
  chroma: string;
  audio: boolean;
  clipboard: boolean;
  hw_decode: boolean;
  vd_count: number;
  physical_off: boolean;
  block_input: boolean;
  vd_size: string; // window | screen | fixed
  vd_width: number;
  vd_height: number;
  vd_scale: boolean;
  /** Every host display in its own window. */
  multi_window: boolean;
  /** Microphone on after connecting. */
  mic: boolean;
  /** Keyboard captured after connecting. */
  grab_keyboard: boolean;
  /** Folders shown on the host as a drive. */
  shared_folders: SharedFolder[];
  print_mode: string; // print | open | save
  /** HDR10 video when both the host desktop and this monitor are HDR. */
  hdr: boolean;
  /** How the session travels: auto | udp | tcp (QUIC over TCP). */
  transport: string;
};

export type Host = {
  name: string;
  address: string;
  paired: boolean;
  /** Unix seconds, 0 = never. */
  last_connected: number;
  /** The name the host gives itself ('' until connected). */
  server_name: string;
  /** Named on this computer (otherwise follows server_name). */
  custom_name: boolean;
  /** Own connection settings; null = the defaults. */
  settings: Defaults | null;
};

export type ClientState = {
  version: string;
  /** This computer's name as hosts show it. */
  computer: string;
  /** Set on this computer ('' = the computer name). */
  client_name: string;
  computer_name: string;
  /** Look for a new version at start. */
  check_updates: boolean;
  update: UpdateInfo;
  /** Hardware decoding summary of this computer. */
  decode: string;
  hosts: Host[];
  defaults: Defaults;
};

/** Where a connection attempt is. */
export type Phase =
  | { phase: 'idle' }
  | { phase: 'connecting'; label: string }
  | { phase: 'pairing'; label: string }
  | { phase: 'pin_changed'; label: string }
  | { phase: 'verify'; label: string; fingerprint: string };

/** Host management dialog that is open. */
export type Local = { kind: 'add' } | { kind: 'rename'; host: Host } | { kind: 'delete'; host: Host } | null;

export function ago(unix: number): string {
  if (!unix) return '从未连接';
  const s = Date.now() / 1000 - unix;
  if (s < 60) return '刚刚';
  if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时前`;
  if (s < 86400 * 30) return `${Math.floor(s / 86400)} 天前`;
  return new Date(unix * 1000).toLocaleDateString('zh-CN');
}

/** Stable colour for a device card, from its name. */
export function hue(name: string): number {
  let h = 7;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h % 360;
}
