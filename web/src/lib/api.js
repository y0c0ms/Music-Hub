/**
 * Every network call the app makes. The field names below are the server's
 * JSON field names (crates/core/src/model.rs), not guesses.
 *
 * Two invariants hold everywhere:
 *
 *   - Same-origin only. The server binary serves this bundle and the API, so no
 *     hostname or port appears in this file. Nothing else would work anyway:
 *     the session is an HttpOnly cookie and audio must be proxied to avoid
 *     mixed content.
 *   - Auth failures are routed to one place. Any call can discover that the
 *     session died or that Navidrome was unlinked from another device, so the
 *     `gate` store is the single thing App.svelte watches to decide whether to
 *     show the app, the sign-in screen, or the link form.
 */
import { writable } from 'svelte/store';

/** 'loading' | 'signedOut' | 'unlinked' | 'ready' */
export const gate = writable('loading');

export class ApiError extends Error {
  constructor(status, code, message) {
    super(message || code || `HTTP ${status}`);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

async function body(res) {
  try {
    return await res.json();
  } catch {
    return null;
  }
}

/**
 * How long one request may take before it is abandoned.
 *
 * A request that is neither answered nor refused is the case a phone hits most:
 * wifi associated, server unreachable. `fetch` then waits for the OS TCP
 * timeout, which is minutes. Nothing in the UI can tell that apart from a slow
 * server, so the boot spinner spun forever and the offline screen — the one
 * thing that works without a server — could not be reached at all. Reported
 * 2026-11-09 ("it tries to connect to the server forever and just hangs").
 *
 * The server answers in well under a second on the LAN, so this is generous.
 */
const REQUEST_TIMEOUT_MS = 8000;

/**
 * Boot asks whether there is a server at all, and the answer decides between
 * the app and the offline screen. It gets a shorter fuse: waiting 8s behind a
 * spinner before offering the music already on the device is itself the bug.
 */
export const BOOT_TIMEOUT_MS = 5000;

/**
 * A transport command is a tap. `lib/player.js` retries three times with its own
 * backoff before falling back to the device's own music, so the whole ladder
 * multiplies this figure: at 8s a dead link meant ~36s before a tap did
 * anything. 4s keeps the ladder under 20s and still leaves room for a slow
 * server, which answers in milliseconds here.
 */
const COMMAND_TIMEOUT_MS = 4000;

async function request(path, { method = 'GET', json, signal, timeoutMs = REQUEST_TIMEOUT_MS } = {}) {
  // One controller for both deadlines: the caller's signal (a search being
  // typed over) and the timeout. The caller's abort must stay an AbortError so
  // it can be ignored; a timeout is a dead link and must not look like one.
  const ctrl = new AbortController();
  let timedOut = false;
  const onAbort = () => ctrl.abort();
  if (signal) {
    if (signal.aborted) ctrl.abort();
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  const timer = setTimeout(() => {
    timedOut = true;
    ctrl.abort();
  }, timeoutMs);

  const init = { method, credentials: 'include', signal: ctrl.signal };
  if (json !== undefined) {
    init.headers = { 'Content-Type': 'application/json' };
    init.body = JSON.stringify(json);
  }

  let res;
  try {
    res = await fetch(path, init);
  } catch (err) {
    if (err?.name === 'AbortError' && !timedOut) throw err;
    throw new ApiError(0, 'offline', 'Cannot reach the server');
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', onAbort);
  }

  if (res.ok) return res.status === 204 ? null : body(res);

  const payload = (await body(res)) || {};
  const code = payload.error || '';

  // The server reuses 401 for two unrelated things: no session, and Navidrome
  // rejecting the credentials being linked. Only the first means "sign in
  // again", so discriminate on the code — every 401 carries one.
  if (res.status === 401 && code === 'unauthenticated') {
    gate.set('signedOut');
  } else if (res.status === 428 || (res.status === 401 && code === 'navidrome_auth')) {
    gate.set('unlinked');
  }

  throw new ApiError(res.status, code, payload.message);
}

// --- session -----------------------------------------------------------------

export const LOGIN_URL = '/auth/login';

/** @returns {Promise<{username,displayName,navidromeUser,navidromeLinked}>} */
/** @param {{timeoutMs?: number}} [opts] boot passes [`BOOT_TIMEOUT_MS`]. */
export const me = (opts) => request('/api/me', opts);

export const link = (username, password) =>
  request('/api/link', { method: 'POST', json: { username, password } });

export const unlink = () => request('/api/link', { method: 'DELETE' });

export async function logout() {
  try {
    await fetch('/auth/logout', { method: 'POST', credentials: 'include' });
  } finally {
    // A hard navigation, not a store update: it throws away every cached
    // response and in-memory queue belonging to the account that just left.
    window.location.href = '/';
  }
}

// --- settings ----------------------------------------------------------------

export const settings = () => request('/api/settings');

export const setListenBrainz = (token) =>
  request('/api/settings/listenbrainz', { method: 'PUT', json: { token } });

export const deleteListenBrainz = () =>
  request('/api/settings/listenbrainz', { method: 'DELETE' });

// --- library -----------------------------------------------------------------

/** @returns {Promise<{recent:[],frequent:[],random:[]}>} tracks, not albums */
export const home = () => request('/api/library/home');

/**
 * Songs by ordering - the unit both the home screen and the mixes work in.
 * sort: 'newest' | 'frequent' | 'recent' | 'starred' | 'random'
 */
export const songs = (sort = 'newest', size = 100, genre = '') =>
  request(
    `/api/library/songs?sort=${encodeURIComponent(sort)}&size=${size}` +
      (genre ? `&genre=${encodeURIComponent(genre)}` : ''),
  );

/**
 * Genres the library has been classified as, most common first.
 *
 * Derived from Last.fm and kept in music-hub's own store - the audio files are
 * never touched, and Navidrome's own genre field stays as empty as it was.
 */
export const genres = () => request('/api/library/genres');

/** What this account uses, what all the music uses, and what the disk has left. */
export const storage = () => request('/api/library/storage');

export const artists = () => request('/api/library/artists');
export const artist = (id) => request(`/api/library/artist/${encodeURIComponent(id)}`);
export const album = (id) => request(`/api/library/album/${encodeURIComponent(id)}`);
export const playlists = () => request('/api/library/playlists');
export const playlist = (id) => request(`/api/library/playlist/${encodeURIComponent(id)}`);
export const createPlaylist = (name, songIds = []) =>
  request('/api/library/playlists', { method: 'POST', json: { name, songIds } });
export const addToPlaylist = (id, songIds) =>
  request(`/api/library/playlist/${encodeURIComponent(id)}/tracks`, {
    method: 'POST',
    json: { songIds },
  });
export const renamePlaylist = (id, name) =>
  request(`/api/library/playlist/${encodeURIComponent(id)}`, { method: 'PATCH', json: { name } });
/** Removes the grouping only - Subsonic's deletePlaylist never touches the files. */
export const deletePlaylist = (id) =>
  request(`/api/library/playlist/${encodeURIComponent(id)}`, { method: 'DELETE' });
export const song = (id) => request(`/api/library/song/${encodeURIComponent(id)}`);
export const starred = () => request('/api/library/starred');

/**
 * Tracks like this one that are ALREADY ON DISK, for "play similar".
 *
 * Not the Discover deck: that one is for finding music to download and records a
 * verdict on everything it shows. This only reads, so queueing a track never
 * affects what Discover offers you later.
 */
export const similar = (id) => request(`/api/library/similar/${encodeURIComponent(id)}`);

/**
 * Remove a track from disk. The server decides whether it is yours to delete;
 * anything outside your own download folders comes back 403 by design.
 */
export const deleteSong = (id) =>
  request(`/api/library/song/${encodeURIComponent(id)}`, { method: 'DELETE' });

/** type: alphabeticalByName | newest | frequent | recent | random | starred. */
export const albums = ({ type = 'alphabeticalByName', size = 60, offset = 0 } = {}) =>
  request(
    `/api/library/albums?type=${encodeURIComponent(type)}&size=${size}&offset=${offset}`,
  );

export const star = (id) =>
  request(`/api/library/star/${encodeURIComponent(id)}`, { method: 'POST' });
export const unstar = (id) =>
  request(`/api/library/star/${encodeURIComponent(id)}`, { method: 'DELETE' });
export const scrobble = (id) =>
  request(`/api/library/scrobble/${encodeURIComponent(id)}`, { method: 'POST' });

// --- search and acquisition ---------------------------------------------------

/**
 * The local index. Milliseconds, and `acquirable` is always empty here — what
 * could be fetched from the internet lives behind acquireSearch.
 *
 * @returns {Promise<{tracks:[],albums:[],artists:[],acquirable:[]}>}
 */
export const search = (q, signal) => request(`/api/search?q=${encodeURIComponent(q)}`, { signal });

/**
 * The yt-dlp scrape: one to two seconds, so it is a separate call the UI can
 * wait on without holding up the library results.
 *
 * @returns {Promise<Array<{sourceId,title,artist,duration,thumb}>>}
 */
export const acquireSearch = (q, signal, n = 8) =>
  request(`/api/acquire/search?q=${encodeURIComponent(q)}&n=${n}`, { signal });

/**
 * Whole albums from YouTube Music, which a plain video search cannot find - it
 * only ever returns the fan-uploaded "Full Album" video. Slower than
 * acquireSearch (one search plus a resolve per album), so the UI fires it
 * alongside rather than waiting on it.
 *
 * @returns {Promise<Array<{playlistId,title,artist,trackCount,thumb,tracks}>>}
 */
export const acquireAlbums = (q, signal, n = 8) =>
  request(`/api/acquire/albums?q=${encodeURIComponent(q)}&n=${n}`, { signal });

/** Server validates sourceId against ^[\w-]{5,20}$ and answers 202 with the Job. */
export const acquire = (item) =>
  request('/api/acquire', {
    method: 'POST',
    json: {
      sourceId: item.sourceId,
      title: item.title,
      artist: item.artist || '',
      album: item.album || '',
      albumArtist: item.albumArtist || '',
    },
  });

export const jobs = () => request('/api/acquire/jobs');

/** For a library album, the online album that completes it and its missing tracks. */
export const albumMissing = (albumId) =>
  request(`/api/acquire/album-missing?albumId=${encodeURIComponent(albumId)}`);

// --- audiobooks --------------------------------------------------------------
//
// A book is one ordered thing you are partway through, so progress is a chapter
// id plus an offset INSIDE that chapter - never a whole-book offset, which would
// be wrong the moment a chapter is re-encoded or the file list changes.

/** @returns {Promise<Array<{id,title,author,coverUrl,chapterCount,durationSec,progress,listenedSec,speed,sleepMinutes}>>} */
export const books = () => request('/api/books');

/**
 * Search online audiobook sources (YouTube, LibriVox, media.ccc.de).
 * @returns {Promise<Array<{source,id,title,author,durationSec,chapters,thumb,detail}>>}
 */
export const booksSearch = (q, signal, n = 15) =>
  request(`/api/books/search?q=${encodeURIComponent(q)}&n=${n}`, { signal });

/** Queue a book download. Takes a whole search result; the server reads what it needs. */
export const booksGet = (result) => request('/api/books/get', { method: 'POST', json: result });

/** In-flight and recent book downloads: [{id,title,source,state,detail,percent,bookId,at}]. */
export const booksJobs = () => request('/api/books/jobs');

/** The contents of one search result: [{title,startSec,endSec}], fetched on demand. */
export const booksChapters = (result) =>
  request('/api/books/chapters', { method: 'POST', json: result });

/** The book plus its ordered chapters. */
export const book = (id) => request(`/api/books/${encodeURIComponent(id)}`);

/** Server-side so the position follows you between devices, like playback does. */
export const saveBookProgress = (id, progress) =>
  request(`/api/books/${encodeURIComponent(id)}/progress`, { method: 'PUT', json: progress });

/** Per-book playback rate and sleep timer - 1.5x suits one narrator, not another. */
export const saveBookSettings = (id, settings) =>
  request(`/api/books/${encodeURIComponent(id)}`, { method: 'PUT', json: settings });

/**
 * Delete a book from the library.
 *
 * Removes the FILES, for everyone - there is one shared music root and no
 * per-user copy of a book. The listening position is deliberately kept, so
 * re-downloading the same book restores where you were.
 */
export const deleteBook = (id) =>
  request(`/api/books/${encodeURIComponent(id)}`, { method: 'DELETE' });

// --- discovery ----------------------------------------------------------------

export const deck = (n = 20) => request(`/api/discovery/deck?n=${n}`);

/** Recent releases from artists you listen to (ListenBrainz), minus what you own. */
export const newReleases = () => request('/api/discovery/new-releases');

/**
 * Enqueue a New Releases row. `mbid` is the release's own YouTube Music
 * playlist for YouTube-sourced rows; the server downloads exactly that
 * playlist rather than searching by name.
 */
export const acquireRelease = (rel) =>
  request('/api/discovery/acquire-release', {
    method: 'POST',
    json: {
      artist: rel.artist,
      title: rel.title,
      kind: rel.kind || '',
      playlistId: String(rel.mbid || '').startsWith('OLAK5uy_') ? rel.mbid : '',
    },
  });

/** The source id of a public upload for a release, for a 30s preview. */
export const releaseSource = (rel) =>
  request(
    `/api/discovery/release-source?artist=${encodeURIComponent(rel.artist)}&title=${encodeURIComponent(rel.title)}`,
  );

/**
 * The tracklist of a release playlist, so a New Releases row can be opened and
 * picked apart before anything is downloaded. Resolved on the server, which
 * means it takes a few seconds the first time.
 */
export const releaseAlbum = (listId) =>
  request(`/api/acquire/album?list=${encodeURIComponent(listId)}`);

/** Record that the user is not interested in a release, so it stops appearing. */
export const dismissRelease = (rel) =>
  request('/api/discovery/dismiss-release', {
    method: 'POST',
    json: { artist: rel.artist, title: rel.title, mbid: rel.mbid || '' },
  });

/**
 * Published date and view count for one candidate. A full yt-dlp extraction on
 * the server, so it is fetched per card rather than with the deck, and it answers
 * 200 with an empty object when the source will not give up its metadata - the
 * card is still usable without it.
 */
export const sourceMeta = (id) => request(`/api/discovery/meta/${encodeURIComponent(id)}`);

/** Title and artist ride along because a candidate like is what triggers the download. */
const swipe = (verb, card) =>
  request(`/api/discovery/${verb}`, {
    method: 'POST',
    json: { kind: card.kind, id: card.id, title: card.title, artist: card.artist || '' },
  });

export const like = (card) => swipe('like', card);
export const dislike = (card) => swipe('dislike', card);

// --- shared playback ----------------------------------------------------------
//
// See docs/design/session-playback.md. One playback state per account lives on
// the server; every device renders it and any device can drive it, but only one
// makes sound.

/**
 * Send one command.
 *
 * Deliberately bypasses `request`, whose contract is "throw on non-2xx and keep
 * only the error code". Here **409 is not an error**: it means another device
 * changed the state first, and the body IS the current state. Losing that body
 * would leave the caller with nothing to rebase on, which is exactly the race
 * the version field exists to prevent.
 *
 * Returns { stale, state }.
 */
export async function playbackCommand(cmd) {
  // An explicit controller rather than `AbortSignal.timeout`: that helper needs
  // Chrome 103+, and on an older Android WebView it would throw on every tap
  // and push every command down the local-fallback path.
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), COMMAND_TIMEOUT_MS);
  let res;
  try {
    res = await fetch('/api/playback/command', {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(cmd),
      // The same deadline as every other request: a tap that hangs on a dead
      // link is the "it takes minutes to change a song" report of 2026-11-09.
      // `lib/player.js` keeps its own retry ladder; it needs a failure to climb.
      signal: ctrl.signal,
    });
  } finally {
    clearTimeout(timer);
  }
  if (res.status === 409) return { stale: true, state: await res.json() };
  if (!res.ok) throw new ApiError(res.status, '', 'playback command failed');
  return { stale: false, state: await res.json() };
}

/**
 * Report where the active device actually is.
 *
 * `durationMs` is what lets the SERVER decide when half the track has been
 * heard. Scrobbling moved server-side because with one shared state every device
 * that believed it was playing used to submit its own play.
 *
 * Failures are swallowed: this fires every few seconds and a missed report only
 * costs a slightly stale progress bar on the remotes.
 */
export const playbackProgress = (deviceId, positionMs, playing, durationMs) =>
  request('/api/playback/progress', {
    method: 'POST',
    json: { deviceId, positionMs: Math.round(positionMs), playing, durationMs: Math.round(durationMs || 0) },
  }).catch(() => null);

/** Move sound to another device. 404 means that device was never seen. */
export const playbackTransfer = (deviceId) =>
  request('/api/playback/transfer', { method: 'POST', json: { deviceId } });

/** [{id, name, lastSeen, active}] */
export const playbackDevices = () => request('/api/playback/devices');

/** A plain read, for the rare case a client needs state without the stream. */
export const playbackState = () => request('/api/playback/state');

/**
 * What the server is, so it can be compared with what this bundle is.
 *
 * Unauthenticated on purpose: asking a service to name itself should not need a
 * session, and it is the first thing worth checking when a bug report and the
 * code disagree.
 */
export const serverVersion = () => request('/api/version');
