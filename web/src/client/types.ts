// Shapes exchanged with nya-client (client/src/webui.rs).

export type Defaults = {
  mode: string; // office | game
  fullscreen: boolean;
  display: number;
  bitrate_kbps: number;
  unlimited_bitrate: boolean;
  bitrate_policy: string; // auto | quality | balanced | smooth | fixed
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
};

export type Host = {
  name: string;
  address: string;
  paired: boolean;
  /** Unix seconds, 0 = never. */
  last_connected: number;
};

export type ClientState = {
  version: string;
  computer: string;
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
