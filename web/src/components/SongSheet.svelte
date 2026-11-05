<script>
  /**
   * Song info, reachable by holding any track row.
   *
   * Delete lives here rather than behind a swipe because it is irreversible and
   * this library is full of near-duplicate downloads - the user needs to read
   * the artist, the duration and the file name before removing anything. The
   * two-step confirm exists for the same reason.
   *
   * The server decides what may be deleted; this only asks. A track outside the
   * user's own folders comes back refused, which is the intended answer and is
   * shown as such rather than dressed up as an error.
   */
  import { createEventDispatcher } from 'svelte';
  import { Trash2, ListPlus, Star, X, ListEnd, Radio, Download, Check } from 'lucide-svelte';
  import * as saved from '../lib/saved.js';
  import Artwork from './Artwork.svelte';
  import Spinner from './Spinner.svelte';
  import { fmtTime } from '../lib/format.js';
  import { drop, enqueue, play } from '../lib/player.js';
  import PlaylistPicker from './PlaylistPicker.svelte';
  import * as api from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { noteLibraryChanged } from '../lib/library-events.js';

  export let track = null;

  const { downloads: savedTracks, downloading } = saved;
  // Derived from the store rather than saved.has(), so the icon flips the moment
  // a download finishes - has() is a plain function and Svelte cannot know when
  // its answer changed.
  $: isSaved = !!track && $savedTracks.some((d) => d.id === track.id);
  $: savingThis = !!track && !!$downloading[track.id];

  async function toggleSaved() {
    if (!track) return;
    if (isSaved) {
      await saved.remove(track.id);
      toast('Removed from this device');
      return;
    }
    const ok = await saved.download(track);
    toast(ok ? 'Available offline' : 'Could not save');
  }

  const dispatch = createEventDispatcher();

  let confirming = false;
  let busy = false;
  let findingSimilar = false;
  // The track whose playlist sheet is open, layered above this one so
  // dismissing it returns here rather than losing the song entirely.
  let picking = null;

  $: if (track) confirming = false;

  function close() {
    dispatch('close');
  }

  /**
   * Queue tracks like this one, from what is already on disk.
   *
   * The seed plays first so the transition is continuous rather than a jump to
   * something unfamiliar. A thin answer is reported honestly instead of silently
   * playing the seed alone: on a singles-heavy library some tracks genuinely have
   * no neighbours, and pretending otherwise looks like a broken button.
   */
  async function playSimilar() {
    findingSimilar = true;
    try {
      const { seed, songs } = await api.similar(track.id);
      if (!songs?.length) {
        toast(`Nothing similar to "${track.title}" is on disk yet`);
        return;
      }
      play([seed || track, ...songs], 0);
      toast(`Playing ${songs.length} like "${track.title}"`);
      close();
    } catch (err) {
      toast(err.message || 'Could not find similar tracks', 'error');
    } finally {
      findingSimilar = false;
    }
  }

  async function remove() {
    if (!confirming) {
      confirming = true;
      return;
    }
    busy = true;
    try {
      await api.deleteSong(track.id);
      // The file is gone from disk. Without this the audio element plays on from
      // its buffer and the queue keeps offering a track that can only 404.
      drop(track.id);
      toast(`Deleted ${track.title}`);
      // The caller updates its own list; every OTHER mounted screen still holds
      // the deleted track and would keep offering it until the app restarts,
      // because tabs are hidden rather than unmounted.
      noteLibraryChanged();
      dispatch('deleted', track);
      close();
    } catch (err) {
      toast(err.message || 'Could not delete that track', 'error');
      confirming = false;
    } finally {
      busy = false;
    }
  }

  async function star() {
    const want = !track.starred;
    try {
      await (want ? api.star(track.id) : api.unstar(track.id));
      track = { ...track, starred: want };
      dispatch('changed', track);
    } catch (err) {
      toast(err.message || 'Could not update favourite', 'error');
    }
  }
</script>

{#if track}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div
    class="fixed inset-0 z-40 bg-slate-950/70 backdrop-blur-sm flex items-end"
    on:click|self={close}
  >
    <div class="w-full rounded-t-2xl border-t border-slate-800 bg-slate-950 p-4 pb-safe space-y-4">
      <div class="flex items-start gap-3">
        <Artwork src={track.coverUrl} alt="" class="w-16 h-16 rounded-xl shrink-0" />
        <div class="min-w-0 flex-1">
          <p class="text-base font-semibold text-slate-100 break-words">{track.title}</p>
          <p class="text-xs text-slate-400 truncate">{track.artist || 'Unknown artist'}</p>
          <p class="text-[11px] text-slate-600 mt-0.5">
            {fmtTime(track.duration)}{track.year ? ` · ${track.year}` : ''}
          </p>
        </div>
        <button
          on:click={close}
          aria-label="Close"
          class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg text-slate-500 active:bg-slate-900"
        >
          <X class="w-4 h-4" />
        </button>
      </div>

      <div class="grid grid-cols-4 gap-2">
        <button
          on:click={() => (picking = track)}
          class="h-12 rounded-xl border border-slate-800 flex flex-col items-center justify-center gap-1 text-[11px] text-slate-300 active:bg-slate-900"
        >
          <ListPlus class="w-4 h-4" /> Playlist
        </button>
        <button
          on:click={() => { enqueue([track]); toast('Added to queue'); close(); }}
          class="h-12 rounded-xl border border-slate-800 flex flex-col items-center justify-center gap-1 text-[11px] text-slate-300 active:bg-slate-900"
        >
          <ListEnd class="w-4 h-4" /> Queue
        </button>
        <button
          on:click={star}
          class="h-12 rounded-xl border border-slate-800 flex flex-col items-center justify-center gap-1 text-[11px] text-slate-300 active:bg-slate-900"
        >
          <Star class="w-4 h-4 {track.starred ? 'text-amber-400 fill-amber-400' : ''}" />
          {track.starred ? 'Starred' : 'Star'}
        </button>
        <button
          on:click={toggleSaved}
          disabled={savingThis}
          class="h-12 rounded-xl border border-slate-800 flex flex-col items-center justify-center gap-1 text-[11px] text-slate-300 active:bg-slate-900 disabled:opacity-60"
        >
          {#if isSaved}
            <Check class="w-4 h-4 text-emerald-400" /> Saved
          {:else}
            <Download class="w-4 h-4 {savingThis ? 'animate-pulse' : ''}" />
            {savingThis ? 'Saving' : 'Offline'}
          {/if}
        </button>
      </div>

      <!-- Full width, below the three-up row: this starts playback and replaces
           the queue, which is a bigger action than adding a star. -->
      <button
        on:click={playSimilar}
        disabled={findingSimilar}
        class="w-full h-12 rounded-xl border border-slate-800 flex items-center justify-center gap-2
               text-sm font-medium text-slate-200 active:bg-slate-900 disabled:opacity-60"
      >
        {#if findingSimilar}
          <Spinner /> Finding similar…
        {:else}
          <Radio class="w-4 h-4" /> Play similar
        {/if}
      </button>

      <button
        on:click={remove}
        disabled={busy}
        class="w-full h-12 rounded-xl flex items-center justify-center gap-2 text-sm font-medium
               border transition-colors disabled:opacity-60
               {confirming
                 ? 'bg-red-600 border-red-500 text-white'
                 : 'border-red-900/70 text-red-300 active:bg-red-950/40'}"
      >
        <Trash2 class="w-4 h-4" />
        {#if busy}
          Deleting…
        {:else if confirming}
          Delete permanently — tap again
        {:else}
          Delete from disk
        {/if}
      </button>

      {#if confirming && !busy}
        <p class="text-[11px] text-center text-slate-500 -mt-2">
          This removes the file. It is not in a bin anywhere.
        </p>
      {/if}
    </div>
  </div>
{/if}

<PlaylistPicker track={picking} on:close={() => (picking = null)} />
