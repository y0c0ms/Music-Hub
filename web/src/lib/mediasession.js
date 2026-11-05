/**
 * The bridge between the player and whatever owns media controls on this
 * platform.
 *
 * Two implementations, because the same app runs in two places that disagree
 * about how a media session works:
 *
 *   Browser / installed PWA  -> `navigator.mediaSession`, the Web API.
 *   Android (Capacitor)      -> a native plugin.
 *
 * The Android WebView does NOT implement the Web API - `'mediaSession' in
 * navigator` is false there, verified on device. Every guarded block written
 * against the Web API silently did nothing, which is why headphone buttons,
 * the lockscreen and Bluetooth controls were all dead while audio played fine.
 * The plugin does not polyfill the Web API either, so this adapter is the only
 * place that knows which of the two is in play.
 *
 * The native side also drags a foreground service up with it. That matters
 * more than the buttons: without it Android kills the whole app seconds after
 * the screen goes off - measured, the process disappears entirely and playback
 * dies with it.
 */

import { Capacitor } from '@capacitor/core';
import { MediaSession as NativeSession } from '@jofr/capacitor-media-session';

const native = Capacitor.isNativePlatform();
const web = !native && typeof navigator !== 'undefined' && 'mediaSession' in navigator;

/**
 * Artwork has to travel as a data URL on Android.
 *
 * The plugin loads artwork from Java, and `/api/cover/{id}` sits behind
 * RequireLinked - a Java-side fetch carries none of the WebView's session
 * cookie and gets a 401, so the lockscreen would just show a blank square.
 * Fetching here means the cookie comes along.
 */
const artworkCache = new Map();

async function artworkDataUrl(url) {
  if (!url) return null;
  if (artworkCache.has(url)) return artworkCache.get(url);

  const promise = (async () => {
    try {
      const res = await fetch(url, { credentials: 'include' });
      if (!res.ok) return null;
      const blob = await res.blob();
      if (blob.size > 2_000_000) return null; // don't push absurd images over the bridge
      return await new Promise((resolve) => {
        const reader = new FileReader();
        reader.onloadend = () => resolve(typeof reader.result === 'string' ? reader.result : null);
        reader.onerror = () => resolve(null);
        reader.readAsDataURL(blob);
      });
    } catch {
      return null;
    }
  })();

  artworkCache.set(url, promise);
  return promise;
}

export async function setMetadata(track) {
  if (!track) return;
  const title = track.title || 'Unknown title';
  const artist = track.artist || 'Unknown artist';
  const album = track.album || '';

  if (native) {
    const src = await artworkDataUrl(track.coverUrl);
    try {
      await NativeSession.setMetadata({
        title,
        artist,
        album,
        artwork: src ? [{ src, sizes: '512x512', type: 'image/jpeg' }] : [],
      });
    } catch {
      // A failed notification update must never take playback down with it.
    }
    return;
  }

  if (!web || typeof window.MediaMetadata !== 'function') return;
  navigator.mediaSession.metadata = new window.MediaMetadata({
    title,
    artist,
    album,
    artwork: track.coverUrl
      ? [
          { src: track.coverUrl, sizes: '192x192', type: 'image/jpeg' },
          { src: track.coverUrl, sizes: '512x512', type: 'image/jpeg' },
        ]
      : [],
  });
}

/** 'playing' | 'paused' | 'none'. On Android this starts and stops the service. */
export function setPlaybackState(state) {
  if (native) {
    NativeSession.setPlaybackState({ playbackState: state }).catch(() => {});
    return;
  }
  if (web) navigator.mediaSession.playbackState = state;
}

export function setPositionState({ duration, position, playbackRate = 1 }) {
  if (!Number.isFinite(duration) || duration <= 0) return;
  const pos = Math.max(0, Math.min(position, duration));
  if (native) {
    NativeSession.setPositionState({ duration, position: pos, playbackRate }).catch(() => {});
    return;
  }
  if (!web || !navigator.mediaSession.setPositionState) return;
  try {
    navigator.mediaSession.setPositionState({ duration, position: pos, playbackRate });
  } catch {
    // Some engines throw if position briefly exceeds duration mid-seek.
  }
}

/** `handlers` is keyed by media session action name. */
export function setActionHandlers(handlers) {
  for (const [action, fn] of Object.entries(handlers)) {
    if (native) {
      NativeSession.setActionHandler({ action }, fn).catch(() => {});
      continue;
    }
    if (!web) continue;
    try {
      navigator.mediaSession.setActionHandler(action, fn);
    } catch {
      // Older browsers reject unknown actions rather than ignoring them.
    }
  }
}

/** True when something is listening, so the player can skip needless work. */
export const active = native || web;
