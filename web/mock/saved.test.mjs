/**
 * Tests for lib/saved.js — run with `node mock/saved.test.mjs`.
 *
 * The behaviour worth testing is not "does it store a blob", it is what happens
 * when the two stores disagree. The Cache API can be evicted by the browser
 * under storage pressure WITHOUT touching localStorage, so the index can claim a
 * track that has no audio behind it. An index that lies is worse than no index:
 * it offers a song that plays silence.
 */
import assert from 'node:assert/strict';

// --- minimal browser stubs ----------------------------------------------------

const store = new Map();
globalThis.localStorage = {
  getItem: (k) => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => store.set(k, String(v)),
  removeItem: (k) => store.delete(k),
};

class FakeCache {
  constructor() {
    this.entries = new Map();
  }
  async put(url, res) {
    this.entries.set(String(url), res);
  }
  async match(url) {
    return this.entries.get(String(url));
  }
  async delete(url) {
    return this.entries.delete(String(url));
  }
  async keys() {
    return [...this.entries.keys()].map((u) => ({ url: new URL(u, 'https://x.test').href }));
  }
}
const caches_ = new Map();
globalThis.caches = {
  open: async (name) => {
    if (!caches_.has(name)) caches_.set(name, new FakeCache());
    return caches_.get(name);
  },
  delete: async (name) => caches_.delete(name),
};
Object.defineProperty(globalThis, 'navigator', {
  value: { storage: { estimate: async () => ({ quota: 1e9 }) } },
  configurable: true,
});
globalThis.URL.createObjectURL = () => 'blob:stub';
globalThis.URL.revokeObjectURL = () => {};

const audio = new Uint8Array(2048);
globalThis.fetch = async (url) => ({
  ok: true,
  status: 200,
  clone() {
    return this;
  },
  blob: async () => ({ size: audio.length, type: 'audio/mpeg' }),
});
// The module wraps the blob in a Response; Node 18+ has one, but it will not
// accept our fake blob, so give it something that just carries the value.
globalThis.Response = class {
  constructor(body) {
    this.body = body;
  }
  // rebuildFromCache reads the size back off the cached response, so the stub
  // has to actually behave like one - an incomplete stub made a passing code
  // path look like a failure.
  clone() {
    return this;
  }
  async blob() {
    return this.body;
  }
  // restoreFromMirror stores the index as a JSON string and reads it back with
  // text(); a string body round-trips through this unchanged.
  async text() {
    return typeof this.body === 'string' ? this.body : JSON.stringify(this.body);
  }
};

const saved = await import('../src/lib/saved.js');
const { get } = await import('svelte/store');

const track = (id) => ({
  id,
  title: `Song ${id}`,
  artist: 'Someone',
  streamUrl: `/api/stream/${id}`,
  coverUrl: `/api/cover/${id}`,
  duration: 180,
});

async function offlineClearAll() {
  await saved.clear();
  const c = await caches.open('music-hub-offline');
  c.entries.clear();
}

let failures = 0;
async function test(name, fn) {
  try {
    await fn();
    console.log(`  PASS  ${name}`);
  } catch (err) {
    failures += 1;
    console.log(`  FAIL  ${name}\n        ${err.message}`);
  }
}

await test('download makes a track available and synchronously visible', async () => {
  assert.equal(saved.has('a'), false);
  const ok = await saved.download(track('a'));
  assert.equal(ok, true, 'download reported failure');
  // has() must be synchronous: the player calls it while building a state frame
  // and cannot await.
  assert.equal(saved.has('a'), true);
  assert.equal(get(saved.downloads).length, 1);
});

await test('the cover is fetched too, so artwork survives offline', async () => {
  const cover = await caches.open('music-hub-cover');
  assert.ok(await cover.match('/api/cover/a'), 'cover was not cached');
});

await test('downloading the same track twice is a no-op', async () => {
  await saved.download(track('a'));
  assert.equal(get(saved.downloads).length, 1);
});

await test('reconcile drops index entries whose audio was evicted', async () => {
  await saved.download(track('b'));
  assert.equal(saved.count(), 2);

  // Simulate the browser reclaiming Cache API storage without touching
  // localStorage — the exact split-brain this guards against.
  const cache = await caches.open('music-hub-offline');
  cache.entries.delete('/api/stream/b');

  const pruned = await saved.reconcile();
  assert.equal(pruned, 1, 'expected exactly one pruned entry');
  assert.equal(saved.has('b'), false, 'b still claimed after its audio vanished');
  assert.equal(saved.has('a'), true, 'a was pruned but its audio is present');
});

await test('objectUrlFor self-heals when the audio is gone', async () => {
  await saved.download(track('c'));
  const cache = await caches.open('music-hub-offline');
  cache.entries.delete('/api/stream/c');
  const url = await saved.objectUrlFor('c');
  assert.equal(url, '', 'returned a URL for audio that does not exist');
  // and it must correct the index rather than leave the lie in place
  assert.equal(saved.has('c'), false, 'index still claims c');
});

await test('remove clears both halves', async () => {
  await saved.download(track('d'));
  await saved.remove('d');
  assert.equal(saved.has('d'), false);
  const cache = await caches.open('music-hub-offline');
  assert.equal(await cache.match('/api/stream/d'), undefined, 'audio left behind');
});

await test('downloadMany skips what is already there and counts failures', async () => {
  const res = await saved.downloadMany([track('a'), track('e'), track('f')]);
  assert.equal(res.total, 2, 'should have skipped the already-downloaded track');
  assert.equal(res.failed, 0);
});

await test('reconcile refuses a wholesale delete', async () => {
  await offlineClearAll();
  await saved.download(track('w1'));
  await saved.download(track('w2'));
  assert.equal(saved.count(), 2);

  // Every lookup misses. That is far more likely to be a lookup bug than the
  // browser evicting every file at once, and wiping the list is unrecoverable
  // while a stale entry heals itself on use.
  const cache = await caches.open('music-hub-offline');
  const backup = new Map(cache.entries);
  cache.entries.clear();

  const pruned = await saved.reconcile();
  assert.equal(pruned, 0, 'reported pruning during a suspected lookup failure');
  assert.equal(saved.count(), 2, 'the whole index was destroyed');
  cache.entries = backup;
});

await test('reconcile changes nothing when the Cache API is unavailable', async () => {
  await offlineClearAll();
  await saved.download(track('n1'));
  const real = globalThis.caches;
  delete globalThis.caches;
  const pruned = await saved.reconcile();
  globalThis.caches = real;
  assert.equal(pruned, 0);
  assert.equal(saved.count(), 1, 'index wiped because it could not be checked');
});

await test('rebuildFromCache recovers audio the index lost', async () => {
  await offlineClearAll();
  await saved.download(track('r1'));
  // Lose the index but keep the audio - the exact split reported on device.
  store.delete('music-hub.offline.v1');
  const reloaded = await import('../src/lib/saved.js?bust=' + Date.now());
  assert.equal(reloaded.count(), 0, 'precondition: index is empty');

  const found = await reloaded.rebuildFromCache(async (id) => track(id));
  assert.equal(found, 1, 'did not recover the orphaned audio');
  assert.equal(reloaded.has('r1'), true);
});

await test('clear empties the index', async () => {
  await saved.clear();
  assert.equal(saved.count(), 0);
  assert.equal(get(saved.downloads).length, 0);
});

// The 2026-08-30 bug, reproduced: localStorage cleared under the app while the
// audio (and now the mirror) survive in the Cache. Recovery must work with NO
// network - the failure of the old path was that it needed api.song().
await test('restoreFromMirror rebuilds a wiped index offline, no network', async () => {
  await offlineClearAll();
  assert.equal(await saved.download(track('m1')), true);
  assert.equal(await saved.download(track('m2')), true);
  assert.equal(saved.count(), 2);

  // Simulate the platform clearing Web Storage while CacheStorage is untouched:
  // the exact asymmetry that caused the bug. The audio and the mirror remain.
  store.delete('music-hub.offline.v1');
  const before = JSON.parse(store.get('music-hub.offline.v1') || 'null');
  assert.equal(before, null, 'precondition: localStorage is wiped');

  // Fetch is set to reject, proving recovery needs no server.
  const savedFetch = globalThis.fetch;
  globalThis.fetch = async () => {
    throw new Error('network must not be used');
  };
  try {
    const n = await saved.restoreFromMirror();
    assert.equal(n, 2, 'did not restore both tracks from the mirror');
    assert.equal(saved.has('m1'), true);
    assert.equal(saved.has('m2'), true);
    assert.equal(JSON.parse(store.get('music-hub.offline.v1')).length, 2, 'localStorage not rewritten');
  } finally {
    globalThis.fetch = savedFetch;
  }
});

await test('restoreFromMirror leaves an intact index alone', async () => {
  await offlineClearAll();
  assert.equal(await saved.download(track('k1')), true);
  const n = await saved.restoreFromMirror();
  assert.equal(n, 0, 'restored over a healthy index');
  assert.equal(saved.count(), 1);
});

await test('rebuildFromCache ignores the synthetic index key', async () => {
  await offlineClearAll();
  assert.equal(await saved.download(track('x1')), true);
  // The mirror is now in the cache under /__offline-index__. Wipe the index and
  // rebuild: the mirror key must NOT be mistaken for an orphaned song.
  store.delete('music-hub.offline.v1');
  const found = await saved.rebuildFromCache(async (id) => {
    assert.notEqual(id, '_offline-index_', 'treated the mirror as a song id');
    return track(id);
  });
  // x1 is a real orphan and recovers; the mirror key does not become a track.
  assert.equal(saved.has('x1'), true);
  assert.equal(get(saved.downloads).some((t) => t.streamUrl === '/__offline-index__'), false);
});

console.log(failures ? `\n  ${failures} failing` : '\n  all green');
process.exit(failures ? 1 : 0);
