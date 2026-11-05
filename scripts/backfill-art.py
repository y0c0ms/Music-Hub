#!/usr/bin/env python3
"""Embed cover art into library tracks that were downloaded without it.

Why this exists
---------------
Downloads carried no artwork until music-hub v29 (2026-08-18): yt-dlp was never
asked for a thumbnail, and the ffmpeg tag pass destroyed embedded art on Opus
anyway. v29 fixed new downloads; this fixes the ones already on disk.

It runs INSIDE the music-hub container, which already has the three things it
needs - yt-dlp, ffmpeg and mutagen - so there is no new image and no new dependency.

    docker cp scripts/backfill-art.py <container>:/tmp/
    docker exec <container> python3 /tmp/backfill-art.py
    docker exec <container> python3 /tmp/backfill-art.py --apply

How art is found
----------------
The same path a fresh download uses, because that is the path known to work:
`yt-dlp --flat-playlist --dump-json ytsearch3:"<artist> <title>"`, then the
thumbnail of the best-matching hit.

The match is guarded by token overlap. A search for an obscure single happily
returns a different song, and stamping the wrong cover onto somebody's library is
worse than leaving it blank: a blank square is honest, a wrong cover is a lie that
survives every future scan. Everything rejected is printed with the candidate and
its score, so the judgement can be reviewed instead of trusted.

Safety
------
- Dry run is the DEFAULT. Nothing is written without --apply.
- Tags are written with mutagen, never ffmpeg: ffmpeg's Ogg muxer drops
  METADATA_BLOCK_PICTURE, which is the bug that caused all this.
- Files that already carry art are skipped, so re-running is free.
- The audio stream is never touched. This adds one comment; it is not a re-encode.
"""

import argparse
import base64
import json
import os
import re
import struct
import subprocess
import sys
import urllib.request
from urllib.parse import urlsplit, urlunsplit

AUDIO = (".opus", ".mp3", ".flac", ".ogg", ".m4a")
SEARCH_TIMEOUT = 60
FETCH_TIMEOUT = 30

# A cover much larger than this is a poster, not a thumbnail, and bloats every
# file it lands in.
MAX_ART_BYTES = 900_000

# Words that carry no identity. They appear in half of all YouTube music titles
# and most of this library's filenames, so leaving them in makes a weak match
# look strong.
NOISE = {
    "official", "video", "audio", "lyric", "lyrics", "visualizer", "clipe",
    "oficial", "hd", "hq", "remaster", "remastered", "feat", "ft", "featuring",
    "with", "the", "a", "o", "e", "de", "da", "do", "prod", "music",
}

# How much of the file's title must appear in the candidate. 0.7 accepts a
# reordered artist prefix and a trailing "(Visualizer)" - both real cases from
# this library - while still rejecting a different song, which is the case that
# actually matters.
MATCH_RATIO = 0.7


def norm(s):
    s = s.lower()
    s = re.sub(r"\((official|lyric|clipe|video|audio)[^)]*\)", " ", s)
    s = re.sub(r"[^\w\s]", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def tokens(s):
    return {w for w in norm(s).split() if w and w not in NOISE}


def overlap(file_title, hit_title):
    """Fraction of the file's identifying words present in the candidate."""
    want, got = tokens(file_title), tokens(hit_title)
    if not want:
        return 0.0
    return len(want & got) / len(want)


def has_art(f):
    if f is None:
        return False
    try:
        if f.get("metadata_block_picture"):
            return True
    except Exception:
        pass
    tags = getattr(f, "tags", None) or {}
    try:
        for k in tags:
            if "APIC" in str(k) or "covr" in str(k):
                return True
    except Exception:
        pass
    return False


def read_tags(f):
    def one(key):
        try:
            v = f.get(key) or [""]
        except Exception:
            return ""
        return (v[0] if isinstance(v, list) else str(v)).strip()

    title, artist = one("title"), one("artist")
    if not title:
        tags = getattr(f, "tags", None) or {}
        try:
            title = str(tags.get("TIT2", "") or "").strip()
            artist = str(tags.get("TPE1", "") or "").strip()
        except Exception:
            pass
    return title, artist


def search(query, n=3):
    out = subprocess.run(
        ["yt-dlp", "--ignore-config", "--no-warnings", "--flat-playlist",
         "--dump-json", "--", f"ytsearch{n}:{query}"],
        capture_output=True, text=True, timeout=SEARCH_TIMEOUT,
    ).stdout
    hits = []
    for line in out.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            hits.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return hits


def pick(hits, title):
    """Best hit whose title plausibly IS this track, by token overlap.

    Substring containment was the first attempt and it was too strict: it
    rejected "BK', Nansy Silvvz - Amanhecer (Visualizer)" for a file titled
    "Amanhecer feat. Nansy Silvvz" - the same song with the artist moved to the
    front. Token overlap sees through word order, which is exactly what YouTube
    titles vary.

    Returns (hit, thumbnail_or_None, score). A None thumbnail means "not
    confident enough", and the caller leaves the file alone.
    """
    best, best_thumb, best_score = None, None, 0.0
    for h in hits:
        score = overlap(title, h.get("title") or "")
        if score <= best_score and best is not None:
            continue
        thumb = h.get("thumbnail")
        if not thumb and h.get("thumbnails"):
            thumb = h["thumbnails"][-1].get("url")
        best, best_thumb, best_score = h, thumb, score
    if best_score >= MATCH_RATIO and best_thumb:
        return best, best_thumb, best_score
    return best, None, best_score


def fetch(url):
    """Fetch a cover image by http/https only.

    The URL comes out of a third-party MusicBrainz response, and urllib honours
    whatever scheme it is handed - including file://, which would turn a hostile
    cover-art field into a local file read. So the URL is parsed and rebuilt
    from validated parts rather than passed through: an unexpected scheme is
    rejected outright, and the fragment (which is not sent to the server) is
    dropped."""
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.netloc:
        raise ValueError(f"refusing non-http cover url: {url!r}")
    safe = urlunsplit((parts.scheme, parts.netloc, parts.path, parts.query, ""))
    # The rule fires on any non-literal argument and cannot see the two checks
    # above: `safe` is rebuilt by urlunsplit from a scheme already constrained
    # to http/https with a non-empty netloc, so no caller-supplied scheme
    # reaches urlopen and file:// is unreachable. Suppressed here rather than
    # in .semgrepignore, so the rule still guards every other urlopen call.
    with urllib.request.urlopen(safe, timeout=FETCH_TIMEOUT) as r:  # nosemgrep
        return r.read()


def to_jpeg(data):
    """Normalise to JPEG. YouTube serves webp, which many players will not draw."""
    p = subprocess.run(
        ["ffmpeg", "-nostdin", "-loglevel", "error", "-y", "-i", "pipe:0",
         "-vf", r"scale=min(600\,iw):-1", "-f", "mjpeg", "pipe:1"],
        input=data, capture_output=True, timeout=FETCH_TIMEOUT,
    )
    return p.stdout if p.returncode == 0 and p.stdout else None


def jpeg_size(data):
    """Width/height from the JPEG SOF marker, for the picture block header."""
    i = 2
    while i < len(data) - 9:
        if data[i] != 0xFF:
            i += 1
            continue
        m = data[i + 1]
        if m in (0xC0, 0xC1, 0xC2, 0xC3):
            h, w = struct.unpack(">HH", data[i + 5:i + 9])
            return w, h
        if m in (0xD8, 0xD9) or 0xD0 <= m <= 0xD7:
            i += 2
            continue
        i += 2 + struct.unpack(">H", data[i + 2:i + 4])[0]
    return 0, 0


def picture_block(jpg):
    """A FLAC PICTURE block, base64'd - how Ogg/Opus carries cover art."""
    w, h = jpeg_size(jpg)
    mime, desc = b"image/jpeg", b""
    body = struct.pack(">I", 3)                       # 3 = front cover
    body += struct.pack(">I", len(mime)) + mime
    body += struct.pack(">I", len(desc)) + desc
    body += struct.pack(">IIII", w, h, 24, 0)
    body += struct.pack(">I", len(jpg)) + jpg
    return base64.b64encode(body).decode("ascii")


def embed(path, jpg):
    import mutagen
    if path.lower().endswith((".opus", ".ogg", ".flac")):
        f = mutagen.File(path)
        f["metadata_block_picture"] = [picture_block(jpg)]
        f.save()
        return
    from mutagen.id3 import APIC
    from mutagen.mp3 import MP3
    m = MP3(path)
    if m.tags is None:
        m.add_tags()
    m.tags.add(APIC(encoding=3, mime="image/jpeg", type=3, desc="", data=jpg))
    m.save()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default="/music")
    ap.add_argument("--apply", action="store_true", help="write; default is a dry run")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()

    import mutagen  # here so --help works without it

    todo = []
    for root, _, files in os.walk(args.root):
        for name in sorted(files):
            if not name.lower().endswith(AUDIO):
                continue
            p = os.path.join(root, name)
            try:
                f = mutagen.File(p)
            except Exception as e:
                print(f"  SKIP  unreadable: {os.path.relpath(p, args.root)}: {e}")
                continue
            if not has_art(f):
                todo.append((p, f))

    if args.limit:
        todo = todo[: args.limit]

    print(f"{len(todo)} file(s) without cover art"
          f"{'' if args.apply else ' - DRY RUN, nothing will be written'}\n")

    done = skipped = failed = 0
    for p, f in todo:
        rel = os.path.relpath(p, args.root)
        title, artist = read_tags(f)
        if not title:
            print(f"  SKIP  no title tag, cannot search: {rel}")
            skipped += 1
            continue

        try:
            hits = search(f"{artist} {title}".strip())
        except subprocess.TimeoutExpired:
            print(f"  FAIL  search timed out: {rel}")
            failed += 1
            continue

        hit, thumb, score = pick(hits, title)
        if not thumb:
            got = (hit or {}).get("title", "(nothing)")
            print(f"  SKIP  {rel}\n        best was {got!r} at {score:.2f} < {MATCH_RATIO} - left blank on purpose")
            skipped += 1
            continue

        if not args.apply:
            print(f"  WOULD {rel}\n        <- {hit.get('title')!r} ({score:.2f})")
            done += 1
            continue

        try:
            jpg = to_jpeg(fetch(thumb))
            if not jpg or len(jpg) > MAX_ART_BYTES:
                print(f"  FAIL  unusable image ({len(jpg or b'')} bytes): {rel}")
                failed += 1
                continue
            embed(p, jpg)
            print(f"  OK    {rel}  ({len(jpg)} bytes, {score:.2f})")
            done += 1
        except Exception as e:
            print(f"  FAIL  {rel}: {e}")
            failed += 1

    print(f"\n{'embedded' if args.apply else 'would embed'}={done} skipped={skipped} failed={failed}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
