<script>
  import { onMount } from 'svelte';
  import { ChevronRight, Star, Plus } from 'lucide-svelte';
  import AlbumCard from '../components/AlbumCard.svelte';
  import SearchField from '../components/SearchField.svelte';
  import Artwork from '../components/Artwork.svelte';
  import PlaylistSheet from '../components/PlaylistSheet.svelte';
  import { longpress } from '../lib/longpress.js';
  import TrackList from '../components/TrackList.svelte';
  import Spinner from '../components/Spinner.svelte';
  import * as api from '../lib/api.js';
  import { openArtist, openPlaylist, openBook } from '../lib/nav.js';
  import { toast } from '../lib/toast.js';
  import { libraryChanged } from '../lib/library-events.js';

  const TABS = [
    { id: 'playlists', label: 'Playlists' },
    { id: 'artists', label: 'Artists' },
    { id: 'albums', label: 'Albums' },
    { id: 'starred', label: 'Starred' },
    { id: 'books', label: 'Books' },
  ];

  const SORTS = [
    { id: 'alphabeticalByName', label: 'A–Z' },
    { id: 'newest', label: 'Newest' },
    { id: 'frequent', label: 'Most played' },
    { id: 'recent', label: 'Recently played' },
    { id: 'starred', label: 'Starred' },
    { id: 'random', label: 'Random' },
  ];

  const PAGE = 60;

  let active = 'playlists';
  /**
   * Filters the tab currently selected, nothing else.
   *
   * Local, not another round trip: every one of these lists is already in
   * memory, and the whole point of the box is narrowing what is on screen. The
   * term deliberately survives a tab change - the same search re-applied to
   * artists after playlists is usually what you meant.
   */
  let q = '';

  const has = (needle, ...fields) =>
    fields.filter(Boolean).some((f) => String(f).toLowerCase().includes(needle));

  $: needle = q.trim().toLowerCase();
  $: shownPlaylists = !needle ? playlists : playlists?.filter((p) => has(needle, p.name));
  $: shownArtists = !needle ? artists : artists?.filter((a) => has(needle, a.name));
  $: shownAlbums = !needle ? albums : albums.filter((a) => has(needle, a.name, a.artist));
  $: shownStarred = !needle
    ? starred
    : starred && {
        tracks: starred.tracks?.filter((t) => has(needle, t.title, t.artist, t.album)),
        albums: starred.albums?.filter((a) => has(needle, a.name, a.artist)),
        artists: starred.artists?.filter((a) => has(needle, a.name)),
      };

  $: shownBooks = !needle ? books : books?.filter((b) => has(needle, b.title, b.author));

  $: tabLabel = (TABS.find((t) => t.id === active)?.label || '').toLowerCase();
  let loading = false;
  let error = null;

  let playlists = null;
  /** Holding a playlist row opens rename/delete, the same gesture a track row uses. */
  let sheetFor = null;
  let creatingPlaylist = false;
  let newPlaylistName = '';
  let creatingBusy = false;

  async function handleCreatePlaylist() {
    const name = newPlaylistName.trim();
    if (!name || creatingBusy) return;
    creatingBusy = true;
    try {
      const pl = await api.createPlaylist(name, []);
      playlists = [pl, ...(playlists || [])];
      toast(`Created “${name}”`, 'success');
      creatingPlaylist = false;
      newPlaylistName = '';
    } catch (err) {
      toast(err.message || 'Could not create the playlist', 'error');
    } finally {
      creatingBusy = false;
    }
  }
  let artists = null;
  let starred = null;

  let albums = [];
  let books = null;
  let albumSort = 'alphabeticalByName';
  let albumsLoaded = false;
  let moreAlbums = false;
  let loadingMore = false;

  async function ensure(which) {
    if (loading) return;
    error = null;
    try {
      if (which === 'playlists' && !playlists) {
        loading = true;
        playlists = (await api.playlists()) || [];
      } else if (which === 'artists' && !artists) {
        loading = true;
        artists = (await api.artists()) || [];
      } else if (which === 'starred' && !starred) {
        loading = true;
        starred = await api.starred();
      } else if (which === 'books' && !books) {
        loading = true;
        books = (await api.books()) || [];
      } else if (which === 'albums' && !albumsLoaded) {
        loading = true;
        await loadAlbums(true);
      }
    } catch (err) {
      error = err.message || 'Could not load your library';
    } finally {
      loading = false;
    }
  }

  async function loadAlbums(reset) {
    if (reset) {
      albums = [];
      moreAlbums = false;
    }
    const page = await api.albums({ type: albumSort, size: PAGE, offset: albums.length });
    const rows = page || [];
    albums = reset ? rows : [...albums, ...rows];
    // Navidrome reshuffles type=random on every call, so paging it would repeat
    // and skip albums. One page is the only honest answer for that sort.
    moreAlbums = rows.length === PAGE && albumSort !== 'random';
    albumsLoaded = true;
  }

  async function changeSort(id) {
    albumSort = id;
    loading = true;
    error = null;
    try {
      await loadAlbums(true);
    } catch (err) {
      error = err.message || 'Could not load albums';
    } finally {
      loading = false;
    }
  }

  async function more() {
    if (loadingMore) return;
    loadingMore = true;
    try {
      await loadAlbums(false);
    } catch (err) {
      toast(err.message || 'Could not load more albums', 'error');
    } finally {
      loadingMore = false;
    }
  }

  function select(id) {
    active = id;
    ensure(id);
  }

  onMount(() => ensure(active));

  /**
   * Drop cached lists when something is added to or removed from the library.
   *
   * This screen is hidden rather than unmounted on a tab switch, so each list
   * loads once per session. Without this, a book deleted from its own screen is
   * still sitting on the shelf when you navigate back - the delete looks like it
   * did nothing.
   *
   * `first` skips the initial store read, which would otherwise clear the very
   * lists onMount is about to populate.
   */
  let first = true;
  $: if ($libraryChanged >= 0) {
    if (first) {
      first = false;
    } else {
      books = null;
      albums = [];
      albumsLoaded = false;
      ensure(active);
    }
  }

</script>

<div class="px-4 py-4 space-y-4">
  <h1 class="text-xl font-semibold">Library</h1>

  <div class="flex gap-1.5 overflow-x-auto no-scrollbar -mx-4 px-4">
    {#each TABS as t}
      <button
        on:click={() => select(t.id)}
        class="shrink-0 h-11 px-4 rounded-xl text-xs font-medium border transition-colors {active ===
        t.id
          ? 'bg-sky-500 border-sky-500 text-white'
          : 'bg-slate-900/60 border-slate-800 text-slate-300'}"
      >
        {t.label}
      </button>
    {/each}
  </div>

  <SearchField bind:value={q} placeholder="Search your {tabLabel}…" label="Search the library" />

  {#if error}
    <p class="text-xs text-red-300">{error}</p>
  {/if}

  {#if loading}
    <div class="py-16"><Spinner label="Loading" /></div>
  {:else if active === 'playlists'}
    <div class="space-y-3">
      {#if creatingPlaylist}
        <form on:submit|preventDefault={handleCreatePlaylist} class="flex gap-2">
          <!-- svelte-ignore a11y-autofocus -->
          <input
            bind:value={newPlaylistName}
            autofocus
            placeholder="Playlist name"
            aria-label="New playlist name"
            class="flex-1 h-12 px-3 rounded-xl bg-slate-900 border border-slate-800 text-sm
                   text-slate-100 placeholder:text-slate-600 focus:outline-none focus:border-sky-700"
          />
          <button
            type="submit"
            disabled={!newPlaylistName.trim() || creatingBusy}
            class="h-12 px-4 rounded-xl bg-sky-600 text-white text-sm font-medium
                   disabled:opacity-50 active:bg-sky-700 flex items-center justify-center min-w-[70px]"
          >
            {#if creatingBusy}<Spinner class="w-4 h-4" />{:else}Create{/if}
          </button>
          <button
            type="button"
            on:click={() => {
              creatingPlaylist = false;
              newPlaylistName = '';
            }}
            class="h-12 px-3 rounded-xl border border-slate-800 text-slate-400 text-sm active:bg-slate-900"
          >
            Cancel
          </button>
        </form>
      {:else}
        <button
          on:click={() => (creatingPlaylist = true)}
          class="w-full h-12 rounded-xl border border-dashed border-slate-700 flex items-center
                 justify-center gap-2 text-sm text-slate-300 active:bg-slate-900"
        >
          <Plus class="w-4 h-4" /> New playlist
        </button>
      {/if}

      {#if shownPlaylists?.length}
        <ul class="divide-y divide-slate-900">
          {#each shownPlaylists as p (p.id)}
            <li>
              <button
                use:longpress
                on:longpress={() => (sheetFor = p)}
                on:click={() => openPlaylist(p)}
                class="w-full flex items-center gap-3 py-2.5 text-left active:bg-slate-900/50 rounded-lg"
              >
                <Artwork src={p.coverUrl} alt="" class="w-12 h-12 rounded-lg shrink-0" />
                <span class="min-w-0 flex-1">
                  <span class="block text-sm truncate">{p.name}</span>
                  <span class="block text-[11px] text-slate-500 truncate">
                    {p.songCount}
                    {p.songCount === 1 ? 'track' : 'tracks'}{p.owner ? ` · ${p.owner}` : ''}
                  </span>
                </span>
                <ChevronRight class="w-4 h-4 text-slate-600 shrink-0" />
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="py-16 text-center text-xs text-slate-500">
          {needle ? `No playlists match “${q}”.` : 'No playlists yet.'}
        </p>
      {/if}
    </div>
  {:else if active === 'artists'}
    {#if shownArtists?.length}
      <ul class="divide-y divide-slate-900">
        {#each shownArtists as a (a.id)}
          <li>
            <button
              on:click={() => openArtist(a)}
              class="w-full flex items-center gap-3 py-2.5 text-left active:bg-slate-900/50 rounded-lg"
            >
              <Artwork src={a.coverUrl} alt="" class="w-12 h-12 rounded-full shrink-0" />
              <span class="min-w-0 flex-1">
                <span class="block text-sm truncate">{a.name}</span>
                <span class="block text-[11px] text-slate-500">
                  {a.albumCount}
                  {a.albumCount === 1 ? 'album' : 'albums'}
                </span>
              </span>
              <ChevronRight class="w-4 h-4 text-slate-600 shrink-0" />
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="py-16 text-center text-xs text-slate-500">
        {needle ? `No artists match “${q}”.` : 'No artists yet.'}
      </p>
    {/if}
  {:else if active === 'albums'}
    <div class="flex gap-1.5 overflow-x-auto no-scrollbar -mx-4 px-4">
      {#each SORTS as s}
        <button
          on:click={() => changeSort(s.id)}
          class="shrink-0 h-11 px-3 rounded-lg text-[11px] font-medium border {albumSort === s.id
            ? 'border-sky-500/60 text-sky-300 bg-sky-500/10'
            : 'border-slate-800 text-slate-400'}"
        >
          {s.label}
        </button>
      {/each}
    </div>

    {#if shownAlbums.length}
      <div class="grid grid-cols-2 sm:grid-cols-3 gap-3">
        {#each shownAlbums as album (album.id)}
          <AlbumCard {album} width="w-full" />
        {/each}
      </div>
      {#if moreAlbums}
        <button
          on:click={more}
          class="w-full h-12 rounded-xl border border-slate-800 text-xs font-medium text-slate-300 active:bg-slate-900"
        >
          {loadingMore ? 'Loading…' : 'Load more'}
        </button>
      {/if}
    {:else}
      <p class="py-16 text-center text-xs text-slate-500">
        {#if needle}
          No albums match “{q}”{moreAlbums ? ' in the ones loaded so far' : ''}.
        {:else}
          No albums for this sort.
        {/if}
      </p>
      {#if needle && moreAlbums}
        <!--
          Albums arrive a page at a time, so a filter can only see what has been
          fetched. Saying "no matches" and hiding the button would be a lie of
          omission; the way to widen the search is to load more.
        -->
        <button
          on:click={more}
          class="w-full h-12 rounded-xl border border-slate-800 text-xs font-medium text-slate-300 active:bg-slate-900"
        >
          {loadingMore ? 'Loading…' : 'Load more albums to search'}
        </button>
      {/if}
    {/if}
  {:else if active === 'books'}
    {#if shownBooks?.length}
      <ul class="divide-y divide-slate-900">
        {#each shownBooks as b (b.id)}
          {@const pct = b.durationSec ? Math.min(100, (b.listenedSec / b.durationSec) * 100) : 0}
          <button
            on:click={() => openBook(b)}
            class="w-full flex items-center gap-3 py-2.5 text-left active:bg-slate-900/60"
          >
            <Artwork src={b.coverUrl} alt="" class="w-14 h-14 rounded-lg shrink-0" />
            <div class="min-w-0 flex-1">
              <p class="text-sm font-medium truncate">{b.title}</p>
              <p class="text-xs text-slate-400 truncate">{b.author}</p>
              <!-- A book is a thing you are partway through, so the list leads with
                   where you are rather than how long it is. A shelf of bare titles
                   makes you open each one to find the one you were reading. -->
              {#if b.progress}
                <div class="mt-1.5 flex items-center gap-2">
                  <div class="h-1 flex-1 rounded-full bg-slate-900 overflow-hidden">
                    <div class="h-full bg-sky-500" style="width: {pct}%"></div>
                  </div>
                  <span class="text-[11px] tabular-nums text-sky-300/90 shrink-0">
                    {Math.round(pct)}%
                  </span>
                </div>
              {:else}
                <p class="text-[11px] text-slate-500 mt-0.5">
                  {b.chapterCount} chapters · not started
                </p>
              {/if}
            </div>
            <ChevronRight class="w-4 h-4 text-slate-600 shrink-0" />
          </button>
        {/each}
      </ul>
    {:else if books}
      <!-- `books` is null until the fetch resolves. Guarding on it rather than on
           the filtered list stops the tab claiming the shelf is empty in the gap
           before it has been read. -->
      <p class="py-16 text-center text-xs text-slate-500">
        {#if needle}
          No books match “{q}”.
        {:else}
          No audiobooks yet. Download one from Search and it will appear here.
        {/if}
      </p>
    {/if}
  {:else if active === 'starred'}
    {#if shownStarred?.tracks?.length || shownStarred?.albums?.length || shownStarred?.artists?.length}
      {#if shownStarred.albums?.length}
        <section>
          <h2 class="text-sm font-semibold mb-2.5">Albums</h2>
          <div class="flex gap-3 overflow-x-auto no-scrollbar -mx-4 px-4 pb-1">
            {#each shownStarred.albums as album (album.id)}
              <AlbumCard {album} />
            {/each}
          </div>
        </section>
      {/if}
      {#if shownStarred.artists?.length}
        <section>
          <h2 class="text-sm font-semibold mb-1.5">Artists</h2>
          <ul class="divide-y divide-slate-900">
            {#each shownStarred.artists as a (a.id)}
              <li>
                <button
                  on:click={() => openArtist(a)}
                  class="w-full flex items-center gap-3 py-2.5 text-left active:bg-slate-900/50 rounded-lg"
                >
                  <Artwork src={a.coverUrl} alt="" class="w-11 h-11 rounded-full shrink-0" />
                  <span class="text-sm truncate flex-1">{a.name}</span>
                  <ChevronRight class="w-4 h-4 text-slate-600 shrink-0" />
                </button>
              </li>
            {/each}
          </ul>
        </section>
      {/if}
      {#if shownStarred.tracks?.length}
        <section>
          <h2 class="text-sm font-semibold mb-1.5">Songs</h2>
          <TrackList tracks={shownStarred.tracks} />
        </section>
      {/if}
    {:else}
      <div class="py-16 flex flex-col items-center gap-2 text-center">
        <Star class="w-6 h-6 text-slate-700" />
        <p class="text-xs text-slate-500">
          {needle ? `Nothing starred matches “${q}”.` : 'Nothing starred yet.'}
        </p>
      </div>
    {/if}
  {/if}

</div>

<PlaylistSheet
  playlist={sheetFor}
  on:close={() => (sheetFor = null)}
  on:renamed={(e) => (playlists = playlists.map((p) => (p.id === e.detail.id ? e.detail : p)))}
  on:deleted={(e) => (playlists = playlists.filter((p) => p.id !== e.detail.id))}
/>
