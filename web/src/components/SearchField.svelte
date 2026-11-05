<script>
  /**
   * The search input, shared by the Search screen and the Library.
   *
   * Extracted rather than copied so the two never drift: they are the same
   * control doing the same job, and a second hand-tuned copy of the focus ring
   * and clear-button geometry is a second thing to keep in sync.
   *
   * Deliberately uncontrolled beyond `value`: what a query *means* differs
   * completely between the two - one calls the server, the other filters a list
   * already on screen - so this owns the input and nothing else.
   */
  import { Search as SearchIcon, X } from 'lucide-svelte';
  import { createEventDispatcher } from 'svelte';

  export let value = '';
  export let placeholder = 'Search…';
  export let label = 'Search';

  const dispatch = createEventDispatcher();

  function onInput(e) {
    value = e.currentTarget.value;
    dispatch('input', value);
  }

  function clear() {
    value = '';
    dispatch('input', '');
    dispatch('clear');
  }
</script>

<div class="relative">
  <SearchIcon class="w-4 h-4 text-slate-500 absolute left-3 top-1/2 -translate-y-1/2" />
  <input
    {value}
    on:input={onInput}
    type="search"
    enterkeyhint="search"
    autocomplete="off"
    autocapitalize="none"
    autocorrect="off"
    {placeholder}
    aria-label={label}
    class="w-full h-12 bg-slate-900 border border-slate-800 rounded-xl pl-10 pr-11 text-sm placeholder:text-slate-600 focus:outline-none focus:border-sky-500/60"
  />
  {#if value}
    <button
      on:click={clear}
      aria-label="Clear search"
      class="absolute right-0 top-0 w-11 h-12 flex items-center justify-center text-slate-500"
    >
      <X class="w-4 h-4" />
    </button>
  {/if}
</div>
