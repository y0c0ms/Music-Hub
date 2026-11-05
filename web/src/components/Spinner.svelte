<script>
  import { Loader2 } from 'lucide-svelte';
  export let label = '';
  let klass = 'w-5 h-5';
  export { klass as class };
</script>

<div class="flex items-center justify-center gap-2 text-slate-500">
  <Loader2 class="{klass} animate-spin" />
  {#if label}<span class="text-xs">{label}</span>{/if}
</div>
