<script>
  import { onMount } from 'svelte';
  import {
    ChevronLeft,
    LogOut,
    Unlink,
    Download,
    CheckCircle2,
    Sun,
    Moon,
    Smartphone,
    Volume2,
    CloudOff,
  } from 'lucide-svelte';
  import * as saved from '../lib/saved.js';
  import { theme, resolved, setTheme } from '../lib/theme.js';
  import * as api from '../lib/api.js';
  import { hardUpdate } from '../lib/appUpdate.js';
  import { toast } from '../lib/toast.js';
  import Spinner from '../components/Spinner.svelte';
  import {
    isNative,
    canSelfUpdate,
    installedVersion,
    isNewer,
    installUpdate,
    latestRelease,
    canChangeServer,
    changeServer,
  } from '../lib/updater.js';
  import {
    devices,
    activeId,
    connected,
    deviceId,
    transfer,
    refreshDevices,
  } from '../lib/player.js';

  let movingTo = '';

  // The list is only refreshed when the stream opens, and Settings can be opened
  // long after that.
  onMount(refreshDevices);

  async function moveTo(d) {
    movingTo = d.id;
    try {
      await transfer(d.id);
      toast(`Playing on ${d.name}`, 'success');
    } catch {
      toast(`Could not move playback to ${d.name}`, 'error');
    } finally {
      movingTo = '';
    }
  }

  // System first: it is the default, and the note asked for a fresh install to
  // follow the device.
  const THEMES = [
    { id: 'system', label: 'System', icon: Smartphone },
    { id: 'light', label: 'Light', icon: Sun },
    { id: 'dark', label: 'Dark', icon: Moon },
  ];

  // --- versions --------------------------------------------------------------
  //
  // Compiled into the bundle by vite.config.js; the server's copy comes over the
  // wire. Showing both is the point: a service worker means this tab can be
  // running an older build than the one deployed, and on 2026-08-25 exactly that
  // sent a diagnosis after code the browser was not executing.
  const clientVersion = __MH_VERSION__;
  const clientCommit = __MH_COMMIT__;
  let server = null;

  $: mismatch =
    server && server.commit !== 'unknown' && clientCommit !== 'unknown'
      ? server.commit !== clientCommit
      : false;

  const short = (sha) => (sha && sha !== 'unknown' ? sha.slice(0, 7) : '—');

  onMount(async () => {
    try {
      server = await api.serverVersion();
    } catch {
      server = null; // an unreachable server is already obvious elsewhere
    }
  });

  // --- app updates -----------------------------------------------------------
  //
  // Android only: a PWA updates itself. The APK is the newest GitHub release's
  // `.apk` asset.
  let current = null;
  let latest = null;
  let installing = false;

  $: updateAvailable = latest?.ready && current && isNewer(latest.version, current);

  async function loadUpdate() {
    if (!isNative) return;
    current = await installedVersion();
    try {
      latest = await latestRelease();
    } catch {
      latest = null; // no release configured is not an error worth showing
    }
  }

  async function runUpdate() {
    installing = true;
    try {
      await installUpdate(latest.url);
      toast('Downloaded - Android will ask you to confirm');
    } catch (err) {
      toast(err.message || 'Could not download the update', 'error');
    } finally {
      installing = false;
    }
  }

  // Builds older than the in-app updater cannot install anything themselves;
  // hand them the file and let Android's own download flow take over.
  function downloadManually() {
    window.open(latest.url, '_system');
  }

  export let user = null;
  export let onBack = () => {};

  let settings = null;
  let store = null;
  let loading = true;
  let error = null;

  // ListenBrainz state
  let showTokenInput = false;
  let token = '';
  let saving = false;
  let tokenError = null;

  /** Bytes as the shortest sensible unit; storage figures are unreadable raw. */
  function human(bytes) {
    if (!Number.isFinite(bytes) || bytes < 0) return '—';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let n = bytes;
    while (n >= 1024 && i < u.length - 1) {
      n /= 1024;
      i++;
    }
    return `${n >= 10 || i === 0 ? Math.round(n) : n.toFixed(1)} ${u[i]}`;
  }

  onMount(async () => {
    // Independent of the rest: a failed walk of the library must not stop the
    // account section from rendering.
    api
      .storage()
      .then((v) => (store = v))
      .catch(() => (store = null));
    loadUpdate(); // independent of settings; must not block or fail the screen
    try {
      settings = await api.settings();
    } catch (err) {
      error = err.message || 'Could not load settings';
    } finally {
      loading = false;
    }
  });

  async function unlinkNavidrome() {
    if (!window.confirm('Unlink your Navidrome account? You will have to enter the password again.'))
      return;
    try {
      await api.unlink();
      window.location.reload();
    } catch (err) {
      toast(err.message || 'Could not unlink', 'error');
    }
  }

  async function saveListenBrainz() {
    if (!token) return;
    saving = true;
    tokenError = null;
    try {
      settings = await api.setListenBrainz(token);
      showTokenInput = false;
      token = '';
    } catch (err) {
      if (err.status === 400) {
        tokenError = 'That token was not accepted by ListenBrainz';
      } else if (err.status === 502) {
        tokenError = 'ListenBrainz could not be reached and the token was not changed.';
      } else {
        tokenError = err.message || 'Could not save token';
      }
    } finally {
      saving = false;
    }
  }

  async function disconnectListenBrainz() {
    if (!window.confirm('Disconnect ListenBrainz? We will use the server default if one is configured.'))
      return;
    saving = true;
    try {
      settings = await api.deleteListenBrainz();
      showTokenInput = false;
      token = '';
    } catch (err) {
      toast(err.message || 'Could not disconnect', 'error');
    } finally {
      saving = false;
    }
  }

  const { downloads: savedTracks } = saved;
  let savedBytes = 0;
  let clearingSaved = false;
  function refreshSavedUsage() {
    saved.usage().then((u) => {
      savedBytes = u.bytes;
    });
  }
  refreshSavedUsage();
  $: if ($savedTracks) refreshSavedUsage();
  const savedMB = (n) => (n >= 1e9 ? `${(n / 1e9).toFixed(1)} GB` : `${Math.round(n / 1e6)} MB`);

  async function clearSaved() {
    clearingSaved = true;
    try {
      await saved.clear();
      toast('Offline music removed');
    } finally {
      clearingSaved = false;
    }
  }
</script>

<div class="absolute inset-0 z-30 bg-slate-950 overflow-y-auto overscroll-contain">
  <div class="sticky top-0 z-10 flex items-center gap-1 bg-slate-950/95 backdrop-blur px-2 h-14">
    <button
      on:click={onBack}
      aria-label="Back"
      class="w-11 h-11 shrink-0 flex items-center justify-center text-slate-300"
    >
      <ChevronLeft class="w-6 h-6" />
    </button>
    <h1 class="text-sm font-medium">Settings</h1>
  </div>

  <div class="px-4 py-4 space-y-8 pb-12">
    <!-- Which device is making sound. Also reachable from the full player, but
         asked for here on 2026-08-24: the player only exists while something is
         playing, and "move the sound over there" is a thing you want to do
         before pressing play, not after. -->
    <section>
      <h2 class="text-sm font-semibold mb-3 text-slate-200">Playback device</h2>
      <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-4 space-y-3">
        <p class="text-xs text-slate-400">
          One device makes sound; the rest control it. Sound only moves when you move it here, or
          when the device playing disappears.
        </p>
        <ul class="space-y-1">
          {#each $devices as d (d.id)}
            <li>
              <button
                on:click={() => moveTo(d)}
                disabled={d.id === $activeId || movingTo === d.id}
                class="w-full flex items-center gap-3 rounded-lg px-3 py-2.5 text-left border {d.id ===
                $activeId
                  ? 'border-sky-800 bg-sky-950/40'
                  : 'border-slate-800 active:bg-slate-900'}"
              >
                <Volume2
                  class="w-4 h-4 shrink-0 {d.id === $activeId ? 'text-sky-400' : 'text-slate-500'}"
                />
                <span class="flex-1 min-w-0">
                  <span
                    class="block text-sm truncate {d.id === $activeId
                      ? 'text-sky-200'
                      : 'text-slate-200'}"
                  >
                    {d.name}{d.id === deviceId ? ' (this device)' : ''}
                  </span>
                  <span class="block text-[10px] text-slate-500">
                    {d.id === $activeId ? 'Making sound' : 'Tap to play here'}
                  </span>
                </span>
                {#if movingTo === d.id}
                  <span class="text-[10px] text-slate-400">moving…</span>
                {/if}
              </button>
            </li>
          {/each}
          {#if !$devices.length}
            <li class="text-xs text-slate-500">No devices are signed in yet.</li>
          {/if}
        </ul>
        {#if !$connected}
          <p class="text-[11px] text-amber-200">
            Not connected to the server, so this list may be out of date.
          </p>
        {/if}
      </div>
    </section>

    <!-- Appearance. Unconditional, unlike the App section below it, which appears
         only on Android and only when an update is actually waiting. -->
    <section>
      <h2 class="text-sm font-semibold mb-3 text-slate-200">Appearance</h2>
      <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-4 space-y-3">
        <div class="grid grid-cols-3 gap-2" role="radiogroup" aria-label="Theme">
          {#each THEMES as opt}
            <button
              role="radio"
              aria-checked={$theme === opt.id}
              on:click={() => setTheme(opt.id)}
              class="h-16 rounded-lg border flex flex-col items-center justify-center gap-1.5
                     text-[11px] font-medium active:scale-[0.98] transition-transform
                     {$theme === opt.id
                ? 'bg-sky-500 border-sky-500 text-white'
                : 'border-slate-700 text-slate-300'}"
            >
              <svelte:component this={opt.icon} class="w-4 h-4" />
              {opt.label}
            </button>
          {/each}
        </div>
        <p class="text-[11px] text-slate-500">
          {#if $theme === 'system'}
            Following your device, currently {$resolved}. It moves with the phone, including
            on a schedule if you have one set.
          {:else}
            Always {$theme}, whatever the device is set to.
          {/if}
        </p>
      </div>
    </section>

    <!-- App updates: Android only, and only when a release is actually there -->
    {#if isNative && latest?.ready}
      <section>
        <h2 class="text-sm font-semibold mb-3 text-slate-200">App</h2>
        <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-4 space-y-3">
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <p class="text-[13px] text-slate-300">
                Installed <span class="text-slate-100 font-medium">{current || '—'}</span>
              </p>
              <p class="text-[11px] text-slate-500">
                Latest <span class="text-slate-300">{latest.version}</span>
                {#if latest.size}· {(latest.size / 1048576).toFixed(1)} MB{/if}
              </p>
            </div>
            {#if !updateAvailable}
              <span class="shrink-0 flex items-center gap-1 text-[11px] text-emerald-400">
                <CheckCircle2 class="w-3.5 h-3.5" /> Up to date
              </span>
            {/if}
          </div>

          {#if updateAvailable}
            <button
              on:click={canSelfUpdate ? runUpdate : downloadManually}
              disabled={installing}
              class="w-full h-11 rounded-lg bg-sky-500 text-white text-sm font-medium
                     flex items-center justify-center gap-2 active:scale-[0.99] disabled:opacity-60"
            >
              {#if installing}
                <Spinner /> Downloading…
              {:else}
                <Download class="w-4 h-4" />
                {canSelfUpdate ? `Update to ${latest.version}` : `Download ${latest.version}`}
              {/if}
            </button>
            <p class="text-[11px] text-slate-500">
              {#if canSelfUpdate}
                Android will ask you to confirm. The download is removed once it installs.
              {:else}
                This build predates in-app updates, so it downloads the file instead.
                Install it once and the button updates the app directly from then on.
              {/if}
            </p>
          {/if}
        </div>
      </section>
    {/if}

    <!-- Server: Android only. The app asked for this address on first launch. -->
    {#if canChangeServer}
      <section>
        <h2 class="text-sm font-semibold mb-3 text-slate-200">Server</h2>
        <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-4 space-y-3">
          <p class="text-[13px] text-slate-300 break-all">{window.location.origin}</p>
          <button
            on:click={changeServer}
            class="w-full h-11 rounded-lg border border-slate-700 text-slate-200 text-sm font-medium
                   active:scale-[0.99]"
          >
            Change server
          </button>
        </div>
      </section>
    {/if}

    <!-- Version: which build is answering, and which one this tab is running.
         Asked for on 2026-08-25, after a service-worker-cached bundle made a
         bug look unfixed for twenty minutes. -->
    <section>
      <h2 class="text-sm font-semibold mb-3 text-slate-200">Version</h2>
      <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-4 space-y-3">
        <div class="flex items-baseline justify-between gap-3">
          <span class="text-[13px] text-slate-400">This app</span>
          <span class="text-sm font-medium text-slate-100">
            {clientVersion}
            <span class="text-[11px] text-slate-500 font-normal tabular-nums">
              · {short(clientCommit)}
            </span>
          </span>
        </div>
        <div class="flex items-baseline justify-between gap-3">
          <span class="text-[13px] text-slate-400">Server</span>
          <span class="text-sm font-medium {mismatch ? 'text-amber-400' : 'text-slate-100'}">
            {server ? server.version : '—'}
            <span class="text-[11px] font-normal tabular-nums {mismatch
                ? 'text-amber-500/80'
                : 'text-slate-500'}">
              · {short(server?.commit)}
            </span>
          </span>
        </div>
        {#if server?.builtAt}
          <div class="flex items-baseline justify-between gap-3">
            <span class="text-[13px] text-slate-400">Built</span>
            <span class="text-[11px] text-slate-500 tabular-nums">
              {new Date(server.builtAt).toLocaleString()}
            </span>
          </div>
        {/if}

        {#if mismatch}
          <!-- Not a warning about the server: this tab is the stale one, because
               the server is what it just answered with. -->
          <p class="text-[11px] text-amber-400">
            This tab is running an older build than the server. Reload to pick up the
            current one.
          </p>
          <button
            on:click={hardUpdate}
            class="w-full h-11 rounded-lg bg-sky-500 text-white text-sm font-medium
                   flex items-center justify-center active:scale-[0.99]"
          >
            Reload
          </button>
        {:else if server}
          <p class="text-[11px] text-slate-500">This tab is running the deployed build.</p>
        {/if}
      </div>
    </section>

    <!-- Offline music. Deliberately OUTSIDE the {#if store} guard below: that
         block needs server data, and the whole point of this one is that it
         still works when there is no server to ask. -->
    <section>
      <h2 class="text-sm font-semibold mb-3 text-slate-200">Offline music</h2>
      <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-4 space-y-3">
        <p class="text-xs text-slate-400">
          Songs kept on this device. They play with no connection, and the app opens straight to
          them when it cannot reach the server. Use Offline on any song, album or playlist.
        </p>
        <div class="flex items-center gap-2 text-sm">
          <CloudOff class="w-4 h-4 text-slate-500" />
          <span class="tabular-nums">{$savedTracks.length}</span>
          <span class="text-slate-500">songs · {savedMB(savedBytes)}</span>
        </div>
        <button
          on:click={clearSaved}
          disabled={clearingSaved || !$savedTracks.length}
          class="w-full h-11 rounded-lg border border-slate-800 text-xs font-medium text-slate-300
                 active:bg-slate-900 disabled:opacity-40"
        >
          {clearingSaved ? 'Removing…' : 'Remove all offline music'}
        </button>
      </div>
    </section>

    <!-- Storage: what this account costs, and what the disk has left -->
    {#if store}
      <section>
        <h2 class="text-sm font-semibold mb-3 text-slate-200">Storage</h2>
        <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-4 space-y-4">
          <div class="flex items-baseline justify-between gap-3">
            <span class="text-[13px] text-slate-400">Your downloads</span>
            <span class="text-sm font-medium text-slate-100">
              {human(store.you.bytes)}
              <span class="text-[11px] text-slate-500 font-normal">· {store.you.tracks} tracks</span>
            </span>
          </div>
          <div class="flex items-baseline justify-between gap-3">
            <span class="text-[13px] text-slate-400">All music on the server</span>
            <span class="text-sm text-slate-300">
              {human(store.everyone.bytes)}
              <span class="text-[11px] text-slate-500">· {store.everyone.tracks} tracks</span>
            </span>
          </div>

          {#if store.disk.total > 0}
            <div class="pt-1 space-y-2">
              <!--
                Three segments, not two: your music, everything else already on
                the disk, and what is free. Showing only "music vs free" would
                imply the rest of the disk is available when it is not.
              -->
              <div class="h-2 rounded-full overflow-hidden bg-slate-800 flex">
                <div
                  class="h-full bg-sky-500"
                  style="width: {(store.you.bytes / store.disk.total) * 100}%"
                ></div>
                <div
                  class="h-full bg-slate-500"
                  style="width: {(Math.max(0, store.disk.used - store.you.bytes) /
                    store.disk.total) *
                    100}%"
                ></div>
              </div>
              <div class="flex items-center justify-between text-[11px]">
                <span class="flex items-center gap-1.5 text-slate-400">
                  <span class="w-2 h-2 rounded-full bg-sky-500"></span> You
                  <span class="w-2 h-2 rounded-full bg-slate-500 ml-2"></span> Everything else
                </span>
                <span class="text-slate-300">
                  <span class="text-emerald-400 font-medium">{human(store.disk.free)}</span> free
                  of {human(store.disk.total)}
                </span>
              </div>
            </div>
          {/if}
        </div>
      </section>
    {/if}

    <!-- Account Block -->
    <section>
      <h2 class="text-sm font-semibold mb-3 text-slate-200">Account</h2>
      <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-4 space-y-4">
        <p class="text-[13px] text-slate-400">
          Signed in as <span class="text-slate-200 font-medium">{user?.displayName || user?.username || '—'}</span>
          {#if user?.navidromeUser}
            <br />
            Navidrome <span class="text-slate-200 font-medium">{user.navidromeUser}</span>
          {/if}
        </p>
        <div class="flex gap-2">
          <button
            on:click={api.logout}
            class="flex-1 h-11 rounded-lg bg-slate-800 text-xs font-medium text-slate-200 flex items-center justify-center gap-2 active:bg-slate-700 transition-colors"
          >
            <LogOut class="w-4 h-4" /> Sign out
          </button>
          <button
            on:click={unlinkNavidrome}
            class="flex-1 h-11 rounded-lg border border-slate-700 text-xs font-medium text-slate-300 flex items-center justify-center gap-2 active:bg-slate-800 transition-colors"
          >
            <Unlink class="w-4 h-4" /> Unlink Navidrome
          </button>
        </div>
      </div>
    </section>

    <!-- ListenBrainz Block -->
    <section>
      <h2 class="text-sm font-semibold mb-3 text-slate-200">ListenBrainz</h2>
      <div class="rounded-xl border border-slate-800 bg-slate-900/30 p-4">
        {#if loading}
          <div class="py-4"><Spinner label="Loading settings…" /></div>
        {:else if error}
          <p class="text-xs text-red-400">{error}</p>
        {:else if settings}
          <div class="space-y-4">
            {#if settings.listenBrainzTokenSet}
              <div class="text-[13px] text-slate-300 space-y-1">
                <p>Connected as <span class="font-medium text-slate-100">{settings.listenBrainzUser}</span></p>
                <p class="text-slate-500 font-mono tracking-widest">••••••••</p>
              </div>
            {:else if settings.source === 'server'}
              <p class="text-[13px] text-slate-400 leading-relaxed">
                A server-wide default is in use. Setting a personal token will override it.
              </p>
            {:else}
              <p class="text-[13px] text-slate-400 leading-relaxed">
                Discovery recommendations are personal, so without your own token you get someone else's taste.
              </p>
            {/if}

            {#if showTokenInput}
              <div class="space-y-3 pt-2">
                <div>
                  <input
                    type="password"
                    autocomplete="off"
                    bind:value={token}
                    placeholder="User token"
                    class="w-full h-12 px-3 rounded-lg bg-slate-950 border border-slate-700 text-sm text-slate-200 focus:border-sky-500 focus:outline-none placeholder:text-slate-600"
                    disabled={saving}
                  />
                  {#if tokenError}
                    <p class="mt-2 text-xs text-red-400">{tokenError}</p>
                  {/if}
                </div>
                <div class="flex gap-2">
                  <button
                    on:click={saveListenBrainz}
                    disabled={saving || !token}
                    class="flex-1 h-10 rounded-lg bg-sky-600 text-white text-xs font-medium active:bg-sky-500 disabled:opacity-50 transition-colors"
                  >
                    {saving ? 'Saving…' : 'Save'}
                  </button>
                  <button
                    on:click={() => { showTokenInput = false; tokenError = null; }}
                    disabled={saving}
                    class="flex-1 h-10 rounded-lg border border-slate-700 text-slate-300 text-xs font-medium active:bg-slate-800 disabled:opacity-50 transition-colors"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            {:else}
              <div class="flex gap-2 pt-1">
                <button
                  on:click={() => showTokenInput = true}
                  class="flex-1 h-10 rounded-lg border border-slate-700 text-slate-200 text-xs font-medium active:bg-slate-800 transition-colors"
                >
                  Change token
                </button>
                {#if settings.listenBrainzTokenSet}
                  <button
                    on:click={disconnectListenBrainz}
                    class="flex-1 h-10 rounded-lg border border-slate-700 text-red-400 text-xs font-medium active:bg-slate-800 transition-colors"
                  >
                    Disconnect
                  </button>
                {/if}
              </div>
            {/if}
            
            <p class="text-[12px] pt-1">
              <a
                href="https://listenbrainz.org/settings/"
                target="_blank"
                rel="noopener noreferrer"
                class="text-sky-400 hover:underline"
              >
                Find your token on ListenBrainz
              </a>
            </p>
          </div>
        {/if}
      </div>
    </section>
  </div>
</div>
