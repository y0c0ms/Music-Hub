/**
 * The player: one <audio> element for the whole app, driven by ONE playback
 * state that lives on the server.
 *
 * See docs/design/session-playback.md. Every device signed into the account
 * renders the same queue, track and position, and any of them can drive it - but
 * only one makes sound. The others are remotes.
 *
 * **Every export below keeps the signature it had before this became shared.**
 * Nine components import this module and none of them touch the audio element;
 * they call play()/next()/toggle() and read the `player` store. That is why this
 * could be replaced wholesale without editing a single component, and it is
 * worth preserving.
 *
 * The shape of every control is the same three steps:
 *
 *   1. optimistic local patch, so the UI answers the tap immediately
 *   2. POST /api/playback/command
 *   3. the state that comes back - or the next SSE frame - is authoritative
 *
 * Two things to keep straight while reading:
 *
 *   - The server's queue holds track **IDs**; this store holds track
 *     **objects**. `cache` bridges them, and `hydrate` fills in anything queued
 *     by another device that this one has never seen.
 *   - `el.src` is set **only when this device is active**. That single condition
 *     is the whole enforcement mechanism for "one device makes sound".
 */
import { derived, get, writable } from 'svelte/store';
import * as api from './api.js';
import * as saved from './saved.js';
import * as session from './mediasession.js';
import { bookIdFor, rateFor, stepSleep } from './bookmark.js';

const el = new Audio();
el.preload = 'metadata';
// An <audio> without controls renders nothing, so attaching it costs no layout
// and avoids the mobile engines that only wire background playback and
// lockscreen controls to elements that are actually in the document.
document.body.appendChild(el);

// --- this device --------------------------------------------------------------

const DEVICE_KEY = 'music-hub.device';

/**
 * A stable id for this browser, so the server can tell devices apart and the
 * picker can name them. Survives reloads; a new profile or a cleared store is
 * simply a new device, which is honest.
 */
function loadDevice() {
  try {
    const raw = localStorage.getItem(DEVICE_KEY);
    if (raw) return JSON.parse(raw);
  } catch {
    // Private mode, or a corrupt value. Fall through and mint a fresh one.
  }
  const id =
    globalThis.crypto?.randomUUID?.() ??
    `dev-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  const dev = { id, name: guessName() };
  try {
    localStorage.setItem(DEVICE_KEY, JSON.stringify(dev));
  } catch {
    // Not fatal: an unstable id means the picker shows a duplicate after a
    // reload, which is better than refusing to play.
  }
  return dev;
}

function guessName() {
  const ua = navigator.userAgent || '';
  const browser = /Firefox/.test(ua)
    ? 'Firefox'
    : /Edg\//.test(ua)
      ? 'Edge'
      : /Chrome|CriOS/.test(ua)
        ? 'Chrome'
        : /Safari/.test(ua)
          ? 'Safari'
          : 'Browser';
  const os = /Android/.test(ua)
    ? 'Android'
    : /iPhone|iPad/.test(ua)
      ? 'iOS'
      : /Linux/.test(ua)
        ? 'Linux'
        : /Windows/.test(ua)
          ? 'Windows'
          : /Mac OS/.test(ua)
            ? 'Mac'
            : '';
  // The Capacitor shell reports a WebView; calling that "the app" is more
  // useful to a human picking a device than "Chrome on Android".
  if (/wv\)/.test(ua) || /Median|Capacitor/.test(ua)) return 'Music Hub app';
  return os ? `${browser} on ${os}` : browser;
}

const device = loadDevice();
export const deviceId = device.id;
export const deviceName = device.name;

// --- stores -------------------------------------------------------------------

/**
 * `order` is the playback sequence as indices into `queue`, which is what makes
 * shuffle non-destructive - the same contract the server keeps.
 */
export const player = writable({
  queue: [],
  order: [],
  cursor: -1,
  track: null,
  playing: false,
  loading: false,
  shuffle: false,
  repeat: 'off',
  error: null,
});

export const progress = writable({ time: 0, duration: 0, buffered: 0 });
export const expanded = writable(false);

/** Devices signed into this account, for the picker. */
export const devices = writable([]);

/** Which device is making sound; '' when nobody is. */
export const activeId = writable('');

/** Is THIS device the one making sound. */
export const isActive = derived(activeId, (id) => id === deviceId);

/** Is the event stream up. A remote with no stream is showing stale state. */
export const connected = writable(false);

/**
 * Disconnected for long enough to be worth telling the user about.
 *
 * `connected` flips on every blip, and a reconnect takes about a second, so
 * rendering it directly made the player flash "Reconnecting..." at anyone with a
 * flaky link - reported 2026-08-25 as "quite annoying, considering the main task
 * of a music app is to keep playing music". The banner is for an outage the user
 * needs to know about, not for the transport doing its job.
 */
export const offline = writable(false);
let offlineTimer = null;
const OFFLINE_AFTER = 5000;

function setLink(up) {
  connected.set(up);
  clearTimeout(offlineTimer);
  if (up) {
    offline.set(false);
    // The server is back. Stop serving locally and DISCARD the local state
    // rather than merging it - see the note on localCommand.
    servingLocally.set(false);
    localSt = null;
    return;
  }
  offlineTimer = setTimeout(() => {
    offline.set(true);
    // Latch into local mode as soon as the link is genuinely down, not on the
    // first command that fails. Without this every button press pays the whole
    // retry ladder - 300+900+2400ms of backoff plus three request timeouts -
    // before anything happens, which is what "pause takes a while to stop" was.
    if (saved.count() > 0) servingLocally.set(true);
  }, OFFLINE_AFTER);
}

// --- track cache --------------------------------------------------------------
//
// The server broadcasts IDs. Rendering needs objects, and every client already
// has GET /api/library/song/{id} plus this map, which is exactly why the frames
// stay small.

const cache = new Map();
const pending = new Set();

function remember(tracks) {
  for (const t of tracks || []) if (t?.id) cache.set(t.id, t);
}

// Seed the cache from saved-offline tracks. hydrate() below reaches the API,
// which is NetworkOnly in the service worker and therefore dead with no
// connection - without this an offline queue renders a column of "…" for music
// that is sitting on the disk. reconcile() first, so a track whose audio the
// browser evicted is never offered.
saved
  .restoreFromMirror()
  // The durable copy of the index lives in the offline Cache beside the audio.
  // Restoring from it first means a device whose localStorage was cleared - the
  // 2026-08-30 failure - is whole again before anything reads the index, and it
  // needs no network, unlike rebuildFromCache below.
  .then(() => saved.reconcile())
  // Then recover the other direction: audio still in the cache that the index no
  // longer lists. That happens when even the mirror is gone but the audio is not.
  // Needs the network for the metadata, so it quietly does nothing while offline
  // and succeeds on a later launch.
  .then(() => saved.rebuildFromCache((id) => api.song(id)))
  .then(() => remember(get(saved.downloads)))
  .catch(() => {});

// A track that never hydrates is indistinguishable from one that is loading:
// the queue holds ids, and syncAudio deliberately waits rather than tearing the
// element down. So a fetch that fails without a retry stops the music in
// perfect silence. Bounded attempts, then say so out loud.
const failures = new Map(); // id -> attempts so far
const MAX_HYDRATE_TRIES = 4;

function fetchTrack(id) {
  return api
    .song(id)
    .then((t) => {
      if (t?.id) {
        cache.set(t.id, t);
        failures.delete(id);
      }
    })
    .catch(() => {})
    .finally(() => pending.delete(id));
}

/**
 * Fetch anything another device queued that this one has never seen.
 *
 * `currentId` is the track that is supposed to be making sound. It is the only
 * one whose absence is worth telling the user about - the rest are just names
 * further down a list.
 */
function hydrate(list, currentId) {
  const missing = (list || []).filter((id) => id && !cache.has(id) && !pending.has(id));
  if (!missing.length) return;
  for (const id of missing) pending.add(id);
  Promise.all(missing.map(fetchTrack)).then(() => {
    // Re-render with the names now known. Nothing else changed, so this is a
    // pure repaint rather than a state transition.
    if (last) apply(last);

    for (const id of missing) {
      if (cache.has(id)) continue;
      const n = (failures.get(id) || 0) + 1;
      failures.set(id, n);
      if (n < MAX_HYDRATE_TRIES) {
        // Linear backoff: this is a phone radio waking up, not a thundering herd.
        setTimeout(() => {
          if (!cache.has(id)) hydrate([id], currentId);
        }, 800 * n);
        continue;
      }
      if (id === currentId) {
        reportClientError('hydrate-failed', `attempts=${n}`, `track ${id}`);
        player.update((s) => ({ ...s, loading: false, error: 'Could not load this track' }));
      }
    }
  });
}

// --- applying server state ----------------------------------------------------

let version = 0;
let last = null;
// What positionSeq we last knew about, updated for every frame whether or not
// this device is the one making sound. `appliedPositionSeq` cannot answer this:
// syncAudio returns early on a remote, so it stays behind on exactly the devices
// most likely to be stale.
let lastSeenPositionSeq = 0;

function trackAt(st, queue) {
  const order = st.order || [];
  if (st.cursor < 0 || st.cursor >= order.length) return null;
  const i = order[st.cursor];
  return i >= 0 && i < queue.length ? queue[i] : null;
}

function apply(st) {
  if (!st) return;
  last = st;
  version = st.version || 0;
  lastSeenPositionSeq = st.positionSeq ?? 0;
  activeId.set(st.activeId || '');

  const queue = (st.queue || []).map(
    // A placeholder rather than a hole: a remote must render a queue it has not
    // finished fetching without throwing on `track.title`.
    (id) => cache.get(id) || { id, title: '…', artist: '', streamUrl: '' },
  );
  const track = trackAt(st, queue);

  player.update((p) => ({
    ...p,
    queue,
    order: st.order || [],
    cursor: st.cursor ?? -1,
    track,
    playing: !!st.playing,
    shuffle: !!st.shuffle,
    repeat: st.repeat || 'off',
  }));

  hydrate(st.queue, track?.id);
  syncAudio(st, track);
  syncProgress(st, track);
  if (track) session.setMetadata(track);
  session.setPlaybackState(st.playing ? 'playing' : track ? 'paused' : 'none');
}

/**
 * The enforcement point: this device only ever holds a `src` while it is the
 * active one. A remote keeps a silent element and identical state.
 */
// A saved track plays from a blob: URL built from the Cache API. That keeps
// /api/stream/ unrouted in the service worker - see vite.config.js - so Range and
// seek behave exactly as they do with no worker installed. A blob URL pins its
// data until revoked, so every swap must release the previous one.
let srcObjectUrl = '';

function releaseObjectUrl() {
  if (srcObjectUrl) {
    URL.revokeObjectURL(srcObjectUrl);
    srcObjectUrl = '';
  }
}

/** Point the element at a track: the local copy when there is one, else the API. */
function setSource(track) {
  releaseObjectUrl();
  if (!saved.has(track.id)) {
    el.src = track.streamUrl;
    return;
  }
  // Reading the cache is async, so clear the element and let the existing
  // pendingSeek/loadedmetadata path apply the position when it lands. Setting the
  // network URL first would double-load every saved track.
  const want = track.id;
  el.removeAttribute('src');
  saved
    .objectUrlFor(track.id)
    .then((url) => {
      if (appliedTrackId !== want) {
        if (url) URL.revokeObjectURL(url); // the queue moved on while we read
        return;
      }
      // Falling back to the network on a miss keeps a track playable if the
      // cache was evicted between has() and the read.
      srcObjectUrl = url || '';
      el.src = url || track.streamUrl;
      if (last?.playing) start();
    })
    .catch(() => {
      if (appliedTrackId === want) el.src = track.streamUrl;
    });
}

function teardown() {
  releaseObjectUrl();
  if (!el.getAttribute('src')) return;
  // Keep where we were. Losing the element is not the same as losing the place
  // in the song, and the server only knows what our last progress POST told it.
  if (el.currentTime > 0) localPos = el.currentTime;
  el.pause();
  el.removeAttribute('src');
  el.load();
}

function syncAudio(st, track) {
  const mine = (st.activeId || '') === deviceId;

  // Not this device, or nothing selected at all: never hold a src.
  if (!mine || !track) {
    teardown();
    return;
  }

  // The track is in the queue but its metadata has not arrived yet, so its
  // stream URL is unknown. LEAVE THE ELEMENT ALONE.
  //
  // This used to fall into the teardown above, which was the bug behind
  // "Playback failed" when skipping (reported 2026-08-25): the queue holds IDs
  // and the objects are fetched lazily, so a `next` onto a track this device had
  // not cached - after a reload, or queued from another device - produced one
  // frame with a placeholder. Tearing down on that frame stopped the audio AND
  // aborted the in-flight play(), whose rejection surfaced as a failure. The
  // hydrate() that follows re-applies the same state with the real object.
  if (!track.streamUrl) return;

  const hadSrc = !!el.getAttribute('src');
  const sameTrack = appliedTrackId === track.id;
  // A saved track holds a blob: URL, which carries no identity - comparing it to
  // streamUrl would report a change on EVERY state frame and restart playback
  // several times a second. Ask about the track for those, the attribute for the
  // rest (where it still catches an element torn down underneath us).
  const changedSrc = saved.has(track.id)
    ? !sameTrack || !hadSrc
    : el.getAttribute('src') !== track.streamUrl;
  appliedTrackId = track.id;
  if (changedSrc) {
    setSource(track);
  }

  // Apply the position when a COMMAND moved it - a seek or a jump from any
  // device, or a track change. `positionSeq` is the server's explicit signal;
  // see the comment on State.PositionSeq for why `version` cannot answer this.
  const moved = st.positionSeq !== appliedPositionSeq;
  appliedPositionSeq = st.positionSeq;

  if (changedSrc || moved) {
    let at = (st.positionMs || 0) / 1000;

    // Re-claiming the SAME track after losing the element must not rewind.
    //
    // Reported 2026-08-25: "when it loses the connection and I click play it
    // restarts the song completely". The chain was ActiveGrace (30s) firing while
    // the stream churned (also ~30s), so a frame arrived saying nobody was
    // active, this device tore its element down, and the next frame put the src
    // back with the server's idea of the position - which is only ever as fresh
    // as our last progress POST, and is 0 for a track that had just started.
    //
    // The active device owns the position; that is the whole design. So when the
    // track has not changed and no COMMAND moved playback, our own clock wins.
    if (sameTrack && !hadSrc && !moved && localPos > at) {
      at = localPos;
    }

    // A fresh track starts at zero anyway, and setting currentTime on media that
    // has not loaded yet is a needless second load - which is one more thing
    // that can abort the play() below.
    if (!(changedSrc && at === 0)) {
      try {
        el.currentTime = at;
      } catch {
        pendingSeek = at; // not seekable yet; loadedmetadata retries
      }
    }
  }

  if (st.playing && el.paused) start();
  if (!st.playing && !el.paused) el.pause();
}

// This device's own playback position, kept across a teardown, and the track it
// belongs to. The active device is the authority on where the music is; the
// server only knows what the last progress POST told it.
let localPos = 0;
let appliedTrackId = '';

// -1 rather than 0 so the very first frame counts as "moved" and resumes where
// the state says, instead of starting a restored queue from the top.
let appliedPositionSeq = -1;

let pendingSeek = 0;
el.addEventListener('loadedmetadata', () => {
  if (pendingSeek > 0) {
    try {
      el.currentTime = pendingSeek;
    } catch {
      // Give up rather than loop; a second of lost position is not worth more.
    }
    pendingSeek = 0;
  }
});

function start() {
  const p = el.play();
  if (p && typeof p.catch === 'function') {
    p.catch((err) => {
      const name = err?.name || 'Error';

      // AbortError is NOT a failure. It means a newer load or a pause superseded
      // this play, which is normal when state frames arrive close together -
      // and showing it made skipping look broken while the next frame was
      // already fixing it.
      if (name === 'AbortError') return;

      reportClientError('play-rejected', name, err?.message);
      player.update((s) => ({
        ...s,
        loading: false,
        error:
          name === 'NotAllowedError'
            ? 'Tap play here once to allow audio on this device'
            : 'Playback failed',
      }));
    });
  }
}

/**
 * Tell the server what the browser actually said.
 *
 * This exists because "Playback failed" was a string with no counterpart in any
 * log: the server could only show the command succeed, which proved where the
 * fault was not and nothing about where it was. A player that only breaks on a
 * device whose console nobody can open is undiagnosable without this.
 *
 * Buffered through localStorage rather than fire-and-forget. The first version
 * dropped a failed POST on the floor, reasoning that a diagnostic must never
 * fail the thing it reports on - true, but it also meant the failures caused by
 * THIS SERVER BEING AWAY were exactly the ones that left no trace. A pod restart
 * severed the audio, and the report of it died with the same connection.
 *
 * So: try immediately, keep what does not land, flush on the next healthy frame.
 */
const REPORT_KEY = 'mh.reports';
const MAX_BUFFERED = 20;

function bufferedReports() {
  try {
    const raw = localStorage.getItem(REPORT_KEY);
    const list = raw ? JSON.parse(raw) : [];
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

function saveReports(list) {
  try {
    // Newest kept: an old outage matters less than the one happening now.
    localStorage.setItem(REPORT_KEY, JSON.stringify(list.slice(-MAX_BUFFERED)));
  } catch {
    // Private mode, or full. Losing telemetry is acceptable; throwing is not.
  }
}

function postReport(payload) {
  return fetch('/api/playback/report', {
    method: 'POST',
    credentials: 'include',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  }).then((res) => {
    if (!res.ok) throw new Error(`report ${res.status}`);
  });
}

/** Send anything an earlier outage could not deliver. Called when SSE opens. */
function flushReports() {
  const list = bufferedReports();
  if (!list.length) return;
  saveReports([]);
  // Sequential and best-effort: whatever fails goes back on the pile.
  list.reduce(
    (chain, payload) =>
      chain.then(() =>
        postReport(payload).catch(() => {
          saveReports([...bufferedReports(), payload]);
        }),
      ),
    Promise.resolve(),
  );
}

function reportClientError(event, name, message) {
  try {
    const payload = {
      deviceId,
      event,
      name: String(name || ''),
      message: String(message || ''),
      trackId: get(player).track?.id || '',
      src: el.getAttribute('src') || '',
      at: new Date().toISOString(),
    };
    postReport(payload).catch(() => {
      saveReports([...bufferedReports(), payload]);
    });
  } catch {
    // Never let telemetry throw into a playback path.
  }
}

// --- position -----------------------------------------------------------------
//
// The active device owns the truth and reports every few seconds. A remote
// renders `positionMs + (now - positionAt)`, which is what lets a progress bar
// move without a continuous feed.

function syncProgress(st, track) {
  if ((st.activeId || '') === deviceId) return; // driven by timeupdate instead
  progress.set({
    time: remoteTime(st),
    duration: track?.duration || 0,
    buffered: 0,
  });
}

function remoteTime(st) {
  const base = (st.positionMs || 0) / 1000;
  // No active device means nothing can be making sound, so the position cannot
  // be advancing - whatever `playing` claims. The server enforces this
  // invariant now, but a bar that runs forward in silence is such a convincing
  // lie that it is worth refusing to draw it here as well.
  if (!st.playing || !st.activeId || !st.positionAt) return base;
  const drift = (Date.now() - Date.parse(st.positionAt)) / 1000;
  return drift > 0 ? base + drift : base;
}

// Remotes tick their own clock; the active device gets this from the element.
setInterval(() => {
  if (!last || get(isActive)) return;
  if (!last.playing) return;
  progress.update((p) => ({ ...p, time: remoteTime(last) }));
}, 1000);

let lastReport = 0;
function report(force = false) {
  if (!get(isActive)) return;
  const now = Date.now();
  if (!force && now - lastReport < 5000) return;
  lastReport = now;
  api.playbackProgress(
    deviceId,
    el.currentTime * 1000,
    !el.paused,
    Number.isFinite(el.duration) ? el.duration * 1000 : 0,
  );
}

// --- sending commands ---------------------------------------------------------

// next/prev are RELATIVE: replaying one against a newer state would skip twice,
// which is the exact bug the version field exists to prevent.
//
// But "stale" has two causes and only one of them means the user's intent was
// already satisfied:
//
//   * another device tapped next at the same version -> the winner advanced the
//     queue, so dropping our tap is correct.
//   * this tab simply missed a frame -> NOBODY advanced anything, and dropping
//     the tap makes the button feel dead. Backgrounded tabs are throttled by the
//     browser, so this is the common case, not the exotic one.
//
// `positionSeq` tells them apart exactly: it changes only when a command moved
// the cursor or the position. Unchanged means nothing advanced, so we rebase onto
// the state the 409 handed us and send once more. A 409 is returned before any
// mutation, so that is exactly one application, never two.
const RELATIVE = new Set(['next', 'prev']);

// --- offline fallback ---------------------------------------------------------
//
// Playback is server-authoritative: controls post a command and the UI only moves
// when a state frame comes back. Correct with a server, and completely dead
// without one - which would make saved music unplayable on a plane, the entire
// point of saving it.
//
// So once the retry ladder in send() has genuinely given up, the same command is
// applied to a local state object of the SAME SHAPE and pushed through apply().
// Every consumer - queue view, mini player, media session, syncAudio - keeps
// working unchanged, because none of them can tell where the frame came from.
//
// Deliberately never synced back afterwards: replaying a local session could
// overwrite what a device with a connection did in the meantime, and losing a
// local queue is a far smaller harm than corrupting shared state.

/** True while commands are being served locally because the server is unreachable. */
export const servingLocally = writable(false);

let localSt = null;

/**
 * Serve commands locally from now on. Called by the boot path when the app
 * started with no server, so the first tap does not wait for the 5s link timer.
 */
export function serveLocally() {
  servingLocally.set(true);
}

function blankLocal() {
  return {
    queue: [],
    order: [],
    cursor: -1,
    playing: false,
    shuffle: false,
    repeat: 'off',
    positionMs: 0,
    positionSeq: 0,
    activeId: deviceId,
    version: 0,
  };
}

/** Shuffle `order` but keep `keep` first, so toggling shuffle never skips a track. */
function shuffledOrder(order, keep) {
  const rest = order.filter((i) => i !== keep);
  for (let i = rest.length - 1; i > 0; i -= 1) {
    const j = Math.floor(Math.random() * (i + 1));
    [rest[i], rest[j]] = [rest[j], rest[i]];
  }
  return keep === undefined || keep < 0 ? rest : [keep, ...rest];
}

function localCommand(action, extra) {
  const st = { ...(localSt || blankLocal()) };
  st.version += 1;
  st.activeId = deviceId;
  // positionSeq is the "playback was moved" signal syncAudio keys on. Bump it
  // only for real moves, never for shuffle/repeat/enqueue - the same distinction
  // the server makes, and getting it wrong yanks playback to a stale position.
  const move = () => {
    st.positionSeq += 1;
  };

  switch (action) {
    case 'play': {
      st.queue = [...(extra.tracks || [])];
      const idx = Math.min(Math.max(extra.index || 0, 0), Math.max(st.queue.length - 1, 0));
      const natural = st.queue.map((_, i) => i);
      // Mirror the server: an explicit `shuffle` on the command wins over the flag.
      const shuffle = extra.shuffle === undefined ? st.shuffle : extra.shuffle;
      st.order = shuffle ? shuffledOrder(natural, idx) : natural;
      st.cursor = st.order.indexOf(idx);
      st.playing = st.queue.length > 0;
      st.positionMs = 0;
      move();
      break;
    }
    case 'enqueue': {
      const base = st.queue.length;
      const add = extra.tracks || [];
      st.queue = [...st.queue, ...add];
      st.order = [...st.order, ...add.map((_, i) => base + i)];
      break;
    }
    case 'toggle':
      st.playing = !st.playing;
      break;
    case 'pause':
      st.playing = false;
      break;
    case 'stop':
      st.playing = false;
      st.cursor = -1;
      st.positionMs = 0;
      move();
      break;
    case 'next': {
      if (st.repeat === 'one' && !extra.manual) {
        st.positionMs = 0;
        move();
        break;
      }
      if (st.cursor >= st.order.length - 1) {
        if (st.repeat === 'all' && st.order.length) st.cursor = 0;
        else {
          st.playing = false;
          break;
        }
      } else st.cursor += 1;
      st.positionMs = 0;
      move();
      break;
    }
    case 'prev':
      st.cursor = Math.max(st.cursor - 1, 0);
      st.positionMs = 0;
      move();
      break;
    case 'jump':
      st.cursor = Math.min(Math.max(extra.cursor || 0, 0), Math.max(st.order.length - 1, 0));
      st.positionMs = 0;
      st.playing = true;
      move();
      break;
    case 'removeAt': {
      const at = extra.cursor;
      if (at < 0 || at >= st.order.length) break;
      st.order = st.order.filter((_, i) => i !== at);
      if (at < st.cursor) st.cursor -= 1;
      else if (at === st.cursor) {
        st.cursor = Math.min(st.cursor, st.order.length - 1);
        st.positionMs = 0;
        move();
      }
      if (!st.order.length) {
        st.cursor = -1;
        st.playing = false;
      }
      break;
    }
    case 'seek':
      st.positionMs = Math.max(0, Math.round((extra.seconds || 0) * 1000));
      move();
      break;
    case 'shuffle': {
      st.shuffle = !st.shuffle;
      const current = st.order[st.cursor];
      const natural = st.queue.map((_, i) => i);
      st.order = st.shuffle ? shuffledOrder(natural, current) : natural;
      st.cursor = Math.max(st.order.indexOf(current), 0);
      break;
    }
    case 'repeat':
      st.repeat = extra.repeat || 'off';
      break;
    default:
      break;
  }

  localSt = st;
  apply(st);
}

async function send(action, extra = {}) {
  // Already known to be offline with music on the disk: apply immediately. Going
  // through the retry ladder here would make every control feel broken, and it
  // cannot succeed - the same transport just failed and the event stream is down.
  if (get(servingLocally)) {
    localCommand(action, extra);
    return;
  }

  const seqBefore = lastSeenPositionSeq;
  try {
    const first = await api.playbackCommand({ action, deviceId, version, ...extra });
    apply(first.state);
    // The server answered, so any local session is over. Its state is discarded
    // rather than merged - see the note on localCommand.
    servingLocally.set(false);
    localSt = null;
    if (!first.stale) return;

    if (RELATIVE.has(action) && (first.state?.positionSeq ?? 0) !== seqBefore) {
      return; // somebody really did advance it; our tap is satisfied
    }
    reportClientError('stale-rebased', action, `seq=${seqBefore} v=${version}`);
    const again = await api.playbackCommand({ action, deviceId, version, ...extra });
    apply(again.state);
  } catch (err) {
    // A transport failure is NOT a user-facing event if it heals on its own.
    //
    // The previous version showed "Lost the server - the queue did not advance"
    // on the first failure. Reported 2026-08-25: pressing play again just worked,
    // which makes the message both alarming and false - nothing was lost, one
    // request was. Shouting about a recoverable blip is worse than the blip.
    //
    // Retrying is safe even for next/prev, which must never apply twice: the
    // retry carries the SAME version, so if the first attempt actually landed the
    // server answers 409 and the positionSeq check above sees that playback
    // already moved and stops. The version field is exactly the guard that makes
    // an ambiguous transport failure unambiguous.
    for (const wait of [300, 900, 2400]) {
      await new Promise((r) => setTimeout(r, wait));
      try {
        const again = await api.playbackCommand({ action, deviceId, version, ...extra });
        apply(again.state);
        if (again.stale && RELATIVE.has(action) && (again.state?.positionSeq ?? 0) === seqBefore) {
          // Genuinely stale rather than already-applied: rebase once.
          const rebased = await api.playbackCommand({ action, deviceId, version, ...extra });
          apply(rebased.state);
        }
        return; // recovered without the user ever knowing
      } catch {
        // keep trying
      }
    }
    // Three failures across ~3.6s is a real outage, not a blip.
    reportClientError('command-failed', action, err?.message);
    // If there is music saved on this device, serve the command locally rather
    // than showing an error over songs sitting on the disk. Doing this only after
    // the retry ladder means a blip never drops the app out of sync with the
    // server - only a real outage does.
    if (saved.count() > 0) {
      servingLocally.set(true);
      localCommand(action, extra);
      return;
    }
    player.update((s) => ({ ...s, error: 'Cannot reach the server' }));
  }
}

const ids = (tracks) => (tracks || []).filter((t) => t?.id).map((t) => t.id);

// --- public controls ----------------------------------------------------------

/**
 * Replace the queue with `tracks` and start at `index`. `opts.shuffle: false` plays
 * the list in its own order without changing the shuffle flag (album Play).
 */
export function play(tracks, index = 0, opts = {}) {
  const list = (tracks || []).filter((t) => t && t.streamUrl);
  if (!list.length) return;
  remember(list);
  const first = Math.min(Math.max(index, 0), list.length - 1);
  // Optimistic: show the tapped track immediately rather than after a round trip.
  player.update((p) => ({ ...p, queue: list, track: list[first], playing: true }));
  const shuffle = opts.shuffle;
  send('play', { tracks: ids(list), index: first, ...(shuffle === undefined ? {} : { shuffle }) });
}

/** Append without disturbing what is currently playing. */
export function enqueue(tracks) {
  const add = (tracks || []).filter((t) => t && t.streamUrl);
  if (!add.length) return;
  remember(add);
  send('enqueue', { tracks: ids(add) });
}

/**
 * Drop every copy of `trackId` from the queue once it has been deleted from
 * disk. A queue entry whose file is gone can only fail, and if it is the track
 * playing right now the audio element carries on from its buffer - which is
 * exactly what "I deleted this and it kept playing" looks like.
 */
export function drop(trackId) {
  if (!trackId) return;
  cache.delete(trackId);
  // Stop the sound here and now if this device is the one making it. The server
  // will advance the queue, but the element must go quiet without waiting for a
  // round trip - a deleted file keeps playing perfectly well out of the buffer.
  if (get(isActive) && get(player).track?.id === trackId) {
    el.pause();
    el.removeAttribute('src');
    el.load();
  }
  send('drop', { trackId });
}

/**
 * Stop playback when the book being played has been deleted from disk.
 *
 * The song version, `drop`, tells the server to advance to the next track. A
 * book has nowhere to advance to - it is played one chapter at a time and the
 * whole thing, every chapter, is what just got deleted - so this stops instead.
 * Guarded by `bookIdFor` so deleting a book you are NOT listening to never
 * silences the one you are.
 */
export function dropBook(bookId) {
  if (!bookId) return;
  const cur = get(player).track;
  if (!cur || bookIdFor(cur) !== bookId) return;
  // Silence this device now; a deleted file plays on from the buffer otherwise.
  if (get(isActive)) {
    el.pause();
    el.removeAttribute('src');
    el.load();
  }
  stop();
}

export function toggle() {
  const p = get(player);
  if (!p.track) return;
  player.update((s) => ({ ...s, error: null }));
  // `toggle` on the server makes the sending device active when it RESUMES,
  // which is what makes "play here" work: the tap that sent this is the user
  // gesture this tab needed.
  send('toggle');
}

export function pause() {
  // Deliberately not `toggle`: pausing from a phone must not steal audio from
  // the desktop, and the server's `pause` never reassigns the active device.
  send('pause');
}

/** `manual` distinguishes a user pressing next from a track ending. */
export function next(manual = false) {
  send('next', { manual });
}

/** Restarts the track first, like every other music player. */
export function prev() {
  // The 3-second rule is the server's (PrevRestartSeconds), applied to the
  // position it holds, so every device behaves the same.
  send('prev');
}

/** Jump to an absolute position in the play order, e.g. from the queue list. */
export function jump(cursor) {
  send('jump', { cursor });
}

/**
 * Remove one row from the queue, by its position in the play order.
 *
 * Position and not track id, because the queue can hold the same track twice and
 * `drop` removes the first match - which is not the row that was swiped. `drop`
 * is for a file that no longer exists; this is for "not this one, not now".
 */
export function removeAt(cursor) {
  send('removeAt', { cursor });
}

export function seek(seconds) {
  if (!Number.isFinite(seconds)) return;
  const d = el.duration;
  const max = Number.isFinite(d) && d > 0 ? d : Infinity;
  const to = Math.min(Math.max(seconds, 0), max === Infinity ? seconds : max);
  // Move the element first when it is ours: a scrubber that waits for a round
  // trip feels broken.
  if (get(isActive) && el.getAttribute('src')) el.currentTime = to;
  progress.update((p) => ({ ...p, time: to }));
  send('seek', { seconds: to });
}

export function toggleShuffle() {
  send('shuffle');
}

export function cycleRepeat() {
  const wheel = { off: 'all', all: 'one', one: 'off' };
  send('repeat', { repeat: wheel[get(player).repeat] || 'all' });
}

/** Shuffle-play an arbitrary list from a random starting point. */
export async function shufflePlay(tracks) {
  const list = (tracks || []).filter((t) => t && t.streamUrl);
  if (!list.length) return;
  // `play` builds its order from the shuffle flag the server already holds, so
  // shuffle has to be on BEFORE the play lands - awaited, not fired alongside.
  if (!get(player).shuffle) await send('shuffle');
  remember(list);
  await send('play', {
    tracks: ids(list),
    index: Math.floor(Math.random() * list.length),
  });
}

export function stop() {
  send('stop');
  progress.set({ time: 0, duration: 0, buffered: 0 });
  // 'none' tears the Android notification down and stops the foreground
  // service, so a stopped player does not sit in the shade forever.
  session.setPlaybackState('none');
}

/** Discovery previews use their own element and must silence this one first. */
export function yieldAudio() {
  // Only meaningful on the device making sound. On a remote there is nothing to
  // yield, and sending `pause` would silence someone else's speakers because
  // this person opened Discover.
  if (get(isActive) && !el.paused) send('pause');
}

/** Move sound to another device. */
export async function transfer(id) {
  try {
    apply(await api.playbackTransfer(id));
  } catch {
    // 404 means the device was never seen; refreshDevices will drop it.
  }
  refreshDevices();
}

export async function refreshDevices() {
  try {
    devices.set(await api.playbackDevices());
  } catch {
    // Leave the last known list rather than blanking the picker.
  }
}

// --- element wiring -----------------------------------------------------------

// A watchdog for the SILENT stall: the shared state says this device is playing
// but the element is making no sound and firing nothing to react to. Android
// drops audio focus on a blip - a notification, or a Bluetooth headset that has
// attached to ANOTHER device - and pauses playback with no event; start() also
// swallows the AbortError a superseded play() throws. Either way the track just
// sits at 0:00 with no spinner and no error (reported: the queue stops after ~6
// songs at 0:00 of the next one, and music dies when the headset is on another
// device until you disconnect it there). Every recovery path above is wired to
// an event - 'error', 'waiting' - that never fires in this case.
//
// bufferingAt records a real 'waiting': while the browser is genuinely loading it
// shows a spinner and recovers itself, so the watchdog leaves that alone and only
// nudges the silent, non-buffering case.
let bufferingAt = 0;
let watchdogTime = -1;
let stalledTicks = 0;
let stallNudges = 0;
let stallTrackId = '';
const MAX_STALL_NUDGES = 20;

el.addEventListener('playing', () => {
  netRetries = 0;
  player.update((p) => ({ ...p, playing: true, loading: false, error: null }));
  report(true);
});
el.addEventListener('waiting', () => {
  bufferingAt = Date.now();
  player.update((p) => ({ ...p, loading: true }));
});
el.addEventListener('pause', () => {
  // An involuntary OS pause - Android dropping audio focus on a notification or a
  // Bluetooth route change - fires 'pause' while the shared state still says play.
  // Reporting that as a stop makes the server authoritative-paused and defeats the
  // stall watchdog, which is why playback "stopped on its own". Only report a pause
  // we actually intended (the state already says paused, or this is a remote).
  if (get(isActive) && last?.playing) return;
  report(true);
});
el.addEventListener('seeked', () => report(true));

// A cut stream is not a broken track.
//
// The audio src is an HTTP request to this server. Every deploy replaces the
// pod, which severs it mid-track; wifi dropping and the proxy hiccupping look
// identical. All three arrive here as code 2 (NETWORK), and this handler used to
// give up - so a 20-second rollout ended the listening session, and the state
// still said `playing: true` with nobody making sound.
//
// Resume where we were instead. Bounded, because a file that is genuinely gone
// must not be retried all evening.
let netRetries = 0;
let netTimer = null;
const MAX_NET_RETRIES = 6;

el.addEventListener('error', () => {
  if (!el.getAttribute('src')) return;
  // MediaError.code is the one number worth having: 1 ABORTED, 2 NETWORK,
  // 3 DECODE, 4 SRC_NOT_SUPPORTED. "This track would not play" reads the same
  // for a 404 and a codec the device cannot handle, and those need opposite
  // fixes.
  const code = el.error?.code;
  reportClientError('media-error', `code=${code ?? '?'} try=${netRetries}`, el.error?.message);

  if (code === 2 && netRetries < MAX_NET_RETRIES) {
    const at = el.currentTime || 0;
    // 1s, 2s, 4s ... capped. A pod restart is back inside 20s; a dead network
    // is not, and this must not become a busy loop either way.
    const wait = Math.min(1000 * 2 ** netRetries, 15000);
    netRetries += 1;
    clearTimeout(netTimer);
    player.update((p) => ({ ...p, loading: true, error: 'Reconnecting…' }));
    netTimer = setTimeout(() => {
      if (!el.getAttribute('src') || !get(isActive)) return;
      pendingSeek = at; // loadedmetadata puts us back where the music was
      el.load();
      start();
    }, wait);
    return;
  }

  player.update((p) => ({
    ...p,
    playing: false,
    loading: false,
    error: code === 4 ? 'This device cannot play that file' : 'This track would not play',
  }));
});

// A track ending is reported, never acted on locally: the server advances the
// queue and every device learns about it from the same broadcast. Doing it here
// as well would advance twice.
el.addEventListener('ended', () => {
  if (!get(isActive)) return;
  send('next', { manual: false });
});

el.addEventListener('durationchange', () => {
  progress.update((p) => ({
    ...p,
    duration: Number.isFinite(el.duration) ? el.duration : p.duration,
  }));
  positionState();
});

// See the watchdog note above. Runs on every device but only acts on the active
// one, and stays out of the network-retry ladder's way (netRetries > 0). If the
// state says play and the element is not progressing - and nothing is actively
// buffering - re-seat and re-issue play. Bounded per track, and it resumes on its
// own the moment the audio route comes back, with no tap.
setInterval(() => {
  const shouldPlay = get(isActive) && !!last?.playing && !!el.getAttribute('src');
  if (!shouldPlay || netRetries > 0) {
    watchdogTime = -1;
    stalledTicks = 0;
    return;
  }
  if (appliedTrackId !== stallTrackId) {
    stallTrackId = appliedTrackId;
    stallNudges = 0;
  }
  const t = el.currentTime;
  const progressing = !el.paused && t !== watchdogTime;
  watchdogTime = t;
  if (progressing || Date.now() - bufferingAt < 6000) {
    stalledTicks = 0;
    return;
  }
  // ~4s of a silent, non-buffering stall before the first nudge, so a normal
  // sub-second load between tracks is never touched.
  stalledTicks += 1;
  if (stalledTicks >= 2 && stallNudges < MAX_STALL_NUDGES) {
    stallNudges += 1;
    if (el.readyState < 2) {
      try {
        el.load();
      } catch {
        // load() throws only on a detached element; the next frame re-seats it.
      }
    }
    start();
  }
}, 2000);

// --- audiobooks: playback rate and the sleep timer ----------------------------
//
// BOTH LIVE HERE, WITH THE AUDIO ELEMENT, and neither travels in the shared
// playback state. Rate is a property of the speaker, not of the queue: the
// desktop has no business knowing the phone is at 1.5x, and putting it in the
// shared state would make one device's preference silently change another's.
//
// Speed is per BOOK, so it has to be re-applied whenever the current track
// changes - including from music back to a book, and crucially from a book back
// to MUSIC. Leaving 2x set after an audiobook would play the next song at double
// speed, which is the regression this feature could most easily cause.
const bookPrefs = new Map(); // bookId -> { speed, sleepMinutes }

/** Milliseconds of listening left before the sleep timer pauses; 0 = disarmed. */
export const sleepRemaining = writable(0);
let sleepLeftMs = 0;
let lastSleepTick = 0;

/**
 * Remember one book's playback settings and apply them if it is playing now.
 *
 * Called by the book screen on load and on every change, so a device that never
 * opens the screen simply plays at 1x rather than guessing.
 */
export function setBookPrefs(bookId, { speed, sleepMinutes } = {}) {
  if (!bookId) return;
  const prev = bookPrefs.get(bookId) || {};
  const next = {
    speed: Number.isFinite(speed) && speed > 0 ? speed : prev.speed || 1,
    sleepMinutes: Number.isFinite(sleepMinutes) ? sleepMinutes : prev.sleepMinutes || 0,
  };
  bookPrefs.set(bookId, next);

  if (currentBookId() === bookId) {
    applyRate();
    armSleep(next.sleepMinutes);
  }
}

function currentBookId() {
  const track = get(player).track;
  return track ? bookIdFor(track) : null;
}

/**
 * Force the element's rate to match whatever is playing.
 *
 * Called from timeupdate rather than once on track change, because setting
 * `src` resets playbackRate in some browsers - so a rate applied at load time
 * silently reverts on the next chapter. Comparing before assigning keeps this
 * free on the ~4/second it runs.
 */
function applyRate() {
  const want = rateFor(get(player).track, bookPrefs);
  if (el.playbackRate !== want) el.playbackRate = want;
}

function armSleep(minutes) {
  sleepLeftMs = minutes > 0 ? minutes * 60_000 : 0;
  lastSleepTick = Date.now();
  sleepRemaining.set(sleepLeftMs);
}

/**
 * Count the sleep timer down using the AUDIO as the clock, not setTimeout.
 *
 * A setTimeout in a backgrounded tab is throttled and on a phone with the screen
 * off may not fire at all - which is precisely the situation a sleep timer
 * exists for. timeupdate keeps firing while audio is actually playing, so the
 * thing making the sound is what drives the countdown.
 *
 * Counting elapsed WALL time rather than elapsed track position, because "stop
 * in 30 minutes" means thirty real minutes; at 1.5x the track position advances
 * half as fast again and would cut the timer short.
 *
 * The per-tick delta is capped so a frozen tab that resumes hours later burns
 * one tick's worth rather than the whole timer.
 */
function tickSleep() {
  if (sleepLeftMs <= 0) return;
  const now = Date.now();
  sleepLeftMs = stepSleep(sleepLeftMs, now - lastSleepTick);
  lastSleepTick = now;
  if (sleepLeftMs > 0) {
    sleepRemaining.set(sleepLeftMs);
    return;
  }
  sleepLeftMs = 0;
  sleepRemaining.set(0);
  // pause(), not a local el.pause(): the state is shared, and stopping only this
  // device would leave the server believing the book is still playing - which
  // would keep advancing the saved position through a book nobody is hearing.
  pause();
}

el.addEventListener('timeupdate', () => {
  if (!get(isActive)) return;
  localPos = el.currentTime;
  const duration = Number.isFinite(el.duration) && el.duration > 0 ? el.duration : 0;
  const buffered = el.buffered.length ? el.buffered.end(el.buffered.length - 1) : 0;
  progress.update((p) => ({ time: el.currentTime, duration: duration || p.duration, buffered }));
  applyRate();
  tickSleep();
  // No scrobble here any more. The server owns it, because with one shared
  // state every device that believed it was playing used to submit its own
  // play - polluting Navidrome, ListenBrainz and the lb-feedback-sync loop.
  report();
});

// --- the event stream ---------------------------------------------------------
//
// SSE, not WebSocket: the traffic is server -> clients with commands going back
// over ordinary POSTs, which is exactly SSE's shape and needs no dependency.

let es = null;
let retry = 1000;
// When the stream went down, so a recovery can say how long it was out. "It keeps
// disconnecting" is unfalsifiable without a number.
let downSince = 0;
// Timestamps of recent disconnects.
//
// The first version of this only reported gaps of 5s or more, on the reasoning
// that a blink is not worth a log line. That reasoning missed the actual bug: the
// stream was dying every 30 seconds and recovering in about one, so the real
// failure - forty outages an hour - produced ZERO reports and I read that silence
// as health. It was found in a latency histogram instead, by accident.
//
// Frequency is the signal. A one-second gap that happens constantly is worse than
// a ten-second gap that happens once, because every gap is a window where this
// tab's version is stale and a tap gets thrown away.
const drops = [];
const FLAP_WINDOW = 5 * 60 * 1000;
let lastFlapReport = 0;

// When the last frame of ANY kind arrived.
//
// A stream can stop delivering without the socket reporting an error - which is
// exactly how this bug survived: Firefox dropped it, the tab looked healthy, and
// the only symptom was that commands started failing because the version was
// stale. Belt and braces on the server-side fix: if nothing arrives for well
// over three heartbeats, stop believing the connection and rebuild it.
let lastFrame = 0;
const STALE_AFTER = 40000; // server pings every 10s

setInterval(() => {
  if (!lastFrame || document.visibilityState !== 'visible') return;
  const quiet = Date.now() - lastFrame;
  if (quiet < STALE_AFTER) return;
  reportClientError('stream-silent', `quiet=${Math.round(quiet / 1000)}s`, 'forcing resync');
  // force: this path exists precisely for a stream that looks connected and is
  // not, so it must not be talked out of it by the liveness check in resync.
  // `lastFrame` is deliberately NOT touched here - the reconnect's own first
  // frame updates it, and faking it would hide a reconnect that never lands.
  resync('silent', true);
}, 10000);

function connect() {
  if (es) es.close();
  const q = new URLSearchParams({ device: deviceId, name: deviceName });
  es = new EventSource(`/api/playback/stream?${q}`, { withCredentials: true });

  es.addEventListener('open', () => {
    setLink(true);
    retry = 1000;
    if (downSince) {
      const secs = Math.round((Date.now() - downSince) / 1000);
      downSince = 0;
      const now = Date.now();
      while (drops.length && drops[0] < now - FLAP_WINDOW) drops.shift();
      if (secs >= 5) {
        reportClientError('stream-recovered', `down=${secs}s drops5m=${drops.length}`, '');
      } else if (drops.length >= 3 && now - lastFlapReport > FLAP_WINDOW) {
        // Throttled to one line per window: while flapping this would otherwise
        // report as often as it fails, and drown the thing it is measuring.
        lastFlapReport = now;
        reportClientError('stream-flapping', `drops=${drops.length} in 5m`, `last gap ${secs}s`);
      }
    }
    lastFrame = Date.now();
    refreshDevices();
    // The connection coming back is the first moment an outage's reports can
    // actually be delivered, which is the whole point of buffering them.
    flushReports();
  });

  es.addEventListener('state', (ev) => {
    lastFrame = Date.now();
    try {
      apply(JSON.parse(ev.data));
    } catch {
      // A frame we cannot parse is not worth tearing the stream down for.
    }
  });

  // The server sends `event: ping` every 10s. Nothing needs the value; what
  // matters is that a message event arrived, both for the browser's own liveness
  // timer and for the watchdog below.
  es.addEventListener('ping', () => {
    lastFrame = Date.now();
  });

  es.addEventListener('error', () => {
    if (!downSince) {
      downSince = Date.now();
      drops.push(downSince);
    }
    setLink(false);
    // EventSource retries on its own, but not after the server closes cleanly,
    // and not with a bound. Backing off to 30s keeps a sleeping laptop from
    // hammering the server all night.
    es.close();
    es = null;
    setTimeout(connect, retry);
    retry = Math.min(retry * 2, 30000);
  });
}

connect();

// A phone that was asleep comes back with state that may be minutes old.
//
// Three versions of this, and the middle one caused a worse bug than it fixed:
//
//  1. guarded by `!get(connected)` - which skipped reconnecting in exactly the
//     case its own comment described, because a socket killed while the process
//     was frozen never fires `error` and leaves `connected` true.
//  2. unconditional (v43) - which fixed that and introduced a 30-second
//     reconnect cycle. Streams measured 11m59s and 5m02s before v43 and a rigid
//     30.0-32.7s after it, because every visibility or online event now tore
//     down a PERFECTLY HEALTHY stream. Each teardown leaves a gap where this
//     tab's version is stale, so it manufactured the exact symptom it was
//     shipped to investigate.
//  3. this one: ask whether the stream is actually alive.
//
// `lastFrame` is the honest answer, and it exists now: the server sends a ping
// every 10s, so silence for 25s means dead regardless of what `connected` claims.
// A healthy stream is left alone; a stale one is rebuilt. `why` is reported so
// the next person does not have to infer the trigger from a duration histogram.
const RESYNC_IF_QUIET = 25000;

function resync(why, force = false) {
  const quiet = lastFrame ? Date.now() - lastFrame : Infinity;
  if (!force && es && get(connected) && quiet < RESYNC_IF_QUIET) return;
  if (lastFrame) {
    reportClientError('resync', why, `quiet=${Math.round(quiet / 1000)}s`);
  }
  retry = 1000;
  if (es) {
    es.close();
    es = null;
  }
  connect();
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') resync('visible');
});

// A wifi-to-cellular handover is precisely "the connection is unstable", and the
// old socket is already dead by the time this fires.
window.addEventListener('online', () => resync('online'));

// --- Media Session ------------------------------------------------------------
//
// This is what makes it feel like a music app rather than a web page: lockscreen
// artwork, headset buttons, and the car stereo's next-track control.
//
// Which of those actually exist depends on the platform, and lib/mediasession.js
// owns that difference.

function positionState() {
  session.setPositionState({
    duration: el.duration,
    position: el.currentTime,
    playbackRate: el.playbackRate || 1,
  });
}

if (session.active) {
  session.setActionHandlers({
    play: () => toggle(),
    pause: () => pause(),
    stop: () => stop(),
    previoustrack: () => prev(),
    nexttrack: () => next(true),
    seekbackward: (d) => seek(el.currentTime - (d?.seekOffset || 10)),
    seekforward: (d) => seek(el.currentTime + (d?.seekOffset || 10)),
    seekto: (d) => seek(d?.seekTime ?? 0),
  });

  el.addEventListener('play', positionState);
  el.addEventListener('seeked', positionState);

  // setPositionState on every timeupdate is wasteful; once a second keeps the
  // lockscreen scrubber honest without burning battery.
  let lastPush = 0;
  el.addEventListener('timeupdate', () => {
    const now = Date.now();
    if (now - lastPush < 1000) return;
    lastPush = now;
    positionState();
  });
}
