<script>
  /**
   * The track list of an album found online, reachable by tapping its card.
   *
   * Exists because "download the whole album" is often not what someone wants -
   * a record has two songs you care about and eleven you do not, and without
   * this the only choices were all of it or none of it.
   *
   * No release date is shown, deliberately. The only date YouTube Music returns
   * for an album is `modified_date`, which is when the playlist was last touched
   * - 2026 for a 2013 record. A wrong year presented as fact is worse than an
   * absent one, so views and runtime are shown instead; both are real.
   */
  import { Download, Loader2, Check, X, AlertTriangle, ChevronRight } from 'lucide-svelte';
  import Artwork from './Artwork.svelte';
  import { fmtTime } from '../lib/format.js';
  import { isAlbumDone } from '../lib/albumState.js';

  export let album = null;
  /** sourceId -> true only while the POST is in flight. */
  export let queuingTrack = {};
  /**
   * sourceId -> the live job for that track.
   *
   * This used to be a "queued" boolean, which was wrong: it was set once and
   * never updated, so a track that had finished downloading still read "queued"
   * forever. The job carries real state (queued | downloading | done | failed)
   * and is refreshed every two seconds, so the sheet now says the same thing the
   * results list above it says.
   */
  export let trackJobs = {};
  export let albumQueuing = false;
  export let onGetTrack = () => {};
  export let onGetAlbum = () => {};
  /** Called with the album when its "downloaded" state is tapped, to open it in the library. */
  export let onOpenAlbum = () => {};
  export let onClose = () => {};

  // Compact rather than exact: "19.5M plays" is the useful fact, and the precise
  // figure is noise at this size.
  function views(n) {
    if (!n) return '';
    if (n >= 1e9) return `${(n / 1e9).toFixed(1)}B plays`;
    if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M plays`;
    if (n >= 1e3) return `${Math.round(n / 1e3)}K plays`;
    return `${n} plays`;
  }

  function runtime(sec) {
    if (!sec) return '';
    const m = Math.round(sec / 60);
    return m >= 60 ? `${Math.floor(m / 60)}h ${m % 60}m` : `${m} min`;
  }

  // Shared with the card that opens this sheet, so the two cannot disagree.
  $: albumDone = isAlbumDone(album, trackJobs);

  $: meta = album
    ? [
        `${album.trackCount} ${album.trackCount === 1 ? 'track' : 'tracks'}`,
        runtime(album.durationSec),
        views(album.views),
      ].filter(Boolean)
    : [];
</script>

{#if album}
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
        <Artwork src={album.thumb} alt="" class="w-16 h-16 rounded-xl shrink-0" />
        <div class="min-w-0 flex-1">
          <p class="text-base font-semibold text-slate-100 break-words">{album.title}</p>
          <p class="text-xs text-slate-400 truncate">{album.artist || 'Unknown artist'}</p>
          <!-- sky-300, not another grey. This line is the only hard information on the
             sheet - track count, runtime, plays - and at slate-600 on slate-950 it
             was barely legible. Giving it the app's accent separates it from the
             artist line above without competing with the title. -->
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

      {#if albumDone}
        <button
          on:click={() => onOpenAlbum(album)}
          class="w-full h-12 rounded-xl border border-emerald-800/60 bg-emerald-950/20 flex items-center
                 justify-center gap-2 text-sm font-medium text-slate-100 active:bg-slate-900"
        >
          <Check class="w-4 h-4 text-emerald-400" /> Album downloaded
          <ChevronRight class="w-4 h-4 text-slate-400" />
        </button>
      {:else}
        <button
          on:click={() => onGetAlbum(album)}
          disabled={albumQueuing}
          class="w-full h-12 rounded-xl border border-slate-800 flex items-center justify-center gap-2
                 text-sm font-medium text-slate-200 active:bg-slate-900 disabled:opacity-60"
        >
          {#if albumQueuing}
            <Loader2 class="w-4 h-4 animate-spin" /> Queuing…
          {:else}
            <Download class="w-4 h-4" /> Download all {album.trackCount}
          {/if}
        </button>
      {/if}

      <ul class="space-y-1">
        {#each album.tracks || [] as tr, i (tr.sourceId)}
          {@const job = trackJobs[tr.sourceId]}
          <li class="flex items-center gap-3 rounded-lg px-1 py-1.5">
            <span class="w-6 shrink-0 text-right text-[11px] tabular-nums text-slate-500">
              {i + 1}
            </span>
            <div class="min-w-0 flex-1">
              <p class="text-sm truncate">{tr.title}</p>
              {#if tr.duration}
                <p class="text-[11px] text-slate-400">{fmtTime(tr.duration)}</p>
              {/if}
            </div>
            <button
              on:click={() => onGetTrack({ ...tr, album: album.title, albumArtist: album.artist })}
              disabled={queuingTrack[tr.sourceId] || (job && job.state !== 'failed')}
              aria-label="Download {tr.title}"
              class="w-10 h-10 shrink-0 rounded-lg border border-slate-800 flex items-center
                     justify-center active:bg-slate-900 disabled:opacity-60"
            >
              {#if job?.state === 'done'}
                <Check class="w-4 h-4 text-emerald-400" />
              {:else if job?.state === 'failed'}
                <AlertTriangle class="w-4 h-4 text-red-400" />
              {:else if job || queuingTrack[tr.sourceId]}
                <Loader2 class="w-4 h-4 animate-spin" />
              {:else}
                <Download class="w-4 h-4" />
              {/if}
            </button>
          </li>
        {/each}
      </ul>
    </div>
  </div>
{/if}
