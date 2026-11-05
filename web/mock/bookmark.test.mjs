/**
 * Tests for the book bookmark rules.
 *
 * These exist because every failure here is silent. A bookmark that saves the
 * wrong number, or that lets a song overwrite a book, looks identical to a
 * working one right up until the day you come back to a 14-hour book and find
 * yourself at minute three.
 *
 * Both shapes of book are exercised throughout, because they fail differently:
 *
 *   ONE FILE   (YouTube)  - every chapter shares a trackId; chapter boundaries
 *                           are offsets. Progress is one timestamp. Off-by-one
 *                           at a boundary puts you in the wrong chapter.
 *   PER-CHAPTER (LibriVox) - every chapter is its own file starting at 0. The
 *                           trackId is what distinguishes chapter 3 from
 *                           chapter 40, and dropping it loses the book.
 *
 * Run: node mock/bookmark.test.mjs
 */
import assert from 'node:assert/strict';
import {
  registerBook,
  forgetAll,
  bookIdFor,
  chapterIndexAt,
  decideSave,
  resumeAdvice,
  ago,
  SAVE_EVERY_MS,
  STALE_MS,
  REWIND_AFTER_MS,
  REWIND_SEC,
  END_GUARD_SEC,
  rateFor,
  stepSleep,
  MAX_SLEEP_STEP_MS,
} from '../src/lib/bookmark.js';

let passed = 0;
function test(name, fn) {
  try {
    fn();
    passed++;
    console.log(`  ok  ${name}`);
  } catch (err) {
    console.error(`  FAIL ${name}\n       ${err.message}`);
    process.exitCode = 1;
  }
}

const NOW = 1_800_000_000_000;
const DAY = 24 * 3600 * 1000;

/** One long file: 9 chapters of 1000s, all the same trackId. */
const oneFile = (over = {}) => ({
  id: 'bk-1',
  chapters: Array.from({ length: 9 }, (_, i) => ({
    title: `Chapter ${i + 1}`,
    trackId: 'tr-whole',
    startSec: i * 1000,
    endSec: (i + 1) * 1000,
  })),
  ...over,
});

/** Per-chapter files: 9 chapters, each its own track starting at 0. */
const perChapter = (over = {}) => ({
  id: 'bk-2',
  chapters: Array.from({ length: 9 }, (_, i) => ({
    title: `Chapter ${i + 1}`,
    trackId: `tr-${i + 1}`,
    startSec: 0,
    endSec: 1000,
  })),
  ...over,
});

// --- which chapter am I in -----------------------------------------------------

test('one file: position maps to the chapter containing it', () => {
  const cs = oneFile().chapters;
  assert.equal(chapterIndexAt(cs, 'tr-whole', 0), 0);
  assert.equal(chapterIndexAt(cs, 'tr-whole', 999), 0);
  assert.equal(chapterIndexAt(cs, 'tr-whole', 6500), 6, 'chapter 7');
  assert.equal(chapterIndexAt(cs, 'tr-whole', 8999), 8, 'last chapter');
});

test('one file: a boundary belongs to the chapter that STARTS there', () => {
  // Half-open [start, end). Closed intervals would report the previous chapter
  // for one tick at every single transition.
  const cs = oneFile().chapters;
  assert.equal(chapterIndexAt(cs, 'tr-whole', 1000), 1, 'not chapter 1');
  assert.equal(chapterIndexAt(cs, 'tr-whole', 7000), 7);
});

test('one file: a position past the end of the book matches nothing', () => {
  assert.equal(chapterIndexAt(oneFile().chapters, 'tr-whole', 99_999), -1);
});

test('per-chapter: the trackId picks the chapter, not the offset', () => {
  const cs = perChapter().chapters;
  assert.equal(chapterIndexAt(cs, 'tr-1', 500), 0);
  assert.equal(chapterIndexAt(cs, 'tr-7', 500), 6, 'same offset, different file');
  assert.equal(chapterIndexAt(cs, 'tr-99', 500), -1, 'a file not in this book');
});

// --- the gate: music must never touch a book -----------------------------------

test('a song playing writes nothing, even long after the last save', () => {
  forgetAll();
  registerBook(oneFile());
  assert.equal(
    decideSave({ track: 'song-42', positionSec: 95, playing: true, reason: 'tick', lastSavedAt: 0, nowMs: NOW }),
    null,
    'a song was allowed to write a book bookmark',
  );
});

test('a song does not write on the deliberate moments either', () => {
  forgetAll();
  registerBook(oneFile());
  for (const reason of ['pause', 'trackChange', 'hidden']) {
    assert.equal(decideSave({ track: 'song-42', positionSec: 95, reason, nowMs: NOW }), null, `wrote on ${reason}`);
  }
});

test('registerBook keys off trackId, so both shapes are recognised', () => {
  forgetAll();
  registerBook(oneFile());
  registerBook(perChapter());
  assert.equal(bookIdFor('tr-whole'), 'bk-1');
  assert.equal(bookIdFor('tr-7'), 'bk-2');
  assert.equal(bookIdFor('song-42'), null);
});

test('a chapter is recognised on a cold launch, before any book screen opened', () => {
  forgetAll(); // fresh app start: nothing registered
  const out = decideSave({
    track: { id: 'tr-whole', bookId: 'bk-1' }, // server says it is a book
    positionSec: 6500,
    playing: true,
    reason: 'tick',
    lastSavedAt: 0,
    nowMs: NOW,
  });
  assert.deepEqual(out, { bookId: 'bk-1', trackId: 'tr-whole', positionSec: 6500 });
});

test('a track object with no bookId is still not a book', () => {
  forgetAll();
  assert.equal(decideSave({ track: { id: 'song-42' }, positionSec: 95, playing: true, nowMs: NOW }), null);
});

// --- when to write -------------------------------------------------------------

test('writes while playing once the throttle has elapsed', () => {
  forgetAll();
  registerBook(oneFile());
  assert.deepEqual(
    decideSave({
      track: 'tr-whole', positionSec: 6421, playing: true, reason: 'tick',
      lastSavedAt: NOW - SAVE_EVERY_MS - 1, lastSavedPositionSec: 6300, nowMs: NOW,
    }),
    { bookId: 'bk-1', trackId: 'tr-whole', positionSec: 6421 },
  );
});

test('throttles ticks inside the window', () => {
  forgetAll();
  registerBook(oneFile());
  assert.equal(
    decideSave({
      track: 'tr-whole', positionSec: 6421, playing: true, reason: 'tick',
      lastSavedAt: NOW - 1000, lastSavedPositionSec: 6300, nowMs: NOW,
    }),
    null,
  );
});

test('pause always writes, throttle or not - it is the whole point', () => {
  forgetAll();
  registerBook(oneFile());
  assert.deepEqual(
    decideSave({
      track: 'tr-whole', positionSec: 6421, playing: false, reason: 'pause',
      lastSavedAt: NOW - 10, lastSavedPositionSec: 6420, nowMs: NOW,
    }),
    { bookId: 'bk-1', trackId: 'tr-whole', positionSec: 6421 },
  );
});

test('a paused player does not keep refreshing updatedAt', () => {
  forgetAll();
  registerBook(oneFile());
  // If this wrote, a book left paused for a month would look freshly listened to
  // and would never be offered a restart.
  assert.equal(
    decideSave({ track: 'tr-whole', positionSec: 6421, playing: false, reason: 'tick', lastSavedAt: NOW - 600_000, nowMs: NOW }),
    null,
  );
});

test('the first seconds are not worth a bookmark', () => {
  forgetAll();
  registerBook(oneFile());
  assert.equal(decideSave({ track: 'tr-whole', positionSec: 1, playing: true, nowMs: NOW }), null);
});

test('ONE FILE: there is no track change between chapters, so tick/pause/hidden are the only writers', () => {
  forgetAll();
  registerBook(oneFile());
  // Crossing from chapter 6 into chapter 7 in a single-file book fires no
  // trackChange, because the track never changes. If the throttle also refused,
  // nothing would ever be written for a 14-hour book.
  const crossing = decideSave({
    track: 'tr-whole', positionSec: 7001, playing: true, reason: 'tick',
    lastSavedAt: NOW - SAVE_EVERY_MS - 1, lastSavedPositionSec: 6990, nowMs: NOW,
  });
  assert.ok(crossing, 'the tick must carry a single-file book on its own');
  assert.equal(crossing.positionSec, 7001);
});

// --- where to resume -----------------------------------------------------------

test('no progress means start, at the first chapter', () => {
  const a = resumeAdvice(oneFile(), NOW);
  assert.equal(a.mode, 'start');
  assert.equal(a.trackId, 'tr-whole');
  assert.equal(a.positionSec, 0);
});

test('mid-chapter and recent means resume at the exact spot', () => {
  const book = oneFile({ progress: { trackId: 'tr-whole', positionSec: 6512, updatedAt: NOW - 60_000 } });
  const a = resumeAdvice(book, NOW);
  assert.equal(a.mode, 'resume');
  assert.equal(a.index, 6, 'chapter 7');
  assert.equal(a.positionSec, 6512);
  assert.equal(a.rewoundSec, 0);
});

test('a day later, resume but hand back the last 15 seconds', () => {
  const book = oneFile({ progress: { trackId: 'tr-whole', positionSec: 6512, updatedAt: NOW - DAY } });
  const a = resumeAdvice(book, NOW);
  assert.equal(a.mode, 'resume', 'a day is not stale');
  assert.equal(a.rewoundSec, REWIND_SEC);
  assert.equal(a.positionSec, 6512 - REWIND_SEC);
});

test('the rewind never crosses back into the previous chapter', () => {
  // 5s into chapter 7, which starts at 6000. A flat 15s rewind would land at
  // 5990 - the end of chapter 6 - and the screen would say you are resuming a
  // chapter you already finished.
  const book = oneFile({ progress: { trackId: 'tr-whole', positionSec: 6005, updatedAt: NOW - REWIND_AFTER_MS - 1 } });
  const a = resumeAdvice(book, NOW);
  assert.equal(a.index, 6);
  assert.equal(a.positionSec, 6000, 'clamped to the start of chapter 7');
  assert.equal(a.rewoundSec, 5);
});

test('a week later it recommends the chapter start but keeps the exact spot', () => {
  const book = oneFile({ progress: { trackId: 'tr-whole', positionSec: 6512, updatedAt: NOW - STALE_MS - 1 } });
  const a = resumeAdvice(book, NOW);
  assert.equal(a.mode, 'restart');
  assert.equal(a.positionSec, 6000, 'the beginning of chapter 7, not of the book');
  assert.equal(a.exactSec, 6512, 'the earned position is still offered, not discarded');
  assert.equal(a.index, 6);
});

test('finishing a chapter points at the next one - a seek, same file', () => {
  const book = oneFile({ progress: { trackId: 'tr-whole', positionSec: 6995, updatedAt: NOW - 60_000 } });
  const a = resumeAdvice(book, NOW);
  assert.equal(a.mode, 'next');
  assert.equal(a.index, 7, 'chapter 8');
  assert.equal(a.trackId, 'tr-whole', 'same file');
  assert.equal(a.positionSec, 7000, 'the offset where chapter 8 begins');
});

test('PER-CHAPTER: finishing a chapter moves to the next FILE at 0', () => {
  const book = perChapter({ progress: { trackId: 'tr-7', positionSec: 995, updatedAt: NOW - 60_000 } });
  const a = resumeAdvice(book, NOW);
  assert.equal(a.mode, 'next');
  assert.equal(a.trackId, 'tr-8', 'a different file');
  assert.equal(a.positionSec, 0);
});

test('finishing the last chapter is done, not a crash', () => {
  const book = oneFile({ progress: { trackId: 'tr-whole', positionSec: 8995, updatedAt: NOW - 60_000 } });
  assert.equal(resumeAdvice(book, NOW).mode, 'done');
});

test('END_GUARD is what makes "nearly finished" count as finished', () => {
  const just = oneFile({ progress: { trackId: 'tr-whole', positionSec: 7000 - END_GUARD_SEC, updatedAt: NOW - 60_000 } });
  assert.equal(resumeAdvice(just, NOW).mode, 'next');
  const notYet = oneFile({ progress: { trackId: 'tr-whole', positionSec: 7000 - END_GUARD_SEC - 1, updatedAt: NOW - 60_000 } });
  assert.equal(resumeAdvice(notYet, NOW).mode, 'resume');
});

test('a position that lands in no chapter says start, not a wrong guess', () => {
  // Chapter marks changed under us, or the file did.
  const book = oneFile({ progress: { trackId: 'tr-whole', positionSec: 99_999, updatedAt: NOW - 60_000 } });
  assert.equal(resumeAdvice(book, NOW).mode, 'start');
});

test('a bookmark for a file that is no longer part of the book says start', () => {
  const book = perChapter({ progress: { trackId: 'tr-gone', positionSec: 500, updatedAt: NOW - 60_000 } });
  assert.equal(resumeAdvice(book, NOW).mode, 'start');
});

test('position 0 is a real position, not a missing one', () => {
  // `!p.positionSec` would treat the very start of a book as "never listened",
  // silently discarding a bookmark. Guarding on the type is what prevents it.
  const book = oneFile({ progress: { trackId: 'tr-whole', positionSec: 0, updatedAt: NOW - 60_000 } });
  const a = resumeAdvice(book, NOW);
  assert.equal(a.index, 0);
  assert.equal(a.mode, 'resume');
});

// --- the scenario the user actually described ----------------------------------

test('an hour into chapter 7, then a day of music, then back to the book', () => {
  forgetAll();
  const book = oneFile();
  registerBook(book);

  // Stopped 6512s in - the middle of chapter 7. Pause writes it.
  const atPause = decideSave({ track: 'tr-whole', positionSec: 6512, playing: false, reason: 'pause', nowMs: NOW });
  assert.deepEqual(atPause, { bookId: 'bk-1', trackId: 'tr-whole', positionSec: 6512 });

  // Then songs, for a day. Every one of these is refused.
  for (const t of [30, 120, 200]) {
    assert.equal(
      decideSave({ track: 'song-9', positionSec: t, playing: true, reason: 'tick', nowMs: NOW + t * 1000 }),
      null,
      'music overwrote the book bookmark',
    );
  }

  // Come back a day later.
  const stored = oneFile({ progress: { trackId: 'tr-whole', positionSec: 6512, updatedAt: NOW } });
  const a = resumeAdvice(stored, NOW + DAY);
  assert.equal(a.mode, 'resume');
  assert.equal(a.index, 6, 'landed in a different chapter');
  assert.equal(a.positionSec, 6512 - REWIND_SEC, 'not the start of chapter 7, and not the music');
});

test('ago() reads like a person wrote it', () => {
  assert.equal(ago(30_000), 'just now');
  assert.equal(ago(5 * 60_000), '5 min ago');
  assert.equal(ago(3 * 3600_000), '3 hours ago');
  assert.equal(ago(9 * DAY), '9 days ago');
});


// --- playback rate ------------------------------------------------------------
//
// THE REGRESSION THIS FEATURE COULD MOST EASILY CAUSE. Listen to a book at 2x,
// then play a song: the song must play at 1x. A rate that is only ever SET when
// a book starts, and never reset, plays every song afterwards at double speed -
// and nobody would connect that to the audiobook feature.

test('music plays at 1x, even straight after a book at 2x', () => {
  forgetAll();
  registerBook(oneFile());
  const prefs = new Map([['bk-1', { speed: 2 }]]);

  assert.equal(rateFor({ id: 'tr-whole', bookId: 'bk-1' }, prefs), 2, 'the book should be at 2x');
  assert.equal(rateFor({ id: 'song-42' }, prefs), 1, 'a song inherited the book speed');
  assert.equal(rateFor(null, prefs), 1, 'nothing playing must not hold a rate');
});

test('a book with no stored preference plays at 1x', () => {
  forgetAll();
  registerBook(oneFile());
  assert.equal(rateFor({ id: 'tr-whole', bookId: 'bk-1' }, new Map()), 1);
});

test('a nonsensical stored speed never reaches the audio element', () => {
  forgetAll();
  registerBook(oneFile());
  // playbackRate = 0 silently stops playback and looks like a broken file.
  for (const speed of [0, -1, NaN, undefined, null, 'fast']) {
    const prefs = new Map([['bk-1', { speed }]]);
    assert.equal(rateFor({ id: 'tr-whole', bookId: 'bk-1' }, prefs), 1, `speed ${speed} leaked through`);
  }
});

test('rateFor works from the registry alone, without a bookId on the track', () => {
  forgetAll();
  registerBook(oneFile());
  const prefs = new Map([['bk-1', { speed: 1.5 }]]);
  assert.equal(rateFor({ id: 'tr-whole' }, prefs), 1.5);
});

// --- the sleep timer ----------------------------------------------------------

test('the sleep timer counts down and stops at zero', () => {
  let left = 30 * 60_000;
  left = stepSleep(left, 1000);
  assert.equal(left, 30 * 60_000 - 1000);
  assert.equal(stepSleep(500, 1000), 0, 'must not go negative');
  assert.equal(stepSleep(0, 1000), 0, 'a disarmed timer stays disarmed');
});

test('one tick cannot swallow the whole timer', () => {
  // A phone freezes a backgrounded tab and resumes it hours later. Taking that
  // delta at face value would end a 30-minute timer in a single step, right when
  // someone put their phone down and expected 30 more minutes.
  const left = 30 * 60_000;
  assert.equal(stepSleep(left, 3 * 3600_000), left - MAX_SLEEP_STEP_MS);
});

test('a backwards clock does not add time', () => {
  // Wall-clock deltas can go negative across an NTP correction or a DST change.
  assert.equal(stepSleep(60_000, -5000), 60_000);
});

console.log(`\n${passed} passed`);
