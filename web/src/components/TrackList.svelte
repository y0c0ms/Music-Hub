<script>
  import { Star, Volume2, ListEnd, MoreHorizontal } from 'lucide-svelte';
  import Artwork from './Artwork.svelte';
  import SongSheet from './SongSheet.svelte';
  import { fmtTime } from '../lib/format.js';
  import { player, play, enqueue } from '../lib/player.js';
  import { longpress } from '../lib/longpress.js';
  import { swipeable } from '../lib/swipe.js';
  import { subtitleFor, removedIds, markRemoved } from '../lib/track.js';
  import * as api from '../lib/api.js';
  import { toast } from '../lib/toast.js';

  export let tracks = [];
  export let showArt = true;
  export let numbered = false;

  /** Holding a row opens its details; see lib/longpress.js for why the tap is suppressed. */
  let sheetFor = null;
  // Only one row is ever mid-swipe, so a single offset drives the whole list
  // rather than a per-row store.
  let swipingId = null;
  let dx = 0;
  // Suppresses the spring-back transition during the drag itself: animating
  // every pointermove would lag a finger.
  let dragging = false;

  const COMMIT = 72;

  function onSwipe(t, e) {
    swipingId = t.id;
    dragging = true;
    dx = e.detail.dx;
  }

  function onSwipeEnd(t, e) {
    const { commit, dir } = e.detail;
    dragging = false;
    dx = 0;
    swipingId = null;
    if (!commit) return;
    if (dir === 'right') {
      enqueue([t]);
      toast(`Queued “${t.title}”`, 'success');
    } else {
      sheetFor = t;
    }
  }

  $: currentId = $player.track?.id;

  // Rows are driven by `visible`, never `tracks`, so a track deleted from any
  // other list on screen disappears here too - and so the index handed to
  // play() always matches what was actually tapped.
  $: visible = tracks.filter((t) => !$removedIds.has(t.id));

  async function toggleStar(i) {
    const t = visible[i];
    const want = !t.starred;
    const at = tracks.findIndex((x) => x.id === t.id);
    tracks[at] = { ...t, starred: want };
    tracks = tracks;
    try {
      await (want ? api.star(t.id) : api.unstar(t.id));
    } catch (err) {
      // Put the star back rather than lie about what Navidrome stored.
      tracks[at] = { ...tracks[at], starred: !want };
      tracks = tracks;
      toast(err.message || 'Could not update favourite', 'error');
    }
  }

  // Recorded globally rather than spliced out locally, because the same song is
  // usually on screen in two or three other lists at the same time.
  function dropped(e) {
    markRemoved(e.detail.id);
  }

  function updated(e) {
    const t = e.detail;
    tracks = tracks.map((x) => (x.id === t.id ? { ...x, starred: t.starred } : x));
  }
</script>

<ul class="divide-y divide-slate-900">
  {#each visible as t, i (t.id + ':' + i)}
    <li
      use:swipeable
      on:swipe={(e) => onSwipe(t, e)}
      on:swipeend={(e) => onSwipeEnd(t, e)}
      class="relative overflow-hidden touch-pan-y"
    >
      <!--
        What the swipe will do, revealed underneath rather than drawn on the
        row: the row slides off it, so the icon is already there and reads as
        one movement. Both sides are always mounted and simply sit outside the
        visible area until the row uncovers them.

        Opacity tracks the distance so the colour arrives as you commit - it is
        the only feedback that says "let go now" before anything happens.
      -->
      <div
        class="absolute inset-0 flex items-center justify-between pointer-events-none"
        aria-hidden="true"
      >
        <div
          class="h-full w-1/2 flex items-center gap-2 pl-4 text-sm font-medium text-white bg-sky-600"
          style="opacity: {swipingId === t.id && dx > 0 ? Math.min(1, dx / COMMIT) : 0}"
        >
          <ListEnd class="w-5 h-5" />
          Queue
        </div>
        <div
          class="h-full w-1/2 flex items-center justify-end gap-2 pr-4 text-sm font-medium text-slate-200 bg-slate-700"
          style="opacity: {swipingId === t.id && dx < 0 ? Math.min(1, -dx / COMMIT) : 0}"
        >
          Options
          <MoreHorizontal class="w-5 h-5" />
        </div>
      </div>

      <div
        class="relative flex items-center gap-3 bg-slate-950 {dragging && swipingId === t.id
          ? ''
          : 'transition-transform duration-200 ease-out'}"
        style="transform: translate3d({swipingId === t.id ? dx : 0}px,0,0)"
      >
      <button
        use:longpress
        on:longpress={() => (sheetFor = t)}
        on:click={() => play(visible, i)}
        class="flex-1 min-w-0 flex items-center gap-3 py-2.5 pl-1 text-left active:bg-slate-900/50 rounded-lg"
      >
        {#if numbered}
          <span
            class="w-6 shrink-0 text-center text-xs tabular-nums {currentId === t.id
              ? 'text-sky-400'
              : 'text-slate-600'}"
          >
            {#if currentId === t.id}
              <Volume2 class="w-3.5 h-3.5 mx-auto" />
            {:else}
              {i + 1}
            {/if}
          </span>
        {:else if showArt}
          <Artwork src={t.coverUrl} alt="" class="w-11 h-11 rounded-lg shrink-0" />
        {/if}

        <span class="min-w-0 flex-1">
          <span
            class="block text-sm truncate {currentId === t.id
              ? 'text-sky-400 font-medium'
              : 'text-slate-100'}"
          >
            {t.title}
          </span>
          <span class="block text-[11px] text-slate-500 truncate">
            {subtitleFor(t)}
          </span>
        </span>

        <span class="text-[11px] text-slate-600 tabular-nums shrink-0">{fmtTime(t.duration)}</span>
      </button>

      <button
        on:click={() => toggleStar(i)}
        aria-label={t.starred ? 'Remove from favourites' : 'Add to favourites'}
        aria-pressed={!!t.starred}
        class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg active:bg-slate-900/50"
      >
        <Star
          class="w-4 h-4 {t.starred ? 'text-amber-400 fill-amber-400' : 'text-slate-600'}"
        />
      </button>
      </div>
    </li>
  {/each}
</ul>

<SongSheet
  track={sheetFor}
  on:close={() => (sheetFor = null)}
  on:deleted={dropped}
  on:changed={updated}
/>
