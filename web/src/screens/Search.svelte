<script>
  import { onDestroy, onMount } from 'svelte';
  import { AlertTriangle, Check, Download, Loader2, Play } from 'lucide-svelte';
  import AlbumCard from '../components/AlbumCard.svelte';
  import AcquireAlbumCard from '../components/AcquireAlbumCard.svelte';
  import AcquireAlbumSheet from '../components/AcquireAlbumSheet.svelte';
  import BookResultSheet from '../components/BookResultSheet.svelte';
  import SearchField from '../components/SearchField.svelte';
  import TrackList from '../components/TrackList.svelte';
  import Artwork from '../components/Artwork.svelte';
  import Spinner from '../components/Spinner.svelte';
  import * as api from '../lib/api.js';
  import { isAlbumDone, isAlbumBusy } from '../lib/albumState.js';
  import { play } from '../lib/player.js';
  import { fmtTime, fmtPlays } from '../lib/format.js';
  import { openArtist, setTab, openAlbum as navToAlbum, tab } from '../lib/nav.js';
  import { toast } from '../lib/toast.js';
  import { noteLibraryChanged } from '../lib/library-events.js';

  let q = '';
  let results = null;
  let searching = false;
  let error = null;

  // The internet half of the answer shells out to yt-dlp and arrives seconds
  // after the local half, so it gets its own state and never holds up the
  // library results.
  let acquirable = [];
  let acqLoading = false;
  let acqError = '';

  // Albums are a THIRD request, not part of acquirable: finding them needs a
  // YouTube Music search plus a resolve per album, so it lands well after the
  // track list and must not hold it up.
  let albums = [];
  let albLoading = false;
  let albError = '';
  // playlistId -> true while its tracks are being queued, and -> true once done.
  let queueing = {};
  // The album whose track list is open, plus per-track queue state so a single
  // song off an album can be taken without the other twelve.
  let openAlbum = null;
  let queuingTrack = {};

  // Audiobooks are a FOURTH request against different sources (YouTube, LibriVox,
  // media.ccc.de) with their own job queue, so like albums they get their own
  // state and never hold up the track list.
  let bookResults = [];
  let bookLoading = false;
  let bookError = '';
  // "<source>:<id>" -> job id. The id alone is not unique - two sources can use
  // the same one - so the row is keyed by source and id together.
  let bookReq = {};
  let bookJobs = [];
  $: bookJobById = Object.fromEntries(bookJobs.map((j) => [j.id, j]));
  const bookKey = (b) => `${b.source}:${b.id}`;
  // The book whose detail sheet is open. Tapping a row opens it; the download
  // icon on the row still works without opening anything.
  let openBook = null;
  // Human length for the row - "10h 18m", not fmtTime's "10:18:00", which reads
  // as a timestamp. Two readings of the same book differ mainly by length, so
  // this is a primary distinguisher, not decoration.
  const bookLen = (sec) => {
    if (!sec) return '';
    const m = Math.round(sec / 60);
    return m < 60 ? `${m} min` : `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`;
  };

  async function getTrack(track) {
    if (queuingTrack[track.sourceId] || trackJobs[track.sourceId]) return;
    queuingTrack = { ...queuingTrack, [track.sourceId]: true };
    try {
      const job = await api.acquire(track);
      jobs = [...jobs.filter((j) => j.id !== job.id), job];
      // Record the JOB, not a "queued" boolean. The flag never learned the
      // download had finished, so every track sat on "queued" forever - which is
      // exactly what the main list avoids by reading job state live.
      requested = { ...requested, [track.sourceId]: job.id };
      // Kick the poll: it stops itself when idle, so a download started from a
      // quiet screen would otherwise sit on its initial "queued" forever, never
      // learning it had progressed. refreshJobs re-arms the 2s cycle while busy.
      startPolling();
      toast(`Queued ${track.title}`);
    } catch (err) {
      toast(err?.message || 'Could not queue that track', 'error');
    } finally {
      const next = { ...queuingTrack };
      delete next[track.sourceId];
      queuingTrack = next;
    }
  }

  // Songs or albums, not both. Albums cost a YouTube Music search PLUS a resolve
  // per album, so running them on every keystroke would be wasteful when most
  // searches are for a track. Making it an explicit choice means the expensive
  // lookup only happens when it is actually wanted.
  let onlineMode = 'songs';

  function setMode(m) {
    if (m === onlineMode) return;
    onlineMode = m;
    const term = q.trim();
    if (term) run(term);
  }

  let debounce;
  let ctrl;
  // Responses can land out of order on a flaky phone connection; only the newest
  // query is allowed to write to the results.
  let seq = 0;

  let jobs = [];
  /** sourceId -> job id, so a row can find its own job in the shared list. */
  let requested = {};
  let poll;

  function onQuery() {
    clearTimeout(debounce);
    ctrl?.abort();
    const term = q.trim();
    if (!term) {
      seq++;
      results = null;
      acquirable = [];
      acqError = '';
      acqLoading = false;
      albums = [];
      albError = '';
      albLoading = false;
      bookResults = [];
      bookError = '';
      bookLoading = false;
      searching = false;
      error = null;
      return;
    }
    searching = true;
    acqLoading = true;
    debounce = setTimeout(() => run(term), 300);
  }

  function run(term) {
    const mine = ++seq;
    ctrl = new AbortController();
    const signal = ctrl.signal;

    api
      .search(term, signal)
      .then((r) => {
        if (mine !== seq) return;
        results = r;
        error = null;
      })
      .catch((err) => {
        if (err?.name === 'AbortError' || mine !== seq) return;
        error = err.message || 'Search failed';
        results = null;
      })
      .finally(() => {
        if (mine === seq) searching = false;
      });

    if (onlineMode === 'albums') {

    albLoading = true;
    api
      .acquireAlbums(term, signal)
      .then((rows) => {
        if (mine !== seq) return;
        albums = rows || [];
        albError = '';
      })
      .catch((err) => {
        if (err?.name === 'AbortError' || mine !== seq) return;
        albums = [];
        albError = err.message || 'Could not search for albums';
      })
      .finally(() => {
        if (mine === seq) albLoading = false;
      });

    }

    if (onlineMode === 'songs') {

    api
      .acquireSearch(term, signal)
      .then((rows) => {
        if (mine !== seq) return;
        acquirable = rows || [];
        acqError = '';
      })
      .catch((err) => {
        if (err?.name === 'AbortError' || mine !== seq) return;
        acquirable = [];
        // "Nothing matched" and "the lookup broke" are different facts, and the
        // user is entitled to know which one happened.
        acqError = err.message || 'Could not reach the download search';
      })
      .finally(() => {
        if (mine === seq) acqLoading = false;
      });

    }

    if (onlineMode === 'books') {

    bookLoading = true;
    api
      .booksSearch(term, signal)
      .then((rows) => {
        if (mine !== seq) return;
        bookResults = rows || [];
        bookError = '';
      })
      .catch((err) => {
        if (err?.name === 'AbortError' || mine !== seq) return;
        bookResults = [];
        bookError = err.message || 'Could not search audiobooks';
      })
      .finally(() => {
        if (mine === seq) bookLoading = false;
      });

    }
  }

  /**
   * Queue every track on an album.
   *
   * Sequential, not Promise.all: each POST starts a yt-dlp download server-side,
   * and firing fourteen at once would saturate the box and make the whole queue
   * slower than doing them in order. A partial failure keeps what already queued
   * rather than rolling back - those downloads are real and useful.
   */
  async function getAlbum(album) {
    if (queueing[album.playlistId]) return;
    queueing = { ...queueing, [album.playlistId]: true };
    let queued = 0;
    let failed = 0;
    try {
      for (const track of album.tracks || []) {
        try {
          const job = await api.acquire({ ...track, album: album.title, albumArtist: album.artist });
          jobs = [...jobs.filter((j) => j.id !== job.id), job];
          requested = { ...requested, [track.sourceId]: job.id };
          queued += 1;
        } catch {
          failed += 1;
        }
      }
      // One kick after the batch: the gate releases these two at a time, so most
      // sit at "queued" for a while - the poll is what walks them to done.
      if (queued) startPolling();
      if (!queued) toast('Could not queue this album', 'error');
      else if (failed) toast(`Queued ${queued} of ${queued + failed} tracks`);
      else toast(`Queued ${queued} tracks from ${album.title}`);
    } finally {
      const next = { ...queueing };
      delete next[album.playlistId];
      queueing = next;
    }
  }

  async function getBook(b) {
    const key = bookKey(b);
    if (bookReq[key]) return;
    try {
      const job = await api.booksGet(b);
      bookJobs = [job, ...bookJobs.filter((j) => j.id !== job.id)];
      bookReq = { ...bookReq, [key]: job.id };
      startPolling();
      toast(`Queued ${b.title}`);
    } catch (err) {
      toast(err?.message || 'Could not queue that book', 'error');
    }
  }

  // --- downloads -------------------------------------------------------------

  $: jobById = Object.fromEntries(jobs.map((j) => [j.id, j]));
  // sourceId -> live job, so the album sheet shows the same truth as the list
  // above it rather than its own stale idea of "queued".
  $: trackJobs = Object.fromEntries(
    Object.entries(requested)
      .map(([sourceId, jobId]) => [sourceId, jobById[jobId]])
      .filter(([, job]) => job),
  );
  // Card and sheet must agree about an album, so they share one implementation
  // (lib/albumState.js, tested in mock/album-state.test.mjs).
  $: albumDone = (al) => isAlbumDone(al, trackJobs);
  $: albumBusy = (al) => isAlbumBusy(al, trackJobs);

  // The "Album downloaded" tick opens the finished album in the library. The
  // sheet only knows the YouTube id; the library album_id comes from a track
  // that finished - its resolved song carries albumId.
  async function openDownloadedAlbum(al) {
    const done = (al?.tracks || [])
      .map((tr) => trackJobs[tr.sourceId])
      .find((j) => j?.state === 'done' && j.songId);
    openAlbum = null;
    setTab('library');
    if (!done) return;
    try {
      const s = await api.song(done.songId);
      if (s?.albumId) navToAlbum({ id: s.albumId, name: al.title });
    } catch {
      // stay on the library tab; the album is there even if the lookup hiccuped
    }
  }

  $: active = jobs.filter((j) => j.state === 'queued' || j.state === 'downloading');
  // Job ids whose track has already been announced, so a track is announced once
  // rather than on every 2s poll for as long as the job stays in the list.
  const announced = new Set();

  async function refreshJobs() {
    try {
      jobs = (await api.jobs()) || [];
      // A job gaining a songId is the exact moment the file became a real,
      // playable track. Anything showing the library is now out of date.
      for (const j of jobs) {
        if (j.state === 'done' && j.songId && !announced.has(j.id)) {
          announced.add(j.id);
          noteLibraryChanged();
        }
      }
    } catch {
      // A failed poll is not worth interrupting a search over; the next tick
      // either succeeds or the 401 handler takes over the whole screen.
    }
    // Book downloads have their own queue and their own endpoint. Only bother the
    // server for it once at least one book has actually been requested.
    if (Object.keys(bookReq).length) {
      try {
        bookJobs = (await api.booksJobs()) || [];
        for (const j of bookJobs) {
          if (j.state === 'done' && j.bookId && !announced.has(j.id)) {
            announced.add(j.id);
            noteLibraryChanged();
          }
        }
      } catch {
        // Same reasoning as above.
      }
    }
    // Read the freshly assigned array rather than the `active` reactive value,
    // which has not been recomputed yet at this point in the tick.
    // A finished job is not the end of the story: its songId arrives once the
    // library scan indexes the file, and that is what turns the tick into a
    // play button. Keep polling for it, but only briefly - the server stops
    // looking after its own deadline, so waiting longer would poll forever.
    const busy =
      jobs.some((j) => j.state === 'queued' || j.state === 'downloading') ||
      bookJobs.some((j) => j.state === 'queued' || j.state === 'downloading');
    const indexing = jobs.some((j) => j.state === 'done' && !j.songId);
    if (indexing && !waitingSince) waitingSince = Date.now();
    if (!indexing) waitingSince = 0;
    const stillWorthWaiting = indexing && Date.now() - waitingSince < INDEX_WAIT;
    if (busy || stillWorthWaiting) startPolling();
    else stopPolling();
  }

  function startPolling() {
    if (poll) return;
    poll = setInterval(refreshJobs, 2000);
  }

  function stopPolling() {
    clearInterval(poll);
    poll = null;
  }

  // Matches the server's own give-up window for indexing a new download.
  const INDEX_WAIT = 60_000;
  let waitingSince = 0;

  // Play a track the moment it exists, rather than making the user go Home and
  // pull to refresh. The job carries the id the scan produced; the track itself
  // is read back so the player gets real tags and artwork.
  // The download finished but the scan has not indexed it, so there is no track
  // to play yet. Home is where it will appear, and it will not reload on its own:
  // tabs stay mounted, so Home's onMount already ran. Announce the change first
  // so the refresh is in flight by the time the tab is on screen.
  function goHomeAndRefresh(a) {
    noteLibraryChanged();
    setTab('home');
    toast(`"${a.title}" is downloaded - refreshing your library`, 'success');
  }

  async function playDownloaded(job, a) {
    try {
      const track = await api.song(job.songId);
      play([track], 0);
    } catch (err) {
      toast(err.message || `Could not play "${a.title}" yet`, 'error');
    }
  }

  async function download(a) {
    try {
      const job = await api.acquire(a);
      if (job?.id) {
        requested = { ...requested, [a.sourceId]: job.id };
        jobs = [job, ...jobs.filter((j) => j.id !== job.id)];
      }
      startPolling();
      toast(`Downloading "${a.title}"`, 'success');
    } catch (err) {
      toast(err.message || 'Could not start the download', 'error');
    }
  }

  onMount(() => {
    refreshJobs();
    // Tabs stay mounted, so switching to Search never re-runs onMount. Re-fetch
    // the shared job list each time this tab becomes active, so a download
    // started elsewhere (e.g. Complete album) shows in the queue here too.
    return tab.subscribe((t) => {
      if (t === 'search') refreshJobs();
    });
  });
  onDestroy(() => {
    clearTimeout(debounce);
    ctrl?.abort();
    stopPolling();
  });
</script>

<div class="px-4 py-4 space-y-5">
  <SearchField
    bind:value={q}
    on:input={onQuery}
    placeholder="Songs, albums, artists…"
  />

  {#if active.length}
    <section class="rounded-xl border border-slate-800 bg-slate-900/40 p-3 space-y-2">
      <p class="text-[11px] uppercase tracking-wider text-slate-500">
        Downloading · {active.length}
      </p>
      {#each active as j (j.id)}
        <div class="flex items-center gap-2.5">
          <Loader2 class="w-3.5 h-3.5 text-sky-400 animate-spin shrink-0" />
          <span class="text-xs truncate flex-1">{j.title}</span>
          <span class="text-[10px] uppercase tracking-wide text-slate-500 shrink-0">{j.state}</span>
        </div>
      {/each}
    </section>
  {/if}

  {#if error}
    <p class="text-xs text-red-300">{error}</p>
  {/if}

  {#if searching && !results}
    <div class="py-16"><Spinner label="Searching your library" /></div>
  {/if}

  {#if results}
    {@const emptyLocal =
      !results.tracks?.length && !results.albums?.length && !results.artists?.length}

    {#if results.tracks?.length}
      <section>
        <h2 class="text-sm font-semibold mb-1.5">Songs</h2>
        <TrackList tracks={results.tracks} />
      </section>
    {/if}

    {#if results.albums?.length}
      <section>
        <h2 class="text-sm font-semibold mb-2.5">Albums</h2>
        <div class="flex gap-3 overflow-x-auto no-scrollbar -mx-4 px-4 pb-1">
          {#each results.albums as album (album.id)}
            <AlbumCard {album} />
          {/each}
        </div>
      </section>
    {/if}

    {#if results.artists?.length}
      <section>
        <h2 class="text-sm font-semibold mb-1.5">Artists</h2>
        <ul class="divide-y divide-slate-900">
          {#each results.artists as a (a.id)}
            <li>
              <button
                on:click={() => openArtist(a)}
                class="w-full flex items-center gap-3 py-2.5 text-left active:bg-slate-900/50 rounded-lg"
              >
                <Artwork src={a.coverUrl} alt="" class="w-11 h-11 rounded-full shrink-0" />
                <span class="min-w-0 flex-1">
                  <span class="block text-sm truncate">{a.name}</span>
                  <span class="block text-[11px] text-slate-500">
                    {a.albumCount}
                    {a.albumCount === 1 ? 'album' : 'albums'}
                  </span>
                </span>
              </button>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if emptyLocal}
      <p class="pt-8 text-center text-xs text-slate-500">Nothing in your library matches “{q}”.</p>
    {/if}
  {/if}

  {#if q.trim()}
    <!-- Kept visually separate on purpose: everything below this line is not on
         the server yet and pressing Download changes what is on disk. -->
    <section class="pt-3 border-t border-dashed border-slate-800">
      <h2 class="text-sm font-semibold">Not in your library</h2>
      <p class="text-[11px] text-slate-500 mt-0.5 mb-2.5">
        Searched online. Downloading adds the file to the server for everyone.
      </p>

      <!-- Songs or albums. An explicit choice, because the album lookup is a
           YouTube Music search plus a resolve per album and is not worth running
           for someone who is looking for one track. -->
      <div class="flex gap-1 p-0.5 mb-3 rounded-lg bg-slate-900/70 border border-slate-800 w-fit">
        {#each [['songs', 'Songs'], ['albums', 'Albums'], ['books', 'Audiobooks']] as [mode, label] (mode)}
          <button
            on:click={() => setMode(mode)}
            aria-pressed={onlineMode === mode}
            class="px-3 h-8 rounded-md text-xs font-medium transition-colors {onlineMode === mode
              ? 'bg-slate-100 text-slate-900'
              : 'text-slate-400 active:bg-slate-800'}"
          >
            {label}
          </button>
        {/each}
      </div>
      {#if onlineMode === 'albums'}
        {#if albLoading}
          <div class="grid grid-cols-2 gap-3" aria-busy="true">
            {#each [0, 1] as i (i)}
              <div>
                <div class="w-full aspect-square rounded-xl bg-slate-900 animate-pulse"></div>
                <div class="h-3 w-2/3 mt-2 rounded bg-slate-900 animate-pulse"></div>
              </div>
            {/each}
          </div>
          <p class="mt-2 text-[11px] text-slate-600">Asking YouTube Music — albums take a few seconds.</p>
        {:else if albError}
          <p class="flex items-start gap-2 text-[11px] text-red-300">
            <AlertTriangle class="w-3.5 h-3.5 mt-0.5 shrink-0" />
            <span>{albError}</span>
          </p>
        {:else if albums.length}
          <div class="grid grid-cols-2 gap-3">
            {#each albums as al (al.playlistId)}
              <AcquireAlbumCard
                album={al}
                queuing={!!queueing[al.playlistId] || albumBusy(al)}
                done={albumDone(al)}
                onGet={getAlbum}
                onOpen={(a) => (openAlbum = a)}
              />
            {/each}
          </div>
        {:else}
          <p class="text-[11px] text-slate-500">No albums found for “{q}”.</p>
        {/if}
      {:else if onlineMode === 'books'}
        {#if bookLoading}
          <ul class="space-y-2" aria-busy="true">
            {#each [0, 1, 2] as i (i)}
              <li class="flex items-center gap-3 rounded-xl border border-slate-800/70 p-2">
                <div class="w-12 h-12 rounded-lg bg-slate-900 animate-pulse shrink-0"></div>
                <div class="flex-1 space-y-1.5">
                  <div class="h-3 w-2/3 rounded bg-slate-900 animate-pulse"></div>
                  <div class="h-2.5 w-1/3 rounded bg-slate-900 animate-pulse"></div>
                </div>
              </li>
            {/each}
          </ul>
          <p class="mt-2 text-[11px] text-slate-600">
            Searching YouTube, LibriVox and media.ccc.de — a few seconds.
          </p>
        {:else if bookError}
          <p class="flex items-start gap-2 text-[11px] text-red-300">
            <AlertTriangle class="w-3.5 h-3.5 mt-0.5 shrink-0" />
            <span>{bookError}</span>
          </p>
        {:else if bookResults.length}
          <ul class="space-y-2">
            {#each bookResults as b (bookKey(b))}
              {@const job = bookJobById[bookReq[bookKey(b)]]}
              <li class="flex items-center gap-2 rounded-xl border border-slate-800 bg-slate-900/40 p-2">
                <!-- Tapping the artwork or text opens the detail sheet; the row is
                     a button so a truncated title can always be seen in full
                     before deciding. The download icon stays separate for the
                     one-tap case. -->
                <button
                  on:click={() => (openBook = b)}
                  class="flex items-center gap-3 min-w-0 flex-1 text-left active:opacity-70"
                >
                  <Artwork src={b.thumb} alt="" class="w-12 h-12 rounded-lg shrink-0" />
                  <span class="min-w-0 flex-1">
                    <span class="block text-sm truncate">{b.title}</span>
                    <span class="block text-[11px] text-slate-400 truncate">
                      {b.author || 'Unknown author'}{b.durationSec
                        ? ` · ${bookLen(b.durationSec)}`
                        : ''}{b.views ? ` · ${fmtPlays(b.views)}` : ''}
                    </span>
                    <span class="block text-[10px] text-slate-600 truncate">{b.detail || b.source}</span>
                    {#if job && (job.state === 'downloading' || job.state === 'queued')}
                      <span class="block text-[10px] mt-0.5 text-sky-400 truncate">
                        {job.detail || job.state}{job.percent ? ` · ${job.percent}%` : ''}
                      </span>
                    {:else if job?.state === 'failed'}
                      <span class="block text-[10px] mt-0.5 text-red-300 truncate"
                        >{job.detail || 'Download failed'}</span
                      >
                    {/if}
                  </span>
                </button>

                {#if job?.state === 'done'}
                  <span
                    class="w-11 h-11 shrink-0 flex items-center justify-center text-emerald-400"
                    aria-label="Added to the library"
                  >
                    <Check class="w-5 h-5" />
                  </span>
                {:else if job && (job.state === 'downloading' || job.state === 'queued')}
                  <span class="w-11 h-11 shrink-0 flex items-center justify-center text-slate-400">
                    <Loader2 class="w-5 h-5 animate-spin" />
                  </span>
                {:else}
                  <button
                    on:click={() => getBook(b)}
                    aria-label="Download {b.title}"
                    class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg border border-slate-700 text-slate-200 active:bg-slate-800"
                  >
                    <Download class="w-5 h-5" />
                  </button>
                {/if}
              </li>
            {/each}
          </ul>
        {:else}
          <p class="text-[11px] text-slate-500">No audiobooks found for “{q}”.</p>
        {/if}
      {:else}
      {#if acqLoading}
        <ul class="space-y-2" aria-busy="true">
          {#each [0, 1, 2] as i (i)}
            <li class="flex items-center gap-3 rounded-xl border border-slate-800/70 p-2">
              <div class="w-12 h-12 rounded-lg bg-slate-900 animate-pulse shrink-0"></div>
              <div class="flex-1 space-y-1.5">
                <div class="h-3 w-2/3 rounded bg-slate-900 animate-pulse"></div>
                <div class="h-2.5 w-1/3 rounded bg-slate-900 animate-pulse"></div>
              </div>
            </li>
          {/each}
        </ul>
        <p class="mt-2 text-[11px] text-slate-600">Asking yt-dlp — this takes a second or two.</p>
      {:else if acqError}
        <p class="flex items-start gap-2 text-[11px] text-red-300">
          <AlertTriangle class="w-3.5 h-3.5 mt-0.5 shrink-0" />
          <span>{acqError}</span>
        </p>
      {:else if acquirable.length}
        <ul class="space-y-2">
          {#each acquirable as a (a.sourceId)}
            {@const job = jobById[requested[a.sourceId]]}
            <li
              class="flex items-center gap-3 rounded-xl border border-slate-800 bg-slate-900/40 p-2"
            >
              <Artwork src={a.thumb} alt="" class="w-12 h-12 rounded-lg shrink-0" />
              <div class="min-w-0 flex-1">
                <p class="text-sm truncate">{a.title}</p>
                <p class="text-[11px] text-slate-500 truncate">
                  {a.artist || 'Unknown artist'}{a.duration ? ` · ${fmtTime(a.duration)}` : ''}
                </p>
                {#if job?.detail}
                  <p
                    class="text-[10px] mt-0.5 truncate {job.state === 'failed'
                      ? 'text-red-300'
                      : 'text-slate-500'}"
                  >
                    {job.detail}
                  </p>
                {/if}
              </div>

              {#if job?.state === 'done' && job.songId}
                <button
                  on:click={() => playDownloaded(job, a)}
                  aria-label="Play {a.title}"
                  class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg border border-emerald-800 text-emerald-400 active:bg-emerald-950/40"
                >
                  <Play class="w-5 h-5" />
                </button>
              {:else if job?.state === 'done'}
                <!-- On disk, but the library scan has not indexed it yet, so
                     there is still nothing to PLAY - a play button here would
                     404. It is not nothing to tap, though: this used to be an
                     inert <span>, which left the download looking finished and
                     the tick looking broken. Taking the user Home and refreshing
                     is the action that actually pays off, because the track
                     appears there the moment the scan catches up. -->
                <button
                  on:click={() => goHomeAndRefresh(a)}
                  aria-label="Downloaded {a.title} - go to your library"
                  title="Downloaded - open your library"
                  class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg border border-emerald-800 text-emerald-400 active:bg-emerald-950/40"
                >
                  <Check class="w-5 h-5" />
                </button>
              {:else if job?.state === 'failed'}
                <button
                  on:click={() => download(a)}
                  aria-label="Retry download"
                  class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg border border-red-900 text-red-300"
                >
                  <AlertTriangle class="w-4 h-4" />
                </button>
              {:else if job}
                <span
                  class="w-11 h-11 shrink-0 flex items-center justify-center text-sky-400"
                  title={job.state}
                >
                  <Loader2 class="w-5 h-5 animate-spin" />
                </span>
              {:else}
                <button
                  on:click={() => download(a)}
                  aria-label="Download {a.title}"
                  class="w-11 h-11 shrink-0 flex items-center justify-center rounded-lg border border-slate-700 text-sky-400 active:bg-slate-800"
                >
                  <Download class="w-5 h-5" />
                </button>
              {/if}
            </li>
          {/each}
        </ul>
      {:else}
        <p class="text-[11px] text-slate-600">Nothing found online for “{q}”.</p>
      {/if}

      {/if}
    </section>
  {:else if !searching && !error}
    <p class="py-16 text-center text-xs text-slate-500">
      Search your library — and everything that is not in it yet.
    </p>
  {/if}
</div>

<AcquireAlbumSheet
  album={openAlbum}
  {queuingTrack}
  {trackJobs}
  albumQueuing={!!(openAlbum && queueing[openAlbum.playlistId])}
  onGetTrack={getTrack}
  onGetAlbum={getAlbum}
  onOpenAlbum={openDownloadedAlbum}
  onClose={() => (openAlbum = null)}
/>

<BookResultSheet
  book={openBook}
  job={openBook ? bookJobById[bookReq[bookKey(openBook)]] : null}
  onGet={getBook}
  onClose={() => (openBook = null)}
/>
