// The "本机" section talks to its own part of the program (app/src/app/host.rs
// in the windows repository): every command and event is prefixed `host.`.
import { call as appCall, on as appOn } from '../lib/ipc';

export { errorText } from '../lib/ipc';

export function call<T = unknown>(cmd: string, args?: unknown): Promise<T> {
  return appCall<T>(`host.${cmd}`, args);
}

export function on<T = any>(event: string, cb: (data: T) => void): () => void {
  return appOn<T>(`host.${event}`, cb);
}
