<script>
  import { Home, Search, Sparkles, Library } from 'lucide-svelte';
  import { tab, setTab } from '../lib/nav.js';

  const TABS = [
    { id: 'home', label: 'Home', icon: Home },
    { id: 'search', label: 'Search', icon: Search },
    { id: 'discover', label: 'Discover', icon: Sparkles },
    { id: 'library', label: 'Library', icon: Library },
  ];
</script>

<nav class="border-t border-slate-800/80 bg-slate-950/95 backdrop-blur pb-safe">
  <div class="flex">
    {#each TABS as t}
      <button
        on:click={() => setTab(t.id)}
        aria-current={$tab === t.id ? 'page' : undefined}
        class="flex-1 h-14 flex flex-col items-center justify-center gap-1 transition-colors active:bg-slate-900/60 {$tab ===
        t.id
          ? 'text-sky-400'
          : 'text-slate-500'}"
      >
        <svelte:component this={t.icon} class="w-5 h-5" />
        <span class="text-[10px] font-medium leading-none">{t.label}</span>
      </button>
    {/each}
  </div>
</nav>
