import { get, writable } from 'svelte/store';
import { App as CapacitorApp } from '@capacitor/app';
import { expanded } from './player.js';

/** 'home' | 'search' | 'discover' | 'library' */
export const tab = writable('home');

/**
 * Drill-down overlays, hoisted out of the screens so an album tapped on Home and
 * an album tapped in Library open the same thing.
 *
 * A stack rather than a single slot: opening an album from inside an artist has
 * to come back to the artist, not dump the user on the tab they started from.
 * Entries are { type: 'album' | 'artist' | 'playlist' | 'settings', id, title }.
 */
export const stack = writable([]);

/**
 * Every navigation step gets a real history entry.
 *
 * Android's back button and the browser's back gesture both work by popping
 * browser history — they cannot see a store. The previous version kept a single
 * `armed` flag and pushed at most ONE entry no matter how deep the user was,
 * and pushed nothing at all when switching tabs. So back from any tab root left
 * the app instead of returning to Home, which on Android looks like the app
 * minimising to the launcher.
 *
 * `steps` mirrors the entries this app owns, newest last, each holding the undo
 * for its own step. When it empties, the next back belongs to the platform:
 * in the browser that leaves the site, on Android it backgrounds the app, which
 * is the correct behaviour at the root of a tab stack.
 */
const steps = [];

function record(undo) {
  steps.push(undo);
  history.pushState({ mh: steps.length }, '');
}

/** True when there is somewhere to go back to inside the app. */
export const canGoBack = () => steps.length > 0;

function pushLayer(entry) {
  stack.update((s) => [...s, entry]);
  record(() => stack.update((s) => s.slice(0, -1)));
}

export const openAlbum = (a) => pushLayer({ type: 'album', id: a.id, title: a.name });
export const openArtist = (a) => pushLayer({ type: 'artist', id: a.id, title: a.name });
export const openPlaylist = (p) => pushLayer({ type: 'playlist', id: p.id, title: p.name });
export const openBook = (b) => pushLayer({ type: 'book', id: b.id, title: b.title });
export const openSettings = () => pushLayer({ type: 'settings' });

/** The full-screen player is a layer too, so back collapses it before anything else. */
export function expandPlayer() {
  if (get(expanded)) return;
  expanded.set(true);
  record(() => expanded.set(false));
}

/**
 * Switching tabs is a navigation step, so back returns to the tab you came from.
 * Any open drill-down goes with it and is restored if the user comes back.
 */
export function setTab(id) {
  const from = get(tab);
  if (from === id) return;
  const layers = get(stack);
  const wasExpanded = get(expanded);

  tab.set(id);
  stack.set([]);
  expanded.set(false);

  record(() => {
    tab.set(from);
    stack.set(layers);
    expanded.set(wasExpanded);
  });
}

/**
 * In-app back (the arrow in a header). Routed through history rather than
 * mutating the store directly, so the button and the hardware key cannot
 * disagree about how deep the stack is.
 */
export function back() {
  if (steps.length) history.back();
}

/**
 * Wired once from App.svelte. Returns a teardown for symmetry with onMount.
 *
 * Two inputs have to end up in the same place. The browser's back gesture fires
 * `popstate` on its own. Android's hardware key does NOT: Capacitor 6 ships no
 * back handling in its core, so without the listener below the key falls
 * through to `Activity.finish()` and closes the app no matter how deep the user
 * is — the history entries above are correct and simply never consulted.
 *
 * `@capacitor/app` delivers the key to JS; we translate it into the same
 * `history.back()` the browser would have done. The guard is our own step
 * count, not the WebView's `canGoBack()`, because the WebView's history also
 * holds the Keycloak redirect chain — trusting it would walk the user back into
 * the login page from the home screen. With nothing of ours left to pop we
 * minimise rather than exit, so audio keeps playing and the app resumes where
 * it was, which is what a music player should do.
 */
export function listen() {
  const onPop = () => {
    const undo = steps.pop();
    if (undo) undo();
  };
  window.addEventListener('popstate', onPop);

  let removeKey = () => {};
  CapacitorApp.addListener('backButton', () => {
    if (steps.length) history.back();
    else CapacitorApp.minimizeApp();
  })
    .then((h) => {
      removeKey = () => h.remove();
    })
    .catch(() => {});

  return () => {
    window.removeEventListener('popstate', onPop);
    removeKey();
  };
}
