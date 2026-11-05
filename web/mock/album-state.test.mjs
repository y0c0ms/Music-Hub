/**
 * Tests for album download state.
 *
 * The bug these lock down was reported from the phone: "even the songs that I
 * clicked download still appear as queued, all of them, even though they were
 * all downloaded already". The cause was a latched boolean rather than a reading
 * of the live jobs - so these tests are mostly about making sure state can still
 * move once it has been set.
 *
 * Run: node mock/album-state.test.mjs
 */
import assert from 'node:assert/strict';
import { isAlbumDone, isAlbumBusy, albumJobs } from '../src/lib/albumState.js';

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

const album = { playlistId: 'OLAK5uy_x', tracks: [{ sourceId: 'a' }, { sourceId: 'b' }, { sourceId: 'c' }] };
const jobs = (...states) =>
  Object.fromEntries(states.map((s, i) => [['a', 'b', 'c'][i], s ? { state: s } : undefined]));

test('nothing started: not done, not busy', () => {
  assert.equal(isAlbumDone(album, {}), false);
  assert.equal(isAlbumBusy(album, {}), false);
});

test('all three finished: done, and no longer busy', () => {
  const j = jobs('done', 'done', 'done');
  assert.equal(isAlbumDone(album, j), true);
  assert.equal(isAlbumBusy(album, j), false);
});

test('THE REPORTED BUG: a finished download stops reading as queued', () => {
  // Every track queued, then every track finished. The old latched flag stayed
  // on "queued" through this transition; the derived reading must not.
  const queued = jobs('queued', 'queued', 'queued');
  assert.equal(isAlbumBusy(album, queued), true, 'should be busy while queued');
  assert.equal(isAlbumDone(album, queued), false);

  const finished = jobs('done', 'done', 'done');
  assert.equal(isAlbumBusy(album, finished), false, 'still reading as in-progress after finishing');
  assert.equal(isAlbumDone(album, finished), true);
});

test('partly finished is busy, not done', () => {
  const j = jobs('done', 'downloading', 'queued');
  assert.equal(isAlbumDone(album, j), false);
  assert.equal(isAlbumBusy(album, j), true);
});

test('a failed track is not busy, so the button can be pressed again', () => {
  const j = jobs('done', 'failed', 'done');
  assert.equal(isAlbumBusy(album, j), false, 'a failure must not block a retry forever');
  assert.equal(isAlbumDone(album, j), false, 'and must not count as downloaded');
});

test('one track never started: not done', () => {
  const j = jobs('done', 'done', null);
  assert.equal(isAlbumDone(album, j), false);
});

test('an album with no track list under-claims rather than over-claims', () => {
  // `tracks` is omitempty on the wire. Saying "downloaded" here would send
  // someone offline without the music.
  assert.equal(isAlbumDone({ playlistId: 'x' }, {}), false);
  assert.equal(isAlbumBusy({ playlistId: 'x' }, {}), false);
});

test('survives null and undefined without throwing', () => {
  for (const a of [null, undefined, {}, { tracks: null }]) {
    assert.equal(isAlbumDone(a, {}), false);
    assert.equal(isAlbumBusy(a, {}), false);
    assert.deepEqual(albumJobs(a, {}), []);
  }
  assert.equal(isAlbumDone(album), false, 'trackJobs defaults to empty');
});

console.log(`\n${passed} passed`);
