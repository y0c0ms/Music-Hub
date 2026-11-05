<script>
  import { AlertTriangle, Eye, EyeOff, Link2, Loader2, LogIn, Music2 } from 'lucide-svelte';
  import * as api from '../lib/api.js';

  /** 'signedOut' | 'unlinked' */
  export let mode;
  export let onLinked = () => {};

  let username = '';
  let password = '';
  let reveal = false;
  let busy = false;
  let problem = '';

  async function submit() {
    if (busy) return;
    if (!username.trim() || !password) {
      problem = 'Enter your Navidrome username and password.';
      return;
    }
    busy = true;
    problem = '';
    try {
      const user = await api.link(username.trim(), password);
      password = '';
      onLinked(user);
    } catch (err) {
      problem =
        err.message ||
        (err.code === 'navidrome_unreachable'
          ? 'Navidrome is not answering. Try again in a moment.'
          : 'Linking failed.');
    } finally {
      busy = false;
    }
  }
</script>

<div class="min-h-[100dvh] flex flex-col items-center justify-center px-6 py-10 pt-safe pb-safe">
  <div class="w-full max-w-sm space-y-6">
    <div class="flex flex-col items-center gap-3 text-center">
      <div
        class="w-14 h-14 rounded-2xl bg-sky-500/15 border border-sky-500/30 flex items-center justify-center"
      >
        <Music2 class="w-7 h-7 text-sky-400" />
      </div>
      <div>
        <h1 class="text-xl font-semibold">Music Hub</h1>
        <p class="text-xs text-slate-500 mt-1">
          {mode === 'signedOut'
            ? 'Your library, everywhere.'
            : 'One more step before the music starts.'}
        </p>
      </div>
    </div>

    {#if mode === 'signedOut'}
      <!-- A real navigation, not fetch: /auth/login 302s to Keycloak and an XHR
           would die on CORS. -->
      <a
        href={api.LOGIN_URL}
        class="w-full h-12 rounded-xl bg-sky-500 text-white text-sm font-semibold flex items-center justify-center gap-2 active:scale-[0.98] transition-transform"
      >
        <LogIn class="w-4 h-4" /> Sign in
      </a>
      <p class="text-[11px] text-slate-600 text-center">
        You will be sent to your identity provider and back again.
      </p>
    {:else}
      <form on:submit|preventDefault={submit} class="space-y-3">
        <p class="text-xs text-slate-400 leading-relaxed">
          Navidrome has no single sign-on, so link your account once and this server keeps the
          credential for you.
        </p>

        <label class="block">
          <span class="text-[11px] uppercase tracking-wider text-slate-500">Navidrome username</span
          >
          <input
            bind:value={username}
            autocomplete="username"
            autocapitalize="none"
            autocorrect="off"
            spellcheck="false"
            required
            class="mt-1 w-full h-12 bg-slate-900 border border-slate-800 rounded-xl px-3 text-sm focus:outline-none focus:border-sky-500/60"
          />
        </label>

        <label class="block">
          <span class="text-[11px] uppercase tracking-wider text-slate-500">Password</span>
          <span class="mt-1 relative block">
            <!-- Explicit value/on:input instead of bind:value: Svelte refuses a
                 dynamic `type` on a two-way bound input. -->
            <input
              value={password}
              on:input={(e) => (password = e.currentTarget.value)}
              type={reveal ? 'text' : 'password'}
              autocomplete="current-password"
              required
              class="w-full h-12 bg-slate-900 border border-slate-800 rounded-xl px-3 pr-12 text-sm focus:outline-none focus:border-sky-500/60"
            />
            <button
              type="button"
              on:click={() => (reveal = !reveal)}
              aria-label={reveal ? 'Hide password' : 'Show password'}
              class="absolute right-0 top-0 w-12 h-12 flex items-center justify-center text-slate-500"
            >
              {#if reveal}<EyeOff class="w-4 h-4" />{:else}<Eye class="w-4 h-4" />{/if}
            </button>
          </span>
        </label>

        {#if problem}
          <p class="flex items-start gap-2 text-[11px] text-red-300">
            <AlertTriangle class="w-3.5 h-3.5 mt-0.5 shrink-0" />
            <span>{problem}</span>
          </p>
        {/if}

        <button
          type="submit"
          disabled={busy}
          class="w-full h-12 rounded-xl bg-sky-500 text-white text-sm font-semibold flex items-center justify-center gap-2 disabled:opacity-60 active:scale-[0.98] transition-transform"
        >
          {#if busy}
            <Loader2 class="w-4 h-4 animate-spin" /> Linking…
          {:else}
            <Link2 class="w-4 h-4" /> Link Navidrome
          {/if}
        </button>
      </form>

      <button
        on:click={api.logout}
        class="w-full h-11 text-xs text-slate-500 active:text-slate-300">Sign out instead</button
      >
    {/if}
  </div>
</div>
