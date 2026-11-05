/**
 * Remembering where you were in a book.
 *
 * THE QUESTION THIS ANSWERS. You are an hour into a book, you stop and listen to
 * music for a day, then you come back. You must land where you stopped - not at
 * the start of the chapter, and emphatically not wherever the music got to.
 *
 * ── the shape of a book ──────────────────────────────────────────────────────
 *
 * A chapter is `{title, trackId, startSec, endSec}` and progress is
 * `{trackId, positionSec}`. Which chapter you are in is DERIVED from the
 * position, never stored.
 *
 * That covers both kinds of book with one model:
 *
 *   YouTube  - one long file, chapter marks are offsets into it. Every chapter
 *              shares the same trackId, so progress collapses to exactly one
 *              timestamp.
 *   LibriVox - already split, one file per chapter. Each chapter has its own
 *              trackId with startSec 0.
 *
 * An earlier draft stored `chapterId + offset` on the reasoning that a global
 * offset breaks when a file is re-encoded or the file list changes. That is true
 * for the multi-file case and false for the single-file one: when a book is ONE
 * immutable file, a plain offset is strictly more robust, because there is no
 * chapter id left to go stale. Storing the trackId alongside the position keeps
 * the multi-file case honest without costing the single-file case anything.
 *
 * ── the two guarantees ───────────────────────────────────────────────────────
 *
 *   1. Something must WRITE the position while you listen. Nothing does by
 *      default: pausing does not save, and closing the app does not save. A
 *      position held only in memory is lost the moment the tab is dropped, which
 *      on a phone is whenever the OS feels like it.
 *
 *   2. Music must never write to the book. The player has one notion of "current
 *      position", shared by everything it plays. Ungated, a 3-minute song writes
 *      three minutes into the book's bookmark and silently throws away hours.
 *      The gate is `bookIdFor()` returning null, checked before every write.
 *
 * With a single-file book there are no track changes between chapters, so the
 * only things that ever write are the periodic tick, an explicit pause, and the
 * app being backgrounded. There is no fourth chance - which is why `pagehide` is
 * wired rather than `beforeunload`.
 */

/** How often to write while playing. */
export const SAVE_EVERY_MS = 10_000;

/** Below this, "resume" and "start from the beginning" are the same thing. */
export const MIN_POSITION_SEC = 3;

/** This close to the end, a chapter is finished for practical purposes. */
export const END_GUARD_SEC = 10;

/** Older than this and the thread of the story is gone. One week, per the user. */
export const STALE_MS = 7 * 24 * 60 * 60 * 1000;

/** Come back after a break and you have lost the last sentence. Give it back. */
export const REWIND_AFTER_MS = 60 * 60 * 1000;
export const REWIND_SEC = 15;

/**
 * track id -> book id, populated when a book is opened.
 *
 * Only a fallback. The authority is `bookId` on the track itself, because on a
 * cold launch - you, the next day - playback state comes back from the server
 * before any book screen has mounted, and a registry-only gate would refuse
 * every write in exactly the situation this feature exists for.
 */
const owner = new Map();

export function registerBook(book) {
  for (const ch of book?.chapters || []) {
    if (ch?.trackId) owner.set(ch.trackId, book.id);
  }
}

export function bookIdFor(track) {
  if (!track) return null;
  if (typeof track === 'string') return owner.get(track) || null;
  return track.bookId || owner.get(track.id) || null;
}

export function forgetAll() {
  owner.clear();
}

/**
 * Which chapter contains this position?
 *
 * Half-open `[startSec, endSec)` so a position exactly on a boundary belongs to
 * the chapter starting there, not the one ending there - otherwise every
 * chapter transition briefly reports the previous chapter.
 *
 * Returns -1 when nothing matches, which happens for real: chapter marks come
 * from the source and can change if a book is re-fetched, and a position past
 * the end of the file is possible after a re-encode. Callers must handle it
 * rather than indexing blindly.
 */
export function chapterIndexAt(chapters, trackId, positionSec) {
  if (!chapters?.length) return -1;
  for (let i = 0; i < chapters.length; i++) {
    const c = chapters[i];
    if (c.trackId !== trackId) continue;
    const start = c.startSec || 0;
    const end = c.endSec ?? Infinity;
    if (positionSec >= start && positionSec < end) return i;
  }
  return -1;
}

/**
 * Should this moment be written, and as what?
 *
 * Pure: everything it needs is an argument, so the rules can be tested without a
 * player, a clock or a network. Returns null for "do not write".
 *
 * `reason` is why we are asking - 'tick' during playback, or one of the
 * deliberate moments ('pause', 'trackChange', 'hidden') that must always write
 * regardless of the throttle, because there may not be another chance.
 */
export function decideSave({
  track,
  positionSec,
  playing = false,
  reason = 'tick',
  lastSavedAt = 0,
  lastSavedPositionSec = -1,
  nowMs,
}) {
  const bookId = bookIdFor(track);
  const trackId = typeof track === 'string' ? track : track?.id;

  // THE GATE. Not part of a book, so this position belongs to something else and
  // the bookmark must be left exactly as it is.
  if (!bookId) return null;

  if (!Number.isFinite(positionSec) || positionSec < MIN_POSITION_SEC) return null;

  const deliberate = reason !== 'tick';

  // A tick only counts while sound is actually coming out. A paused player
  // reports the same position forever, and writing it would keep `updatedAt`
  // fresh - defeating the staleness check and stopping a book abandoned for a
  // month from ever being offered a restart.
  if (!deliberate && !playing) return null;
  if (!deliberate && nowMs - lastSavedAt < SAVE_EVERY_MS) return null;

  // Nothing moved. Common when a stream stalls; the write would be pure noise.
  if (Math.abs(positionSec - lastSavedPositionSec) < 1 && !deliberate) return null;

  return { bookId, trackId, positionSec: Math.floor(positionSec) };
}

/**
 * Where should the big button send you, and what should it say?
 *
 * Modes:
 *   start   - never listened, or the bookmark no longer points at anything real
 *   resume  - carry on, possibly rewound a little
 *   restart - been too long; the chapter's beginning is recommended, but the
 *             exact spot is still offered rather than thrown away
 *   next    - that chapter was finished; the place is the start of the next one
 *   done    - finished the last chapter
 *
 * Every mode returns `{trackId, positionSec}` so the caller never has to know
 * which shape of book it is holding.
 */
export function resumeAdvice(book, nowMs) {
  const chapters = book?.chapters || [];
  const first = { mode: 'start', index: 0, trackId: chapters[0]?.trackId, positionSec: chapters[0]?.startSec || 0 };
  const p = book?.progress;
  if (!p || typeof p.positionSec !== 'number' || !chapters.length) return first;

  const index = chapterIndexAt(chapters, p.trackId, p.positionSec);

  // The position does not land in any chapter of this book: the marks changed, or
  // the file did. Saying "start" honestly beats guessing an index and dropping
  // someone confidently into the wrong hour.
  if (index === -1) return first;

  const ch = chapters[index];
  const positionSec = p.positionSec;
  const ageMs = Math.max(0, nowMs - (p.updatedAt || nowMs));
  const end = ch.endSec ?? Infinity;

  if (Number.isFinite(end) && positionSec >= end - END_GUARD_SEC) {
    const nextIndex = index + 1;
    if (nextIndex >= chapters.length) return { mode: 'done', index, ageMs, trackId: ch.trackId, positionSec };
    const nx = chapters[nextIndex];
    return {
      mode: 'next',
      index: nextIndex,
      trackId: nx.trackId,
      positionSec: nx.startSec || 0,
      ageMs,
    };
  }

  if (ageMs >= STALE_MS) {
    // Recommend the chapter's beginning, but keep the exact spot on screen.
    // Silently discarding a place someone earned over 40 minutes is not ours to do.
    return {
      mode: 'restart',
      index,
      trackId: ch.trackId,
      positionSec: ch.startSec || 0,
      exactSec: positionSec,
      ageMs,
    };
  }

  // Never rewind back past the start of the chapter you were in.
  const floor = ch.startSec || 0;
  const rewound = ageMs >= REWIND_AFTER_MS ? Math.min(REWIND_SEC, positionSec - floor) : 0;
  return {
    mode: 'resume',
    index,
    trackId: ch.trackId,
    positionSec: positionSec - rewound,
    rewoundSec: rewound,
    ageMs,
  };
}

/** "9 days ago". Only ever used to explain a recommendation, so it is coarse. */
export function ago(ms) {
  const m = Math.floor(ms / 60000);
  if (m < 1) return 'just now';
  if (m < 60) return `${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} ${h === 1 ? 'hour' : 'hours'} ago`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d} ${d === 1 ? 'day' : 'days'} ago`;
  const mo = Math.floor(d / 30);
  return `${mo} ${mo === 1 ? 'month' : 'months'} ago`;
}

/**
 * Wire the player to the bookmark.
 *
 * Kept out of the Svelte components on purpose: saving has to keep happening
 * while the book screen is closed - that is the entire scenario - so it cannot
 * live in something that unmounts when you navigate away.
 *
 * `pagehide` rather than `beforeunload`: mobile browsers kill a backgrounded tab
 * without firing `beforeunload`, and that is exactly when the last stretch of
 * listening would be lost.
 */
export function attach({ player, progress, save, now = () => Date.now() }) {
  let last = { at: 0, positionSec: -1, trackId: null };
  let state = { track: null, playing: false, positionSec: 0 };

  function write(reason) {
    const decision = decideSave({
      track: state.track,
      positionSec: state.positionSec,
      playing: state.playing,
      reason,
      lastSavedAt: last.at,
      lastSavedPositionSec: last.trackId === state.track?.id ? last.positionSec : -1,
      nowMs: now(),
    });
    if (!decision) return;
    last = { at: now(), positionSec: decision.positionSec, trackId: decision.trackId };
    Promise.resolve(save(decision)).catch(() => {
      // A failed write must not break playback. The next tick tries again, and
      // the throttle is keyed on the attempt, so this self-heals within 10s.
    });
  }

  const unPlayer = player.subscribe((st) => {
    const track = st?.track || null;
    if ((track?.id || null) !== (state.track?.id || null)) write('trackChange');
    const wasPlaying = state.playing;
    state = { ...state, track, playing: !!st?.playing };
    if (wasPlaying && !st?.playing) write('pause');
  });

  const unProgress = progress.subscribe((p) => {
    state = { ...state, positionSec: Math.floor(p?.time || 0) };
    write('tick');
  });

  const onHide = () => write('hidden');
  if (typeof window !== 'undefined') {
    window.addEventListener('pagehide', onHide);
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'hidden') onHide();
    });
  }

  return () => {
    write('hidden');
    unPlayer();
    unProgress();
    if (typeof window !== 'undefined') window.removeEventListener('pagehide', onHide);
  };
}

// --- playback rate and the sleep timer ---------------------------------------
//
// These two decisions live here rather than in player.js because player.js
// constructs an Audio element at module scope and cannot be loaded outside a
// browser. Pulling the logic out means the one regression that would matter -
// music inheriting a book's speed - is actually testable.

/**
 * What playback rate should the element be at, given what is playing?
 *
 * Returns 1 for anything that is not a book, and that is the whole point.
 * Listening to an audiobook at 2x and then playing a song must not play the song
 * at 2x; a rate that is only ever SET when a book starts, and never reset, does
 * exactly that.
 */
export function rateFor(track, prefs) {
  const bookId = bookIdFor(track);
  if (!bookId) return 1;
  const speed = prefs?.get?.(bookId)?.speed;
  return Number.isFinite(speed) && speed > 0 ? speed : 1;
}

/** Largest slice of time one tick may consume. */
export const MAX_SLEEP_STEP_MS = 5_000;

/**
 * Advance the sleep timer by the time since the last tick.
 *
 * Ticks come from the audio element, so they only arrive while sound is coming
 * out - which is what a sleep timer should measure. The cap stops a tab that was
 * frozen and resumed from swallowing the whole timer in one step.
 */
export function stepSleep(leftMs, deltaMs) {
  if (leftMs <= 0) return 0;
  const step = Math.min(Math.max(deltaMs, 0), MAX_SLEEP_STEP_MS);
  return Math.max(leftMs - step, 0);
}
