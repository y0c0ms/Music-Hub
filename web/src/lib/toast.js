import { writable } from 'svelte/store';

export const toasts = writable([]);

// crypto.randomUUID() only exists in a secure context and this app is reachable
// over plain HTTP on a NodePort, where it is undefined. A counter is enough for
// keying a list that never leaves the tab.
let seq = 0;

/** kind: 'info' | 'success' | 'error' */
export function toast(message, kind = 'info', ms) {
  if (!message) return;
  const id = ++seq;
  // Errors stay on screen roughly twice as long: a failed delete is the one
  // message in this app that must not be missed.
  const life = ms ?? (kind === 'error' ? 9000 : 4000);
  toasts.update((list) => [...list, { id, message, kind }]);
  setTimeout(() => dismiss(id), life);
  return id;
}

export function dismiss(id) {
  toasts.update((list) => list.filter((t) => t.id !== id));
}
