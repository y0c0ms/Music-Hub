<script>
  import Artwork from './Artwork.svelte';
  import { openAlbum } from '../lib/nav.js';

  export let album;
  /** Horizontal rows need a fixed width; grids want the cell width. */
  export let width = 'w-36';
</script>

<button
  on:click={() => openAlbum(album)}
  class="{width} shrink-0 text-left active:scale-95 transition-transform"
>
  <Artwork
    src={album.coverUrl}
    alt={album.name}
    class="w-full aspect-square rounded-xl border border-slate-800/60"
  />
  <p class="mt-2 text-xs font-medium text-slate-100 line-clamp-2 leading-snug">{album.name}</p>
  <p class="text-[11px] text-slate-500 truncate">{album.artist}</p>
</button>
