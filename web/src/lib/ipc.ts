// Page <-> Rust messages (crates/nya-webui).
//   call(cmd, args)  -> window.ipc.postMessage({id, cmd, args}); Rust answers via __nya.reply
//   on(event, cb)    <- Rust calls __nya.event(name, data)
// Outside the app (plain browser, `npm run dev`) a mock backend answers instead.

type Pending = { resolve: (v: unknown) => void; reject: (e: Error) => void };
type Listener = (data: any) => void;
export type Mock = (cmd: string, args: any, emit: (event: string, data: unknown) => void) => Promise<unknown>;

declare global {
  interface Window {
    ipc?: { postMessage(msg: string): void };
    __nya?: { reply(msg: { id: number; ok: boolean; data?: unknown; error?: string }): void; event(name: string, data: unknown): void };
  }
}

const pending = new Map<number, Pending>();
const listeners = new Map<string, Set<Listener>>();
let nextId = 1;
let mock: Mock | null = null;

function dispatch(name: string, data: unknown) {
  listeners.get(name)?.forEach((l) => l(data));
}

window.__nya = {
  reply(msg) {
    const p = pending.get(msg.id);
    if (!p) return;
    pending.delete(msg.id);
    if (msg.ok) p.resolve(msg.data);
    else p.reject(new Error(msg.error ?? '未知错误'));
  },
  event: dispatch,
};

/** Running inside the app (WebView2 with the Rust side attached)? */
export const inApp = typeof window.ipc?.postMessage === 'function';

export function setMock(m: Mock) {
  mock = m;
}

export function call<T = unknown>(cmd: string, args?: unknown): Promise<T> {
  if (!inApp) {
    if (!mock) return Promise.reject(new Error('没有连接到程序'));
    return mock(cmd, args ?? null, dispatch) as Promise<T>;
  }
  const id = nextId++;
  return new Promise<T>((resolve, reject) => {
    pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
    window.ipc!.postMessage(JSON.stringify({ id, cmd, args: args ?? null }));
  });
}

/** Subscribe to an event from Rust; returns the unsubscribe function. */
export function on<T = any>(event: string, cb: (data: T) => void): () => void {
  let set = listeners.get(event);
  if (!set) listeners.set(event, (set = new Set()));
  set.add(cb);
  return () => set!.delete(cb);
}

export function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
