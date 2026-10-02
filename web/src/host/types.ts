// Shapes exchanged with the "本机" model (windows repo: app/src/app/host.rs).

export type Config = {
  port: number;
  bind: string;
  name: string;
  encoder: string;
  office_bitrate_kbps: number;
  game_bitrate_kbps: number;
  max_fps: number;
  audio: boolean;
  log_level: string;
  /** Look for new versions (installing is always the user's choice). */
  check_updates: boolean;
};

export type Event = { unix: number; kind: string; text: string };

export type Status = {
  server_version: string;
  listen: string;
  listen_error: string;
  host: { running: boolean; console_session: number; stream: string } | null;
  /** The operating client. */
  session: Conn | null;
  /** Clients watching (not operating). */
  viewers: Conn[];
  recent: Event[];
};

export type Snapshot = {
  elevated: boolean;
  computer: string;
  version: string;
  svc: 'not_installed' | 'stopped' | 'running' | 'pending' | 'unknown';
  live: boolean;
  points_here: boolean | null;
  status: Status | null;
  code: string;
  fingerprint: string;
  config: Config;
  encoders: string[];
  clients: { fingerprint: string; name: string; paired_at: string }[];
  load_error: string | null;
  busy: string | null;
  log_dir: string;
};

export type Component = {
  id: string;
  name: string;
  product: string;
  purpose: string;
  status: string | null;
  installed: boolean;
  url: string;
  note: string;
};

export type Install = {
  current: string | null;
  status: string;
  log: { error: boolean; text: string }[];
  reboot: boolean;
  done: boolean;
} | null;

export function time(unix: number): string {
  if (!unix) return '';
  const d = new Date(unix * 1000);
  const today = new Date();
  const hm = d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
  if (d.toDateString() === today.toDateString()) return hm;
  return `${d.getMonth() + 1}月${d.getDate()}日 ${hm}`;
}

export function since(unix: number): string {
  const m = Math.floor((Date.now() / 1000 - unix) / 60);
  if (m < 1) return '刚刚连接';
  if (m < 60) return `已连接 ${m} 分钟`;
  return `已连接 ${Math.floor(m / 60)} 小时 ${m % 60} 分钟`;
}

export type Conn = { client_name: string; client_version: string; remote_addr: string; since_unix: number };
