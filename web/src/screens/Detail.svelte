<script>
  import { onMount, onDestroy } from 'svelte';
  import { ChevronLeft, Play, Shuffle, Download, Check, Plus, Loader2, AlertTriangle } from 'lucide-svelte';
  import * as saved from '../lib/saved.js';
  import { toast } from '../lib/toast.js';
  import AlbumCard from '../components/AlbumCard.svelte';
  import Artwork from '../components/Artwork.svelte';
  import TrackList from '../components/TrackList.svelte';
  import Spinner from '../components/Spinner.svelte';
  import CompleteAlbumSheet from '../components/CompleteAlbumSheet.svelte';
  import * as api from '../lib/api.js';
  import { fmtTime, plural } from '../lib/format.js';
  import { play, shufflePlay } from '../lib/player.js';

  export let entry;
  export let onBack = () => {};

  let data = null;
  let loading = true;
  let error = null;

  onMount(async () => {
    try {
      if (entry.type === 'album') data = await api.album(entry.id);
      else if (entry.type === 'artist') data = await api.artist(entry.id);
      else data = await api.playlist(entry.id);
    } catch (err) {
      error = err.message || 'Could not open that';
    } finally {
      loading = false;
    }
  });

  $: head = data?.album || data?.artist || data?.playlist || null;
  $: tracks = data?.tracks || [];

  const { downloads: savedTracks } = saved;
  let savingAll = false;
  let savedSoFar = 0;
  // Recomputed whenever either the list or the saved index changes.
  $: allSaved = tracks.length > 0 && tracks.every((x) => $savedTracks.some((d) => d.id === x.id));

  async function saveAll() {
    if (savingAll || !tracks.length) return;
    savingAll = true;
    savedSoFar = 0;
    try {
      const { total, failed } = await saved.downloadMany(tracks, (done) => {
        savedSoFar = done;
      });
      if (!total) toast('Already on this device');
      else if (failed) toast(`Saved ${total - failed} of ${total}`);
      else toast(`${total} songs available offline`);
    } finally {
      savingAll = false;
    }
  }

  async function removeAllSaved() {
    for (const x of tracks) {
      // eslint-disable-next-line no-await-in-loop
      await saved.remove(x.id);
    }
    toast('Removed from this device');
  }
  $: totalSeconds = tracks.reduce((n, t) => n + (t.duration || 0), 0);

  // "Complete album": ask the server which online album matches this one and
  // what it is missing. Opens a confirm sheet; never auto-downloads.
  let completing = false;
  let completion = null;
  async function findMissing() {
    if (completing) return;
    completing = true;
    try {
      completion = await api.albumMissing(entry.id);
    } catch (e) {
      toast(e.message || 'Could not check the album');
    } finally {
      completing = false;
    }
  }

  // Live progress for "Complete album": the sheet hands back a job per queued
  // track. Poll them and, as each finishes, refetch the album so the new track
  // becomes playable in place - no manual exit-and-refresh.
  let pending = [];
  let pollTimer = null;
  let pollCount = 0;

  function stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  function hasTrack(title) {
    const t = (title || '').toLowerCase();
    return (data?.tracks || []).some((x) => (x.title || '').toLowerCase() === t);
  }

  function handleQueued(queued) {
    pending = queued.map((q) => ({ ...q, state: 'queued' }));
    pollCount = 0;
    if (pending.length && !pollTimer) pollTimer = setInterval(poll, 2000);
  }

  async function poll() {
    pollCount += 1;
    let jobs;
    try {
      jobs = await api.jobs();
    } catch {
      return;
    }
    const byId = new Map(jobs.map((j) => [j.id, j]));
    pending = pending.map((p) => ({ ...p, state: byId.get(p.jobId)?.state || p.state }));
    // A finished download is on disk and scanned; refetch to pull it in.
    if (pending.some((p) => p.state === 'done')) {
      try {
        data = await api.album(entry.id);
      } catch {
        /* keep the current view */
      }
    }
    pending = pending.filter((p) => !hasTrack(p.title));
    const failedOnly = pending.length > 0 && pending.every((p) => p.state === 'failed');
    if (!pending.length || failedOnly || pollCount >= 120) stopPolling();
  }

  onDestroy(stopPolling);
</script>

<div class="absolute inset-0 z-30 bg-slate-950 overflow-y-auto overscroll-contain">
  <div class="sticky top-0 z-10 flex items-center gap-1 bg-slate-950/95 backdrop-blur px-2 h-14">
    <button
      on:click={onBack}
      aria-label="Back"
      class="w-11 h-11 shrink-0 flex items-center justify-center text-slate-300"
    >
      <ChevronLeft class="w-6 h-6" />
    </button>
    <p class="text-sm font-medium truncate">{head?.name || entry.title || ''}</p>
  </div>

  {#if loading}
    <div class="py-20"><Spinner label="Loading" /></div>
  {:else if error}
    <p class="px-4 py-16 text-center text-xs text-red-300">{error}</p>
  {:else if head}
    <div class="px-4 pb-8 space-y-5">
      <div class="flex gap-4">
        <Artwork
          src={head.coverUrl}
          alt=""
          class="w-28 h-28 shrink-0 {entry.type === 'artist'
            ? 'rounded-full'
            : 'rounded-xl'} border border-slate-800/60"
        />
        <div class="min-w-0 flex-1 self-end space-y-1">
          <h1 class="text-lg font-semibold leading-tight break-words">{head.name}</h1>
          {#if entry.type === 'album'}
            <p class="text-xs text-slate-400 truncate">{head.artist}</p>
            <p class="text-[11px] text-slate-500">
              {head.year ? `${head.year} · ` : ''}{plural(tracks.length, 'track', 'tracks')} · {fmtTime(
                totalSeconds,
              )}
            </p>
          {:else if entry.type === 'artist'}
            <p class="text-[11px] text-slate-500">
              {plural(data.albums?.length || 0, 'album', 'albums')}
            </p>
          {:else}
            <p class="text-[11px] text-slate-500">
              {plural(tracks.length, 'track', 'tracks')} · {fmtTime(totalSeconds)}{head.owner
                ? ` · ${head.owner}`
                : ''}
            </p>
          {/if}
        </div>
      </div>

      {#if tracks.length}
        <div class="flex gap-3">
          <button
            on:click={() => play(tracks, 0, { shuffle: false })}
            class="flex-1 h-12 rounded-xl bg-sky-500 text-white text-sm font-semibold flex items-center justify-center gap-2 active:scale-[0.98] transition-transform"
          >
            <Play class="w-4 h-4 fill-current" /> Play
          </button>
          <button
            on:click={() => shufflePlay(tracks)}
            class="flex-1 h-12 rounded-xl border border-slate-700 text-sm font-medium flex items-center justify-center gap-2 active:bg-slate-900"
          >
            <Shuffle class="w-4 h-4" /> Shuffle
          </button>
          <button
            on:click={allSaved ? removeAllSaved : saveAll}
            disabled={savingAll}
            aria-label={allSaved ? 'Remove from this device' : 'Save for offline'}
            class="w-12 h-12 shrink-0 rounded-xl border border-slate-700 flex items-center justify-center active:bg-slate-900 disabled:opacity-60"
          >
            {#if savingAll}
              <span class="text-[10px] font-semibold tabular-nums">{savedSoFar}/{tracks.length}</span>
            {:else if allSaved}
              <Check class="w-4 h-4 text-emerald-400" />
            {:else}
              <Download class="w-4 h-4" />
            {/if}
          </button>
        </div>

        {#if entry.type === 'album'}
          <button
            on:click={findMissing}
            disabled={completing}
            class="w-full h-10 rounded-xl border border-slate-800 flex items-center justify-center gap-2
                   text-xs font-medium text-slate-300 active:bg-slate-900 disabled:opacity-60"
          >
            {#if completing}
              <Loader2 class="w-4 h-4 animate-spin" /> Checking…
            {:else}
              <Plus class="w-4 h-4" /> Complete album
            {/if}
          </button>
        {/if}

        <TrackList {tracks} numbered={entry.type === 'album'} showArt={entry.type !== 'album'} />

        {#if pending.length}
          <div class="space-y-1">
            <p class="px-1 text-[11px] font-medium text-slate-500">
              Downloading {pending.filter((p) => p.state !== 'failed').length}…
            </p>
            {#each pending as p (p.jobId)}
              <div class="flex items-center gap-3 rounded-lg px-1 py-1.5">
                <span class="w-6 shrink-0 flex justify-center">
                  {#if p.state === 'failed'}
                    <AlertTriangle class="w-4 h-4 text-red-400" />
                  {:else}
                    <Loader2 class="w-4 h-4 animate-spin text-sky-400" />
                  {/if}
                </span>
                <p class="flex-1 truncate text-sm text-slate-300">{p.title}</p>
              </div>
            {/each}
          </div>
        {/if}
      {/if}

      {#if entry.type === 'artist'}
        {#if data.albums?.length}
          <div class="grid grid-cols-2 sm:grid-cols-3 gap-3">
            {#each data.albums as album (album.id)}
              <AlbumCard {album} width="w-full" />
            {/each}
          </div>
        {:else}
          <p class="py-12 text-center text-xs text-slate-500">No albums for this artist.</p>
        {/if}
      {:else if !tracks.length}
        <p class="py-12 text-center text-xs text-slate-500">This one is empty.</p>
      {/if}
    </div>
  {/if}
  {#if completion}
    <CompleteAlbumSheet
      {completion}
      albumName={head?.name || ''}
      albumTitle={head?.name || ''}
      albumArtist={head?.artist || ''}
      onQueued={handleQueued}
      onClose={() => (completion = null)}
    />
  {/if}
</div>
