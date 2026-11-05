<script>
  import { Music2 } from 'lucide-svelte';

  export let src = '';
  export let alt = '';
  /** Tailwind size/shape classes; the caller owns the box. */
  let klass = '';
  export { klass as class };

  let failed = false;
  $: if (src) failed = false;
</script>

{#if src && !failed}
  <img
    {src}
    {alt}
    loading="lazy"
    decoding="async"
    on:error={() => (failed = true)}
    class="{klass} object-cover bg-slate-900"
  />
{:else}
  <div
    class="{klass} bg-slate-900 border border-slate-800/80 flex items-center justify-center"
    aria-hidden="true"
  >
    <Music2 class="w-1/3 h-1/3 text-slate-500" />
  </div>
{/if}
