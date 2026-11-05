/**
 * Mock backend for UX work. Dev only - never shipped, never imported by src/.
 *
 * `npm run dev` proxies /api to 127.0.0.1:8080, so this stands in for the Rust
 * server and lets the UI be driven without Postgres, an OIDC provider or a
 * four-minute container build in the loop.
 *
 * It serves REAL audio: synthesised WAV with a correct header and byte-range
 * support. A silent stub would make every seek look like it worked, which is
 * exactly the class of bug this is here to catch.
 *
 *   node web/mock/server.mjs
 */
import { createServer } from "node:http";

const PORT = process.env.MOCK_PORT ? Number(process.env.MOCK_PORT) : 8080;

// ---------------------------------------------------------------- fake library

const ARTISTS = [
  "Slow J",
  "Dillaz",
  "Duman",
  "Piruka",
  "Coldplay",
  "Anathema",
  "Sotam",
  "Febre 90s",
  "Teoman",
  "YNW Melly",
  "Marina Lima",
  "Bob Dylan",
];
const WORDS = [
  "Ainda Tenho",
  "Underground",
  "Super Crónico",
  "Lemon Tree",
  "One Last Goodbye",
  "Murder on My Mind",
  "Papel e Tinta",
  "Menino Bom",
  "Pedras no Meu Sapato",
  "A Sky Full of Stars",
  "Freestyle",
  "Boom Bap",
  "Noite Fria",
  "Ressaca",
  "Catch The Rainbow",
  "Sex, Drugs, Etc.",
  "Shut Up",
  "O Mine",
  "Vibes",
];
const GENRES = [
  "hip-hop",
  "brazilian",
  "rock",
  "turkish rock",
  "indie",
  "folk",
  "trap",
];

// Deterministic so a reload does not reshuffle the whole app mid-discussion.
function rng(seed) {
  let s = seed >>> 0 || 1;
  return () => (s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32;
}
const hash = (s) =>
  [...s].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);

function makeTrack(i) {
  const r = rng(i + 1);
  const artist = ARTISTS[Math.floor(r() * ARTISTS.length)];
  const title =
    WORDS[Math.floor(r() * WORDS.length)] + (r() > 0.7 ? " (Remix)" : "");
  const id = `song-${i}`;
  return {
    id,
    title,
    artist,
    album: r() > 0.5 ? `${WORDS[Math.floor(r() * WORDS.length)]} EP` : "",
    albumId: `album-${i % 12}`,
    // 1:30 to 6:30, so the scrubber has a range of real lengths to render.
    duration: 90 + Math.floor(r() * 300),
    year: 2015 + Math.floor(r() * 11),
    starred: r() > 0.82,
    coverUrl: `/api/cover/${id}`,
    streamUrl: `/api/stream/${id}`,
    genre: GENRES[Math.floor(r() * GENRES.length)],
  };
}

const SONGS = Array.from({ length: 60 }, (_, i) => makeTrack(i));
const byId = new Map(SONGS.map((s) => [s.id, s]));

let PLAYLISTS = [
  {
    id: "pl-1",
    name: "Boom Bap",
    owner: "demo",
    songCount: 5,
    tracks: ["song-0", "song-3", "song-7", "song-11", "song-15"],
  },
  {
    id: "pl-2",
    name: "RapPT",
    owner: "demo",
    songCount: 9,
    tracks: ["song-1", "song-4", "song-8"],
  },
  {
    id: "pl-3",
    name: "Late night",
    owner: "demo",
    songCount: 3,
    tracks: ["song-2", "song-5"],
  },
  {
    id: "pl-4",
    name: "Ginga",
    owner: "demo",
    songCount: 2,
    tracks: ["song-6"],
  },
];
let plSeq = 5;

// Half the deck is on disk, half is a 30-second preview - the two cases the
// discovery scrubber has to render differently.
const DECK = Array.from({ length: 14 }, (_, i) => {
  const t = makeTrack(200 + i);
  const owned = i % 2 === 0;
  return {
    kind: owned ? "owned" : "candidate",
    id: owned ? t.id : `yt-${i}abcde`,
    title: t.title,
    artist: t.artist,
    album: t.album,
    duration: t.duration,
    coverUrl: `/api/cover/${t.id}`,
    previewUrl: `/api/discovery/preview/${owned ? "owned" : "candidate"}/${owned ? t.id : `yt-${i}abcde`}`,
    reason: owned
      ? "downloaded for you in Weekly-Jams"
      : `because you play ${t.artist}`,
  };
});

let JOBS = [];
let jobSeq = 1;

// ------------------------------------------------------------------- fake audio

/**
 * A WAV of `seconds`, as a real PCM sine at a pitch derived from the id.
 *
 * Audible and position-dependent on purpose: the pitch steps every 5 seconds, so
 * dragging the scrubber to the middle is something you can *hear* landing in the
 * middle rather than something you have to take on trust.
 */
function wav(seconds, seed) {
  const rate = 8000;
  const n = Math.floor(rate * seconds);
  const buf = Buffer.alloc(44 + n * 2);
  buf.write("RIFF", 0);
  buf.writeUInt32LE(36 + n * 2, 4);
  buf.write("WAVEfmt ", 8);
  buf.writeUInt32LE(16, 16);
  buf.writeUInt16LE(1, 20);
  buf.writeUInt16LE(1, 22);
  buf.writeUInt32LE(rate, 24);
  buf.writeUInt32LE(rate * 2, 28);
  buf.writeUInt16LE(2, 32);
  buf.writeUInt16LE(16, 34);
  buf.write("data", 36);
  buf.writeUInt32LE(n * 2, 40);
  const base = 180 + (seed % 12) * 40;
  for (let i = 0; i < n; i++) {
    const step = Math.floor(i / rate / 5);
    const freq = base * (1 + step * 0.12);
    const v = Math.sin((2 * Math.PI * freq * i) / rate) * 0.25 * 32767;
    buf.writeInt16LE(v | 0, 44 + i * 2);
  }
  return buf;
}

const audioCache = new Map();
function audio(id, seconds) {
  const key = `${id}:${seconds}`;
  if (!audioCache.has(key)) audioCache.set(key, wav(seconds, hash(id)));
  return audioCache.get(key);
}

function sendRange(req, res, buf, type) {
  const range = req.headers.range;
  res.setHeader("Accept-Ranges", "bytes");
  res.setHeader("Content-Type", type);
  if (!range) {
    res.writeHead(200, { "Content-Length": buf.length });
    return res.end(buf);
  }
  const m = /bytes=(\d*)-(\d*)/.exec(range);
  const start = m[1] ? Number(m[1]) : 0;
  const end = m[2] ? Number(m[2]) : buf.length - 1;
  const slice = buf.subarray(start, end + 1);
  res.writeHead(206, {
    "Content-Range": `bytes ${start}-${end}/${buf.length}`,
    "Content-Length": slice.length,
  });
  res.end(slice);
}

function cover(id) {
  const h = hash(id);
  const a = `hsl(${h % 360} 55% 32%)`;
  const b = `hsl(${(h * 7) % 360} 45% 16%)`;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="300" height="300">
    <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/>
    </linearGradient></defs>
    <rect width="300" height="300" fill="url(#g)"/>
    <circle cx="150" cy="150" r="52" fill="none" stroke="rgba(255,255,255,.35)" stroke-width="10"/>
    <circle cx="150" cy="150" r="8" fill="rgba(255,255,255,.55)"/>
  </svg>`;
}

// ---------------------------------------------------------------------- routing

const json = (res, code, v) => {
  res.writeHead(code, { "Content-Type": "application/json" });
  res.end(JSON.stringify(v));
};
const readBody = (req) =>
  new Promise((resolve) => {
    let s = "";
    req.on("data", (c) => (s += c));
    req.on("end", () => {
      try {
        resolve(JSON.parse(s || "{}"));
      } catch {
        resolve({});
      }
    });
  });

const shuffled = (n, seed) => {
  const r = rng(seed);
  return [...SONGS].sort(() => r() - 0.5).slice(0, n);
};

// --- audiobooks -------------------------------------------------------------
//
// MODULE SCOPE, NOT INSIDE THE HANDLER. The first version declared these in the
// request function, so every request rebuilt them and a PUT to /progress mutated
// an object that was discarded a millisecond later - progress silently never
// stuck. Fixture state has to outlive the request that changes it.
//
// Shape notes, because this is the part that is expensive to get wrong:
//
//   * A book is one ordered thing you are PARTWAY THROUGH. Progress is
//     therefore per book, not per session, and is a chapter id plus an offset
//     inside it - not a global offset, which would break the moment a chapter
//     is re-encoded or the file list changes.
//   * Chapters carry streamUrl individually, so a book is really an ordered
//     playlist. A single long video gets split at its chapter marks on the way
//     in, so the client never has to care which of the two it came from.
//   * speed and sleepMinutes live on the BOOK, not globally: 1.5x suits one
//     narrator and not another, and it should be remembered per book.
const DAY = 864e5;
const BOOK_PROGRESS = {
  // Three ages on purpose, because the resume button says something different
  // for each and the difference is the feature:
  //   an hour ago  -> resume at the exact spot, rewound 15s
  //   nine days    -> recommend restarting the chapter, exact spot still offered
  //   two seconds from the end of a chapter -> the place is the NEXT chapter
  "bk-1": { trackId: "bk-1-whole", positionSec: 3212, updatedAt: Date.now() - 36e5, started: true },
  "bk-4": { trackId: "bk-4-whole", positionSec: 8600, updatedAt: Date.now() - 9 * DAY, started: true },
  "bk-5": { trackId: "bk-5-c4", positionSec: 1265, updatedAt: Date.now() - 12e5, started: true },
};
// Two shapes, because the real sources produce both and the client must handle
// each: bk-1 and bk-4 are ONE file with chapter offsets (the YouTube shape),
// the rest are one file per chapter (the LibriVox shape).
const mkChapters = (bookId, n, base, oneFile = false) => {
  let at = 0;
  return Array.from({ length: n }, (_, i) => {
    const dur = base + ((i * 137) % 900);
    const ch = oneFile
      ? { trackId: `${bookId}-whole`, startSec: at, endSec: at + dur }
      : { trackId: `${bookId}-c${i + 1}`, startSec: 0, endSec: dur };
    at += dur;
    return { title: `Chapter ${i + 1}`, ...ch };
  });
};
const BOOKS = [
  { id: "bk-1", title: "Project Hail Mary", author: "Andy Weir", coverUrl: "/api/cover/bk-1",
    chapters: mkChapters("bk-1", 32, 1100, true), speed: 1.5, sleepMinutes: 0 },
  { id: "bk-2", title: "The Left Hand of Darkness", author: "Ursula K. Le Guin", coverUrl: "/api/cover/bk-2",
    chapters: mkChapters("bk-2", 20, 980), speed: 1, sleepMinutes: 0 },
  { id: "bk-3", title: "Piranesi", author: "Susanna Clarke", coverUrl: "/api/cover/bk-3",
    chapters: mkChapters("bk-3", 14, 860), speed: 1.25, sleepMinutes: 15 },
  { id: "bk-4", title: "The Wager", author: "David Grann", coverUrl: "/api/cover/bk-4",
    chapters: mkChapters("bk-4", 11, 1240, true), speed: 1, sleepMinutes: 30 },
  { id: "bk-5", title: "Cloud Atlas", author: "David Mitchell", coverUrl: "/api/cover/bk-5",
    chapters: mkChapters("bk-5", 6, 860), speed: 1.25, sleepMinutes: 0 },
];
const bookSummary = (b) => {
  const total = b.chapters.reduce((n, c) => n + (c.endSec - c.startSec), 0);
  const pr = BOOK_PROGRESS[b.id];
  let listened = 0;
  if (pr) {
    const at = b.chapters.findIndex(
      (c) => c.trackId === pr.trackId && pr.positionSec >= c.startSec && pr.positionSec < c.endSec,
    );
    if (at >= 0) {
      listened =
        b.chapters.slice(0, at).reduce((n, c) => n + (c.endSec - c.startSec), 0) +
        (pr.positionSec - b.chapters[at].startSec);
    }
  }
  return { id: b.id, title: b.title, author: b.author, coverUrl: b.coverUrl,
    chapterCount: b.chapters.length, durationSec: total,
    progress: pr || null, listenedSec: listened, speed: b.speed, sleepMinutes: b.sleepMinutes };
};

const server = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  const p = url.pathname;
  const seg = p.split("/").filter(Boolean);
  const m = req.method;
  const q = (k, d) => url.searchParams.get(k) ?? d;

  // Latency, so loading states are visible rather than theoretical.
  await new Promise((r) => setTimeout(r, 40));

  if (p === "/api/me")
    return json(res, 200, {
      username: "demo",
      displayName: "demo",
      navidromeUser: "demo",
      navidromeLinked: true,
    });
  if (p === "/api/settings")
    return json(res, 200, {
      listenBrainzLinked: false,
      navidromeUser: "demo",
    });

  if (p === "/api/library/home")
    return json(res, 200, {
      recent: shuffled(12, 1),
      frequent: shuffled(12, 2),
      random: shuffled(12, 3),
    });

  if (p === "/api/library/songs") {
    const size = Number(q("size", 100));
    const genre = q("genre", "");
    let out = genre ? SONGS.filter((s) => s.genre === genre) : SONGS;
    if (q("sort") === "starred") out = out.filter((s) => s.starred);
    return json(res, 200, out.slice(0, size));
  }

  if (p === "/api/library/genres")
    return json(
      res,
      200,
      GENRES.map((name) => ({
        name,
        count: SONGS.filter((s) => s.genre === name).length,
      })),
    );

  if (p === "/api/library/storage") {
    // Plausible shapes: the account is a slice of the music, the music is a
    // slice of a disk that also holds everything else.
    const yours = SONGS.slice(0, 24).reduce(
      (n, s) => n + s.duration * 42000,
      0,
    );
    const all = SONGS.reduce((n, s) => n + s.duration * 42000, 0);
    const total = 400 * 1024 ** 3;
    return json(res, 200, {
      you: { bytes: yours, tracks: 24 },
      everyone: { bytes: all, tracks: SONGS.length },
      disk: { total, free: total - 71 * 1024 ** 3, used: 71 * 1024 ** 3 },
    });
  }

  if (p === "/api/library/starred")
    return json(
      res,
      200,
      SONGS.filter((s) => s.starred),
    );
  if (p === "/api/library/artists")
    return json(
      res,
      200,
      ARTISTS.map((n, i) => ({
        id: `artist-${i}`,
        name: n,
        albumCount: 1 + (i % 3),
        coverUrl: `/api/cover/artist-${i}`,
      })),
    );
  if (p === "/api/library/albums")
    return json(
      res,
      200,
      Array.from({ length: 12 }, (_, i) => ({
        id: `album-${i}`,
        name: `${WORDS[i % WORDS.length]} EP`,
        artist: ARTISTS[i % ARTISTS.length],
        songCount: 5,
        year: 2020 + (i % 6),
        coverUrl: `/api/cover/album-${i}`,
      })),
    );

  // --- playlists ---
  if (p === "/api/library/playlists" && m === "GET")
    return json(
      res,
      200,
      PLAYLISTS.map(({ tracks, ...rest }) => ({
        ...rest,
        songCount: tracks.length,
      })),
    );

  if (p === "/api/library/playlists" && m === "POST") {
    const b = await readBody(req);
    if (!b.name?.trim())
      return json(res, 400, { error: "a playlist needs a name" });
    const pl = {
      id: `pl-${plSeq++}`,
      name: b.name.trim(),
      owner: "demo",
      tracks: [...(b.songIds || [])],
    };
    PLAYLISTS.push(pl);
    return json(res, 200, { ...pl, songCount: pl.tracks.length });
  }

  if (seg[2] === "playlist" && seg[3]) {
    const pl = PLAYLISTS.find((x) => x.id === seg[3]);
    if (!pl) return json(res, 404, { error: "no such playlist" });

    if (seg[4] === "tracks" && m === "POST") {
      const b = await readBody(req);
      pl.tracks.push(...(b.songIds || []));
      return json(res, 200, { added: (b.songIds || []).length });
    }
    if (m === "GET")
      return json(res, 200, {
        playlist: {
          id: pl.id,
          name: pl.name,
          owner: pl.owner,
          songCount: pl.tracks.length,
        },
        tracks: pl.tracks.map((id) => byId.get(id)).filter(Boolean),
      });
    if (m === "PATCH" || m === "PUT") {
      const b = await readBody(req);
      if (!b.name?.trim())
        return json(res, 400, { error: "a playlist needs a name" });
      pl.name = b.name.trim();
      return json(res, 200, {
        id: pl.id,
        name: pl.name,
        owner: pl.owner,
        songCount: pl.tracks.length,
      });
    }
    if (m === "DELETE") {
      PLAYLISTS = PLAYLISTS.filter((x) => x.id !== pl.id);
      return json(res, 200, { deleted: pl.id });
    }
  }

  // --- one song ---
  if (seg[2] === "song" && seg[3]) {
    const s = byId.get(seg[3]);
    if (!s) return json(res, 404, { error: "no such track" });
    if (m === "DELETE")
      return json(res, 200, {
        deleted: s.id,
        message: "Removed from your library. The shared copy stays.",
      });
    return json(res, 200, s);
  }

  if (seg[2] === "star") return json(res, 200, { ok: true });
  if (seg[2] === "scrobble") return json(res, 200, { ok: true });

  // --- search + downloads ---
  if (p === "/api/search") {
    const needle = (q("q", "") || "").toLowerCase();
    const hit = (s) =>
      (s.title + " " + s.artist).toLowerCase().includes(needle);
    return json(res, 200, {
      tracks: SONGS.filter(hit).slice(0, 10),
      albums: [],
      artists: [],
      acquirable: [],
    });
  }
  if (p === "/api/acquire/search") {
    const needle = q("q", "") || "track";
    return json(
      res,
      200,
      Array.from({ length: 6 }, (_, i) => ({
        sourceId: `yt${String(i).padStart(4, "0")}x`,
        title: `${needle} - result ${i + 1}`,
        artist: ARTISTS[i % ARTISTS.length],
        duration: 150 + i * 30,
        thumb: `/api/cover/yt-${i}`,
      })),
    );
  }
  if (p === "/api/books") return json(res, 200, BOOKS.map(bookSummary));
  if (p.startsWith("/api/books/") && m === "DELETE") {
    const id = p.split("/")[3];
    const at = BOOKS.findIndex((b) => b.id === id);
    if (at < 0) return json(res, 404, { error: "no such book" });
    BOOKS.splice(at, 1);
    // Progress is deliberately KEPT, matching the server: re-adding the same
    // book restores where everyone was.
    return json(res, 200, { ok: true });
  }
  if (p.startsWith("/api/books/") && p.endsWith("/progress") && m === "PUT") {
    const id = p.split("/")[3];
    const b = await readBody(req);
    BOOK_PROGRESS[id] = { ...b, updatedAt: Date.now() };
    return json(res, 200, BOOK_PROGRESS[id]);
  }
  if (p.startsWith("/api/books/") && m === "PUT") {
    const id = p.split("/")[3];
    const bk = BOOKS.find((x) => x.id === id);
    const b = await readBody(req);
    if (bk) Object.assign(bk, b);            // speed / sleepMinutes
    return json(res, 200, bookSummary(bk));
  }
  if (p.startsWith("/api/books/")) {
    const id = p.split("/")[3];
    const bk = BOOKS.find((x) => x.id === id);
    if (!bk) return json(res, 404, { error: "no such book" });
    return json(res, 200, { ...bookSummary(bk), chapters: bk.chapters });
  }

  if (p === "/api/acquire/albums") {
    const needle = q("q", "") || "album";
    // Shaped exactly like the Go handler's model.AcquireAlbum, including the
    // OLAK5uy_ playlist id - the UI keys off playlistId, so a mock that invented
    // a different field would let a broken binding pass locally.
    return json(
      res,
      200,
      Array.from({ length: 3 }, (_, i) => ({
        playlistId: `OLAK5uy_mock${i}aBTOVwrcZJepB1C9P3n5Sfy5c`,
        title: `${needle} ${["Deluxe Edition", "", "(Remastered)"][i]}`.trim(),
        artist: ARTISTS[i % ARTISTS.length],
        trackCount: [12, 9, 14][i],
        // Real shapes: views is the playlist's play count and durationSec the sum
        // of its tracks. Deliberately NO release date - YouTube Music only
        // returns modified_date, which is not one.
        views: [19533318, 421000, 8800][i],
        durationSec: [12, 9, 14][i] * 213,
        thumb: `/api/cover/yt-album-${i}`,
        tracks: Array.from({ length: [12, 9, 14][i] }, (_, k) => ({
          sourceId: `alb${i}trk${String(k).padStart(2, "0")}`,
          title: `${needle} track ${k + 1}`,
          artist: ARTISTS[i % ARTISTS.length],
          duration: 180 + k * 7,
        })),
      })),
    );
  }
  if (p === "/api/acquire" && m === "POST") {
    const b = await readBody(req);
    const job = {
      id: `job-${jobSeq++}`,
      title: `${b.artist} - ${b.title}`,
      state: "queued",
    };
    JOBS.unshift(job);
    // Downloads finish, then the "library scan" catches up a beat later - the
    // exact two-stage timing the search screen's play button depends on.
    setTimeout(() => (job.state = "downloading"), 800);
    setTimeout(() => (job.state = "done"), 2600);
    setTimeout(
      () => (job.songId = SONGS[Math.floor(Math.random() * SONGS.length)].id),
      6000,
    );
    return json(res, 202, job);
  }
  if (p === "/api/acquire/jobs") return json(res, 200, JOBS);

  // --- discovery ---
  if (p === "/api/discovery/deck") {
    DECK.push(DECK.shift()); // rotate, so a new deck genuinely differs
    return json(res, 200, DECK.slice(0, Number(q("n", 20))));
  }
  if (p === "/api/discovery/like" || p === "/api/discovery/dislike") {
    const b = await readBody(req);
    const like = p.endsWith("like");
    return json(res, 200, {
      ok: true,
      action:
        b.kind === "candidate"
          ? like
            ? "queued"
            : "skipped"
          : like
            ? "kept"
            : "deleted",
      message: like ? "queued for download" : "won't suggest that again",
    });
  }
  if (seg[1] === "discovery" && seg[2] === "preview") {
    const kind = seg[3];
    const id = decodeURIComponent(seg[4] || "");
    const card = DECK.find((c) => c.id === id);
    // Owned previews are the whole file; a candidate is a 30s clip, which is
    // the difference the scrubber has to show.
    const secs = kind === "owned" ? (card?.duration ?? 200) : 30;
    return sendRange(req, res, audio(id, secs), "audio/wav");
  }

  // --- media ---
  if (seg[1] === "stream" && seg[2]) {
    const s = byId.get(decodeURIComponent(seg[2]));
    return sendRange(
      req,
      res,
      audio(s?.id ?? "x", s?.duration ?? 180),
      "audio/wav",
    );
  }
  if (seg[1] === "cover" && seg[2]) {
    res.writeHead(200, {
      "Content-Type": "image/svg+xml",
      "Cache-Control": "no-store",
    });
    return res.end(cover(decodeURIComponent(seg[2])));
  }

  if (p === "/healthz") return json(res, 200, { ok: true });
  json(res, 404, { error: `mock has no route for ${m} ${p}` });
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`mock api listening on http://127.0.0.1:${PORT}`);
  console.log(
    `  ${SONGS.length} songs, ${PLAYLISTS.length} playlists, ${DECK.length} deck cards`,
  );
});
