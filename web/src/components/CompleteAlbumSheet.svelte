<script>
  /**
   * Confirm-before-download for completing a partial library album.
   *
   * The library cannot know an album is incomplete on its own - a single
   * downloaded from an album sheet becomes a one-track album. This sheet takes
   * the `/api/acquire/album-missing` result, shows WHICH album was matched and
   * exactly which tracks are missing, and only downloads when the user taps.
   * It never auto-downloads.
   */
  import { Loader2, X, Download } from 'lucide-svelte';
  import { fmtTime } from '../lib/format.js';
  import * as api from '../lib/api.js';
  import { toast } from '../lib/toast.js';

  /** The `/api/acquire/album-missing` result: `{ found, album?, missing?, have? }`. */
  export let completion = null;
  /** The local album's display name, for the "not found" copy. */
  export let albumName = '';
  export let onClose = () => {};
  /** Called with the queued jobs `[{sourceId,title,jobId}]` so the album view can track them. */
  export let onQueued = () => {};
  /** The LOCAL album to fold the downloads into, so they share its album_id and join it
   *  in place - the online match is only used to find the tracklist, never to tag. */
  export let albumTitle = '';
  export let albumArtist = '';

  let downloading = false;

  $: album = completion?.album || null;
  $: missing = completion?.missing || [];

  async function downloadMissing() {
    if (downloading || !missing.length) return;
    downloading = true;
    try {
      // One job per missing track, tagged into the album so it groups; the
      // backend dedup skips any copy already on the server. Collect the jobs so
      // the album view can show them downloading and become playable in place.
      const results = await Promise.allSettled(
        missing.map(async (t) => {
          const job = await api.acquire({
            sourceId: t.sourceId,
            title: t.title,
            artist: t.artist || album.artist,
            album: albumTitle || album.title,
            albumArtist: albumArtist || album.artist,
          });
          return { sourceId: t.sourceId, title: t.title, jobId: job.id };
        }),
      );
      const queued = results.filter((r) => r.status === 'fulfilled').map((r) => r.value);
      const failed = results.length - queued.length;
      if (queued.length) onQueued(queued);
      toast(
        failed
          ? `Downloading ${queued.length}, ${failed} couldn't start`
          : `Downloading ${queued.length} missing ${queued.length === 1 ? 'track' : 'tracks'}`,
      );
      onClose();
    } catch (e) {
      toast(e.message || 'Could not start the downloads');
    } finally {
      downloading = false;
    }
  }
</script>

{#if completion}
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
        <div class="min-w-0 flex-1">
          <p class="text-base font-semibold text-slate-100">Complete this album</p>
          {#if completion.found && album}
            <p class="text-xs text-slate-400 truncate">
              Matched <span class="text-slate-200">{album.title}</span>{album.artist
                ? ` · ${album.artist}`
                : ''}
            </p>
          {/if}
        </div>
        <button
          on:click={onClose}
          aria-label="Close"
          class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg text-slate-500 active:bg-slate-900"
        >
          <X class="w-4 h-4" />
        </button>
      </div>

      {#if !completion.found}
        <p class="py-8 text-center text-sm text-slate-400">
          Couldn't find the full album online to complete “{albumName}”.
        </p>
      {:else if !missing.length}
        <p class="py-8 text-center text-sm text-slate-400">This album is already complete.</p>
      {:else}
        <p class="text-xs text-slate-400">
          {missing.length}
          {missing.length === 1 ? 'track is' : 'tracks are'} missing and will be downloaded:
        </p>
        <ul class="space-y-1">
          {#each missing as tr, i (tr.sourceId)}
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
            </li>
          {/each}
        </ul>
        <button
          on:click={downloadMissing}
          disabled={downloading}
          class="w-full h-12 rounded-xl bg-sky-500 text-white text-sm font-semibold flex items-center
                 justify-center gap-2 active:scale-[0.98] transition-transform disabled:opacity-60"
        >
          {#if downloading}
            <Loader2 class="w-4 h-4 animate-spin" /> Starting…
          {:else}
            <Download class="w-4 h-4" /> Download {missing.length} missing {missing.length === 1
              ? 'track'
              : 'tracks'}
          {/if}
        </button>
      {/if}
    </div>
  </div>
{/if}
