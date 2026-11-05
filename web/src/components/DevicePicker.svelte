<script>
  /**
   * Which device is making sound, and moving it.
   *
   * See docs/design/session-playback.md. One playback state per account; every
   * device renders it, one plays it.
   *
   * The honesty requirement here is the autoplay policy. A tab cannot start
   * audio without a prior user gesture *in that tab*, so "play on the desktop"
   * from a phone works only once the desktop has been interacted with since it
   * loaded. Rather than offer a button that silently does nothing, a device that
   * has not been seen recently is labelled as needing a tap on the device
   * itself. Spotify's web player has exactly the same constraint.
   */
  import { Laptop, Smartphone, Speaker, Volume2, X } from 'lucide-svelte';
  import { devices, deviceId, activeId, connected, transfer, refreshDevices } from '../lib/player.js';
  import { toast } from '../lib/toast.js';

  export let onClose = () => {};

  let busy = '';

  refreshDevices();

  // Anything quieter than this has almost certainly lost its stream, so a
  // transfer to it would land nowhere. The stream heartbeat is 20s.
  const STALE_MS = 70_000;

  function stale(d) {
    return Date.now() - Date.parse(d.lastSeen) > STALE_MS;
  }

  function icon(name) {
    const n = (name || '').toLowerCase();
    if (n.includes('app') || n.includes('android') || n.includes('ios') || n.includes('phone')) {
      return Smartphone;
    }
    if (n.includes('esp') || n.includes('speaker')) return Speaker;
    return Laptop;
  }

  async function pick(d) {
    if (d.id === $activeId) return;
    busy = d.id;
    try {
      await transfer(d.id);
      toast(`Playing on ${d.name}`, 'success');
    } catch {
      toast(`Could not move playback to ${d.name}`, 'error');
    } finally {
      busy = '';
    }
  }
</script>

<div class="fixed inset-0 z-50 flex items-end sm:items-center sm:justify-center">
  <button class="absolute inset-0 bg-black/60" on:click={onClose} aria-label="Close" />

  <div
    class="relative w-full sm:max-w-sm rounded-t-2xl sm:rounded-2xl border border-slate-800 bg-slate-950 p-4 pb-safe space-y-3"
  >
    <div class="flex items-center gap-2">
      <Volume2 class="w-4 h-4 text-sky-400 shrink-0" />
      <h2 class="flex-1 text-sm font-semibold">Playing on</h2>
      <button on:click={onClose} aria-label="Close" class="w-9 h-9 flex items-center justify-center text-slate-400">
        <X class="w-4 h-4" />
      </button>
    </div>

    {#if !$connected}
      <p class="text-[11px] leading-relaxed rounded-lg border border-amber-900/60 bg-amber-950/30 text-amber-200 px-2.5 py-2">
        Not connected to the server, so this list and the state above may be out of date.
      </p>
    {/if}

    {#if !$devices.length}
      <p class="text-xs text-slate-500 py-2">No other devices are signed in.</p>
    {/if}

    <ul class="space-y-1">
      {#each $devices as d (d.id)}
        {@const isMe = d.id === deviceId}
        {@const isActive = d.id === $activeId}
        {@const cold = stale(d) && !isMe}
        <li>
          <button
            on:click={() => pick(d)}
            disabled={isActive || busy === d.id}
            class="w-full flex items-center gap-3 rounded-xl px-3 py-2.5 text-left border {isActive
              ? 'border-sky-800 bg-sky-950/40'
              : 'border-slate-800 active:bg-slate-900'} disabled:opacity-100"
          >
            <svelte:component
              this={icon(d.name)}
              class="w-4 h-4 shrink-0 {isActive ? 'text-sky-400' : 'text-slate-400'}"
            />
            <span class="flex-1 min-w-0">
              <span class="block text-sm truncate {isActive ? 'text-sky-200' : 'text-slate-200'}">
                {d.name}{isMe ? ' (this device)' : ''}
              </span>
              {#if isActive}
                <span class="block text-[10px] uppercase tracking-wider text-sky-500">Making sound</span>
              {:else if cold}
                <!-- Not a failure, and not hidden: transferring here would be a
                     button that does nothing, because the tab needs its own
                     gesture before it may start audio. -->
                <span class="block text-[10px] text-slate-500">Asleep — open it and tap play there</span>
              {:else}
                <span class="block text-[10px] text-slate-500">Tap to play here</span>
              {/if}
            </span>
            {#if busy === d.id}
              <span class="text-[10px] text-slate-400">moving…</span>
            {/if}
          </button>
        </li>
      {/each}
    </ul>

    <p class="text-[11px] leading-relaxed text-slate-500">
      One device plays; the rest control it. Pausing from a remote does not move the sound.
    </p>
  </div>
</div>
