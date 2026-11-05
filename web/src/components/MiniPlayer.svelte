<script>
  import { Pause, Play, SkipBack, SkipForward } from 'lucide-svelte';
  import Artwork from './Artwork.svelte';
  import { player, progress, next, prev, toggle, seek, offline } from '../lib/player.js';

  export let onExpand = () => {};

  // While a finger is down the bar follows the finger, not the audio clock:
  // committing every input event would fight the element's own timeupdate and
  // make the strip jitter backwards under the thumb.
  let scrubbing = false;
  let scrubValue = 0;

  $: track = $player.track;
  $: duration = $progress.duration || track?.duration || 0;
  $: shown = scrubbing ? scrubValue : $progress.time;
  $: pct = duration > 0 ? Math.min(100, (shown / duration) * 100) : 0;

  function onInput(e) {
    scrubbing = true;
    scrubValue = Number(e.currentTarget.value);
  }

  function onCommit() {
    if (!scrubbing) return;
    scrubbing = false;
    seek(scrubValue);
  }
</script>

{#if track}
  <div class="relative border-t border-slate-800/80 bg-slate-900/95 backdrop-blur">
    <!-- The seek strip straddles the top edge so it can be a 44px touch target
         without making the bar itself 44px taller. Only 3px of it is painted. -->
    <input
      type="range"
      class="seek seek-mini absolute inset-x-0 -top-5 h-11 w-full z-10"
      min="0"
      max={duration || 1}
      step="0.5"
      value={shown}
      aria-label="Seek"
      on:input={onInput}
      on:change={onCommit}
      style="background: linear-gradient(to right, rgb(var(--c-sky-400)) {pct}%, rgb(var(--c-slate-800)) {pct}%) no-repeat center / 100% 3px;"
    />

    <div class="flex items-center gap-2 px-2 h-16">
      <button
        on:click={onExpand}
        class="flex-1 min-w-0 flex items-center gap-3 h-16 text-left"
        aria-label="Open player"
      >
        <Artwork src={track.coverUrl} alt="" class="w-11 h-11 rounded-lg shrink-0" />
        <span class="min-w-0">
          <span class="block text-sm font-medium truncate">{track.title}</span>
          <!-- A dead stream used to be mentioned only inside Settings and the
               device picker, so the app looked completely healthy while none of
               its buttons could work. Reported 2026-08-25 as "it must be a
               problem with the connection" - which it was, and the app was the
               last thing that would tell you. -->
          <span
            class="block text-[11px] truncate {$player.error || $offline
              ? 'text-amber-400'
              : 'text-slate-400'}"
          >
            {#if $offline}Reconnecting…{:else}{$player.error || track.artist}{/if}
          </span>
        </span>
      </button>

      <button
        on:click={prev}
        aria-label="Previous"
        class="w-11 h-11 shrink-0 flex items-center justify-center text-slate-300 active:text-slate-100"
      >
        <SkipBack class="w-5 h-5 fill-current" />
      </button>
      <!-- slate-100, not white: this icon sits on the mini-player bar rather than
           on a filled button, so a fixed white would vanish in the light theme.
           slate-100 is the app's primary foreground and inverts with the theme. -->
      <button
        on:click={toggle}
        aria-label={$player.playing ? 'Pause' : 'Play'}
        class="w-11 h-11 shrink-0 flex items-center justify-center text-slate-100"
      >
        {#if $player.playing}
          <Pause class="w-6 h-6 fill-current" />
        {:else}
          <Play class="w-6 h-6 fill-current" />
        {/if}
      </button>
      <button
        on:click={() => next(true)}
        aria-label="Next"
        class="w-11 h-11 shrink-0 flex items-center justify-center text-slate-300 active:text-slate-100"
      >
        <SkipForward class="w-5 h-5 fill-current" />
      </button>
    </div>
  </div>
{/if}
