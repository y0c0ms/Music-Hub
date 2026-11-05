<script>
  /**
   * The full metadata of an audiobook search hit, reached by tapping its row.
   *
   * Exists because a search for a popular book returns a dozen uploads whose
   * titles all begin "Pride and Prejudice by Jane Austen…" and truncate to the
   * same thing on a phone. Reported 2026-09-07: "they look like they have the
   * same title, not letting me know which one to choose." What actually tells
   * them apart is the reader/channel, the length (a 10-hour and a 17-hour reading
   * of the same book are different products), the cover, and the source. This
   * shows all of it, untruncated, before a download commits anything to disk.
   */
  import { Download, Loader2, Check, X, AlertTriangle } from 'lucide-svelte';
  import Artwork from './Artwork.svelte';
  import { fmtPlays } from '../lib/format.js';
  import * as api from '../lib/api.js';

  export let book = null;
  /** The live job for this book, or null. */
  export let job = null;
  /** True only while the POST is in flight. */
  export let queuing = false;
  export let onGet = () => {};
  export let onClose = () => {};

  // Human length, not a clock. fmtTime turns a 10-hour book into "10:18:00",
  // which reads like a timestamp; "10h 18m" reads like a duration, which is what
  // a book is measured in and the number a listener actually chooses on.
  function bookLength(sec) {
    if (!sec) return '';
    const m = Math.round(sec / 60);
    if (m < 60) return `${m} min`;
    return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`;
  }

  const SOURCE_LABEL = {
    youtube: 'YouTube',
    librivox: 'LibriVox',
    'media.ccc.de': 'media.ccc.de',
  };

  $: done = job?.state === 'done';
  $: busy = queuing || (job && job.state !== 'failed' && job.state !== 'done');
  $: meta = book
    ? [
        SOURCE_LABEL[book.source] || book.source,
        bookLength(book.durationSec),
        book.chapters ? `${book.chapters} chapters` : '',
        fmtPlays(book.views),
      ].filter(Boolean)
    : [];

  // The contents list is not in the search result - the feeds that answer a
  // search omit it - so it is fetched when a result is opened. One request per
  // book, guarded by the book's identity so reopening or a re-render does not
  // refetch.
  let chapters = [];
  let chLoading = false;
  let chError = '';
  let loadedFor = '';

  $: if (book) loadChapters(book);
  else {
    chapters = [];
    chError = '';
    loadedFor = '';
  }

  async function loadChapters(b) {
    const key = `${b.source}:${b.id}`;
    if (key === loadedFor) return;
    loadedFor = key;
    chapters = [];
    chError = '';
    chLoading = true;
    try {
      chapters = (await api.booksChapters(b)) || [];
    } catch (e) {
      chError = e?.message || 'Could not load the chapters';
    } finally {
      chLoading = false;
    }
  }
</script>

{#if book}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div
    class="fixed inset-0 z-40 bg-slate-950/70 backdrop-blur-sm flex items-end"
    on:click|self={onClose}
  >
    <div
      class="w-full max-h-[85dvh] overflow-y-auto overscroll-contain rounded-t-2xl
             border-t border-slate-800 bg-slate-950 p-4 pb-safe space-y-4"
    >
      <div class="flex items-start gap-3">
        <Artwork src={book.thumb} alt="" class="w-20 h-20 rounded-xl shrink-0" />
        <div class="min-w-0 flex-1">
          <!-- break-words, not truncate: the whole point of the sheet is to show
               the title the row could not. -->
          <p class="text-base font-semibold text-slate-100 break-words">{book.title}</p>
          <p class="text-xs text-slate-400 break-words">{book.author || 'Unknown author'}</p>
          <p class="text-[11px] font-medium text-sky-300/90 mt-1">{meta.join(' · ')}</p>
        </div>
        <button
          on:click={onClose}
          aria-label="Close"
          class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg text-slate-500 active:bg-slate-900"
        >
          <X class="w-4 h-4" />
        </button>
      </div>

      {#if book.detail && book.detail !== (SOURCE_LABEL[book.source] || book.source)}
        <p class="text-[11px] text-slate-500 break-words">{book.detail}</p>
      {/if}

      {#if job && (job.state === 'downloading' || job.state === 'queued')}
        <p class="text-[11px] text-sky-400">
          {job.detail || job.state}{job.percent ? ` · ${job.percent}%` : ''}
        </p>
      {:else if job?.state === 'failed'}
        <p class="flex items-start gap-2 text-[11px] text-red-300">
          <AlertTriangle class="w-3.5 h-3.5 mt-0.5 shrink-0" />
          <span>{job.detail || 'Download failed'}</span>
        </p>
      {/if}

      <button
        on:click={() => onGet(book)}
        disabled={busy || done}
        class="w-full h-12 rounded-xl border border-slate-800 flex items-center justify-center gap-2
               text-sm font-medium text-slate-200 active:bg-slate-900 disabled:opacity-60"
      >
        {#if done}
          <Check class="w-4 h-4 text-emerald-400" /> Added to the library
        {:else if busy}
          <Loader2 class="w-4 h-4 animate-spin" /> Downloading…
        {:else}
          <Download class="w-4 h-4" /> Download
        {/if}
      </button>

      <!-- The contents. No per-chapter download, because a book is fetched whole:
           YouTube and media.ccc.de land as one file with marks, and even a
           multi-file LibriVox book is one thing you are partway through. This is
           the album sheet's list minus the per-row button, which had no meaning
           here. -->
      {#if chLoading}
        <div class="flex items-center gap-2 pt-1 text-[11px] text-slate-500">
          <Loader2 class="w-3.5 h-3.5 animate-spin" /> Loading chapters…
        </div>
      {:else if chError}
        <p class="pt-1 text-[11px] text-slate-500">
          Chapters unavailable — the whole book still downloads.
        </p>
      {:else if chapters.length}
        <div class="pt-1">
          <p class="text-[11px] uppercase tracking-wider text-slate-500 mb-1.5">
            {chapters.length === 1 ? '1 chapter' : `${chapters.length} chapters`}
          </p>
          <ul class="divide-y divide-slate-900">
            {#each chapters as c, i (i)}
              <li class="flex items-center gap-3 py-1.5">
                <span class="w-6 shrink-0 text-right text-[11px] tabular-nums text-slate-500">
                  {i + 1}
                </span>
                <span class="min-w-0 flex-1 text-sm truncate">{c.title}</span>
                {#if c.endSec > c.startSec}
                  <span class="shrink-0 text-[11px] tabular-nums text-slate-500">
                    {bookLength(c.endSec - c.startSec)}
                  </span>
                {/if}
              </li>
            {/each}
          </ul>
        </div>
      {/if}
    </div>
  </div>
{/if}
