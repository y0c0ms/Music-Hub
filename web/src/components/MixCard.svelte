<script>
  /**
   * A one-tap mix. The point of this control is that music starts without the
   * user reading anything: pick the card, audio plays, put the phone away.
   *
   * It loads its own tracks on tap rather than up front. Preloading five mixes
   * on every home screen render would be five list fetches for four the user
   * never touches.
   */
  import Spinner from './Spinner.svelte';

  export let title;
  export let subtitle = '';
  export let icon;
  export let tint = 'sky';
  /** @type {() => Promise<any[]>} resolves the tracks to play */
  export let load;
  /** @type {(tracks: any[]) => void} */
  export let start;

  let busy = false;
  let empty = false;

  const tints = {
    sky: 'from-sky-500/20 to-sky-500/5 border-sky-500/30 text-sky-300',
    violet: 'from-violet-500/20 to-violet-500/5 border-violet-500/30 text-violet-300',
    emerald: 'from-emerald-500/20 to-emerald-500/5 border-emerald-500/30 text-emerald-300',
    amber: 'from-amber-500/20 to-amber-500/5 border-amber-500/30 text-amber-300',
  };

  async function go() {
    if (busy) return;
    busy = true;
    empty = false;
    try {
      const tracks = await load();
      if (!tracks?.length) {
        empty = true;
        return;
      }
      start(tracks);
    } catch {
      empty = true;
    } finally {
      busy = false;
    }
  }
</script>

<button
  on:click={go}
  disabled={busy}
  class="relative h-24 rounded-xl border bg-gradient-to-br p-3 text-left flex flex-col justify-between
         active:scale-[0.98] transition-transform disabled:opacity-60 {tints[tint] || tints.sky}"
>
  <svelte:component this={icon} class="w-5 h-5" />
  <div class="min-w-0">
    <p class="text-sm font-semibold text-slate-100 truncate">{title}</p>
    <p class="text-[11px] text-slate-400 truncate">
      {empty ? 'Nothing here yet' : subtitle}
    </p>
  </div>

  {#if busy}
    <div class="absolute inset-0 grid place-items-center rounded-xl bg-slate-950/60">
      <Spinner />
    </div>
  {/if}
</button>
