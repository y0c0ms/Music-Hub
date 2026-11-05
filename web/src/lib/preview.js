/**
 * Discovery previews.
 *
 * Deliberately a second <audio> element rather than the main queue: a preview is
 * not a play. Routing it through the player would clobber whatever the user was
 * listening to, scrobble a track they never chose, and put a discovery card on
 * the lockscreen.
 */
import { writable } from "svelte/store";
import { yieldAudio } from "./player.js";

const el = new Audio();
el.preload = "auto";
el.loop = true;
el.volume = 0.35;
el.dataset.role = "preview";
document.body.appendChild(el);

// A second element that only ever downloads. The next card's audio is already
// buffered by the time it reaches the top of the stack, so a swipe is instant.
const ahead = new Audio();
ahead.preload = "auto";

// A preview must not survive the screen going off.
//
// It loops by design, so a locked phone in a pocket would play a 30-second clip
// on repeat indefinitely - and unlike the main player, which legitimately keeps
// going in the background with lockscreen controls, there is nothing here to
// pause it with. The deck is a foreground activity; when it is not on screen
// there is nothing to preview.
//
// Pause rather than stop, and no auto-resume: coming back to a paused card is
// predictable, whereas audio restarting by itself on unlock is alarming.
document.addEventListener("visibilitychange", () => {
  if (document.hidden) el.pause();
});

/**
 * Every preview is seekable, because the reason to hear a card at all is to
 * recognise it - and that usually means the chorus, not the first four bars.
 *
 * There is no artificial cap here any more. The length is whatever the server
 * sent: a candidate's clip really is a ~30s file, an owned track is the whole
 * thing. The card draws the difference; this just plays what it is given.
 */
export const preview = writable({
  url: null,
  playing: false,
  // Autoplay was refused before a user gesture. Recoverable: the card offers a
  // tap-to-listen button.
  blocked: false,
  // The source gave us nothing - yt-dlp could not fetch a clip, so the server
  // answered 502 and there is no audio to play. NOT recoverable by tapping, and
  // kept separate from `blocked` for exactly that reason: rendering this as
  // "tap to listen" is why a broken deck used to look like a working one.
  //
  // Only set after retries are exhausted. Reported 2026-08-25: a new candidate
  // showed the failure banner and then played fine a second or two later, which
  // made the banner noise rather than information. PreviewClip BLOCKS while
  // yt-dlp fetches the clip, so the first request for anything new is genuinely
  // slow - and slow is not broken.
  failed: false,
  // Waiting on the first bytes. Almost always a candidate whose clip is being
  // cut server-side right now.
  loading: false,
  time: 0,
  duration: 0,
});

el.addEventListener("timeupdate", () =>
  preview.update((p) => ({ ...p, time: el.currentTime })),
);
el.addEventListener("durationchange", () => {
  preview.update((p) => ({
    ...p,
    duration: Number.isFinite(el.duration) ? el.duration : 0,
  }));
});
el.addEventListener("playing", () =>
  preview.update((p) => ({
    ...p,
    playing: true,
    blocked: false,
    failed: false,
    loading: false,
  })),
);
el.addEventListener("pause", () =>
  preview.update((p) => ({ ...p, playing: false })),
);

// A src that answered 502, or a decode failure, lands here rather than in the
// play() rejection path depending on the engine - so both go through retry().
//
// This used to set `failed` on the first error, which is why a brand new
// candidate showed a red "no audio" panel and then played normally a second
// later. PreviewClip runs yt-dlp inline and only caches afterwards, so the first
// request for anything new is slow by construction and the second is instant.
// Declaring failure on attempt one measured the wrong thing.
el.addEventListener("error", () => {
  if (!currentUrl) return;
  retry();
});

// --- attempts ----------------------------------------------------------------
//
// Three tries over ~4s before saying anything. Long enough to cover a clip being
// cut server-side, short enough that a genuinely dead source still gets called
// dead while the user is looking at the card.
let currentUrl = null;
let attempt = 0;
let timer = null;
const MAX_ATTEMPTS = 3;

function retry() {
  clearTimeout(timer);
  if (attempt >= MAX_ATTEMPTS) {
    preview.update((p) => ({ ...p, playing: false, loading: false, failed: true }));
    return;
  }
  attempt += 1;
  preview.update((p) => ({ ...p, playing: false, loading: true, failed: false }));
  timer = setTimeout(
    () => {
      if (!currentUrl) return;
      el.load(); // re-request; by now the clip is usually cached
      attemptPlay();
    },
    700 * attempt,
  );
}

function attemptPlay() {
  const p = el.play();
  if (!p || typeof p.catch !== "function") return;
  p.catch((err) => {
    const name = err?.name || "Error";
    // Superseded by a newer load or a pause. Normal while swiping the deck, and
    // reporting it is what made a working preview look broken.
    if (name === "AbortError") return;
    // Before any user gesture the browser refuses to start audio. Not an error:
    // the card offers tap-to-listen.
    if (name === "NotAllowedError") {
      preview.update((p2) => ({ ...p2, playing: false, loading: false, blocked: true }));
      return;
    }
    retry();
  });
}

/** Jump to a position within whatever was loaded. */
export function seek(seconds) {
  const d = Number.isFinite(el.duration) ? el.duration : 0;
  if (d <= 0) return;
  el.currentTime = Math.max(0, Math.min(seconds, d - 0.25));
  preview.update((p) => ({ ...p, time: el.currentTime }));
}

/**
 * Take the audio output for previews.
 *
 * Separate from start() because a card without a preview still has to silence
 * the library player: otherwise opening Discover while a song is playing leaves
 * it running underneath, and the next preview that does start plays on top of
 * it. Silence is the correct state for a deck card with nothing to preview.
 */
export function claim() {
  yieldAudio();
}

export function start(url) {
  // Yield first, and unconditionally. The early return below is why a card with
  // no preview used to leave the player running.
  yieldAudio();
  clearTimeout(timer);
  currentUrl = url || null;
  attempt = 0;

  if (!url) {
    preview.set(idle());
    return;
  }
  if (el.src !== new URL(url, window.location.origin).href) {
    el.src = url;
  }
  // No `el.currentTime = 0` here. A fresh src starts at zero anyway, and setting
  // currentTime on media with no metadata yet forces a second load - which is
  // one more thing that can abort the play() below and report a working preview
  // as broken. The main player learned this the same day.
  preview.set({ ...idle(), url, loading: true });
  attemptPlay();
}

export function stop() {
  clearTimeout(timer);
  currentUrl = null;
  attempt = 0;
  el.pause();
  el.removeAttribute("src");
  el.load();
  preview.set(idle());
}

export function toggle() {
  if (el.paused) {
    // The tap-to-listen path. Autoplay was refused, so the library player may
    // still be running - starting here without yielding plays both at once.
    yieldAudio();
    // A deliberate tap deserves a fresh set of attempts: whatever failed before
    // may well have been the clip still being cut.
    attempt = 0;
    preview.update((p) => ({ ...p, failed: false, loading: true }));
    attemptPlay();
  } else {
    el.pause();
  }
}

function idle() {
  return {
    url: null,
    playing: false,
    blocked: false,
    failed: false,
    loading: false,
    time: 0,
    duration: 0,
  };
}

export function prefetch(url) {
  if (!url || ahead.getAttribute("src") === url) return;
  ahead.src = url;
  ahead.load();
}
