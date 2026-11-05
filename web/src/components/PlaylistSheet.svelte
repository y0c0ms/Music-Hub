<script>
  /**
   * Rename or delete one playlist, reached by holding its row.
   *
   * Deleting a playlist is not deleting music - Subsonic unlinks the grouping
   * and every file stays where it is. The copy says so out loud, because
   * "delete" everywhere else in this app does remove a file from disk, and that
   * is not a distinction to leave the user guessing about.
   */
  import { createEventDispatcher } from 'svelte';
  import { Trash2, Pencil, X, Check, Download, CloudOff } from 'lucide-svelte';
  import * as saved from '../lib/saved.js';
  import * as api from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import Spinner from './Spinner.svelte';

  export let playlist = null;


  // Saving a playlist offline needs its tracks, and this sheet only carries the

  // playlist row. Fetch on demand rather than loading tracks for a sheet that is

  // usually opened to rename or delete.

  let savingOffline = false;

  let savedSoFar = 0;

  let ofTotal = 0;


  async function saveOffline() {

    if (savingOffline || !playlist) return;

    savingOffline = true;

    savedSoFar = 0;

    ofTotal = playlist.songCount || 0;

    try {

      const data = await api.playlist(playlist.id);

      const tracks = data?.tracks || [];

      ofTotal = tracks.length;

      const res = await saved.downloadMany(tracks, (done) => {

        savedSoFar = done;

      });

      if (!res.total) toast('Already on this device');

      else if (res.failed) toast(`Saved ${res.total - res.failed} of ${res.total}`);

      else toast(`${res.total} songs available offline`);

    } catch {

      toast('Could not save this playlist');

    } finally {

      savingOffline = false;

    }

  }

  const dispatch = createEventDispatcher();

  let name = '';
  let editing = false;
  let confirming = false;
  let busy = false;
  let loaded = null;

  $: if (playlist && loaded !== playlist.id) {
    loaded = playlist.id;
    name = playlist.name;
    editing = false;
    confirming = false;
  }

  function close() {
    dispatch('close');
  }

  async function rename() {
    const next = name.trim();
    if (!next || next === playlist.name) {
      editing = false;
      return;
    }
    busy = true;
    try {
      await api.renamePlaylist(playlist.id, next);
      dispatch('renamed', { ...playlist, name: next });
      toast('Renamed', 'success');
      close();
    } catch (err) {
      toast(err.message || 'Could not rename it', 'error');
    } finally {
      busy = false;
    }
  }

  async function remove() {
    // Two taps, like the track sheet: this is the only irreversible thing here.
    if (!confirming) {
      confirming = true;
      return;
    }
    busy = true;
    try {
      await api.deletePlaylist(playlist.id);
      dispatch('deleted', playlist);
      toast(`Deleted “${playlist.name}”`, 'success');
      close();
    } catch (err) {
      toast(err.message || 'Could not delete it', 'error');
    } finally {
      busy = false;
    }
  }
</script>

{#if playlist}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div
    class="fixed inset-0 z-50 bg-slate-950/70 backdrop-blur-sm flex items-end"
    on:click|self={close}
  >
    <div class="w-full rounded-t-2xl border-t border-slate-800 bg-slate-950 p-4 pb-safe space-y-4">
      <div class="flex items-start gap-3">
        <div class="min-w-0 flex-1">
          <p class="text-base font-semibold text-slate-100 break-words">{playlist.name}</p>
          <p class="text-xs text-slate-400">
            {playlist.songCount}
            {playlist.songCount === 1 ? 'track' : 'tracks'}
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

      {#if editing}
        <form on:submit|preventDefault={rename} class="flex gap-2">
          <!-- svelte-ignore a11y-autofocus -->
          <input
            bind:value={name}
            autofocus
            aria-label="Playlist name"
            class="flex-1 h-12 px-3 rounded-xl bg-slate-900 border border-slate-800 text-sm
                   text-slate-100 focus:outline-none focus:border-sky-700"
          />
          <button
            type="submit"
            disabled={!name.trim() || busy}
            class="h-12 px-4 rounded-xl bg-sky-600 text-white text-sm font-medium
                   disabled:opacity-50 active:bg-sky-700 flex items-center gap-2"
          >
            {#if busy}<Spinner class="w-4 h-4" />{:else}<Check class="w-4 h-4" />Save{/if}
          </button>
        </form>
      {:else}
        <button
          on:click={() => (editing = true)}
          class="w-full h-12 rounded-xl border border-slate-800 flex items-center justify-center
                 gap-2 text-sm text-slate-200 active:bg-slate-900"
        >
          <Pencil class="w-4 h-4" /> Rename
        </button>
      {/if}

      <button

        on:click={saveOffline}

        disabled={savingOffline}

        class="w-full h-12 rounded-xl border border-slate-800 flex items-center justify-center

               gap-2 text-sm text-slate-200 active:bg-slate-900 disabled:opacity-60"

      >

        {#if savingOffline}

          <CloudOff class="w-4 h-4 animate-pulse" />

          Saving {savedSoFar}/{ofTotal}

        {:else}

          <Download class="w-4 h-4" /> Save offline

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
        {#if busy}Deleting…{:else if confirming}Delete it{:else}Delete playlist{/if}
      </button>

      <p class="text-[11px] text-center text-slate-500 -mt-2">
        {#if confirming}
          Only the playlist goes. Every track stays in your library.
        {:else}
          Deleting a playlist never deletes its music.
        {/if}
      </p>
    </div>
  </div>
{/if}
