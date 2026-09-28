// Short notices in the bottom-right corner.
export type Toast = { id: number; kind: 'info' | 'ok' | 'error'; text: string };

let next = 1;
export const toasts = $state<Toast[]>([]);

export function toast(text: string, kind: Toast['kind'] = 'info', ms = kind === 'error' ? 8000 : 3500) {
  const id = next++;
  toasts.push({ id, kind, text });
  setTimeout(() => dismiss(id), ms);
}

export function dismiss(id: number) {
  const i = toasts.findIndex((t) => t.id === id);
  if (i >= 0) toasts.splice(i, 1);
}
