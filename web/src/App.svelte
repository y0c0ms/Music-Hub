<script>
  import { onMount } from 'svelte';
  import { AlertTriangle } from 'lucide-svelte';
  import Spinner from './components/Spinner.svelte';
  import Toasts from './components/Toasts.svelte';
  import TabBar from './components/TabBar.svelte';
  import MiniPlayer from './components/MiniPlayer.svelte';
  import FullPlayer from './components/FullPlayer.svelte';
  import AuthGate from './screens/AuthGate.svelte';
  import HomeScreen from './screens/Home.svelte';
  import SearchScreen from './screens/Search.svelte';
  import DiscoverScreen from './screens/Discover.svelte';
  import LibraryScreen from './screens/Library.svelte';
  import SettingsScreen from './screens/Settings.svelte';
  import Detail from './screens/Detail.svelte';
  import Book from './screens/Book.svelte';
  import * as api from './lib/api.js';
  import { gate } from './lib/api.js';
  import * as saved from './lib/saved.js';
  import Offline from './screens/Offline.svelte';
  import * as nav from './lib/nav.js';
  import { tab, stack } from './lib/nav.js';
  import { listen as listenTheme } from './lib/theme.js';
  import { expanded } from './lib/player.js';
  import * as player from './lib/player.js';
  import { attach as attachBookmark } from './lib/bookmark.js';

  let user = null;
  let bootError = '';
  let offlineBoot = false;
  let savedAtBoot = 0;

  async function boot() {
    bootError = '';
    offlineBoot = false;
    gate.set('loading');
    try {
      user = await api.me();
      gate.set(user.navidromeLinked ? 'ready' : 'unlinked');
    } catch (err) {
      // A 401 already moved the gate to signedOut. Anything else means the
      // server or the network is broken, which is not the user's password
      // problem and must not be dressed up as a sign-in prompt.
      if (err.status !== 401) {
        // ...but if there is music on the disk, showing "cannot reach the server"
        // over it is the wrong answer. reconcile() first so a track the browser
        // evicted is never counted - offering silence is worse than offering
        // nothing.
        try {
          // Restore the index from its durable Cache mirror before counting, so a
          // device whose localStorage was cleared still shows its saved music.
          await saved.restoreFromMirror();
          await saved.reconcile();
        } catch {
          // A failed reconcile just means we trust the index as it stands.
        }
        savedAtBoot = saved.count();
        // Latch the player into local mode now rather than waiting for its 5s link
        // timer, so the first tap on the offline screen responds immediately.
        if (savedAtBoot > 0) {
          offlineBoot = true;
          player.serveLocally();
        }
        else bootError = err.message || 'Cannot reach the server';
      }
    }
  }

  onMount(boot);

  function linked(u) {
    user = u;
    gate.set('ready');
  }

  // Navigation history is owned by lib/nav.js: every layer and every tab change
  // is a real history entry, so the Android back button and the browser back
  // gesture pop screens instead of leaving the app.
  onMount(nav.listen);

  // index.html already set the class before first paint; this takes ownership of
  // it and, while the choice is System, follows the device if it flips mid-session
  // (Android's scheduled dark mode does exactly that at sunset).
  onMount(listenTheme);

  // Keep the book bookmark written while you listen.
  //
  // Lives here, not on the book screen, because the scenario is that you close
  // the book screen: pause, wander off to the music, background the app. A saver
  // that unmounts with the screen would miss every one of those moments.
  //
  // It is inert for everything that is not a chapter of an opened book - the gate
  // is in bookmark.js and is the reason playing a song cannot rewind a book.
  onMount(() =>
    attachBookmark({
      player: player.player,
      progress: player.progress,
      save: ({ bookId, trackId, positionSec }) =>
        api.saveBookProgress(bookId, { trackId, positionSec }),
    }),
  );

  $: top = $stack[$stack.length - 1] || null;
</script>

<Toasts />

{#if bootError}
  <div class="min-h-[100dvh] flex flex-col items-center justify-center gap-3 px-8 text-center">
    <AlertTriangle class="w-7 h-7 text-amber-400" />
    <p class="text-sm font-medium">Music Hub cannot reach the server</p>
    <p class="text-xs text-slate-500">{bootError}</p>
    <!-- Say why there is nothing to fall back to. Without this the screen looks
         identical whether the user saved music and it broke, or never saved any -
         and those need completely different actions from them. -->
    {#if savedAtBoot === 0}
      <p class="text-xs text-slate-600 max-w-xs">
        No music is saved on this device, so there is nothing to play offline. While
        connected, use <span class="text-slate-400">Offline</span> on a song, album or
        playlist.
      </p>
    {:else}
      <p class="text-xs text-slate-600">{savedAtBoot} songs saved on this device.</p>
    {/if}
    <button on:click={boot} class="h-11 px-5 rounded-xl border border-slate-700 text-xs font-medium">
      Try again
    </button>
  </div>
{:else if offlineBoot}
  <Offline onRetry={boot} />
{:else if $gate === 'loading'}
  <div class="min-h-[100dvh] flex items-center justify-center"><Spinner /></div>
{:else if $gate === 'signedOut'}
  <AuthGate mode="signedOut" />
{:else if $gate === 'unlinked'}
  <AuthGate mode="unlinked" onLinked={linked} />
{:else}
  <div class="flex flex-col" style="height: 100vh; height: 100dvh;">
    <main class="flex-1 relative overflow-hidden pt-safe">
      <!-- All four screens stay mounted and are hidden with visibility rather
           than display: it keeps each tab's scroll position and in-flight state
           where the user left it, which display:none throws away. -->
      <div
        class="absolute inset-0 overflow-y-auto overscroll-contain"
        class:invisible={$tab !== 'home'}
        aria-hidden={$tab !== 'home'}
      >
        <HomeScreen {user} />
      </div>
      <div
        class="absolute inset-0 overflow-y-auto overscroll-contain"
        class:invisible={$tab !== 'search'}
        aria-hidden={$tab !== 'search'}
      >
        <SearchScreen />
      </div>
      <div
        class="absolute inset-0 overflow-hidden"
        class:invisible={$tab !== 'discover'}
        aria-hidden={$tab !== 'discover'}
      >
        <DiscoverScreen />
      </div>
      <div
        class="absolute inset-0 overflow-y-auto overscroll-contain"
        class:invisible={$tab !== 'library'}
        aria-hidden={$tab !== 'library'}
      >
        <LibraryScreen {user} />
      </div>

      {#if top}
        {#key `${top.type}:${top.id || 'settings'}`}
          {#if top.type === 'settings'}
            <SettingsScreen onBack={nav.back} {user} />
          {:else if top.type === 'book'}
            <Book entry={top} onBack={nav.back} />
          {:else}
            <Detail entry={top} onBack={nav.back} />
          {/if}
        {/key}
      {/if}
    </main>

    <div class="shrink-0">
      <MiniPlayer onExpand={nav.expandPlayer} />
      <TabBar />
    </div>
  </div>

  {#if $expanded}
    <FullPlayer onClose={nav.back} />
  {/if}
{/if}
