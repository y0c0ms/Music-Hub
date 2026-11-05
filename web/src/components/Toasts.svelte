<script>
  import { fly } from 'svelte/transition';
  import { AlertTriangle, Check, Info, X } from 'lucide-svelte';
  import { toasts, dismiss } from '../lib/toast.js';

  const style = {
    error: 'bg-red-950/95 border-red-800 text-red-100',
    success: 'bg-emerald-950/95 border-emerald-800 text-emerald-100',
    info: 'bg-slate-900/95 border-slate-700 text-slate-100',
  };
  const icon = { error: AlertTriangle, success: Check, info: Info };
</script>

<!-- Above the player and the tab bar: a failed delete must never be hidden. -->
<div class="fixed inset-x-0 top-0 z-50 pt-safe pointer-events-none">
  <div class="px-3 pt-3 space-y-2">
    {#each $toasts as t (t.id)}
      <div
        in:fly={{ y: -16, duration: 180 }}
        out:fly={{ y: -16, duration: 150 }}
        class="pointer-events-auto flex items-start gap-2.5 rounded-xl border px-3 py-2.5 shadow-lg shadow-black/40 backdrop-blur {style[
          t.kind
        ] || style.info}"
        role={t.kind === 'error' ? 'alert' : 'status'}
      >
        <svelte:component this={icon[t.kind] || Info} class="w-4 h-4 mt-0.5 shrink-0" />
        <p class="text-xs leading-relaxed flex-1 break-words">{t.message}</p>
        <button
          on:click={() => dismiss(t.id)}
          aria-label="Dismiss"
          class="-m-2 p-2 shrink-0 opacity-70 active:opacity-100"
        >
          <X class="w-4 h-4" />
        </button>
      </div>
    {/each}
  </div>
</div>
