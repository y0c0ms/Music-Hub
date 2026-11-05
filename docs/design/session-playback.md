# Session playback: one account, one playback state

**Status: SHIPPED.** Server half `feat/session-playback` (2026-08-23), client half and
server-side scrobbling 2026-08-24. Released as `v33`, corrected by `v34` and `v35`.

The server was first written in Go and later ported to Rust; it now lives in
`crates/server/src/playback/`. File references below to the Go tree are historical.

Read the design below for intent, then **"What use changed"** at the end for the three things it got
wrong. Nothing above that line has been rewritten to look prescient.

## What we're building

Your phone controls what plays on your computer. Every device signed into the same account
shows the same queue, the same track and the same position, and any of them can drive it.

**One device makes sound; the others are remotes.** This is the Spotify Connect model, and it
satisfies "two devices can never be on different songs" in the strongest way available — there is
one playback state on the server, and only one device is playing at all.

The rejected alternative was **mirroring** (every device emits audio in sync). It matches the words
"impossible to be listening to a different song" more literally, but it means your phone starts
making noise the moment you press play on the desktop, and it drags in clock sync and drift
correction for a behaviour nobody actually wants. Not building that.

## Why this is a small change and not a rewrite

`web/src/lib/player.js` is already the single choke point. It owns one `new Audio()` element
(line 15) and exports 14 control functions. **Nine components import it, and not one of them
touches the audio element** — they call `play()`, `next()`, `toggle()` and read the `player` store.

So the implementation behind those exports can be replaced wholesale without editing a single
component. Keep every signature; change what happens inside. That property is the reason this is
worth doing now rather than after the player grows further.

Existing pieces we get to reuse:

| Need | Already there |
|---|---|
| per-user identity | persisted sessions carrying `Username` |
| ownership scoping | the `Owner` field on library rows, playlist scoping landed in `baca212` |
| scrobble path | `Library.Scrobble` — already a server endpoint |
| route style | one router, `"POST /api/..."` patterns |

What is **not** there: any realtime transport. No WebSocket, no SSE, no dependency for either.
`web/src/lib/library-events.js` is a client-side Svelte counter, not a transport.

## Transport: SSE, not WebSocket

Server-Sent Events over `net/http`, no new dependency. The traffic is one-directional (server →
clients) with commands going back over ordinary `POST`, which is exactly SSE's shape. WebSocket
would add a library, a handshake and framing for no gain here.

`http.ResponseWriter` + `http.Flusher` is all that's needed, and **the plumbing is already
correct**: the status-recording middleware forwards `Flush()` to the underlying writer
("so streaming proxies keep their fast paths"). A logging middleware that swallows `Flush()` is the
classic way SSE silently buffers forever — this one doesn't, so no change is needed there. Worth
knowing precisely because it looks like a likely culprit if streaming ever misbehaves.

## Server: `playback`

```go
type State struct {
    Queue      []string  `json:"queue"`       // track IDs, not full objects
    Order      []int     `json:"order"`       // playback sequence; shuffle stays non-destructive
    Cursor     int       `json:"cursor"`
    Playing    bool      `json:"playing"`
    Shuffle    bool      `json:"shuffle"`
    Repeat     string    `json:"repeat"`      // off | all | one
    PositionMS int       `json:"positionMs"`
    PositionAt time.Time `json:"positionAt"`  // when PositionMS was true
    ActiveID   string    `json:"activeId"`    // the device making sound; "" = nobody
    Version    uint64    `json:"version"`     // monotonic per user
    UpdatedAt  time.Time `json:"updatedAt"`
}
```

`Queue` holds **IDs, not track objects**. The state is broadcast on every change; sending full
metadata would make each frame kilobytes for no reason, and every client already has
`GET /api/library/song/{id}` plus a cache.

`PositionAt` is what lets a remote render a moving progress bar without the active device
reporting continuously: while `Playing`, a remote shows `PositionMS + (now - PositionAt)`. The
active device posts its real position every ~5 s and immediately on seek/pause/track-change.

`Version` is a monotonic counter per user. Commands carry the version they believed; the server
rejects a stale one with `409` and the current state. Without it, two devices tapping "next" in
the same second skip two tracks.

Store: `map[username]*State` behind a `sync.RWMutex`, plus a hub of subscriber channels. This is
a self-hosted app with a handful of devices — no Redis, no pub/sub. Persist to disk on change the
same way sessions are persisted, so a restart does not lose the queue.

### Endpoints (6)

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/playback/stream` | SSE. Sends full `State` on connect, on every change, and a heartbeat comment every 20 s |
| `POST` | `/api/playback/command` | `{action, args, deviceId, version}` — `play`, `enqueue`, `next`, `prev`, `jump`, `seek`, `toggle`, `pause`, `shuffle`, `repeat`, `drop`, `stop` |
| `POST` | `/api/playback/progress` | active device reports `{positionMs, playing}`; throttled |
| `GET` | `/api/playback/devices` | list, with which is active |
| `POST` | `/api/playback/transfer` | `{deviceId}` — move sound to another device |
| `POST` | `/api/playback/devices` | register/rename this device |

Devices are identified by a UUID in `localStorage`, with a human name defaulting from the user
agent ("Firefox on Linux", "Pixel"). Registration is implicit on first stream connect.

## Frontend: `player.js` keeps its exports

Every export keeps its signature. Inside, each control function becomes:

1. **optimistic local patch** so the UI responds instantly, then
2. `POST /api/playback/command`, and
3. the SSE frame is authoritative — it overwrites the optimistic guess.

New internal state:

```js
export const devices = writable([]);        // for the picker
export const isActive = derived(...);       // is THIS device the one making sound
```

**The audio element is only ever given a `src` when `isActive` is true.** A remote holds the same
state object and renders the same UI, with a silent `<audio>`. That is the whole enforcement
mechanism, and it is one condition in `load()`.

`el.addEventListener('ended', …)` currently calls `next(false)` locally. On the active device it
must instead report to the server and let the broadcast advance everyone. On a remote it can never
fire, because no `src` is ever set.

New component `web/src/components/DevicePicker.svelte` (~120 lines), reachable from
`FullPlayer.svelte` — a list of devices with the active one marked, tap to transfer.

## Three things that will bite

### 1. Browser autoplay policy — unavoidable, plan around it

A tab **cannot** start audio without a prior user gesture in that tab. So "phone tells desktop to
play" only works if the desktop tab has been interacted with at least once since it loaded. After
one gesture, programmatic `el.play()` works for the rest of the page's life.

This is not a bug to fix; Spotify's own web player behaves identically. The design that makes it
invisible:

- A device becomes **active** only by a user action **on that device** — pressing play there, or
  choosing "play here". That action *is* the gesture.
- Once the desktop is active, the phone can drive it indefinitely.
- After a desktop reload, the first play needs one tap there. Show that honestly in the picker:
  *"Reload — tap once to resume control"*, rather than a command that silently does nothing.

### 2. Scrobbling will double-count

`player.js` currently scrobbles client-side once playback passes 50 % (the `scrobbled` flag). The
moment several devices share state, whichever ones think they are playing would each POST
`/api/library/scrobble/{id}` — polluting ListenBrainz and any love/hate feedback loop
downstream.

**Move it server-side.** The server already owns position via `/api/playback/progress` and already
has `Library.Scrobble`. Scrobble once per `(user, track, cursor-visit)` when reported position
crosses 50 %. Delete the client-side trigger in the same commit — leaving both is the failure mode.

### 3. The active device can vanish

Tab closed, laptop asleep, phone locked. When the active device's SSE stream drops and does not
return within ~30 s: set `Playing=false` and clear `ActiveID`. **Do not auto-transfer** to another
device — a phone in a pocket suddenly playing out loud is the worst possible behaviour.

## Degrading gracefully

With exactly one device connected, this must feel identical to today. It does: the single device
registers, becomes active on first play, and the SSE round-trip is local. Nothing about the
single-device experience should change, and that is the acceptance bar before shipping.

## Rough size

| Piece | Estimate |
|---|---|
| server `playback` module (state, store, hub, handlers) | ~450 lines + ~250 test |
| route wiring | +6 routes |
| `player.js` rewritten behind the same exports | ~200 lines changed, **0 component changes** |
| `DevicePicker.svelte` | ~120 lines |
| scrobble move + delete client trigger | ~60 lines |

One to two days including tests. The dominant risk is not the code — it is the three items above,
each of which produces a plausible-looking app that is subtly wrong.

---

# Where this stood — handoff, 2026-08-23

**The server was done and tested. The client was untouched.** Branch
`feat/session-playback`, four commits off `623ee2f`.

## Done

| | |
|---|---|
| `playback/state` | pure state machine, 20 tests |
| `playback/store` | per-user store, SSE hub, persistence, reaper, 13 tests |
| `playback` handlers | 6 handlers, 6 tests |
| route wiring | routes wired behind the session check, reaper started |

39 tests in the package, whole repo suite green under `-race`.

Two divergences from `player.js` were caught by writing the tests, and both are
now matched rather than "improved":

- `play(index)` must look the cursor up in `order`. Setting it to 0 silently
  plays track one whenever shuffle is off, because a sequential order leaves the
  tapped index where it is.
- `enqueue` onto an **empty** queue is a **no-op** in `player.js`
  (`if (!p.queue.length) return p`). The server now does the same. If that client
  behaviour is a bug, fix it in both places in one commit — a divergence makes the
  UI jump when the broadcast lands, which reads as a race and is not.

## What remains — all client-side

1. **Rewrite `player.js` behind its existing exports.** Every signature stays;
   nine components keep working untouched. Each control becomes: optimistic local
   patch → `POST /api/playback/command` → the SSE frame is authoritative.
2. **Only set `el.src` when this device is active.** That single condition is the
   whole enforcement mechanism. A remote holds identical state with a silent
   `<audio>`.
3. **`ended` must report to the server**, not call `next()` locally — otherwise
   only one device advances.
4. **Move the scrobble server-side and delete the client trigger in the same
   commit.** The library already has `Scrobble`; the server owns position via
   `/api/playback/progress`. Leaving both is the failure mode: every device
   scrobbles, polluting ListenBrainz and any feedback loop built on it.
5. **`DevicePicker.svelte`**, reachable from `FullPlayer.svelte`.

## The contract

```
GET  /api/playback/stream?device=<id>&name=<label>   SSE: `event: state`, JSON State
GET  /api/playback/state                             plain read
GET  /api/playback/devices                           [{id,name,lastSeen,active}]
POST /api/playback/command   {action,deviceId,version,...}  -> State | 409 State
POST /api/playback/progress  {deviceId,positionMs,playing}  -> State
POST /api/playback/transfer  {deviceId}                     -> State | 404
```

- **`version`** — send the version you last saw. `409` returns the *current*
  state; rebase on it and resend. Send `0` to mean "don't care" before you have
  seen any state. This is what stops two devices tapping next from skipping two
  tracks.
- **`deviceId`** — a UUID in `localStorage`. `play` and a *resuming* `toggle`
  make that device the one making sound; `pause` deliberately does not, so
  pausing from a phone does not steal audio from the desktop.
- **`progress`** is accepted **only from the active device** and does **not**
  bump `version` — position is high-frequency and versioning it would make every
  other device's commands stale within a second.
- **Rendering position on a remote:** while `playing`, show
  `positionMs + (now - positionAt)`. The active device only posts every ~5 s.

## Traps

- **`X-Accel-Buffering: no`** is already set on the stream and must stay. This app
  is typically behind a reverse proxy (nginx), which buffers proxied responses by default and
  would deliver the whole stream at close. Symptom: "the app never updates",
  server looks healthy. There is a test asserting the header.
- **Autoplay policy.** A tab cannot start audio without a prior gesture *in that
  tab*. `transfer` changes who *should* play; the target still has to satisfy its
  own browser. If that tab has had no gesture since loading it will go silent, and
  the picker should say so rather than leave a dead-looking button.
- **The reaper stops playback but never hands it over.** Do not add
  auto-transfer. A phone in a pocket playing out loud because a laptop slept is
  the worst outcome available.
- **Single-device must feel unchanged.** That is the acceptance bar before this
  ships.

---

# What use changed, 2026-08-24

The design held up: SSE needed no dependency, `player.js` was replaced behind its exports with
**zero component edits**, and the three predicted traps (autoplay, the reaper, double-scrobbling)
all behaved as written. What it got wrong was smaller and more interesting — **three cases that were
defensible on paper and wrong in a room with two devices.**

## 1. "Resuming is a deliberate play here" — wrong (v34)

The design had `play` and a resuming `toggle` claim the speakers, on the Spotify-Connect reading
that pressing play somewhere means "play *here*". In use it meant: listening on a phone, tapping a
song on the laptop, and the sound jumped across the room.

They now claim the speakers **only when `ActiveID` is empty** — which the reaper guarantees means
"nobody is playing", and which is also exactly when the tapping device *must* take it, since that
tap is the gesture its browser requires. Sound otherwise moves only on an explicit `transfer`.

This **inverted a passing test**. `TestToggleClaimsTheDeviceOnlyWhenResuming` asserted the old
reading. A test can be green and still describe the wrong product; it now encodes the reported
behaviour, both halves.

## 2. The remote-seek gap the design never mentioned (v34, v35)

Nothing in the design said how a seek reaches the device making sound. The first attempt inferred it
from `Version`, and that was wrong **in both directions**: `Version` bumps for `shuffle`, `repeat`
and `enqueue` too, so an unrelated toggle would yank playback to a position up to five seconds
stale, while the seek itself stayed unreliable. It shipped, and failed in someone's hands.

`State.PositionSeq` now exists and bumps **only when a command moved the position or the cursor**,
detected generically at the end of `Apply` so a future transition cannot forget to opt in. The
client seeks when that number changes: no float comparison, no debounce, no inference. Asserted
both ways — position commands bump it, `shuffle`/`repeat`/`enqueue` do not.

**The lesson worth keeping:** inferring intent from a counter that means something else fails
silently in someone's hands rather than loudly in a test. If a client needs to know "was this an
instruction", the state has to say so out loud.

## 3. Hiding the device picker until it was "needed" — wrong (v34, v35)

The design made the picker appear only once a second device existed, to keep a single-device setup
identical. That optimised for a property which stops mattering the moment playback can live
elsewhere: **"where is this playing" has to be answerable at a glance.** The header now always names
the device, and the picker is also in Settings — the full player only exists while something is
playing, and moving the sound is something you want to do *before* pressing play.

## Actions added after the design

| action | why it is not `drop` |
|---|---|
| `removeAt` | Takes a position in `Order`. `drop` takes a track id and removes the first match, which is right when a file was deleted from disk and wrong when the queue holds the same track twice and the user swiped the second one. |

Queue rows also carry the app's standard swipe language: left opens the same options sheet a song row
opens anywhere else, right removes. Right is "enqueue" everywhere else, and that slot is free inside
the queue because adding to the queue you are looking at means nothing.

## Still true, and worth not relearning

- `X-Accel-Buffering: no` on the stream, with a test asserting it. NPM buffers proxied responses by
  default and would deliver the whole stream at close.
- The reaper stops playback and never hands it over. A phone in a pocket playing out loud because a
  laptop slept is still the worst available outcome.
- Playback survives a dead event stream: a command's own response carries the state, so pressing
  play recovers without waiting for SSE.
