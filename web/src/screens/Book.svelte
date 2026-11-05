<script>
  /**
   * One audiobook: where you are in it, its chapters, and the two controls that
   * only matter for spoken word.
   *
   * WHY THIS IS NOT THE ALBUM SCREEN. An album is a set of interchangeable
   * tracks; a book is one ordered thing you are partway through. Resume is the
   * primary action rather than "play", the chapter you are in is highlighted, and
   * the position is written back to the server so it follows you to another
   * device.
   *
   * A chapter here is a track id plus a [startSec, endSec) range. For a YouTube
   * book every chapter shares one track and differs by offset, so "play chapter
   * 7" is a SEEK inside the file already playing; for a LibriVox book each
   * chapter is its own file. This screen never branches on which - it asks
   * lib/bookmark.js where to go and hands the answer to the player.
   */
  import { onMount } from 'svelte';
  import { ChevronLeft, Play, Gauge, Moon, Check, Trash2 } from 'lucide-svelte';
  import Artwork from '../components/Artwork.svelte';
  import Spinner from '../components/Spinner.svelte';
  import * as api from '../lib/api.js';
  import {
    player as playerState,
    progress,
    sleepRemaining,
    setBookPrefs,
    play,
    seek,
    toggle,
    dropBook,
  } from '../lib/player.js';
  import { fmtTime } from '../lib/format.js';
  import { toast } from '../lib/toast.js';
  import { noteLibraryChanged } from '../lib/library-events.js';
  import { registerBook, resumeAdvice, chapterIndexAt, ago } from '../lib/bookmark.js';

  export let entry;
  export let onBack = () => {};

  let book = null;
  let error = '';
  let saving = false;
  let starting = false;

  // Narrator speeds. Not a free slider: the useful range for speech is narrow and
  // a slider invites 1.37x, which nobody wants and which is harder to hit on a
  // phone than a preset.
  const SPEEDS = [1, 1.25, 1.5, 1.75, 2];
  const SLEEPS = [0, 15, 30, 45, 60];

  async function load() {
    book = await api.book(entry.id);
    // Tell the bookmarker which tracks belong to this book, and the player how
    // this book should sound. Both are per-book and neither can be guessed.
    registerBook(book);
    setBookPrefs(book.id, { speed: book.speed, sleepMinutes: book.sleepMinutes });
  }

  onMount(async () => {
    try {
      await load();
    } catch (err) {
      error = err?.message || 'Could not load this book';
    }
  });

  $: chapters = book?.chapters || [];

  /**
   * Where the big button goes, decided in lib/bookmark.js rather than here.
   *
   * The rules are fiddly - stale books, finished chapters, a position that lands
   * in no chapter at all - and each fails silently in a way you only discover
   * once you have lost your place. They are pure functions with tests; this
   * screen renders the answer.
   *
   * `nowMs` is captured at load so a label cannot change under the user's finger,
   * and re-stamped after a tap so a book you just resumed stops describing itself
   * as nine days stale.
   */
  let nowMs = Date.now();
  $: advice = book ? resumeAdvice(book, nowMs) : { mode: 'start', index: 0 };
  $: currentIndex = advice.index || 0;

  // What is playing RIGHT NOW, if it is this book, so the highlight follows the
  // audio instead of freezing wherever the last tap was. Matters most for a
  // single-file book, where crossing into the next chapter is not a track change
  // and nothing else would ever move the highlight.
  $: liveIndex =
    $playerState.track && chapters.length
      ? chapterIndexAt(chapters, $playerState.track.id, Math.floor($progress.time || 0))
      : -1;
  $: highlight = liveIndex >= 0 ? liveIndex : currentIndex;

  function human(sec) {
    if (!sec) return '';
    const h = Math.floor(sec / 3600);
    const m = Math.round((sec % 3600) / 60);
    return h ? `${h}h ${m}m` : `${m}m`;
  }

  $: remaining = book ? Math.max(book.durationSec - book.listenedSec, 0) : 0;
  $: pct = book?.durationSec ? Math.min(100, (book.listenedSec / book.durationSec) * 100) : 0;

  async function setSetting(patch) {
    if (!book || saving) return;
    saving = true;
    const before = { speed: book.speed, sleepMinutes: book.sleepMinutes };
    book = { ...book, ...patch }; // optimistic: a tap should answer immediately
    // Apply to the audio before the round trip. Waiting would leave a second of
    // the old speed after a tap, which reads as the control not working.
    setBookPrefs(book.id, { speed: book.speed, sleepMinutes: book.sleepMinutes });
    try {
      await api.saveBookSettings(book.id, patch);
    } catch {
      book = { ...book, ...before };
      setBookPrefs(book.id, before);
      toast('Could not save that setting', 'error');
    } finally {
      saving = false;
    }
  }

  /**
   * Start playing at a point in the book.
   *
   * Seeks rather than reloads when the right file is already playing, which for a
   * single-file book is EVERY chapter change. Replacing the source to move
   * between chapters would re-buffer a fourteen-hour file each time.
   */
  async function playAt(trackId, positionSec) {
    if (!trackId || starting) return;
    starting = true;
    try {
      if ($playerState.track?.id === trackId) {
        seek(positionSec);
        if (!$playerState.playing) toggle();
      } else {
        const track = await api.song(trackId);
        play([track], 0);
        // The player holds a pending seek until metadata lands, so this is safe
        // to issue immediately after play().
        seek(positionSec);
      }
      // Written now rather than waiting for the first ten-second tick: if the app
      // is closed inside those ten seconds, this is the only write that happened.
      await api.saveBookProgress(book.id, { trackId, positionSec });
      await load();
      nowMs = Date.now();
    } catch (err) {
      toast(err?.message || 'Could not start playback', 'error');
    } finally {
      starting = false;
    }
  }

  const resume = () => playAt(advice.trackId, advice.positionSec || 0);
  /** The escape hatch on a stale book: the exact spot is offered, never lost. */
  const resumeExact = () => playAt(advice.trackId, advice.exactSec || 0);
  const playChapter = (ch) => playAt(ch.trackId, ch.startSec || 0);

  const clock = (sec) => fmtTime(Math.floor(sec || 0));

  /**
   * Two-step delete, matching how a song is removed elsewhere in the app.
   *
   * A tap arms it and a second tap does it. No modal: on a phone a dialog over a
   * list is easy to dismiss by accident in either direction, and the button
   * changing colour and wording in place is harder to fire without reading.
   *
   * The warning has to say it removes the book for EVERYONE. There is one shared
   * music root and no per-user copy, so this is not "remove from my shelf" - and
   * someone deleting a book they finished would not expect to take it off
   * another person's shelf mid-chapter.
   */
  let confirmingDelete = false;
  let deleting = false;

  async function remove() {
    if (!confirmingDelete) {
      confirmingDelete = true;
      return;
    }
    deleting = true;
    try {
      await api.deleteBook(book.id);
      // If this book is what is playing, its files are gone - stop, do not play
      // on from the buffer. A no-op when a different book or a song is playing.
      dropBook(book.id);
      toast(`Deleted ${book.title}`);
      // Every other screen is hidden rather than unmounted, so without this the
      // shelf keeps offering a book whose files are gone until the app restarts.
      noteLibraryChanged();
      onBack();
    } catch (err) {
      toast(err?.message || 'Could not delete this book', 'error');
      confirmingDelete = false;
    } finally {
      deleting = false;
    }
  }

  const LABEL = {
    start: () => 'Start listening',
    resume: () => `Resume chapter ${currentIndex + 1} · ${clock(advice.positionSec)}`,
    restart: () => `Restart chapter ${currentIndex + 1}`,
    next: () => `Start chapter ${currentIndex + 1}`,
    done: () => 'Start over',
  };
</script>

<div class="absolute inset-0 z-30 overflow-y-auto overscroll-contain bg-slate-950 pt-safe">
  <div class="p-4 space-y-5">
    <button
      on:click={onBack}
      class="h-10 -ml-2 px-2 flex items-center gap-1 text-sm text-slate-400 active:bg-slate-900 rounded-lg"
    >
      <ChevronLeft class="w-4 h-4" /> Library
    </button>

    {#if error}
      <p class="text-sm text-red-300">{error}</p>
    {:else if !book}
      <div class="py-16 flex justify-center"><Spinner /></div>
    {:else}
      <div class="flex gap-4">
        <Artwork src={book.coverUrl} alt="" class="w-28 h-28 rounded-xl shrink-0" />
        <div class="min-w-0 flex-1">
          <h1 class="text-lg font-semibold leading-tight break-words">{book.title}</h1>
          <p class="text-sm text-slate-400 truncate">{book.author}</p>
          <p class="text-[11px] font-medium text-sky-300/90 mt-1">
            {book.chapterCount} chapters · {human(book.durationSec)}
          </p>
        </div>
      </div>

      <!-- Progress is the headline, not a detail. It is the reason this screen
           exists rather than reusing the album view. -->
      <div class="space-y-1.5">
        <div class="h-1.5 rounded-full bg-slate-900 overflow-hidden">
          <div class="h-full bg-sky-500 rounded-full" style="width: {pct}%"></div>
        </div>
        <p class="text-[11px] text-slate-500">
          {#if book.progress}
            {Math.round(pct)}% · {human(remaining)} left
          {:else}
            Not started · {human(book.durationSec)}
          {/if}
        </p>
      </div>

      <div class="space-y-2">
        <button
          on:click={resume}
          disabled={starting}
          class="w-full h-12 rounded-xl bg-sky-500 text-white text-sm font-semibold
                 flex items-center justify-center gap-2 active:scale-[0.98] transition-transform
                 disabled:opacity-70"
        >
          <Play class="w-4 h-4 fill-current" />
          {LABEL[advice.mode]()}
        </button>

        <!--
          The button alone cannot explain itself. After a week away, "Restart
          chapter 7" looks like the app forgot where you were - so it says why,
          and puts the exact spot one tap away instead of quietly dropping it.
        -->
        {#if advice.mode === 'restart'}
          <p class="text-[11px] text-slate-500 text-center">
            You last listened {ago(advice.ageMs)}, so the chapter start is probably
            where you want to be.
          </p>
          <button
            on:click={resumeExact}
            disabled={starting}
            class="w-full h-10 rounded-xl border border-slate-800 text-xs font-medium
                   text-slate-300 active:bg-slate-900 disabled:opacity-70"
          >
            Or pick up exactly where I stopped, at {clock(advice.exactSec)}
          </button>
        {:else if advice.mode === 'resume' && advice.rewoundSec}
          <p class="text-[11px] text-slate-500 text-center">
            Rewound {advice.rewoundSec}s, since you stopped {ago(advice.ageMs)}.
          </p>
        {:else if advice.mode === 'next'}
          <p class="text-[11px] text-slate-500 text-center">You finished chapter {currentIndex}.</p>
        {:else if advice.mode === 'done'}
          <p class="text-[11px] text-slate-500 text-center">You reached the end of this book.</p>
        {/if}
      </div>

      <div class="grid grid-cols-2 gap-3">
        <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-3">
          <p class="flex items-center gap-1.5 text-[11px] text-slate-400 mb-2">
            <Gauge class="w-3.5 h-3.5" /> Speed
          </p>
          <div class="flex flex-wrap gap-1">
            {#each SPEEDS as s (s)}
              <button
                on:click={() => setSetting({ speed: s })}
                class="px-2 h-7 rounded-md text-[11px] font-medium {book.speed === s
                  ? 'bg-slate-100 text-slate-900'
                  : 'text-slate-400 active:bg-slate-800'}"
              >
                {s}x
              </button>
            {/each}
          </div>
        </div>

        <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-3">
          <p class="flex items-center gap-1.5 text-[11px] text-slate-400 mb-2">
            <Moon class="w-3.5 h-3.5" /> Sleep
          </p>
          <div class="flex flex-wrap gap-1">
            {#each SLEEPS as s (s)}
              <button
                on:click={() => setSetting({ sleepMinutes: s })}
                class="px-2 h-7 rounded-md text-[11px] font-medium {book.sleepMinutes === s
                  ? 'bg-slate-100 text-slate-900'
                  : 'text-slate-400 active:bg-slate-800'}"
              >
                {s === 0 ? 'Off' : `${s}m`}
              </button>
            {/each}
          </div>
          <!-- A timer you cannot see is a timer you do not trust. -->
          {#if $sleepRemaining > 0}
            <p class="text-[11px] text-sky-300/90 mt-1.5 tabular-nums">
              {Math.ceil($sleepRemaining / 60000)} min left
            </p>
          {/if}
        </div>
      </div>

      <button
        on:click={remove}
        disabled={deleting}
        class="w-full h-11 rounded-xl flex items-center justify-center gap-2 text-sm font-medium
               border transition-colors disabled:opacity-60
               {confirmingDelete
                 ? 'bg-red-600 border-red-500 text-white'
                 : 'border-red-900/70 text-red-300 active:bg-red-950/40'}"
      >
        <Trash2 class="w-4 h-4" />
        {#if deleting}
          Deleting…
        {:else if confirmingDelete}
          Delete permanently — tap again
        {:else}
          Delete this book
        {/if}
      </button>
      {#if confirmingDelete && !deleting}
        <p class="text-[11px] text-center text-slate-500 -mt-3">
          This removes the files for everyone, not just you. Your place is kept, so
          downloading it again puts you back where you were.
        </p>
      {/if}

      <section>
        <h2 class="text-sm font-semibold mb-2">Chapters</h2>
        <ul class="space-y-0.5">
          {#each chapters as ch, i (ch.trackId + ':' + ch.startSec)}
            <li>
              <button
                on:click={() => playChapter(ch)}
                disabled={starting}
                class="w-full flex items-center gap-3 rounded-lg px-2 py-2 text-left
                       {i === highlight ? 'bg-sky-950/40' : 'active:bg-slate-900'}"
              >
                <span class="w-6 shrink-0 text-right text-[11px] tabular-nums text-slate-500">
                  {i + 1}
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block text-sm truncate {i === highlight ? 'text-sky-300' : ''}">
                    {ch.title}
                  </span>
                  <span class="block text-[11px] text-slate-500">
                    {clock(ch.endSec - ch.startSec)}
                  </span>
                </span>
                {#if i < highlight}
                  <Check class="w-4 h-4 text-slate-600 shrink-0" />
                {/if}
              </button>
            </li>
          {/each}
        </ul>
      </section>
      <div class="h-24"></div>
    {/if}
  </div>
</div>
