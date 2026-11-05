<script>
  /**
   * What the app shows when it starts with no server but has downloaded music.
   *
   * Deliberately NOT the full app with failing tabs. Search, Discover and the
   * library all need the API, and rendering four screens that can only show
   * spinners and errors would suggest the app is broken rather than that it is
   * offline on purpose. This shows the one thing that genuinely works.
   */
  import { CloudOff, Play, Shuffle, RefreshCw } from 'lucide-svelte';
  import TrackList from '../components/TrackList.svelte';
  import MiniPlayer from '../components/MiniPlayer.svelte';
  import FullPlayer from '../components/FullPlayer.svelte';
  import * as saved from '../lib/saved.js';
  import * as player from '../lib/player.js';
  import { expanded } from '../lib/player.js';
  import { plural } from '../lib/format.js';

  export let onRetry = () => {};

  const { downloads } = saved;

  let used = 0;
  saved.usage().then((u) => {
    used = u.bytes;
  });

  const gb = (n) => (n >= 1e9 ? `${(n / 1e9).toFixed(1)} GB` : `${Math.round(n / 1e6)} MB`);
</script>

<div class="flex flex-col" style="height: 100vh; height: 100dvh;">
  <main class="flex-1 overflow-y-auto overscroll-contain pt-safe">
    <div class="px-5 pt-6 pb-3">
      <div class="flex items-center gap-2 text-amber-400">
        <CloudOff class="w-4 h-4" />
        <span class="text-xs font-medium uppercase tracking-wide">Offline</span>
      </div>
      <h1 class="mt-2 text-2xl font-semibold">Downloads</h1>
      <p class="mt-1 text-xs text-slate-500">
        {$downloads.length}
        {plural($downloads.length, 'song', 'songs')} · {gb(used)} on this device
      </p>

      <div class="mt-4 flex gap-2">
        <button
          on:click={() => player.play($downloads, 0)}
          disabled={!$downloads.length}
          class="h-10 px-4 rounded-xl bg-white text-slate-900 text-xs font-semibold
                 flex items-center gap-2 disabled:opacity-40"
        >
          <Play class="w-4 h-4" /> Play
        </button>
        <button
          on:click={() => player.shufflePlay($downloads)}
          disabled={!$downloads.length}
          class="h-10 px-4 rounded-xl border border-slate-700 text-xs font-semibold
                 flex items-center gap-2 disabled:opacity-40"
        >
          <Shuffle class="w-4 h-4" /> Shuffle
        </button>
        <button
          on:click={onRetry}
          class="h-10 px-4 rounded-xl border border-slate-700 text-xs font-semibold
                 flex items-center gap-2 ml-auto"
        >
          <RefreshCw class="w-4 h-4" /> Retry
        </button>
      </div>
    </div>

    {#if $downloads.length}
      <TrackList tracks={$downloads} />
    {:else}
      <p class="px-5 py-16 text-center text-sm text-slate-500">
        Nothing downloaded yet. Connect once and use Download on a song, album or
        playlist to keep it here.
      </p>
    {/if}
    <div class="h-24"></div>
  </main>

  <div class="shrink-0">
    <MiniPlayer onExpand={() => expanded.set(true)} />
  </div>
</div>

{#if $expanded}
  <FullPlayer onClose={() => expanded.set(false)} />
{/if}
