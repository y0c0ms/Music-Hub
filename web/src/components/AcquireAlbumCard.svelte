<script>
  /**
   * An album found online, shown as a card rather than a row.
   *
   * Deliberately mirrors AlbumCard's visual language - square art, title, artist
   * underneath - because an album IS a different kind of thing from a track and
   * the list should say so at a glance. The first version reused the track row
   * and only differed by a small "Album" label, which read as "a song with a
   * badge" rather than "a record".
   *
   * It cannot literally reuse AlbumCard: that one navigates into a library album
   * that exists, and this one is not on the server yet - the whole point of the
   * card is the download action.
   */
  import { Download, Loader2, Check } from 'lucide-svelte';
  import Artwork from './Artwork.svelte';

  export let album;
  export let queuing = false;
  export let done = false;
  export let onGet = () => {};
  export let onOpen = () => {};
</script>

<div class="text-left">
  <div class="relative">
    <!-- The artwork opens the track list; only the button downloads. Tapping a
         record should show you the record - wanting two songs off it is at least
         as common as wanting all of them. -->
    <button
      on:click={() => onOpen(album)}
      aria-label="Open {album.title}"
      class="block w-full active:scale-95 transition-transform"
    >
    <Artwork
      src={album.thumb}
      alt={album.title}
      class="w-full aspect-square rounded-xl border border-slate-800/60"
    />
    </button>
    <!-- The action sits ON the artwork so the card stays a card. A button in the
         flow below would push the titles out of alignment across the grid. -->
    <button
      on:click={() => onGet(album)}
      disabled={queuing || done}
      aria-label={done ? `${album.title} queued` : `Download ${album.title}`}
      class="absolute bottom-1.5 right-1.5 h-9 w-9 rounded-full backdrop-blur
             bg-slate-950/75 border border-slate-700 flex items-center justify-center
             active:scale-95 transition-transform disabled:opacity-70"
    >
      {#if done}
        <Check class="w-4 h-4 text-emerald-400" />
      {:else if queuing}
        <Loader2 class="w-4 h-4 animate-spin" />
      {:else}
        <Download class="w-4 h-4" />
      {/if}
    </button>
  </div>

  <button on:click={() => onOpen(album)} class="block w-full text-left">
  <p class="mt-2 text-xs font-medium text-slate-100 line-clamp-2 leading-snug">{album.title}</p>
  <p class="text-[11px] text-slate-500 truncate">
    {album.artist || 'Unknown artist'} · {album.trackCount}
    {album.trackCount === 1 ? 'track' : 'tracks'}
  </p>
  </button>
</div>
