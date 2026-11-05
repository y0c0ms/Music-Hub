<script>
  import { fly } from 'svelte/transition';
  import {
    ChevronDown,
    ListMusic,
    MoreHorizontal,
    Pause,
    Play,
    Repeat,
    Repeat1,
    Shuffle,
    SkipBack,
    SkipForward,
    Star,
    Trash2,
    Volume2,
  } from 'lucide-svelte';
  import { swipeable } from '../lib/swipe.js';
  import SongSheet from './SongSheet.svelte';
  import Artwork from './Artwork.svelte';
  import DevicePicker from './DevicePicker.svelte';
  import { fmtTime } from '../lib/format.js';
  import { isPlaceholderAlbum } from '../lib/track.js';
  import {
    player,
    progress,
    next,
    prev,
    toggle,
    seek,
    toggleShuffle,
    cycleRepeat,
    jump,
    isActive,
    activeId,
    offline,
    devices,
    removeAt,
    drop,
  } from '../lib/player.js';
  import * as api from '../lib/api.js';
  import { toast } from '../lib/toast.js';

  export let onClose = () => {};

  let showQueue = false;
  let showDevices = false;
  let scrubbing = false;
  let scrubValue = 0;

  $: track = $player.track;
  $: duration = $progress.duration || track?.duration || 0;

  // Queue-row swipe. Only one row is ever mid-swipe, so a single offset drives
  // the whole list rather than a store per row - same shape as TrackList.
  //
  // Keyed by ORDER POSITION, not by index into the rendered list. The rendered
  // list now starts with the playing track, so an index is no longer the same
  // number as a position, and mixing them up removes the wrong song.
  let swipingPos = null;
  let qdx = 0;
  let qdragging = false;
  let sheetFor = null;
  const Q_COMMIT = 72;

  function onQueueSwipe(pos, e) {
    swipingPos = pos;
    qdragging = true;
    qdx = e.detail.dx;
  }

  function onQueueSwipeEnd(pos, t, e) {
    const { commit, dir } = e.detail;
    qdragging = false;
    qdx = 0;
    swipingPos = null;
    if (!commit) return;
    if (dir === 'right') {
      removeAt(pos);
      toast(`Removed “${t.title}” from the queue`, 'success');
    } else {
      sheetFor = t;
    }
  }

  $: shown = scrubbing ? scrubValue : $progress.time;
  $: pct = duration > 0 ? Math.min(100, (shown / duration) * 100) : 0;
  $: upNext = $player.order.slice($player.cursor + 1).map((i) => $player.queue[i]);
  // The playing track is part of the queue and now renders as its first row.
  // Asked for on 2026-08-25: reaching a song's options previously meant finding
  // it again by searching, because the one song you are certainly looking at -
  // the one playing - was the only one with no row to swipe.
  $: rows = [
    ...(track ? [{ t: track, pos: $player.cursor, now: true }] : []),
    ...upNext.map((t, i) => ({ t, pos: $player.cursor + 1 + i, now: false })),
  ];
  // Who is making sound, for the header. A neutral label rather than an empty
  // string, because the device list arrives a moment after the first state
  // frame and a blank header reads as a bug.
  $: activeName =
    $devices.find((d) => d.id === $activeId)?.name ||
    ($activeId ? 'another device' : 'Not playing');

  function onInput(e) {
    scrubbing = true;
    scrubValue = Number(e.currentTarget.value);
  }
  function onCommit() {
    if (!scrubbing) return;
    scrubbing = false;
    seek(scrubValue);
  }

  async function toggleStar() {
    if (!track) return;
    const id = track.id;
    const want = !track.starred;
    const mark = (v) => (t) => (t && t.id === id ? { ...t, starred: v } : t);
    player.update((p) => ({ ...p, track: mark(want)(p.track), queue: p.queue.map(mark(want)) }));
    try {
      await (want ? api.star(id) : api.unstar(id));
    } catch (err) {
      player.update((p) => ({
        ...p,
        track: mark(!want)(p.track),
        queue: p.queue.map(mark(!want)),
      }));
      toast(err.message || 'Could not update favourite', 'error');
    }
  }

</script>

{#if track}
  <div
    class="fixed inset-0 z-40 bg-slate-950 flex flex-col pt-safe"
    transition:fly={{ y: 400, duration: 240 }}
  >
    <header class="flex items-center gap-2 px-2 h-14 shrink-0">
      <button
        on:click={onClose}
        aria-label="Close player"
        class="w-11 h-11 flex items-center justify-center text-slate-300"
      >
        <ChevronDown class="w-6 h-6" />
      </button>
      <button
        on:click={() => (showDevices = true)}
        aria-label={$isActive
          ? 'Playing on this device — change'
          : `Playing on ${activeName} — change`}
        class="flex-1 min-w-0 flex items-center justify-center gap-1.5 px-2 h-11"
      >
        <!-- Always visible, and it names the device rather than just offering a
             menu. Asked for on 2026-08-24: with playback able to live somewhere
             else, "where is the sound going" has to be answerable at a glance
             instead of by opening a picker. Sky when it is elsewhere. -->
        <Volume2
          class="w-3.5 h-3.5 shrink-0 {$offline
            ? 'text-amber-400'
            : $isActive
              ? 'text-slate-500'
              : 'text-sky-400'}"
        />
        <span
          class="text-[11px] uppercase tracking-wider truncate {$offline
            ? 'text-amber-400'
            : $isActive
              ? 'text-slate-500'
              : 'text-sky-400'}"
        >
          {#if $offline}Reconnecting…{:else}{$isActive ? 'Now playing' : activeName}{/if}
        </span>
      </button>
      <button
        on:click={() => (showQueue = !showQueue)}
        aria-label="Queue"
        aria-pressed={showQueue}
        class="w-11 h-11 flex items-center justify-center {showQueue
          ? 'text-sky-400'
          : 'text-slate-300'}"
      >
        <ListMusic class="w-5 h-5" />
      </button>
      <!-- Options for the song you are looking at. Asked for on 2026-08-25:
           this view had no way to reach star / play similar / delete, so a
           track arriving from shuffle that you wanted gone had to be hunted
           down in search first just to open the same sheet. -->
      <button
        on:click={() => (sheetFor = track)}
        aria-label="Options for this song"
        class="w-11 h-11 flex items-center justify-center text-slate-300"
      >
        <MoreHorizontal class="w-5 h-5" />
      </button>
    </header>

    {#if showQueue}
      <div class="flex-1 overflow-y-auto px-4 pb-6">
        <p class="text-[11px] uppercase tracking-wider text-slate-500 mb-2">
          Queue · {rows.length}
        </p>
        <ul class="divide-y divide-slate-900">
          {#each rows as r (r.t.id + ':' + r.pos)}
            <li
              class="relative overflow-hidden touch-pan-y"
              use:swipeable
              on:swipe={(e) => onQueueSwipe(r.pos, e)}
              on:swipeend={(e) => onQueueSwipeEnd(r.pos, r.t, e)}
            >
              <!-- Same gesture language as every other song row: left opens
                   options, right is the list-specific action. Here that is
                   remove, because "add to queue" means nothing inside the
                   queue - the slot was free. -->
              <div
                class="absolute inset-y-0 left-0 flex items-center gap-1.5 pl-3 text-rose-300 pointer-events-none"
                style="opacity: {swipingPos === r.pos && qdx > 0 ? Math.min(1, qdx / Q_COMMIT) : 0}"
              >
                <Trash2 class="w-4 h-4" />
                <span class="text-[11px] uppercase tracking-wider">Remove</span>
              </div>
              <div
                class="absolute inset-y-0 right-0 flex items-center gap-1.5 pr-3 text-slate-300 pointer-events-none"
                style="opacity: {swipingPos === r.pos && qdx < 0
                  ? Math.min(1, -qdx / Q_COMMIT)
                  : 0}"
              >
                <span class="text-[11px] uppercase tracking-wider">Options</span>
                <MoreHorizontal class="w-4 h-4" />
              </div>

              <button
                on:click={() => (r.now ? (showQueue = false) : jump(r.pos))}
                class="relative w-full flex items-center gap-3 py-2.5 text-left active:bg-slate-900/50 rounded-lg {r.now
                  ? 'bg-sky-950/40'
                  : 'bg-slate-950'}"
                style="transform: translate3d({swipingPos === r.pos ? qdx : 0}px,0,0); transition: {qdragging
                  ? 'none'
                  : 'transform 160ms ease-out'}"
              >
                <Artwork src={r.t.coverUrl} alt="" class="w-11 h-11 rounded-lg shrink-0" />
                <span class="min-w-0 flex-1">
                  <span class="block text-sm truncate {r.now ? 'text-sky-300' : ''}">
                    {r.t.title}
                  </span>
                  <span class="block text-[11px] truncate {r.now
                      ? 'text-sky-500/80'
                      : 'text-slate-500'}">
                    {r.now ? 'Playing now · ' : ''}{r.t.artist}
                  </span>
                </span>
                <span class="text-[11px] text-slate-600 tabular-nums">
                  {fmtTime(r.t.duration)}
                </span>
              </button>
            </li>
          {/each}
        </ul>
        {#if upNext.length === 0}
          <p class="text-xs text-slate-600 py-6 text-center">Nothing queued after this track.</p>
        {/if}
      </div>
    {:else}
      <div class="flex-1 flex items-center justify-center px-6 min-h-0">
        <Artwork
          src={track.coverUrl}
          alt={track.album || track.title}
          class="w-full max-w-sm aspect-square rounded-2xl border border-slate-800/60 shadow-2xl shadow-black/60"
        />
      </div>
    {/if}

    <div class="shrink-0 px-6 pb-6 pb-safe space-y-4">
      <div class="flex items-end gap-3">
        <div class="min-w-0 flex-1">
          <h2 class="text-lg font-semibold truncate">{track.title}</h2>
          <p class="text-sm text-slate-400 truncate">{track.artist}</p>
          {#if !isPlaceholderAlbum(track.album, track.title)}
            <p class="text-[11px] text-slate-600 truncate">{track.album}</p>
          {/if}
        </div>
        <button
          on:click={toggleStar}
          aria-label={track.starred ? 'Remove from favourites' : 'Add to favourites'}
          aria-pressed={!!track.starred}
          class="w-11 h-11 shrink-0 flex items-center justify-center"
        >
          <Star class="w-5 h-5 {track.starred ? 'text-amber-400 fill-amber-400' : 'text-slate-500'}" />
        </button>
      </div>

      {#if $player.error}
        <p class="text-xs text-amber-400">{$player.error}</p>
      {/if}

      <div>
        <input
          type="range"
          class="seek w-full h-11"
          min="0"
          max={duration || 1}
          step="0.5"
          value={shown}
          aria-label="Seek"
          on:input={onInput}
          on:change={onCommit}
          style="background: linear-gradient(to right, rgb(var(--c-sky-400)) {pct}%, rgb(var(--c-slate-800)) {pct}%) no-repeat center / 100% 4px;"
        />
        <div class="flex justify-between text-[11px] tabular-nums text-slate-500 -mt-1">
          <span>{fmtTime(shown)}</span>
          <span>{fmtTime(duration)}</span>
        </div>
      </div>

      <div class="flex items-center justify-between">
        <button
          on:click={toggleShuffle}
          aria-label="Shuffle"
          aria-pressed={$player.shuffle}
          class="w-11 h-11 flex items-center justify-center {$player.shuffle
            ? 'text-sky-400'
            : 'text-slate-500'}"
        >
          <Shuffle class="w-5 h-5" />
        </button>

        <button
          on:click={prev}
          aria-label="Previous"
          class="w-14 h-14 flex items-center justify-center text-slate-200 active:scale-95 transition-transform"
        >
          <SkipBack class="w-7 h-7 fill-current" />
        </button>

        <button
          on:click={toggle}
          aria-label={$player.playing ? 'Pause' : 'Play'}
          class="w-16 h-16 rounded-full bg-sky-500 text-white flex items-center justify-center shadow-lg shadow-sky-500/25 active:scale-95 transition-transform"
        >
          {#if $player.playing}
            <Pause class="w-7 h-7 fill-current" />
          {:else}
            <Play class="w-7 h-7 fill-current translate-x-0.5" />
          {/if}
        </button>

        <button
          on:click={() => next(true)}
          aria-label="Next"
          class="w-14 h-14 flex items-center justify-center text-slate-200 active:scale-95 transition-transform"
        >
          <SkipForward class="w-7 h-7 fill-current" />
        </button>

        <button
          on:click={cycleRepeat}
          aria-label="Repeat: {$player.repeat}"
          class="w-11 h-11 flex items-center justify-center {$player.repeat === 'off'
            ? 'text-slate-500'
            : 'text-sky-400'}"
        >
          {#if $player.repeat === 'one'}
            <Repeat1 class="w-5 h-5" />
          {:else}
            <Repeat class="w-5 h-5" />
          {/if}
        </button>
      </div>
    </div>
  </div>

  {#if showDevices}
    <DevicePicker onClose={() => (showDevices = false)} />
  {/if}

  <!-- Left-swiping a queue row opens the same sheet a song row opens anywhere
       else, so "options" means one thing across the app. A delete from here also
       has to leave the queue, which drop() does by id - correct in this case,
       because the file is gone and every copy of it is dead. -->
  <SongSheet
    track={sheetFor}
    on:close={() => (sheetFor = null)}
    on:deleted={(e) => drop(e.detail?.id)}
  />
{/if}
