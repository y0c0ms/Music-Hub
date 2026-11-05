# Offline music

Save songs, albums and playlists to the device, play them with no connection, and
open the app when the server cannot be reached.

**This is a web-only change.** The Android shell loads the remote bundle from the
configured server URL, so a deploy reaches every install. No APK patch is needed.

## Why blob URLs and not a service-worker route

The obvious design is a Workbox route for `/api/stream/` serving from a cache.
`vite.config.js` deliberately rules that out:

> Audio is deliberately NOT matched here: an unrouted request never enters the
> service worker's fetch handler at all, which keeps Range/seek behaviour
> identical to having no worker installed.

Routing audio through the worker would have thrown that away and forced a
hand-rolled 206/Range implementation. Instead a downloaded track plays from a
`blob:` URL created from the cached response. Blob URLs seek natively, need no
worker involvement, and leave online playback byte-for-byte unchanged.

The cost is one non-obvious constraint, and it is the thing most likely to break
if this is edited carelessly:

**`syncAudio` must identify a saved track by `track.id`, never by the `src`
attribute.** `changedSrc` compared `el.getAttribute('src')` against
`track.streamUrl`. With a blob URL in `src` that is true on every state frame, so
the player would decide the track had changed several times a second and restart
playback. `appliedTrackId` (already present for the re-claim fix) carries the
identity, and `changedSrc` now asks about the track for saved songs and the
attribute for the rest. Every blob URL is revoked when it is replaced or
when playback moves to another device — a blob pins its data until revoked, so a
long session would otherwise leak an album at a time.

## Two stores, and the one that can lie

Audio lives in the Cache API (`music-hub-offline`); metadata lives in
localStorage under one JSON key. The split is forced: localStorage cannot hold
audio, and the player needs a **synchronous** answer to "is this track local?"
while assembling a state frame, which the Cache API cannot give.

The browser can evict Cache API storage under pressure **without touching
localStorage**. That leaves an index claiming tracks with no audio behind them,
and it would offer a song that plays silence. So:

- `reconcile()` runs at startup and drops entries whose audio is gone.
- `objectUrlFor()` self-heals: a miss removes the entry rather than failing quietly.
- "Downloaded" always means *both* halves exist.
- **`reconcile()` never deletes when it cannot verify.** An earlier version wiped
  the index if the Cache API was unavailable, turning "I could not check" into
  permanent loss. It also refuses a wholesale delete: losing every entry at once
  is far more likely to be a lookup bug than the browser reclaiming every file,
  and the two mistakes do not cost the same - a stale entry heals on use, a wiped
  index does not.
- **`rebuildFromCache()` handles the other direction.** Audio can outlive the
  index, in which case it sits there consuming space, invisible and unplayable.
  It walks the cache, finds `/api/stream/<id>` entries the index does not list,
  and re-fetches their metadata. It needs the network, so it no-ops offline and
  succeeds on a later launch.

Covers are fetched into `music-hub-cover` at download time. `/api/*` is
`NetworkOnly`, so artwork that was never fetched while online would be blank.

`npm run test:saved` covers the disagreement cases, which are the ones worth
testing — including eviction and self-healing.

## Offline playback

Playback is server-authoritative: controls post a command and the UI only moves
when a state frame returns. That is right with a server and dead without one,
which would make downloaded music unplayable — the entire point of downloading it.

So when a command fails with a transport error, `send()` applies the same command
to a local state object **of the same shape** and pushes it through `apply()`.
Every consumer — queue view, mini player, media session, `syncAudio` — keeps
working unchanged, because none of them can tell where the frame came from.

Only a transport failure counts. A 4xx is the server talking, and going local
because of one would strand the app offline while the network was fine.

An offline session is deliberately **not** synced back afterwards. Replaying it
could overwrite what a phone with a connection did in the meantime, and losing a
local queue is a much smaller harm than corrupting shared state.

## Boot

`App.svelte` shows an error screen when `api.me()` fails. With downloads present
that is the wrong answer, so it now reconciles first and, if anything survives,
renders `screens/Offline.svelte` instead — a downloads-only view with the player.

Not the full app with failing tabs: Search, Discover and Library all need the API,
and four screens showing spinners would read as broken rather than offline.

The app shell itself is precached by Workbox (17 entries), which is what lets it
start at all with no network.

## Naming

The app already uses "Download" for fetching a song into the server library. This
feature is labelled **Offline** everywhere to avoid two meanings for one word.

## Open issue: index loss, cause unknown

**Status 2026-08-30: NOT diagnosed. Do not assume it is fixed.**

On a device running v55/v56 the saved-songs count dropped to **0 after a reload**
while the audio was still present and still occupying storage. v57 made
`reconcile()` non-destructive and added `rebuildFromCache()` to re-derive the
index from cached audio, **and it did not recover that device** - the operator
cleared the library and re-downloaded instead.

The v57 changes are sound defence-in-depth and should stay. They are not a fix
for something understood.

Ruled out:

- **a per-session or signed stream URL.** `streamURL()` returns a plain
  `/api/stream/<id>` with no token and no query, stable across sessions.
- **`cleanupOutdatedCaches`.** It only removes stale workbox precaches, not
  arbitrary named caches like `music-hub-offline`.

Still open: why every `cache.match()` missed on that device.

The evidence lives on the device and cannot be read from a server. If it recurs,
before clearing anything, get this from the WebView/browser console:

```js
localStorage.getItem('music-hub.offline.v1')             // index present at all?
await caches.keys()                                       // is music-hub-offline there?
(await (await caches.open('music-hub-offline')).keys()).map((r) => r.url)
```

Those three separate the candidates, and they need different fixes: index gone
but audio present (the recovery path failed - debug `rebuildFromCache`), both
gone (real eviction - nothing is broken, the storage was reclaimed), or both
present but not matching (a key normalisation bug in `cache.match`).

Clearing the library destroys exactly this evidence, so capture it first.
