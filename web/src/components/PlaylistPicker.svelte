<script>
  /**
   * Add-to-playlist sheet.
   *
   * Playlists come from Navidrome under the signed-in user's own credentials,
   * so this list is theirs alone - two people in the house never see each
   * other's. Nothing here needs to filter by owner.
   */
  import { createEventDispatcher } from 'svelte';
  import { X, Plus, ListMusic, Check } from 'lucide-svelte';
  import * as api from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import Spinner from './Spinner.svelte';

  export let track = null;

  const dispatch = createEventDispatcher();

  let lists = [];
  let loading = false;
  let busy = '';
  let creating = false;
  let newName = '';
  let loaded = null;

  // Reload whenever a different track opens the sheet, so a playlist made
  // moments ago on another track is already here.
  $: if (track && loaded !== track.id) {
    loaded = track.id;
    load();
  }

  async function load() {
    loading = true;
    creating = false;
    newName = '';
    try {
      lists = (await api.playlists()) || [];
    } catch (err) {
      toast(err.message || 'Could not load your playlists', 'error');
      lists = [];
    } finally {
      loading = false;
    }
  }

  function close() {
    dispatch('close');
  }

  async function addTo(pl) {
    if (busy) return;
    busy = pl.id;
    try {
      await api.addToPlaylist(pl.id, [track.id]);
      toast(`Added to ${pl.name}`, 'success');
      close();
    } catch (err) {
      toast(err.message || 'Could not add to that playlist', 'error');
    } finally {
      busy = '';
    }
  }

  async function create() {
    const name = newName.trim();
    if (!name || busy) return;
    busy = 'new';
    try {
      // Created with the track already in it: one call, and the playlist is
      // never briefly empty if the second half fails.
      await api.createPlaylist(name, [track.id]);
      toast(`Added to ${name}`, 'success');
      close();
    } catch (err) {
      toast(err.message || 'Could not create the playlist', 'error');
    } finally {
      busy = '';
    }
  }
</script>

{#if track}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div
    class="fixed inset-0 z-50 bg-slate-950/70 backdrop-blur-sm flex items-end"
    on:click|self={close}
  >
    <div class="w-full rounded-t-2xl border-t border-slate-800 bg-slate-950 p-4 pb-safe space-y-3">
      <div class="flex items-start gap-3">
        <div class="min-w-0 flex-1">
          <p class="text-base font-semibold text-slate-100">Add to playlist</p>
          <p class="text-xs text-slate-400 truncate">{track.title}</p>
        </div>
        <button
          on:click={close}
          aria-label="Close"
          class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg text-slate-500 active:bg-slate-900"
        >
          <X class="w-4 h-4" />
        </button>
      </div>

      {#if creating}
        <form on:submit|preventDefault={create} class="flex gap-2">
          <!-- svelte-ignore a11y-autofocus -->
          <input
            bind:value={newName}
            autofocus
            placeholder="Playlist name"
            aria-label="New playlist name"
            class="flex-1 h-12 px-3 rounded-xl bg-slate-900 border border-slate-800 text-sm
                   text-slate-100 placeholder:text-slate-600 focus:outline-none focus:border-sky-700"
          />
          <button
            type="submit"
            disabled={!newName.trim() || busy === 'new'}
            class="h-12 px-4 rounded-xl bg-sky-600 text-white text-sm font-medium
                   disabled:opacity-50 active:bg-sky-700"
          >
            {#if busy === 'new'}<Spinner class="w-4 h-4" />{:else}Create{/if}
          </button>
        </form>
      {:else}
        <button
          on:click={() => (creating = true)}
          class="w-full h-12 rounded-xl border border-dashed border-slate-700 flex items-center
                 justify-center gap-2 text-sm text-slate-300 active:bg-slate-900"
        >
          <Plus class="w-4 h-4" /> New playlist
        </button>
      {/if}

      <div class="max-h-[46vh] overflow-y-auto -mx-1 px-1">
        {#if loading}
          <div class="py-6 flex justify-center"><Spinner class="w-5 h-5" /></div>
        {:else if lists.length === 0}
          <p class="py-6 text-center text-xs text-slate-500">
            No playlists yet. Make your first one above.
          </p>
        {:else}
          <ul class="space-y-1">
            {#each lists as pl (pl.id)}
              <li>
                <button
                  on:click={() => addTo(pl)}
                  disabled={!!busy}
                  class="w-full h-12 px-3 rounded-xl flex items-center gap-3 text-left
                         text-sm text-slate-200 active:bg-slate-900 disabled:opacity-60"
                >
                  <ListMusic class="w-4 h-4 shrink-0 text-slate-500" />
                  <span class="flex-1 min-w-0 truncate">{pl.name}</span>
                  {#if busy === pl.id}
                    <Spinner class="w-4 h-4 shrink-0" />
                  {:else}
                    <span class="text-[11px] text-slate-600 shrink-0">{pl.songCount ?? ''}</span>
                  {/if}
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </div>
  </div>
{/if}
