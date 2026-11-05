/**
 * Downloaded music: audio kept in the Cache API, an index kept in localStorage.
 *
 * WHY NOT A SERVICE-WORKER ROUTE. vite.config.js deliberately leaves /api/stream/
 * unrouted so audio requests never enter the worker and Range/seek behaves exactly
 * as it would with no worker installed. Routing audio through the worker to serve
 * downloads would have thrown that away and forced us to reimplement 206 handling.
 * Instead a downloaded track plays from a `blob:` URL, which seeks natively and
 * needs no worker involvement at all. Online playback is untouched.
 *
 * WHY TWO STORES. The bytes have to live in the Cache API - localStorage cannot
 * hold them and IndexedDB would mean hand-rolling blob chunking. The metadata has
 * to live somewhere readable *synchronously*, because the player decides whether a
 * track is local while assembling a state frame and cannot await there.
 *
 * THE INDEX CAN LIE, SO IT IS RECONCILED. The browser may evict Cache API storage
 * under pressure without touching localStorage, which would leave the app offering
 * a track that plays silence. `reconcile()` runs at startup and drops index entries
 * whose audio is gone. "Downloaded" means both halves exist, never just the index.
 */
import { writable, get } from 'svelte/store';

const CACHE = 'music-hub-offline';
const COVER_CACHE = 'music-hub-cover'; // same cache the service worker fills
const KEY = 'music-hub.offline.v1';
// The index also lives inside the offline Cache, under a synthetic key that is
// not audio. localStorage is the fast synchronous copy the player reads; this is
// the DURABLE copy.
//
// Why both: on 2026-08-30 a device showed 0 saved songs after a reload while the
// audio was still on disk. The index lived only in localStorage, and an Android
// WebView can clear Web Storage while leaving CacheStorage intact - two different
// storage buckets with different durability. v57's rebuildFromCache was meant to
// recover from exactly that, but it needs the network and a signed-in session to
// re-fetch metadata, so a reload while offline or pre-auth recovered nothing and
// never retried. Keeping the index beside the audio means the two share fate:
// restoreFromMirror() rebuilds localStorage from it at boot with no server at all.
//
// The key does not start with /api/stream/, so reconcile() and rebuildFromCache()
// both ignore it.
const INDEX_MIRROR = '/__offline-index__';

/** Downloaded tracks, newest first. Each entry is enough to render offline. */
export const downloads = writable([]);

/** id -> true, for the synchronous check the player needs. */
let present = new Set();

/** In-flight downloads, so the UI can show progress. id -> true */
export const downloading = writable({});

function readIndex() {
  try {
    const raw = localStorage.getItem(KEY);
    const list = raw ? JSON.parse(raw) : [];
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

// One JSON blob under one key, never key-per-track: a partial write would leave
// orphaned cache entries with nothing pointing at them.
function writeIndex(list) {
  try {
    localStorage.setItem(KEY, JSON.stringify(list));
  } catch {
    // Quota or private mode. The cache entry still exists and reconcile() will
    // drop it next boot, which is the safe direction to fail in.
  }
  present = new Set(list.map((t) => t.id));
  downloads.set(list);
}

// Write the durable copy of the index into the offline Cache. Best-effort and
// async: writeIndex() stays synchronous for the player, and callers that care
// about durability await this after a real mutation. Never called from the
// passive boot read, so a wiped localStorage cannot clobber a surviving mirror.
async function mirror(list) {
  if (!supported()) return;
  try {
    const cache = await caches.open(CACHE);
    await cache.put(
      INDEX_MIRROR,
      new Response(JSON.stringify(list), { headers: { 'Content-Type': 'application/json' } }),
    );
  } catch {
    // The cache is unavailable; localStorage is still the working copy.
  }
}

/**
 * Rebuild the localStorage index from the durable mirror when it is missing.
 *
 * The one recovery path that needs no network: if localStorage was cleared but
 * the audio (and the mirror beside it) survived, this restores the whole list
 * offline, before reconcile() runs. If localStorage is already populated it does
 * nothing - localStorage is the source of truth whenever it exists, and the
 * mirror only steps in when it has been lost.
 *
 * Returns the number of entries restored.
 */
export async function restoreFromMirror() {
  if (!supported()) return 0;
  if (readIndex().length > 0) return 0; // localStorage intact; nothing to do
  let cache;
  try {
    cache = await caches.open(CACHE);
  } catch {
    return 0;
  }
  try {
    const res = await cache.match(INDEX_MIRROR);
    if (!res) return 0;
    const list = JSON.parse(await res.text());
    if (!Array.isArray(list) || list.length === 0) return 0;
    writeIndex(list); // repopulates localStorage, present and downloads
    return list.length;
  } catch {
    return 0;
  }
}

/** Synchronous, because the player asks while building a state frame. */
export function has(id) {
  return !!id && present.has(id);
}

export function count() {
  return present.size;
}

function supported() {
  return typeof caches !== 'undefined';
}

/**
 * Drop index entries whose audio is missing. Called once at startup.
 * Returns the number pruned, so a caller can tell the user why things vanished.
 */
export async function reconcile() {
  const list = readIndex();
  // CANNOT CHECK IS NOT PERMISSION TO DELETE. An earlier version wiped the whole
  // index when the Cache API was unavailable, which turned a temporary inability
  // to verify into permanent data loss. Change nothing instead.
  if (!supported()) return 0;

  let cache;
  try {
    cache = await caches.open(CACHE);
  } catch {
    return 0;
  }

  const kept = [];
  let missing = 0;
  for (const t of list) {
    let hit = null;
    try {
      // eslint-disable-next-line no-await-in-loop
      hit = await cache.match(t.streamUrl, { ignoreVary: true });
    } catch {
      hit = null;
    }
    if (hit) kept.push(t);
    else missing += 1;
  }

  // Losing EVERY entry at once is far more likely to be a lookup bug than the
  // browser reclaiming every single file, and the cost of the two mistakes is
  // not symmetric: a stale entry is healed by objectUrlFor() the moment it is
  // used, while a wiped index loses the user's whole library with no way back.
  // So refuse the wholesale delete and let the per-track path sort it out.
  if (list.length > 0 && kept.length === 0) return 0;

  if (missing) writeIndex(kept);
  return missing;
}

/**
 * Rebuild index entries for audio that is in the cache but not in the index.
 *
 * This exists because the index and the audio can drift apart in BOTH
 * directions, and only one of them was handled. If the index is lost - a bug, a
 * cleared localStorage, a browser that reclaimed it separately - the audio is
 * still sitting there taking up space with nothing pointing at it, invisible and
 * unplayable. Recovering it needs the metadata back, which is why `lookup` is
 * passed in rather than imported: this module deliberately knows nothing about
 * the API.
 *
 * Entries whose metadata cannot be fetched are left alone rather than added with
 * placeholder titles, so a later online run can still recover them properly.
 */
export async function rebuildFromCache(lookup) {
  if (!supported() || typeof lookup !== 'function') return 0;
  let cache;
  try {
    cache = await caches.open(CACHE);
  } catch {
    return 0;
  }

  const known = new Set(readIndex().map((t) => t.streamUrl));
  let keys = [];
  try {
    keys = await cache.keys();
  } catch {
    return 0;
  }

  const orphans = [];
  for (const req of keys) {
    let path = '';
    try {
      path = new URL(req.url).pathname;
    } catch {
      continue;
    }
    if (path.startsWith('/api/stream/') && !known.has(path)) orphans.push(path);
  }
  if (!orphans.length) return 0;

  let recovered = 0;
  for (const path of orphans) {
    const id = decodeURIComponent(path.slice('/api/stream/'.length));
    if (!id) continue;
    try {
      // eslint-disable-next-line no-await-in-loop
      const track = await lookup(id);
      if (!track?.id) continue;
      // eslint-disable-next-line no-await-in-loop
      const res = await cache.match(path, { ignoreVary: true });
      // eslint-disable-next-line no-await-in-loop
      const bytes = res ? (await res.clone().blob()).size : 0;
      writeIndex([
        {
          id: track.id,
          title: track.title || '',
          artist: track.artist || '',
          album: track.album || '',
          duration: track.duration || 0,
          coverUrl: track.coverUrl || '',
          streamUrl: track.streamUrl || path,
          bytes,
          at: Date.now(),
        },
        ...readIndex().filter((x) => x.id !== track.id),
      ]);
      recovered += 1;
    } catch {
      // Offline, or the song is gone from the library. Leave the audio in place
      // so a later run can try again.
    }
  }
  if (recovered) await mirror(readIndex()); // re-establish the durable copy too
  return recovered;
}

/** A blob: URL for a downloaded track, or '' if it is not local after all. */
export async function objectUrlFor(id) {
  if (!supported() || !has(id)) return '';
  const entry = get(downloads).find((t) => t.id === id);
  if (!entry) return '';
  try {
    const cache = await caches.open(CACHE);
    const res = await cache.match(entry.streamUrl, { ignoreVary: true });
    if (!res) {
      // Evicted since the last reconcile. Correct the index now rather than
      // letting the player fail silently.
      await remove(id);
      return '';
    }
    return URL.createObjectURL(await res.blob());
  } catch {
    return '';
  }
}

/**
 * Store one track. Downloads the audio AND the cover art: /api/* is NetworkOnly,
 * so an entry whose artwork was never fetched would render blank offline.
 */
export async function download(track) {
  if (!supported() || !track?.id || !track?.streamUrl) return false;
  if (has(track.id)) return true;

  downloading.update((d) => ({ ...d, [track.id]: true }));
  try {
    const res = await fetch(track.streamUrl, { credentials: 'include' });
    if (!res.ok) throw new Error(`audio ${res.status}`);
    const body = await res.blob();

    const cache = await caches.open(CACHE);
    // Store the blob rather than the streamed Response: a Response body can only
    // be consumed once, and we need its size for the index.
    await cache.put(
      track.streamUrl,
      new Response(body, { headers: { 'Content-Type': body.type || 'audio/mpeg' } }),
    );

    if (track.coverUrl) {
      try {
        const cov = await fetch(track.coverUrl, { credentials: 'include' });
        if (cov.ok) (await caches.open(COVER_CACHE)).put(track.coverUrl, cov.clone());
      } catch {
        // Artwork is cosmetic; a missing cover must not fail the download.
      }
    }

    const entry = {
      id: track.id,
      title: track.title || '',
      artist: track.artist || '',
      album: track.album || '',
      duration: track.duration || 0,
      coverUrl: track.coverUrl || '',
      streamUrl: track.streamUrl,
      bytes: body.size || 0,
      at: Date.now(),
    };
    const next = [entry, ...readIndex().filter((t) => t.id !== entry.id)];
    writeIndex(next);
    await mirror(next); // durable copy beside the audio it points at
    return true;
  } catch {
    return false;
  } finally {
    downloading.update((d) => {
      const next = { ...d };
      delete next[track.id];
      return next;
    });
  }
}

/**
 * Store many. Sequential on purpose: a playlist fired off in parallel saturates
 * a phone's connection and makes the progress count meaningless.
 */
export async function downloadMany(tracks, onProgress) {
  const todo = (tracks || []).filter((t) => t?.id && t?.streamUrl && !has(t.id));
  let done = 0;
  let failed = 0;
  for (const t of todo) {
    // eslint-disable-next-line no-await-in-loop
    const ok = await download(t);
    done += 1;
    if (!ok) failed += 1;
    onProgress?.(done, todo.length, failed);
  }
  return { total: todo.length, failed };
}

export async function remove(id) {
  const entry = readIndex().find((t) => t.id === id);
  const next = readIndex().filter((t) => t.id !== id);
  writeIndex(next);
  await mirror(next);
  if (entry && supported()) {
    try {
      (await caches.open(CACHE)).delete(entry.streamUrl, { ignoreVary: true });
    } catch {
      // The index is already correct, which is the half that matters.
    }
  }
}

export async function clear() {
  writeIndex([]);
  if (supported()) {
    try {
      await caches.delete(CACHE);
    } catch {
      /* nothing useful to do */
    }
  }
}

/** Bytes used by downloads, from the index (exact) plus the browser's estimate. */
export async function usage() {
  const listed = readIndex().reduce((n, t) => n + (t.bytes || 0), 0);
  let quota = 0;
  try {
    const est = await navigator.storage?.estimate?.();
    quota = est?.quota || 0;
  } catch {
    /* not available everywhere */
  }
  return { bytes: listed, quota };
}

// Populate the synchronous view immediately so the player can answer has()
// before reconcile() finishes its async pass.
writeIndex(readIndex());
