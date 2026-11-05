<script>
  import { onDestroy, onMount } from 'svelte';
  import {
    AlertTriangle,
    Check,
    ChevronRight,
    Download,
    ListMusic,
    Loader2,
    Pause,
    Play,
    RefreshCw,
    Trash2,
    X,
  } from 'lucide-svelte';
  import AcquireAlbumSheet from '../components/AcquireAlbumSheet.svelte';
  import Artwork from '../components/Artwork.svelte';
  import Spinner from '../components/Spinner.svelte';
  import * as api from '../lib/api.js';
  import * as pv from '../lib/preview.js';
  import { swipeable } from '../lib/swipe.js';
  import { fmtTime } from '../lib/format.js';
  import { preview } from '../lib/preview.js';
  import { drop } from '../lib/player.js';
  import { openAlbum as openAlbumNav, setTab, tab } from '../lib/nav.js';
  import { noteLibraryChanged } from '../lib/library-events.js';
  import { toast } from '../lib/toast.js';

  let cards = [];
  let idx = 0;
  let loading = true;
  let error = null;
  let fetching = false;

  /** Set while a card animates off screen so the next one is not draggable yet. */
  let flying = null;
  let dragging = false;
  let startX = 0;
  let dx = 0;
  let dy = 0;

  /** An owned dislike destroys a file, so it asks first. */
  let confirmDelete = null;

  const THRESHOLD = 96;

  // Two modes: new releases from your artists (default), and the swipe deck.
  let mode = 'new';
  let releases = [];
  let relLoading = false;
  let relError = null;
  let deckLoaded = false;
  let lastReleasesLoad = 0;
  let queuing = {}; // release key -> 'downloading' | 'done'
  let previewingRel = null; // relKey currently auditioned in New releases
  let resolvingRel = null; // relKey whose preview source is being resolved
  let relSwipingId = null; // relKey of the row mid-swipe
  let relDx = 0;
  let relDragging = false;
  const REL_COMMIT = 72;

  // The tracklist of an opened release. Resolved from the server on tap, so it
  // is a separate sheet rather than something the New Releases list carries:
  // the nightly sweep knows each tracklist (it needs it to decide whether the
  // release is already owned) but does not keep it, and putting it in the list
  // payload would make every scroll carry 300 releases' worth of tracks.
  let relSheet = null; // the AcquireAlbum being shown
  let relSheetLoading = false;
  let relSheetFor = null; // relKey of the row being resolved, for its spinner
  let relSheetError = null;

  async function openRelTracks(r) {
    if (relSheetLoading) return;
    // Only a YouTube-sourced release has a playlist id to resolve. The rest
    // come from ListenBrainz, where the release is keyed by MBID and has no
    // public playlist behind it, so there is nothing to list.
    if (!r.mbid || !r.mbid.startsWith('OLAK5uy_')) {
      toast('No tracklist for this one — it comes from ListenBrainz', 'error');
      return;
    }
    relSheetLoading = true;
    relSheetFor = relKey(r);
    relSheetError = null;
    try {
      relSheet = await api.releaseAlbum(r.mbid);
    } catch (e) {
      relSheetError = e.message || 'Could not load the tracklist';
    } finally {
      relSheetLoading = false;
    }
  }

  // --- opening a release's tracks --------------------------------------------
  //
  // Same shape as the Search screen's album sheet, deliberately: a release in
  // the list and an album found in search are the same thing (a YouTube album
  // playlist that is not in the library), and giving them one set of download
  // handlers means one place to fix a bug rather than two that drift apart.

  let relJobs = [];
  let relRequested = {}; // sourceId -> job id
  let relQueuingTrack = {}; // sourceId -> true while the POST is in flight
  let relAlbumQueuing = false;
  let relPoll = null;

  const relAnnounced = new Set();
  $: relJobById = Object.fromEntries(relJobs.map((j) => [j.id, j]));
  $: relTrackJobs = Object.fromEntries(
    Object.entries(relRequested)
      .map(([sourceId, jobId]) => [sourceId, relJobById[jobId]])
      .filter(([, job]) => job),
  );

  async function relRefreshJobs() {
    try {
      relJobs = (await api.jobs()) || [];
    } catch {
      return;
    }
    // A job gaining a songId is the moment the file became a real track. The
    // library screen stays mounted, so it will not reload on its own - tell it.
    for (const j of relJobs) {
      if (j.state === 'done' && j.songId && !relAnnounced.has(j.id)) {
        relAnnounced.add(j.id);
        noteLibraryChanged();
      }
    }
    const busy = relJobs.some((j) => j.state === 'queued' || j.state === 'downloading');
    if (busy) relStartPolling();
    else relStopPolling();
  }
  function relStartPolling() {
    if (relPoll) return;
    relPoll = setInterval(relRefreshJobs, 2000);
  }
  function relStopPolling() {
    clearInterval(relPoll);
    relPoll = null;
  }

  async function relGetTrack(track) {
    if (relQueuingTrack[track.sourceId] || relTrackJobs[track.sourceId]) return;
    relQueuingTrack = { ...relQueuingTrack, [track.sourceId]: true };
    try {
      const job = await api.acquire(track);
      relJobs = [...relJobs.filter((j) => j.id !== job.id), job];
      relRequested = { ...relRequested, [track.sourceId]: job.id };
      relStartPolling();
      toast(`Queued ${track.title}`);
    } catch (err) {
      toast(err?.message || 'Could not queue that track', 'error');
    } finally {
      const next = { ...relQueuingTrack };
      delete next[track.sourceId];
      relQueuingTrack = next;
    }
  }

  /**
   * Queue every track on an opened release.
   *
   * Sequential on purpose: each POST starts a yt-dlp download server-side, and
   * firing fourteen at once saturates the box. A partial failure keeps what
   * already queued, because those downloads are real.
   */
  async function relGetAlbum(album) {
    if (relAlbumQueuing) return;
    relAlbumQueuing = true;
    let queued = 0;
    let failed = 0;
    try {
      for (const track of album.tracks || []) {
        try {
          const job = await api.acquire({
            ...track,
            album: album.title,
            albumArtist: album.artist,
          });
          relJobs = [...relJobs.filter((j) => j.id !== job.id), job];
          relRequested = { ...relRequested, [track.sourceId]: job.id };
          queued += 1;
        } catch {
          failed += 1;
        }
      }
      if (queued) relStartPolling();
      if (failed) toast(`Queued ${queued} of ${queued + failed} tracks`);
      else toast(`Queued ${queued} tracks from ${album.title}`);
    } finally {
      relAlbumQueuing = false;
    }
  }

  function closeRelSheet() {
    relSheet = null;
    relSheetError = null;
  }

  /**
   * The sheet only knows the YouTube playlist; the library album id arrives with
   * a finished job, so that is where it has to come from.
   */
  async function openDownloadedRelAlbum(al) {
    const done = (al?.tracks || [])
      .map((tr) => relTrackJobs[tr.sourceId])
      .find((j) => j?.state === 'done' && j.songId);
    closeRelSheet();
    setTab('library');
    if (!done) return;
    try {
      const s = await api.song(done.songId);
      if (s?.albumId) openAlbumNav({ id: s.albumId, name: al.title });
    } catch {
      // Stay on the library tab; the album is there even if the lookup hiccuped.
    }
  }

  function onRowSwipe(key, e) {
    relSwipingId = key;
    relDragging = true;
    relDx = e.detail.dx;
  }
  function onRowSwipeEnd(r, e) {
    const { commit, dir } = e.detail;
    relDragging = false;
    relDx = 0;
    relSwipingId = null;
    if (!commit) return;
    if (dir === 'right') grabRelease(r);
    else removeRelease(r);
  }

  const relKey = (r) => `${r.artist}\u0000${r.title}`;

  async function loadReleases() {
    relLoading = true;
    relError = null;
    try {
      const r = await api.newReleases();
      releases = r?.releases || [];
      if (!releases.length && r?.note) relError = r.note;
    } catch (e) {
      relError = e.message || 'Could not load new releases';
    } finally {
      relLoading = false;
      lastReleasesLoad = Date.now();
    }
  }

  // Re-pull the list when the Discover tab becomes active, so the overnight
  // precompute (or anything downloaded/dismissed elsewhere) shows up instead of a
  // snapshot from whenever the app was last opened. Throttled: revisiting within
  // the window keeps what is already on screen.
  function refreshReleasesOnActivate() {
    if (relLoading || Date.now() - lastReleasesLoad < 30000) return;
    loadReleases();
  }

  // Group the list into time sections so this week's releases sit at the top and
  // older ones fall below. "New this week" always shows, even when empty.
  const BUCKETS = [
    { label: 'New this week', maxDays: 7 },
    { label: 'This month', maxDays: 31 },
    { label: 'Earlier', maxDays: Infinity },
  ];
  // A YouTube release whose playlist carries no real date gets the source's
  // last-modified stamp instead, which is not a release date. Filing that into
  // "Earlier" would hide a brand-new album, so it is kept in its own section and
  // labelled rather than given a week it does not belong to.
  const UNDATED = 'Date unknown';
  function bucketIndex(r) {
    if (r.dateUncertain || !r.date) return BUCKETS.length;
    const t = Date.parse(r.date + 'T00:00:00Z');
    if (Number.isNaN(t)) return BUCKETS.length;
    const days = (Date.now() - t) / 86400000;
    return BUCKETS.findIndex((b) => days <= b.maxDays);
  }
  $: sections = BUCKETS.map((b, i) => ({
    label: b.label,
    items: releases.filter((r) => bucketIndex(r) === i),
  }))
    .concat([
      { label: UNDATED, items: releases.filter((r) => bucketIndex(r) === BUCKETS.length) },
    ])
    .filter((s, i) => i === 0 || s.items.length);

  function setMode(m) {
    if (m === mode) return;
    stopPreview();
    mode = m;
    if (m === 'explore' && !deckLoaded) {
      deckLoaded = true;
      refill(true);
    }
  }

  async function grabRelease(r) {
    const key = relKey(r);
    if (queuing[key]) return;
    queuing = { ...queuing, [key]: 'downloading' };
    try {
      const { queued } = await api.acquireRelease(r);
      toast(`Downloading ${queued} ${queued === 1 ? 'track' : 'tracks'} — ${r.title}`);
      // Downloaded means not new to you any more: leave the list and record it,
      // so the sweep stops offering it. Same path as a swipe-away.
      await removeRelease(r);
      queuing = { ...queuing, [key]: 'done' };
    } catch (e) {
      toast(e.message || 'Could not start that download', 'error');
      const next = { ...queuing };
      delete next[key];
      queuing = next;
    }
  }

  async function togglePreview(r) {
    const key = relKey(r);
    if (previewingRel === key) {
      pv.toggle();
      return;
    }
    // The nightly precompute already resolved the source: play with no wait.
    if (r.previewUrl) {
      startPreview(r.previewUrl, key);
      return;
    }
    resolvingRel = key;
    try {
      const { sourceId } = await api.releaseSource(r);
      if (sourceId) startPreview(`/api/discovery/preview/candidate/${sourceId}`, key);
      else toast('No preview available for that one');
    } catch {
      toast('Could not load a preview');
    } finally {
      resolvingRel = null;
    }
  }

  async function removeRelease(r) {
    const key = relKey(r);
    if (previewingRel === key) stopPreview();
    releases = releases.filter((x) => relKey(x) !== key);
    try {
      await api.dismissRelease(r);
    } catch {
      // the client-side removal already happened; a failed persist just means it
      // may reappear on the next refresh
    }
  }

  $: top = cards[idx] || null;
  // How much audio exists versus how long the song actually is.
  //
  // `playable` comes from the loaded file, not from an assumption: an owned card
  // is the whole track, a candidate is whatever clip the server fetched. `total`
  // prefers the card's own duration so a 30s clip is still drawn against the
  // real length - that gap is the thing the hatching communicates.
  $: playable = $preview.duration || 0;
  $: total = Math.max(top?.duration || 0, playable);

  $: upcoming = cards.slice(idx + 1, idx + 3);
  $: intent = flying || (Math.abs(dx) > 24 ? (dx > 0 ? 'like' : 'dislike') : null);

  async function refill(reset = false) {
    if (fetching) return;
    fetching = true;
    if (reset) {
      loading = true;
      error = null;
      // Silence the current card before the deck changes underneath it.
      //
      // The preview only restarts when the top card's URL *changes*, and a
      // rebuilt deck can legitimately put the same card back on top - so
      // without this, "New deck" left the old audio running mid-song and
      // looked like the button had done nothing.
      previewing = null;
      pv.stop();
    }
    try {
      const fresh = (await api.deck(20)) || [];
      // The deck is rebuilt server-side on every call and owned cards can repeat
      // inside one session, so dedupe against what is already on the stack.
      const seen = new Set(cards.map((c) => `${c.kind}:${c.id}`));
      const add = reset ? fresh : fresh.filter((c) => !seen.has(`${c.kind}:${c.id}`));
      cards = reset ? fresh : [...cards, ...add];
      if (reset) idx = 0;
      error = null;
    } catch (err) {
      if (reset || !cards.length) error = err.message || 'Could not load the deck';
    } finally {
      fetching = false;
      loading = false;
    }
  }

  // Re-pull when the app returns to the foreground, so reopening it shows the
  // overnight rebuild instead of the snapshot from when it was last opened.
  function onVisible() {
    if (document.visibilityState === 'visible' && $tab === 'discover' && mode === 'new') {
      refreshReleasesOnActivate();
    }
  }
  onMount(() => {
    loadReleases();
    document.addEventListener('visibilitychange', onVisible);
  });

  // --- preview ---------------------------------------------------------------

  let previewing = null;
  let owned = false;

  function startPreview(url, rel) {
    if (!owned) {
      owned = true;
      pv.claim();
    }
    previewing = url;
    previewingRel = rel;
    pv.start(url);
  }
  function stopPreview() {
    if (!previewing && !owned) return;
    previewing = null;
    previewingRel = null;
    owned = false;
    pv.stop();
  }

  // Explore auto-previews the top card; New releases previews only what you tap.
  // Either way a preview needs the audio output and stops when Discover is left.
  $: if ($tab === 'discover' && mode === 'explore' && top && top.previewUrl !== previewing) {
    startPreview(top.previewUrl, null);
  }
  $: if (mode === 'explore' && upcoming[0]?.previewUrl) pv.prefetch(upcoming[0].previewUrl);
  $: if ($tab !== 'discover' && previewing) stopPreview();
  $: if ($tab === 'discover' && mode === 'new') refreshReleasesOnActivate();

  // --- per-card detail -------------------------------------------------------

  // Published date and view count are a full yt-dlp extraction server-side, so
  // they arrive after the card does. Keyed by id and never cleared, because
  // swiping back to a card you have already seen should not pay again; the
  // server caches too, this just avoids the round trip.
  let meta = {};
  let metaFor = null;

  // Only candidates have a source to ask about - an owned card is a local file,
  // and its year would have to come from its tags instead.
  $: if ($tab === 'discover' && top?.kind === 'candidate' && top.id !== metaFor) {
    metaFor = top.id;
    loadMeta(top.id);
  }

  async function loadMeta(id) {
    if (meta[id]) return;
    try {
      const m = await api.sourceMeta(id);
      // Assign a new object: Svelte 4 does not track mutation of a plain object.
      meta = { ...meta, [id]: m || {} };
    } catch {
      // Detail is a nicety. Record the miss so a card that cannot answer is not
      // retried on every swipe back to it.
      meta = { ...meta, [id]: {} };
    }
  }

  // "41,669,465" reads as a number; "41.7M" reads as a quantity, which is all
  // this is for.
  function views(n) {
    if (!n || n < 1000) return null;
    if (n < 1_000_000) return `${Math.round(n / 1000)}K views`;
    return `${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0)}M views`;
  }

  function posted(iso) {
    if (!iso) return null;
    const d = new Date(iso + 'T00:00:00Z');
    if (Number.isNaN(d.getTime())) return null;
    return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', timeZone: 'UTC' });
  }

  onDestroy(() => {
    pv.stop();
    relStopPolling();
    document.removeEventListener('visibilitychange', onVisible);
  });

  // --- swiping ---------------------------------------------------------------

  function onPointerDown(e) {
    if (!top || flying || confirmDelete) return;
    dragging = true;
    startX = e.clientX;
    dy = 0;
    e.currentTarget.setPointerCapture?.(e.pointerId);
  }

  function onPointerMove(e) {
    if (!dragging) return;
    dx = e.clientX - startX;
    dy = Math.abs(dx) * 0.06;
  }

  function onPointerUp() {
    if (!dragging) return;
    dragging = false;
    if (dx > THRESHOLD) decide('like');
    else if (dx < -THRESHOLD) decide('dislike');
    else dx = 0;
  }

  function decide(action) {
    const card = top;
    if (!card || flying) return;
    if (action === 'dislike' && card.kind === 'owned') {
      dx = 0;
      confirmDelete = card;
      return;
    }
    send(card, action);
  }

  function fallbackMessage(card, action, res) {
    if (action === 'like') {
      return card.kind === 'candidate' ? `Downloading “${card.title}”` : `Kept “${card.title}”`;
    }
    if (card.kind === 'owned') {
      return res?.ok ? `Deleted “${card.title}”` : `Could not delete “${card.title}”`;
    }
    return `Skipped “${card.title}”`;
  }

  async function send(card, action) {
    pv.stop();
    previewing = null;
    flying = action;
    dx = action === 'like' ? THRESHOLD : -THRESHOLD;

    // Advance on the animation's clock, not the network's: the deck stays
    // responsive and the result arrives as a toast whenever it arrives.
    setTimeout(advance, 220);

    try {
      const res = action === 'like' ? await api.like(card) : await api.dislike(card);
      const ok = res?.ok !== false;
      // A deleted owned card has to leave the player queue too - it may well be
      // the track playing underneath the deck.
      if (ok && action === 'dislike' && card.kind === 'owned') drop(card.id);
      toast(res?.message || fallbackMessage(card, action, res), ok ? 'success' : 'error');
    } catch (err) {
      toast(err.message || `The server rejected that ${action}`, 'error');
    }
  }

  function advance() {
    flying = null;
    dx = 0;
    dy = 0;
    idx += 1;
    if (cards.length - idx <= 3) refill(false);
  }

  function confirmed() {
    const card = confirmDelete;
    confirmDelete = null;
    if (card) send(card, 'dislike');
  }
</script>

<div class="h-full flex flex-col px-4 py-4">
  <header class="flex items-center justify-between gap-3 shrink-0">
    <div class="min-w-0">
      <h1 class="text-xl font-semibold">Discover</h1>
      <div class="mt-1.5 inline-flex rounded-lg border border-slate-800 p-0.5 text-[11px] font-medium">
        <button
          on:click={() => setMode('new')}
          class="px-2.5 py-1 rounded-md {mode === 'new'
            ? 'bg-slate-100 text-slate-900'
            : 'text-slate-400'}"
        >
          New releases
        </button>
        <button
          on:click={() => setMode('explore')}
          class="px-2.5 py-1 rounded-md {mode === 'explore'
            ? 'bg-slate-100 text-slate-900'
            : 'text-slate-400'}"
        >
          Explore
        </button>
      </div>
    </div>
    <button
      on:click={() => (mode === 'new' ? loadReleases() : refill(true))}
      aria-label="Refresh"
      class="w-11 h-11 shrink-0 flex items-center justify-center rounded-xl border border-slate-800 text-slate-400 active:bg-slate-900"
    >
      <RefreshCw class="w-4 h-4 {(mode === 'new' ? relLoading : fetching) ? 'animate-spin' : ''}" />
    </button>
  </header>

  {#if mode === 'new'}
    {#if relLoading}
      <div class="flex-1 flex items-center justify-center">
        <Spinner label="Finding new releases" />
      </div>
    {:else if relError}
      <div class="flex-1 flex flex-col items-center justify-center gap-3 text-center px-6">
        <AlertTriangle class="w-6 h-6 text-slate-500" />
        <p class="text-xs text-slate-400">{relError}</p>
      </div>
    {:else if !releases.length}
      <div class="flex-1 flex flex-col items-center justify-center gap-3 text-center px-6">
        <Check class="w-7 h-7 text-emerald-400" />
        <p class="text-sm font-medium">All caught up</p>
        <p class="text-xs text-slate-500">No new releases from your artists right now.</p>
      </div>
    {:else}
      <div class="flex-1 min-h-0 overflow-y-auto overscroll-contain -mx-1 px-1 mt-3 space-y-5">
        {#each sections as section (section.label)}
          <section class="space-y-2">
            <h2 class="text-[11px] font-semibold uppercase tracking-wide text-slate-500 px-1">
              {section.label}
            </h2>
            {#if !section.items.length}
              <p class="text-xs text-slate-500 px-1">Nothing new from your artists this week.</p>
            {/if}
            {#each section.items as r (r.artist + ':' + r.title + ':' + r.date)}
          {@const key = relKey(r)}
          {@const st = queuing[key]}
          <div
            use:swipeable
            on:swipe={(e) => onRowSwipe(key, e)}
            on:swipeend={(e) => onRowSwipeEnd(r, e)}
            class="relative overflow-hidden rounded-xl touch-pan-y"
          >
            <!-- The swipe action, revealed underneath: right = download, left = remove. -->
            <div
              class="absolute inset-0 flex items-center justify-between pointer-events-none"
              aria-hidden="true"
            >
              <div
                class="h-full w-1/2 flex items-center gap-2 pl-4 text-sm font-medium text-white bg-sky-600"
                style="opacity: {relSwipingId === key && relDx > 0 ? Math.min(1, relDx / REL_COMMIT) : 0}"
              >
                <Download class="w-5 h-5" /> Download
              </div>
              <div
                class="h-full w-1/2 flex items-center justify-end gap-2 pr-4 text-sm font-medium text-white bg-rose-600"
                style="opacity: {relSwipingId === key && relDx < 0 ? Math.min(1, -relDx / REL_COMMIT) : 0}"
              >
                Remove <X class="w-5 h-5" />
              </div>
            </div>
            <div
              class="relative flex items-center gap-3 border border-slate-800 bg-slate-900 rounded-xl p-2 {relDragging &&
              relSwipingId === key
                ? ''
                : 'transition-transform duration-200 ease-out'}"
              style="transform: translate3d({relSwipingId === key ? relDx : 0}px,0,0)"
            >
              {#if r.kind === 'Album'}
                <!-- An album: the row itself opens the tracklist, the way an
                     album card in Search does. Tapping the title is what anyone
                     would try, and making it a button is what makes that work
                     with a keyboard and a screen reader rather than only a
                     finger. The play button is gone: previewing one arbitrary
                     track of a record is not what an album row is for, and its
                     place is the tracklist. -->
                <button
                  type="button"
                  on:click={() => openRelTracks(r)}
                  class="flex items-center gap-3 min-w-0 flex-1 text-left active:opacity-70"
                >
                  <Artwork src={r.coverUrl} alt="" class="w-14 h-14 rounded-lg shrink-0" />
                  <span class="min-w-0 flex-1 block">
                    <span class="text-sm font-medium truncate block">{r.title}</span>
                    <span class="text-xs text-slate-400 truncate block">{r.artist}</span>
                    <span class="text-[11px] text-slate-500 block">
                      <span class="text-sky-300/90">{r.kind}</span>{r.date ? ` · ${r.date}` : ''}
                    </span>
                  </span>
                  <span class="flex items-center gap-1 text-[11px] text-slate-400 shrink-0">
                    {#if relSheetLoading && relSheetFor === key}
                      <Loader2 class="w-4 h-4 animate-spin" />
                    {:else}
                      <ListMusic class="w-4 h-4" />
                      Tracks
                    {/if}
                  </span>
                </button>
              {:else}
                <!-- A single is one song, so there is no tracklist to open and a
                     Tracks button would be a lie: it would resolve to a
                     one-line list. The row is not a button, because opening it
                     has nothing to show. -->
                <div class="flex items-center gap-3 min-w-0 flex-1">
                  <Artwork src={r.coverUrl} alt="" class="w-14 h-14 rounded-lg shrink-0" />
                  <span class="min-w-0 flex-1 block">
                    <span class="text-sm font-medium truncate block">{r.title}</span>
                    <span class="text-xs text-slate-400 truncate block">{r.artist}</span>
                    <span class="text-[11px] text-slate-500 block">
                      <span class="text-sky-300/90">{r.kind}</span>{r.date ? ` · ${r.date}` : ''}
                    </span>
                  </span>
                </div>
              {/if}
              <!-- Only a single gets a play button, and it replaces Tracks
                   rather than sitting beside it. -->
              {#if r.kind !== 'Album'}
                <button
                  on:click={() => togglePreview(r)}
                  aria-label="Preview {r.title}"
                  class="w-11 h-11 shrink-0 rounded-full border flex items-center justify-center active:bg-slate-800 {st ===
                  'done'
                    ? 'border-emerald-800 text-emerald-400'
                    : 'border-slate-700 text-sky-400'}"
                >
                  {#if resolvingRel === key || (previewingRel === key && $preview.loading)}
                    <Loader2 class="w-5 h-5 animate-spin" />
                  {:else if previewingRel === key && $preview.playing}
                    <Pause class="w-5 h-5 fill-current" />
                  {:else}
                    <Play class="w-5 h-5 fill-current" />
                  {/if}
                </button>
              {/if}
            </div>
          </div>
            {/each}
          </section>
        {/each}
      </div>
    {/if}
  {:else}
  {#if loading}
    <div class="flex-1 flex items-center justify-center"><Spinner label="Building a deck" /></div>
  {:else if error && !top}
    <div class="flex-1 flex flex-col items-center justify-center gap-3 text-center px-6">
      <AlertTriangle class="w-6 h-6 text-red-400" />
      <p class="text-xs text-red-200">{error}</p>
      <button
        on:click={() => refill(true)}
        class="h-11 px-5 rounded-xl border border-slate-700 text-xs font-medium">Try again</button
      >
    </div>
  {:else if !top}
    <div class="flex-1 flex flex-col items-center justify-center gap-3 text-center px-6">
      <Check class="w-7 h-7 text-emerald-400" />
      <p class="text-sm font-medium">Deck cleared</p>
      <p class="text-xs text-slate-500">Nothing left to judge right now.</p>
      <button
        on:click={() => refill(true)}
        class="h-11 px-5 rounded-xl border border-slate-700 text-xs font-medium">Look again</button
      >
    </div>
  {:else}
    <div class="flex-1 min-h-0 relative my-4">
      {#each upcoming as c, i (c.kind + ':' + c.id)}
        <div
          class="absolute inset-0 rounded-2xl border border-slate-800 bg-slate-900"
          style="transform: translateY({(i + 1) * 10}px) scale({1 - (i + 1) * 0.04}); z-index: {10 -
            i};"
          aria-hidden="true"
        ></div>
      {/each}

      <div
        class="absolute inset-0 z-20 rounded-2xl border bg-slate-900 overflow-hidden select-none touch-none flex flex-col {top.kind ===
        'owned'
          ? 'border-amber-900/60'
          : 'border-slate-800'}"
        style="transform: translateX({dx}px) translateY({dy}px) rotate({dx / 22}deg); transition: {dragging
          ? 'none'
          : 'transform 200ms ease-out'}; opacity: {flying ? 0 : 1};"
        on:pointerdown={onPointerDown}
        on:pointermove={onPointerMove}
        on:pointerup={onPointerUp}
        on:pointercancel={onPointerUp}
        role="group"
        aria-label="{top.title} by {top.artist}"
      >
        <div class="relative flex-1 min-h-0">
          <Artwork src={top.coverUrl} alt="" class="w-full h-full" />
          <div
            class="absolute inset-0 bg-gradient-to-t from-slate-950 via-slate-950/30 to-transparent"
          ></div>

          {#if intent === 'like'}
            <div
              class="absolute top-4 left-4 rounded-lg border-2 border-emerald-400 text-emerald-400 px-3 py-1 text-sm font-bold -rotate-12"
            >
              {top.kind === 'candidate' ? 'DOWNLOAD' : 'KEEP'}
            </div>
          {:else if intent === 'dislike'}
            <div
              class="absolute top-4 right-4 rounded-lg border-2 border-red-400 text-red-400 px-3 py-1 text-sm font-bold rotate-12"
            >
              {top.kind === 'owned' ? 'DELETE' : 'SKIP'}
            </div>
          {/if}

          <button
            on:click|stopPropagation={pv.toggle}
            aria-label={$preview.playing ? 'Pause preview' : 'Play preview'}
            class="absolute bottom-3 right-3 w-11 h-11 rounded-full bg-slate-950/70 backdrop-blur border border-slate-700 flex items-center justify-center"
          >
            {#if $preview.playing}
              <Pause class="w-4 h-4 fill-current" />
            {:else}
              <Play class="w-4 h-4 fill-current translate-x-0.5" />
            {/if}
          </button>

          <!--
            The scrubber spans the WHOLE song, but only the part that exists
            locally can be played.

            For a card already on disk that is all of it. For a candidate the
            server only fetched a ~30s clip, so the rest is drawn as 45-degree
            hatching: the bar still shows how long the track really is, and the
            stripes say plainly that the remainder is not here rather than
            pretending a 30s clip is the song.

            stopPropagation matters: the card is a swipe target, and without it
            dragging the scrubber would fling the card away instead of seeking.
          -->
          {#if playable > 0}
            <div
              class="absolute bottom-3 left-3 right-16 flex items-center gap-2"
              on:pointerdown={(e) => e.stopPropagation()}
              on:pointermove={(e) => e.stopPropagation()}
              on:pointerup={(e) => e.stopPropagation()}
            >
              <span class="text-[10px] tabular-nums text-slate-300 w-8 text-right">
                {fmtTime($preview.time)}
              </span>

              <div class="relative flex-1 h-9 flex items-center">
                <!-- the track: played, remaining-but-available, then unavailable -->
                <div class="absolute inset-x-0 h-1.5 rounded-full overflow-hidden bg-slate-700">
                  <div
                    class="absolute inset-y-0 left-0 bg-sky-400"
                    style="width: {(($preview.time || 0) / total) * 100}%"
                  ></div>
                  {#if playable < total}
                    <div
                      class="absolute inset-y-0 right-0 preview-hatch"
                      style="width: {((total - playable) / total) * 100}%"
                    ></div>
                  {/if}
                </div>

                <input
                  type="range"
                  min="0"
                  max={playable}
                  step="0.25"
                  value={Math.min($preview.time, playable)}
                  on:input={(e) => pv.seek(+e.currentTarget.value)}
                  aria-label="Seek within the available audio"
                  class="absolute inset-y-0 left-0 h-9 opacity-0 cursor-pointer"
                  style="width: {(playable / total) * 100}%"
                />
                <!-- the handle, positioned on the real scale -->
                <div
                  class="absolute w-3 h-3 -ml-1.5 rounded-full bg-sky-300 shadow pointer-events-none"
                  style="left: {(Math.min($preview.time, playable) / total) * 100}%"
                ></div>
              </div>

              <span class="text-[10px] tabular-nums text-slate-400 w-8">
                {fmtTime(total)}
              </span>
            </div>
          {/if}
        </div>

        <div class="shrink-0 p-4 space-y-2">
          <div>
            <h2 class="text-base font-semibold leading-tight">{top.title}</h2>
            <p class="text-xs text-slate-400 truncate">
              {top.artist}{top.album ? ` · ${top.album}` : ''}
            </p>
          </div>

          {#if top.reason}
            <p class="text-[11px] text-slate-500">{top.reason}</p>
          {/if}

          <!-- Genres ship with the deck (local store, no network). Posted date and
               views arrive a moment later, per card, so this block appears rather
               than reserving empty space for something that may never come. -->
          {#if top.genres?.length}
            <div class="flex flex-wrap gap-1.5">
              {#each top.genres as g}
                <span
                  class="text-[10px] uppercase tracking-wide rounded-full border border-slate-700 bg-slate-800/60 text-slate-300 px-2 py-0.5"
                >
                  {g}
                </span>
              {/each}
            </div>
          {/if}

          {#if posted(meta[top.id]?.postedAt) || views(meta[top.id]?.views)}
            <p class="text-[11px] text-slate-500 flex items-center gap-1.5 flex-wrap">
              {#if posted(meta[top.id]?.postedAt)}
                <span>Posted {posted(meta[top.id].postedAt)}</span>
              {/if}
              {#if posted(meta[top.id]?.postedAt) && views(meta[top.id]?.views)}
                <span class="text-slate-700">·</span>
              {/if}
              {#if views(meta[top.id]?.views)}
                <span>{views(meta[top.id].views)}</span>
              {/if}
            </p>
          {/if}

          <!-- The consequence of each direction, stated on the card. A left swipe
               on an owned track is not a "no thanks", it is a deletion. -->
          {#if top.kind === 'owned'}
            <p
              class="text-[11px] leading-relaxed rounded-lg border border-amber-900/60 bg-amber-950/30 text-amber-200 px-2.5 py-2"
            >
              Already on the server. Swiping <strong>left deletes the file</strong> from disk —
              permanently, for everyone.
            </p>
          {:else}
            <p
              class="text-[11px] leading-relaxed rounded-lg border border-slate-800 bg-slate-950/60 text-slate-400 px-2.5 py-2"
            >
              Not downloaded. Swiping <strong class="text-sky-300">right downloads it</strong> to the
              server; left costs nothing.
            </p>
          {/if}

          {#if $preview.failed}
            <!-- Loud on purpose. This card cannot be judged: there is no audio.
                 Rendering it as a normal card - which is what happened while the
                 yt-dlp player client was blocked - makes a broken deck look like a
                 working one, and the user swipes on titles alone without knowing. -->
            <p
              class="text-[11px] leading-relaxed rounded-lg border border-rose-900/60 bg-rose-950/30 text-rose-200 px-2.5 py-2"
            >
              <strong>No preview available.</strong> The source refused to hand over audio, so this
              one cannot be previewed. Swiping right still downloads it, but it will probably fail
              too — if every card says this, the downloader is broken rather than the track.
            </p>
          {:else if $preview.loading}
            <!-- A new candidate's clip is cut server-side on demand, so the first
                 request for it genuinely takes a second or two. Reported
                 2026-08-25: the failure banner used to appear here and then the
                 preview played fine, which taught the user to ignore the banner -
                 the worst outcome for a warning that is sometimes real. -->
            <p class="text-[11px] text-slate-500">Preparing the preview…</p>
          {:else if $preview.blocked}
            <p class="text-[11px] text-slate-500">Tap the play button to hear the preview.</p>
          {/if}
        </div>
      </div>
    </div>

    <!-- Buttons, not just gestures: swiping is invisible to a first-time user and
         miserable one-handed on a large phone. -->
    <div class="shrink-0 flex items-center justify-center gap-6 pb-2">
      <button
        on:click={() => decide('dislike')}
        aria-label={top.kind === 'owned' ? 'Delete from server' : 'Skip'}
        class="w-16 h-16 rounded-full border-2 flex items-center justify-center active:scale-95 transition-transform {top.kind ===
        'owned'
          ? 'border-red-500/70 text-red-400 bg-red-950/30'
          : 'border-slate-700 text-slate-400'}"
      >
        {#if top.kind === 'owned'}
          <Trash2 class="w-6 h-6" />
        {:else}
          <X class="w-7 h-7" />
        {/if}
      </button>

      <button
        on:click={() => decide('like')}
        aria-label={top.kind === 'candidate' ? 'Download' : 'Keep'}
        class="w-16 h-16 rounded-full border-2 border-emerald-500/70 text-emerald-400 bg-emerald-950/30 flex items-center justify-center active:scale-95 transition-transform"
      >
        {#if top.kind === 'candidate'}
          <Download class="w-6 h-6" />
        {:else}
          <Check class="w-7 h-7" />
        {/if}
      </button>
    </div>
  {/if}
  {/if}
</div>

<!-- The tracklist of a tapped release. Held back until the album resolves, so
     opening it is a real wait rather than a sheet that fills in: the first tap
     takes a few seconds while yt-dlp reads the playlist, and saying so beats
     showing a header with no tracks under it. -->
{#if relSheetError}
  <div
    class="fixed inset-0 z-40 bg-slate-950/80 backdrop-blur-sm flex items-end"
    role="alertdialog"
    aria-label="Tracklist unavailable"
  >
    <div class="w-full rounded-t-2xl border-t border-slate-800 bg-slate-900 p-5 pb-safe space-y-4">
      <div class="flex items-start gap-3">
        <AlertTriangle class="w-5 h-5 text-amber-400 mt-0.5 shrink-0" />
        <p class="text-sm text-slate-200 break-words">{relSheetError}</p>
      </div>
      <button
        on:click={closeRelSheet}
        class="w-full h-11 rounded-xl border border-slate-700 text-sm font-medium text-slate-200 active:bg-slate-800"
      >
        Close
      </button>
    </div>
  </div>
{:else if relSheet}
  <AcquireAlbumSheet
    album={relSheet}
    queuingTrack={relQueuingTrack}
    trackJobs={relTrackJobs}
    albumQueuing={relAlbumQueuing}
    onGetTrack={relGetTrack}
    onGetAlbum={relGetAlbum}
    onOpenAlbum={openDownloadedRelAlbum}
    onClose={closeRelSheet}
  />
{/if}

{#if confirmDelete}
  <!-- Second confirmation for the one action in this app that destroys data. -->
  <div class="fixed inset-0 z-40 bg-slate-950/80 backdrop-blur-sm flex items-end">
    <div class="w-full rounded-t-2xl border-t border-slate-800 bg-slate-900 p-5 pb-safe space-y-4">
      <div class="flex items-start gap-3">
        <Trash2 class="w-5 h-5 text-red-400 mt-0.5 shrink-0" />
        <div class="min-w-0">
          <p class="text-sm font-semibold">Delete this file from the server?</p>
          <p class="text-xs text-slate-400 mt-1 break-words">
            “{confirmDelete.title}” by {confirmDelete.artist} will be removed from disk. This cannot
            be undone.
          </p>
        </div>
      </div>
      <div class="grid grid-cols-2 gap-3">
        <button
          on:click={() => (confirmDelete = null)}
          class="h-12 rounded-xl border border-slate-700 text-sm font-medium">Keep it</button
        >
        <button on:click={confirmed} class="h-12 rounded-xl bg-red-600 text-sm font-semibold">
          Delete
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  /*
    The part of the song that was never downloaded.

    45-degree grey/white hatching, the standard "nothing here" fill - it reads as
    unavailable at a glance and, unlike a flat grey, cannot be mistaken for the
    normal unplayed remainder of a track that IS on disk.
  */
  .preview-hatch {
    background-image: repeating-linear-gradient(
      45deg,
      rgb(100 116 139 / 0.95) 0,
      rgb(100 116 139 / 0.95) 3px,
      rgb(226 232 240 / 0.9) 3px,
      rgb(226 232 240 / 0.9) 6px
    );
  }
</style>
