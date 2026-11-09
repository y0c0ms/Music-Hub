/**
 * Tests for the request deadline in lib/api.js — run with
 * `node mock/api-timeout.test.mjs`.
 *
 * The case worth testing is the one a phone hits and a desktop does not: a
 * connection that is accepted and then never answered. `fetch` waits for the OS
 * TCP timeout (minutes), so without a deadline the boot spinner never resolves
 * and the offline screen is unreachable. Reported 2026-11-09.
 */
import assert from 'node:assert/strict';

// --- minimal browser stubs ----------------------------------------------------

globalThis.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };

const api = await import('../src/lib/api.js');

const realFetch = globalThis.fetch;
const restore = () => {
  globalThis.fetch = realFetch;
};

// --- a link that accepts and never answers ------------------------------------

let aborted = false;
globalThis.fetch = (_path, init) =>
  new Promise((_resolve, reject) => {
    init.signal.addEventListener('abort', () => {
      aborted = true;
      reject(new DOMException('The operation was aborted.', 'AbortError'));
    });
  });

const started = Date.now();
await assert.rejects(
  () => api.me({ timeoutMs: 60 }),
  (err) => {
    assert.equal(err.name, 'ApiError', `expected an ApiError, got ${err.name}`);
    assert.equal(err.status, 0);
    assert.equal(err.code, 'offline');
    return true;
  },
  'a request that is never answered must be abandoned, not waited on',
);
const took = Date.now() - started;
assert.ok(took < 2000, `the deadline must fire at once, took ${took}ms`);
assert.ok(aborted, 'the abandoned request must be aborted so its socket is freed');

// --- a caller's own abort is not a dead link ----------------------------------

globalThis.fetch = (_path, init) =>
  new Promise((_resolve, reject) => {
    init.signal.addEventListener('abort', () =>
      reject(new DOMException('The operation was aborted.', 'AbortError')),
    );
  });
const ctrl = new AbortController();
const pending = api.search('x', ctrl.signal).then(
  () => 'resolved',
  (err) => err.name,
);
ctrl.abort();
assert.equal(
  await pending,
  'AbortError',
  'a search the user typed over must still look like an abort, not an outage',
);

// --- an ordinary answer is untouched ------------------------------------------

globalThis.fetch = async () => new Response(JSON.stringify({ ok: true }), { status: 200 });
assert.deepEqual(await api.me({ timeoutMs: 60 }), { ok: true });

restore();
console.log('api timeout: 3 PASS');
