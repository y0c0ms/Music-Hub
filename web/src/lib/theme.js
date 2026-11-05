import { writable } from 'svelte/store';

/**
 * Light / Dark / System, with System following the device.
 *
 * Stored in localStorage rather than in the user's server-side settings on
 * purpose: this is a property of the DEVICE, not the account. The same login on
 * a phone at night and a desktop in daylight should be allowed to differ, and a
 * server round-trip on boot would mean rendering the wrong theme first and
 * correcting it visibly.
 *
 * index.html applies the stored choice before first paint. This module owns it
 * from then on and must agree with that inline script — see THEME_BOOT there.
 */

const KEY = 'mh-theme';
/** @typedef {'light'|'dark'|'system'} ThemeChoice */

const media = () => window.matchMedia('(prefers-color-scheme: dark)');

function read() {
  try {
    const v = localStorage.getItem(KEY);
    return v === 'light' || v === 'dark' || v === 'system' ? v : 'system';
  } catch {
    // Private mode, or storage disabled by policy. Following the device is the
    // right answer when we are not allowed to remember one.
    return 'system';
  }
}

/** The choice the user made. */
export const theme = writable(read());

/** What that choice currently resolves to — 'light' or 'dark'. */
export const resolved = writable(
  read() === 'system' ? (media().matches ? 'dark' : 'light') : read(),
);

function apply(choice) {
  const dark = choice === 'system' ? media().matches : choice === 'dark';
  document.documentElement.classList.toggle('dark', dark);
  resolved.set(dark ? 'dark' : 'light');

  // The browser/OS chrome has to move with the app: theme-color paints Android's
  // status bar and the PWA title bar, and color-scheme decides the colour of
  // scrollbars, the keyboard, and native form controls. Leaving these behind is
  // what makes a themed web app still feel like it is in the wrong mode.
  const bg = dark ? '#020617' : '#f8fafc';
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', bg);
  document
    .querySelector('meta[name="color-scheme"]')
    ?.setAttribute('content', dark ? 'dark' : 'light');
}

export function setTheme(choice) {
  theme.set(choice);
  try {
    localStorage.setItem(KEY, choice);
  } catch {
    // Not being able to persist is not a reason to refuse to switch.
  }
  apply(choice);
}

/** Wired once from App.svelte; returns a teardown. */
export function listen() {
  let current = read();
  apply(current);

  // Only relevant while the choice is System, but the listener stays attached so
  // switching back to System picks up a change made while it was overridden.
  const onSystem = () => {
    if (current === 'system') apply('system');
  };
  const mq = media();
  mq.addEventListener('change', onSystem);

  const unsub = theme.subscribe((v) => {
    current = v;
  });

  return () => {
    mq.removeEventListener('change', onSystem);
    unsub();
  };
}
