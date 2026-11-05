<script>
  /**
   * The first screen, built around singles.
   *
   * 428 of 508 tracks in this library are the only track on their album, so an
   * album grid here was mostly one-track shells reading "[Unknown Album]". Rows
   * are songs now, and albums live in the Library tab where the handful of real
   * records can be browsed as records.
   *
   * The mixes at the top exist for the case that actually matters: opening the
   * app, starting music, and putting the phone back in a pocket without reading
   * the screen. They load on tap, not on render.
   */
  import { onMount } from 'svelte';
  import { RefreshCw, Shuffle, Settings, Clock, Flame, History } from 'lucide-svelte';
  import TrackList from '../components/TrackList.svelte';
  import MixCard from '../components/MixCard.svelte';
  import Spinner from '../components/Spinner.svelte';
  import * as api from '../lib/api.js';
  import { shufflePlay, play } from '../lib/player.js';
  import { openSettings } from '../lib/nav.js';
  import { toast } from '../lib/toast.js';
  import { libraryChanged } from '../lib/library-events.js';

  export let user = null;

  const MIX_SIZE = 100;

  let data = null;
  let loading = true;
  let error = null;

  /**
   * `quiet` refreshes in place: the mixes stay on screen while the new data is
   * fetched. A library change is a background fact, and blanking the whole
   * screen to a spinner for it reads as the app having lost your music.
   */
  async function load({ quiet = false } = {}) {
    if (!quiet) loading = true;
    error = null;
    try {
      data = await api.home();
    } catch (err) {
      // A failed quiet refresh leaves the last good screen alone rather than
      // replacing a working Home with an error for something the user did not ask for.
      if (!quiet) error = err.message || 'Could not load your library';
    } finally {
      loading = false;
    }
  }

  onMount(load);

  // This screen is never torn down (App.svelte hides tabs rather than unmounting
  // them), so onMount fires once per session and nothing else would ever refetch.
  // Skip the initial store value; only real changes should trigger a reload.
  let seenChange = $libraryChanged;
  $: if ($libraryChanged !== seenChange) {
    seenChange = $libraryChanged;
    load({ quiet: true });
  }

  function greeting() {
    const h = new Date().getHours();
    if (h < 5) return 'Still up';
    if (h < 12) return 'Good morning';
    if (h < 18) return 'Good afternoon';
    return 'Good evening';
  }

  // Shuffle pulls from a wide slice so it is not the same twenty songs daily;
  // the ordered mixes play in their own order, because "most played, in order"
  // is a different intent from "surprise me".
  const mixes = [
    {
      title: 'Shuffle everything',
      subtitle: 'The whole library, in any order',
      icon: Shuffle,
      tint: 'sky',
      load: () => api.songs('random', MIX_SIZE),
      start: shufflePlay,
    },
    {
      title: 'Recently added',
      subtitle: 'Newest first',
      icon: Clock,
      tint: 'violet',
      load: () => api.songs('newest', MIX_SIZE),
      start: (t) => play(t, 0),
    },
    {
      title: 'Most played',
      subtitle: 'What you keep coming back to',
      icon: Flame,
      tint: 'amber',
      load: () => api.songs('frequent', MIX_SIZE),
      start: (t) => play(t, 0),
    },
    {
      title: 'Played recently',
      subtitle: 'Pick up where you were',
      icon: History,
      tint: 'emerald',
      load: () => api.songs('recent', MIX_SIZE),
      start: (t) => play(t, 0),
    },
  ];

  $: nothing = data && !data.recent?.length && !data.frequent?.length && !data.random?.length;
</script>

<div class="px-4 py-4 space-y-6">
  <header class="flex items-end justify-between gap-3">
    <div class="min-w-0">
      <p class="text-[11px] uppercase tracking-wider text-slate-500">{greeting()}</p>
      <h1 class="text-xl font-semibold truncate">{user?.displayName || 'Music Hub'}</h1>
    </div>
    <div class="flex items-center gap-2">
      <button
        on:click={openSettings}
        aria-label="Settings"
        class="w-11 h-11 shrink-0 flex items-center justify-center rounded-xl border border-slate-800 text-slate-400 active:bg-slate-900"
      >
        <Settings class="w-4 h-4" />
      </button>
      <button
        on:click={load}
        aria-label="Refresh"
        class="w-11 h-11 shrink-0 flex items-center justify-center rounded-xl border border-slate-800 text-slate-400 active:bg-slate-900"
      >
        <RefreshCw class="w-4 h-4 {loading ? 'animate-spin' : ''}" />
      </button>
    </div>
  </header>

  {#if error}
    <div class="rounded-xl border border-red-900/60 bg-red-950/40 p-4">
      <p class="text-xs text-red-200">{error}</p>
      <button on:click={load} class="mt-3 h-11 px-4 rounded-lg bg-red-900/60 text-xs font-medium">
        Try again
      </button>
    </div>
  {:else if loading && !data}
    <div class="py-16"><Spinner label="Loading your library" /></div>
  {:else if data}
    {#if !nothing}
      <section>
        <div class="grid grid-cols-2 gap-3">
          {#each mixes as mix (mix.title)}
            <MixCard {...mix} />
          {/each}
        </div>
      </section>
    {/if}

    {#if data.recent?.length}
      <section>
        <h2 class="text-sm font-semibold mb-1.5">Recently added</h2>
        <TrackList tracks={data.recent} />
      </section>
    {/if}

    {#if data.frequent?.length}
      <section>
        <h2 class="text-sm font-semibold mb-1.5">Most played</h2>
        <TrackList tracks={data.frequent} />
      </section>
    {/if}

    {#if data.random?.length}
      <section>
        <h2 class="text-sm font-semibold mb-1">Something else</h2>
        <p class="text-[11px] text-slate-500 mb-1.5">A fresh handful every time you open the app.</p>
        <TrackList tracks={data.random} />
      </section>
    {/if}

    {#if nothing}
      <p class="py-16 text-center text-xs text-slate-500">
        Nothing in the library yet. Try Search to add something.
      </p>
    {/if}
  {/if}
</div>
