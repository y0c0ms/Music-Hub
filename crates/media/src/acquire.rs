//! Turns "I want this track" into a file in the music library.
//!
//! Everything here shells out to yt-dlp and ffmpeg, which makes it the part of
//! the service most exposed to injection. Two rules hold throughout and are not
//! negotiable:
//!
//! 1. Every external process is started with an argument slice — there is no
//!    shell anywhere in this module, so quoting bugs cannot become command
//!    execution.
//! 2. A source id is checked against [`valid_source_id`] before it is used for
//!    anything. Titles and artist names become path segments only, through
//!    [`safe_segment`] first.
//!
//! A faithful port of the Go `internal/acquire` package. State (the job list,
//! caches, per-id clip locks, alerting) lives in the server; this module is the
//! stateless shell-out engine plus its pure helpers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use mh_core::model::{Acquire, AcquireAlbum, SourceMeta};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::{tags, ytdlp, MediaError};

type Result<T, E = MediaError> = std::result::Result<T, E>;

const SEARCH_LIMIT: Duration = Duration::from_secs(45);
const CLIP_LIMIT: Duration = Duration::from_secs(180);
const FETCH_LIMIT: Duration = Duration::from_secs(900);
const TAG_LIMIT: Duration = Duration::from_secs(60);
const META_LIMIT: Duration = Duration::from_secs(45);

const MAX_CLIPS: usize = 200;
const SEGMENT_RUNES: usize = 80;
const TMP_PREFIX: &str = ".mh-";
/// One argv element: yt-dlp parses it, no shell does.
const CLIP_SECTION: &str = "*0:30-1:00";

const MAX_ALBUMS: usize = 8;
const MAX_RESOLVE: usize = 12;
const MAX_ALBUM_RESOLVES: usize = 6;
/// How many metadata subprocesses (yt-dlp lookups, the tagger) may run at once,
/// across every request.
///
/// Each yt-dlp holds ~35 MB, and the pod's limit is 768 MiB. Nothing bounded them
/// globally: the album search bounds itself to 6 per query, but every typed
/// pause starts another query, and each cover tile the browser loads starts its
/// own lookup, so 30 tiles meant 30 processes (measured). The kernel killed the
/// pod 8 times in two days, each time with 14-36 yt-dlp processes alive.
/// Downloads have their own, smaller budget in [`ytdlp::slot`].
const MAX_LOOKUPS: usize = 4;
static LOOKUP_SLOTS: Semaphore = Semaphore::const_new(MAX_LOOKUPS);
/// Albums for a bare title sit behind a block of loose songs; a single flat dump
/// is cheap to scan deep. "brasilian skies" returned album ids at positions
/// 13-15 of 27, which the old `n*3` cutoff scanned right past.
const ALBUM_SEARCH_DEPTH: usize = 40;

/// The Albums tab of YouTube Music search, as the opaque protobuf YouTube Music
/// itself sends. If album results come back empty while track search works,
/// suspect this first.
const YT_MUSIC_ALBUMS_FILTER: &str = "EgWKAQIYAWoKEAkQChAFEAMQBA%3D%3D";
const YT_MUSIC_ALBUM_TITLE_PREFIX: &str = "Album - ";
/// Marks a search hit as an album (`MPREb_...`) rather than a loose song.
const ALBUM_BROWSE_PREFIX: &str = "MPREb_";

/// Rewrites an Opus comment header in place with mutagen rather than ffmpeg,
/// because ffmpeg DESTROYS embedded cover art on Ogg/Opus (measured 2026-08-18:
/// `--embed-thumbnail` wrote a 51,324-byte `METADATA_BLOCK_PICTURE`; a plain
/// `ffmpeg -c copy -metadata` dropped it). Values arrive as argv, never
/// interpolated. Title, artist and - for album downloads - album + albumartist
/// are written; the album fields only when non-empty, so single songs stay
/// album-less. mutagen writes Vorbis comments that ffprobe reads back (verified).
const TAG_SCRIPT: &str = "\
import sys, mutagen
path, title, artist, album, album_artist = sys.argv[1:6]
f = mutagen.File(path)
if f is None:
    raise SystemExit(\"unsupported audio file: \" + path)
f[\"title\"] = [title]
f[\"artist\"] = [artist]
if album:
    f[\"album\"] = [album]
    if album_artist:
        f[\"albumartist\"] = [album_artist]
f.save()
";

// ----------------------------------------------------------------- validation

/// Whether `s` is safe to hand to yt-dlp.
///
/// YouTube ids are 11 chars of `[A-Za-z0-9_-]`; the 5..=20 window leaves room
/// for other extractors without admitting anything readable as a path, a URL,
/// or an option. Explicit ASCII (not `\w`) so no Unicode look-alike sneaks through.
#[must_use]
pub fn valid_source_id(s: &str) -> bool {
    let len = s.len();
    (5..=20).contains(&len)
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

// -------------------------------------------------------------------- entries

/// A flat-playlist search entry. Shapes are partial on purpose: only the fields
/// used are decoded, so a yt-dlp field addition never breaks parsing.
#[derive(Debug, Default, Deserialize)]
struct YtEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    uploader: String,
    #[serde(default)]
    channel: String,
    /// Set when the extractor found real music metadata.
    #[serde(default)]
    artist: String,
    #[serde(default)]
    track: String,
    #[serde(default)]
    thumbnail: String,
}

/// A resolved album from `--dump-single-json`.
#[derive(Debug, Default, Deserialize)]
struct YtAlbum {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    playlist_count: i32,
    #[serde(default)]
    view_count: i64,
    #[serde(default)]
    entries: Vec<YtEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct MetaRaw {
    #[serde(default)]
    upload_date: String,
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    view_count: Option<i64>,
    #[serde(default)]
    channel: String,
    #[serde(default)]
    uploader: String,
}

// ---------------------------------------------------------------------- search

/// Ask yt-dlp what exists for a query without downloading a byte.
/// `--flat-playlist` is what keeps this fast.
///
/// # Errors
/// Fails when yt-dlp cannot be run or the query is empty.
pub async fn search_source(q: &str, n: usize) -> Result<Vec<Acquire>> {
    let q = q.trim();
    if q.is_empty() {
        return Err(MediaError::Parse("empty query".to_owned()));
    }
    let n = n.clamp(1, MAX_SEARCH_RESULTS);
    // "ytsearchN:query" is one argv element; the trailing "--" stops yt-dlp
    // reading it as an option regardless.
    let query = format!("ytsearch{n}:{q}");
    let out = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--flat-playlist",
            "--dump-json",
            "--",
            &query,
        ],
        SEARCH_LIMIT,
    )
    .await?;
    Ok(parse_search_entries(&out, n))
}

/// The most results one source search returns: `search_source` clamps its `n`
/// to this, and the parser never reserves more than this however `n` arrived.
const MAX_SEARCH_RESULTS: usize = 25;

fn parse_search_entries(out: &[u8], n: usize) -> Vec<Acquire> {
    let n = n.min(MAX_SEARCH_RESULTS);
    let text = String::from_utf8_lossy(out);
    let mut results = Vec::with_capacity(n);
    for line in text.lines() {
        if results.len() >= n {
            break;
        }
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(e) = serde_json::from_str::<YtEntry>(line) else {
            continue;
        };
        if !valid_source_id(&e.id) {
            continue;
        }
        let title = first_non_empty(&[&e.track, &e.title]);
        if title.is_empty() {
            continue;
        }
        results.push(Acquire {
            source_id: e.id,
            title,
            artist: first_non_empty(&[
                &e.artist,
                &clean_channel(&first_non_empty(&[&e.uploader, &e.channel])),
            ]),
            duration: duration_seconds(e.duration),
            thumb: e.thumbnail,
        });
    }
    results
}

/// Resolve one song to the artist YouTube Music attributes to it. Best-effort:
/// any failure yields an empty string and the caller falls back.
pub async fn song_artist(source_id: &str) -> String {
    if !valid_source_id(source_id) {
        return String::new();
    }
    let url = format!("https://music.youtube.com/watch?v={source_id}");
    let Ok(out) = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--no-playlist",
            "--dump-single-json",
            "--",
            &url,
        ],
        META_LIMIT,
    )
    .await
    else {
        return String::new();
    };
    let Ok(e) = serde_json::from_slice::<YtEntry>(&out) else {
        return String::new();
    };
    let raw = first_non_empty(&[
        &e.artist,
        &clean_channel(&first_non_empty(&[&e.uploader, &e.channel])),
    ]);
    tags::clean(&raw, "").artist
}

// ----------------------------------------------------------------- source meta

/// When a source was published and how much it has been played. A FULL
/// extraction (no `--flat-playlist`), so callers cache it and keep it off any
/// critical path.
///
/// # Errors
/// Fails when yt-dlp cannot be run, the id is invalid, or the JSON is unparseable.
pub async fn source_meta(source_id: &str) -> Result<SourceMeta> {
    if !valid_source_id(source_id) {
        return Err(MediaError::Parse(format!(
            "invalid sourceId {:?}",
            trunc(source_id, 40)
        )));
    }
    let url = watch_url(source_id);
    let out = run(
        "yt-dlp",
        &ytdlp::media(&[
            "--ignore-config",
            "--no-warnings",
            "--no-playlist",
            "--skip-download",
            "--dump-single-json",
            "--",
            &url,
        ]),
        META_LIMIT,
    )
    .await?;
    parse_source_meta(&out)
}

fn parse_source_meta(out: &[u8]) -> Result<SourceMeta> {
    let raw: MetaRaw = serde_json::from_slice(out)
        .map_err(|e| MediaError::Parse(format!("parse metadata: {e}")))?;
    let mut m = SourceMeta {
        channel: clean_channel(&first_non_empty(&[&raw.channel, &raw.uploader])),
        views: raw.view_count.filter(|v| *v > 0).unwrap_or(0),
        posted_at: String::new(),
    };
    // upload_date (YYYYMMDD) is the reliable one; timestamp is the fallback.
    let d = raw.upload_date.trim();
    if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) {
        m.posted_at = format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8]);
    } else if let Some(ts) = raw.timestamp.filter(|t| *t > 0) {
        if let Some(dt) = chrono::DateTime::from_timestamp(ts, 0) {
            m.posted_at = dt.format("%Y-%m-%d").to_string();
        }
    }
    Ok(m)
}

// -------------------------------------------------------------------- preview

/// A local path to a ~30s sample of a track not in the library.
///
/// Downloads it on first use. The window starts at 0:30: the first half-minute
/// is intros and silence, the worst thing to judge a song by. The caller
/// serialises concurrent requests for the same id.
///
/// # Errors
/// Fails when yt-dlp cannot be run or the id is invalid.
pub async fn preview_clip(cache_dir: &Path, source_id: &str) -> Result<PathBuf> {
    if !valid_source_id(source_id) {
        return Err(MediaError::Parse(format!(
            "invalid sourceId {:?}",
            trunc(source_id, 40)
        )));
    }
    // source_id passed validation, so it is a single safe path segment.
    let final_path = cache_dir.join(format!("{source_id}.opus"));
    if fresh(&final_path) {
        // Touch so eviction is least-recently-used, not oldest-download.
        if let Ok(f) = std::fs::File::open(&final_path) {
            let _ = f.set_modified(SystemTime::now());
        }
        return Ok(final_path);
    }

    tokio::fs::create_dir_all(cache_dir).await?;
    let stem = tmp_stem();
    let _sweep = TmpSweep::new(cache_dir, &stem);
    let template = cache_dir.join(format!("{stem}.%(ext)s"));
    let template = path_str(&template)?;
    let url = watch_url(source_id);
    run(
        "yt-dlp",
        &ytdlp::media(&[
            "--ignore-config",
            "--no-warnings",
            "--no-playlist",
            "--no-progress",
            "--download-sections",
            CLIP_SECTION,
            "-x",
            "--audio-format",
            "opus",
            "-o",
            &template,
            "--",
            &url,
        ]),
        CLIP_LIMIT,
    )
    .await?;

    let got = find_temp(cache_dir, &stem)?;
    tokio::fs::rename(&got, &final_path).await?;
    evict_clips(cache_dir);
    Ok(final_path)
}

fn evict_clips(cache_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    let mut clips: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name.strip_suffix(".opus") else {
            continue;
        };
        if !valid_source_id(stem) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        clips.push((entry.path(), mtime));
    }
    if clips.len() <= MAX_CLIPS {
        return;
    }
    clips.sort_by_key(|(_, mtime)| *mtime);
    for (path, _) in &clips[..clips.len() - MAX_CLIPS] {
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!(path = %path.display(), error = %e, "preview cache evict");
        }
    }
}

// -------------------------------------------------------------------- download

/// Download one track into `music_root/dest_rel/<artist>/<title>.opus` and
/// return the absolute path written. Skips the download when the same title
/// already exists in the artist folder in any container.
///
/// # Errors
/// Fails when the id is invalid, the title is empty, yt-dlp fails, or the file
/// cannot be moved into place.
pub async fn fetch(
    music_root: &str,
    dest_rel: &str,
    source_id: &str,
    title: &str,
    artist: &str,
    album: &str,
    album_artist: &str,
) -> Result<PathBuf> {
    if !valid_source_id(source_id) {
        return Err(MediaError::Parse(format!(
            "invalid sourceId {:?}",
            trunc(source_id, 40)
        )));
    }
    let mut title = title.trim().to_owned();
    if title.is_empty() {
        return Err(MediaError::Parse("title is required".to_owned()));
    }
    let mut artist = artist.trim().to_owned();
    if artist.is_empty() {
        "Unknown Artist".clone_into(&mut artist);
    }

    // A YouTube title routinely repeats the artist and carries "(Official
    // Video)". Those decide the tags AND the filename, so clean before writing;
    // an ambiguous split is left alone rather than guessed.
    let cleaned = tags::clean(&artist, &title);
    if !cleaned.ambiguous {
        if cleaned.title != title {
            tracing::info!(from = %title, to = %cleaned.title, "acquire cleaned title");
        }
        title = cleaned.title;
        artist = cleaned.artist;
    }

    let dir = Path::new(music_root)
        .join(dest_rel)
        .join(safe_segment(&artist));
    let final_path = dir.join(format!("{}.opus", safe_segment(&title)));

    // Already owned in any container? Hand back the copy on disk.
    if let Some(have) = already_downloaded(&dir, &title) {
        // A track fetched before album tagging has no album tag, so it cannot
        // group into its record. When an album download reaches an on-disk track,
        // stamp the album in place (mutagen bumps mtime -> the scan re-indexes it)
        // rather than re-fetching bytes we already have.
        if !album.trim().is_empty() {
            let _ = retag(&have, &title, &artist, album, album_artist).await.inspect_err(
                |e| tracing::warn!(path = %have.display(), error = %e, "acquire could not re-tag album"),
            );
        }
        tracing::info!(path = %have.display(), "acquire already in library");
        return Ok(have);
    }
    tokio::fs::create_dir_all(&dir).await?;

    let stem = tmp_stem();
    let _sweep = TmpSweep::new(&dir, &stem);
    let template = dir.join(format!("{stem}.%(ext)s"));
    let template = path_str(&template)?;
    // Try the requested id. YouTube Music album entities frequently link
    // Premium-only "Art Track" uploads that yt-dlp cannot fetch; fall back to a
    // public upload found by searching for the track when that happens.
    if let Err(e) = download_media(&template, source_id).await {
        if !is_unavailable(&e.to_string()) {
            return Err(e);
        }
        let Some(alt) = public_alternative(&artist, &title, source_id).await else {
            return Err(e);
        };
        tracing::info!(from = %source_id, to = %alt, "acquire fell back to a public upload");
        download_media(&template, &alt).await?;
    }

    let got = find_temp(&dir, &stem)?;
    // yt-dlp tags from the source, where "artist" and "title" are often
    // "Official Video". Rewrite with what the user asked for, keeping cover art.
    match retag(&got, &title, &artist, album, album_artist).await {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!(path = %got.display(), error = %e, "acquire could not tag (keeping untagged)");
        }
    }

    // Same directory, so the rename is atomic and no partial ever carries the
    // final name.
    tokio::fs::rename(&got, &final_path).await?;
    tracing::info!(path = %final_path.display(), "acquire wrote");
    Ok(final_path)
}

async fn download_media(template: &str, source_id: &str) -> Result<()> {
    let url = watch_url(source_id);
    run(
        "yt-dlp",
        &ytdlp::media(&[
            "--ignore-config",
            "--no-warnings",
            "--no-playlist",
            "--no-progress",
            "-x",
            "--audio-format",
            "opus",
            "--audio-quality",
            "0",
            "--embed-thumbnail",
            "--convert-thumbnails",
            "jpg",
            "-o",
            template,
            "--",
            &url,
        ]),
        FETCH_LIMIT,
    )
    .await
    .map(|_| ())
}

/// yt-dlp messages that mean this exact id is not downloadable (Premium-only,
/// private, removed) rather than a transient failure, so falling back to another
/// upload is worthwhile.
fn is_unavailable(msg: &str) -> bool {
    [
        "Music Premium",
        "members-only",
        "Private video",
        "Video unavailable",
        "This video is unavailable",
        "video is not available",
        "removed by the uploader",
    ]
    .iter()
    .any(|sig| msg.contains(sig))
}

/// The id of a public upload for a track, found by searching and avoiding the id
/// that just failed. `None` when nothing else turns up.
async fn public_alternative(artist: &str, title: &str, avoid: &str) -> Option<String> {
    let query = format!("{artist} {title}");
    let results = search_source(query.trim(), 3).await.ok()?;
    results
        .into_iter()
        .map(|a| a.source_id)
        .find(|id| id != avoid && valid_source_id(id))
}

async fn retag(
    src: &Path,
    title: &str,
    artist: &str,
    album: &str,
    album_artist: &str,
) -> Result<()> {
    let src = path_str(src)?;
    run(
        "python3",
        &["-c", TAG_SCRIPT, &src, title, artist, album, album_artist],
        TAG_LIMIT,
    )
    .await
    .map(|_| ())
}

/// Clean a (title, artist) the way a download names it: trimmed, a default
/// artist, and yt-dlp cruft removed unless the split is ambiguous.
///
/// The scanner stores these cleaned values, so a dedup lookup matches the
/// library with them.
#[must_use]
pub fn clean_identity(title: &str, artist: &str) -> (String, String) {
    let mut title = title.trim().to_owned();
    let mut artist = artist.trim().to_owned();
    if artist.is_empty() {
        "Unknown Artist".clone_into(&mut artist);
    }
    let cleaned = tags::clean(&artist, &title);
    if !cleaned.ambiguous {
        title = cleaned.title;
        artist = cleaned.artist;
    }
    (title, artist)
}

/// Locate an on-disk track for a (user dir, artist, title) the way [`fetch`]
/// names it - same tag cleaning, same `safe_segment` path - so the album repair
/// tool finds a file downloaded earlier. Returns the path and the cleaned title
/// and artist, or `None` when it is not on disk.
fn located(
    music_root: &str,
    dest_rel: &str,
    title: &str,
    artist: &str,
) -> Option<(PathBuf, String, String)> {
    let (title, artist) = clean_identity(title, artist);
    let dir = Path::new(music_root)
        .join(dest_rel)
        .join(safe_segment(&artist));
    already_downloaded(&dir, &title).map(|p| (p, title, artist))
}

/// Whether a track is already on disk, without downloading - the album repair
/// tool's dry run.
#[must_use]
pub fn is_present(music_root: &str, dest_rel: &str, title: &str, artist: &str) -> bool {
    located(music_root, dest_rel, title, artist).is_some()
}

/// Re-tag an on-disk track with its album, NEVER downloading. Returns whether a
/// matching file was found and tagged - "fix the tags of what you already have".
///
/// # Errors
/// Returns [`MediaError`] only if the tag rewrite fails; a missing file is
/// `Ok(false)`, not an error.
pub async fn retag_if_present(
    music_root: &str,
    dest_rel: &str,
    title: &str,
    artist: &str,
    album: &str,
    album_artist: &str,
) -> Result<bool> {
    match located(music_root, dest_rel, title, artist) {
        Some((have, t, a)) => {
            retag(&have, &t, &a, album, album_artist).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Re-tag a specific on-disk file, keeping cover art.
///
/// The library-wide dedup path uses this to fold an already-owned track into a
/// freshly requested album, without a directory scan.
///
/// # Errors
/// Fails when the tagger cannot run or rewrite the file.
pub async fn retag_path(
    path: &Path,
    title: &str,
    artist: &str,
    album: &str,
    album_artist: &str,
) -> Result<()> {
    retag(path, title, artist, album, album_artist).await
}

/// Resolve a YouTube album *playlist* (`OLAK5uy_…` list id) to its tracklist.
///
/// Most albums on YouTube are published as these auto-generated playlists, which
/// [`search_albums`] - YouTube Music `MPREb_` album entities only - does not
/// surface, so an independent release found solely as a playlist (a mixtape) is
/// reachable this way and not the other.
///
/// # Errors
/// Fails when yt-dlp cannot run or the playlist JSON is unparseable.
pub async fn resolve_playlist(list_id: &str) -> Result<AcquireAlbum> {
    let url = format!(
        "https://music.youtube.com/playlist?list={}",
        query_escape(list_id)
    );
    let out = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--flat-playlist",
            "--dump-single-json",
            "--",
            &url,
        ],
        SEARCH_LIMIT,
    )
    .await?;
    parse_album_json(&out)
}
// ------------------------------------------------------- releases / channels

/// List the release entries on a channel's "Releases" shelf.
///
/// `--flat-playlist` keeps this to one request: it returns the entry ids and
/// titles without resolving the individual videos, which measured at ~2.8s per
/// channel against ~1m42s for the full dump. Only the id and title are read by
/// the caller; the rest of the JSON is discarded.
///
/// # Errors
/// Fails when yt-dlp cannot run or its output is not JSON.
pub async fn channel_release_shelf(channel_id: &str) -> Result<Value> {
    if !valid_channel_id(channel_id) {
        return Err(MediaError::Parse(format!("not a channel id: {channel_id}")));
    }
    let url = format!(
        "https://www.youtube.com/channel/{}/releases",
        query_escape(channel_id)
    );
    let out = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--flat-playlist",
            "--dump-single-json",
            "--playlist-end",
            "20",
            "--",
            &url,
        ],
        SEARCH_LIMIT,
    )
    .await?;
    serde_json::from_slice(&out).map_err(|e| MediaError::Parse(format!("release shelf: {e}")))
}

/// The current signed cover URL for a release playlist.
///
/// YouTube signs its thumbnail URLs with a `?sqp=…&rs=…` query, and the
/// signature goes stale: a URL stored in a cache table 404s within days, while a
/// freshly listed URL serves. The unsigned sizes 404 for release playlists, so
/// the only usable cover is the one the playlist reports right now.
///
/// # Errors
/// Fails when yt-dlp cannot run, its output is not JSON, or the playlist has no
/// signed thumbnail.
pub async fn playlist_cover_url(playlist_id: &str) -> Result<String> {
    if !valid_playlist_id(playlist_id) {
        return Err(MediaError::Parse(format!(
            "not a playlist id: {playlist_id}"
        )));
    }
    let url = format!(
        "https://www.youtube.com/playlist?list={}",
        query_escape(playlist_id)
    );
    let out = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--flat-playlist",
            "--dump-single-json",
            "--playlist-end",
            "1",
            "--",
            &url,
        ],
        SEARCH_LIMIT,
    )
    .await?;
    let body: Value = serde_json::from_slice(&out)
        .map_err(|e| MediaError::Parse(format!("playlist cover: {e}")))?;
    body["thumbnails"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["url"].as_str().is_some_and(|u| u.contains('?')))
        .max_by_key(|t| t["width"].as_u64().unwrap_or(0))
        .and_then(|t| t["url"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| MediaError::Parse(format!("no signed cover for {playlist_id}")))
}

/// Read a playlist's own `modified_date` without resolving its videos.
///
/// This is the cheap pre-filter the nightly sweep needs: `--flat-playlist` costs
/// ~1.8s against ~13s for [`release_playlist`], and an artist's shelf is mostly
/// old records. Spending 1.8s to skip a 13s resolve is what makes a 50-artist
/// sweep affordable.
///
/// # Errors
/// Fails when yt-dlp cannot run or its output is not JSON.
pub async fn playlist_modified_date(playlist_id: &str) -> Result<String> {
    if !valid_playlist_id(playlist_id) {
        return Err(MediaError::Parse(format!(
            "not a playlist id: {playlist_id}"
        )));
    }
    let url = format!(
        "https://www.youtube.com/playlist?list={}",
        query_escape(playlist_id)
    );
    let out = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--flat-playlist",
            "--dump-single-json",
            "--playlist-end",
            "1",
            "--",
            &url,
        ],
        SEARCH_LIMIT,
    )
    .await?;
    let body: Value = serde_json::from_slice(&out)
        .map_err(|e| MediaError::Parse(format!("playlist date: {e}")))?;
    // Returned in the same ISO form the rest of the sweep compares against;
    // yt-dlp's raw value is YYYYMMDD, which sorts wrongly against "2026-05-31".
    let raw = body["modified_date"].as_str().unwrap_or("").trim();
    Ok(
        if raw.len() == 8 && raw.bytes().all(|b| b.is_ascii_digit()) {
            format!("{}-{}-{}", &raw[..4], &raw[4..6], &raw[6..8])
        } else {
            String::new()
        },
    )
}

/// A YouTube playlist id, as a channel's Releases shelf reports it.
///
/// A release playlist created through YouTube Music carries an `OLAK5uy_` prefix
/// and is 44 characters; other playlists are 34. This is deliberately *not*
/// [`valid_source_id`], which allows 5-20 characters because it validates a
/// single *video* id. Reusing it here silently rejected every release playlist:
/// the source logged itself healthy and returned nothing, which is the worst
/// shape of bug - no error, no rows, no clue.
fn valid_playlist_id(s: &str) -> bool {
    const PREFIXES: &[&str] = &["OLAK5uy_", "PL", "UU", "LL"];
    (30..=50).contains(&s.len())
        && PREFIXES.iter().any(|p| s.starts_with(p))
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Resolve one release playlist into its track titles, cover and per-track dates.
///
/// Unlike the flat shelf listing this resolves every entry, because the modal
/// upload date can only be computed from real per-track dates. It costs ~13s, so
/// it runs only for playlists that survived [`playlist_modified_date`].
///
/// # Errors
/// Fails when yt-dlp cannot run or its output is not JSON.
pub async fn release_playlist(playlist_id: &str) -> Result<Value> {
    if !valid_playlist_id(playlist_id) {
        return Err(MediaError::Parse(format!(
            "not a playlist id: {playlist_id}"
        )));
    }
    let url = format!(
        "https://www.youtube.com/playlist?list={}",
        query_escape(playlist_id)
    );
    let out = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--dump-single-json",
            "--",
            &url,
        ],
        SEARCH_LIMIT,
    )
    .await?;
    serde_json::from_slice(&out).map_err(|e| MediaError::Parse(format!("release playlist: {e}")))
}

/// A channel id as YouTube writes it: `UC` plus 22 url-safe base64 characters.
fn valid_channel_id(s: &str) -> bool {
    s.len() == 24
        && s.starts_with("UC")
        && s[2..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// ------------------------------------------------------- artist -> channel

/// A channel that has been accepted as belonging to an artist, and why.
pub struct ChannelHit {
    pub channel_id: String,
    pub channel_name: String,
    /// `song` - a known track the user owns was found on it. `name` - only the
    /// channel name matched. `song` is the stronger evidence.
    pub confirmed_by: String,
}

/// Find an artist's YouTube channel, or say plainly that it could not be found.
///
/// Guessing here would put a stranger's album in someone's Discover list, so a
/// channel is only accepted on positive evidence, never on rank order:
///
/// * **a known song** - the caller supplies track titles the user genuinely owns
///   by this artist, and a candidate channel must actually carry one of them.
///   This is the strong test, and it is what catches `Plutonio`, whose channel is
///   named "Plutonio 2765" and would never match on name alone.
/// * **an exact name match** - the fallback for an artist with no known song to
///   offer. Weak on its own, which is why it is reported as `name` rather than
///   presented as certain.
///
/// A candidate matching neither is rejected even when it ranks first: the search
/// happily returns "Dillaz" tracks from a channel that is not Dillaz, and
/// shipping that would be worse than shipping nothing.
///
/// # Errors
/// Fails only when the search itself cannot run; a channel that does not meet the
/// bar is `Ok(None)`, not an error.
pub async fn resolve_artist_channel(
    artist: &str,
    known_songs: &[String],
) -> Result<Option<ChannelHit>> {
    let want = normalise_name(artist);
    if want.is_empty() {
        return Ok(None);
    }
    let url = format!(
        "https://www.youtube.com/results?search_query={}",
        query_escape(&format!("{artist} topic"))
    );
    let out = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--flat-playlist",
            "--dump-single-json",
            "--playlist-end",
            "8",
            "--",
            &url,
        ],
        SEARCH_LIMIT,
    )
    .await?;
    let body: Value = serde_json::from_slice(&out)
        .map_err(|e| MediaError::Parse(format!("channel search: {e}")))?;

    let mut by_name: Option<ChannelHit> = None;
    for e in body["entries"].as_array().into_iter().flatten() {
        let id = e["id"].as_str().unwrap_or("");
        if !valid_channel_id(id) {
            continue;
        }
        let name = e["title"].as_str().unwrap_or("").trim().to_owned();
        if normalise_name(&name) == want && by_name.is_none() {
            by_name = Some(ChannelHit {
                channel_id: id.to_owned(),
                channel_name: name,
                confirmed_by: "name".to_owned(),
            });
        }
    }

    // A known-song confirmation is proof where a name match is only a
    // resemblance, so it wins when both are available.
    if !known_songs.is_empty() {
        if let Some(hit) = confirm_by_song(&body, known_songs).await? {
            return Ok(Some(hit));
        }
    }
    Ok(by_name)
}

/// Look for one of `known_songs` among the candidate channels' uploads.
async fn confirm_by_song(body: &Value, known_songs: &[String]) -> Result<Option<ChannelHit>> {
    let wanted: Vec<String> = known_songs
        .iter()
        .map(|s| normalise_name(s))
        .filter(|s| !s.is_empty())
        .collect();
    if wanted.is_empty() {
        return Ok(None);
    }
    for e in body["entries"].as_array().into_iter().flatten() {
        let id = e["id"].as_str().unwrap_or("");
        if !valid_channel_id(id) {
            continue;
        }
        let url = format!(
            "https://www.youtube.com/channel/{}/videos",
            query_escape(id)
        );
        let Ok(out) = run(
            "yt-dlp",
            &[
                "--ignore-config",
                "--no-warnings",
                "--flat-playlist",
                "--dump-single-json",
                "--playlist-end",
                "60",
                "--",
                &url,
            ],
            SEARCH_LIMIT,
        )
        .await
        else {
            continue;
        };
        let Ok(list) = serde_json::from_slice::<Value>(&out) else {
            continue;
        };
        let found = list["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["title"].as_str())
            .any(|t| {
                let n = normalise_name(t);
                wanted.iter().any(|w| n.contains(w.as_str()))
            });
        if found {
            return Ok(Some(ChannelHit {
                channel_id: id.to_owned(),
                channel_name: e["title"].as_str().unwrap_or("").trim().to_owned(),
                confirmed_by: "song".to_owned(),
            }));
        }
    }
    Ok(None)
}

/// Fold a name to something comparable: accents dropped, lower case, and
/// anything from a bracketed or slashed aside cut off, so "Crônicas (oficial)"
/// and "Crônicas" compare equal.
#[must_use]
pub fn normalise_name(s: &str) -> String {
    let mut folded = String::new();
    for c in s.chars() {
        if matches!(c, '(' | '[' | '/' | '|') {
            break;
        }
        folded.extend(deaccent(c));
    }
    folded
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn deaccent(c: char) -> Vec<char> {
    let lower: String = c.to_lowercase().collect();
    match lower.as_str() {
        "á" | "à" | "ã" | "â" | "ä" | "å" => vec!['a'],
        "é" | "è" | "ê" | "ë" => vec!['e'],
        "í" | "ì" | "î" | "ï" => vec!['i'],
        "ó" | "ò" | "õ" | "ô" | "ö" => vec!['o'],
        "ú" | "ù" | "û" | "ü" => vec!['u'],
        "ç" => vec!['c'],
        "ñ" => vec!['n'],
        _ => lower.chars().collect(),
    }
}

// ---------------------------------------------------------------------- albums

/// Find whole albums for a query.
///
/// Two id sources: the query's own YouTube Music Albums search, and — when that
/// comes up short for a bare title — the top loose song resolved to its artist,
/// whose album search returns the real record. Everything is ranked by title
/// overlap so the named album wins.
///
/// # Errors
/// Fails only when the primary search cannot run (offline, yt-dlp missing).
pub async fn search_albums(q: &str, n: usize) -> Result<Vec<AcquireAlbum>> {
    let q = q.trim();
    if q.is_empty() {
        return Err(MediaError::Parse("empty query".to_owned()));
    }
    let n = n.clamp(1, MAX_ALBUMS);

    let (mut ids, seed) = album_search(q).await?;
    // Short on albums for what looks like a title query? Seed the artist search
    // off the best song hit. The seed failing is not fatal.
    if ids.len() < n && !seed.is_empty() {
        let artist = song_artist(&seed).await;
        if !artist.is_empty() && !artist.eq_ignore_ascii_case(q) {
            if let Ok((more, _)) = album_search(&artist).await {
                ids = merge_unique(&[ids, more]);
            }
        }
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut albums = resolve_albums(ids).await;
    rank_albums_by_title(&mut albums, q);
    albums.truncate(n);
    Ok(albums)
}

/// The online album that best completes a local one, plus the tracks it has
/// that are not owned yet. Built by [`find_album_completion`]; never downloads.
pub struct AlbumCompletion {
    pub album: AcquireAlbum,
    pub missing: Vec<Acquire>,
    pub have: usize,
}

/// Find the online album that completes a local one, anchored on owned titles.
///
/// The match is verified, not guessed: the right album must contain the tracks
/// already on disk. `None` when no confident match exists - the album may live
/// on YouTube only as loose songs, or be indistinguishable from a compilation.
/// Never downloads; the caller confirms the missing tracks first.
///
/// # Errors
/// Fails only when the album search cannot run (offline, yt-dlp missing).
pub async fn find_album_completion(
    name: &str,
    artist: &str,
    owned_titles: &[String],
) -> Result<Option<AlbumCompletion>> {
    let query = format!("{name} {artist}");
    let candidates = search_albums(query.trim(), MAX_ALBUMS).await?;
    Ok(best_completion(candidates, owned_titles))
}

/// Pick the candidate that contains the most owned tracks while being larger
/// than the local album. Requires >= 60% of owned titles to appear, so a weak
/// coincidence does not pass; the user still confirms the result.
fn best_completion(
    candidates: Vec<AcquireAlbum>,
    owned_titles: &[String],
) -> Option<AlbumCompletion> {
    let mut owned: Vec<String> = owned_titles
        .iter()
        .map(|t| norm_title(t))
        .filter(|t| !t.is_empty())
        .collect();
    owned.sort();
    owned.dedup();
    if owned.is_empty() {
        return None;
    }
    let need = (owned.len() * 3).div_ceil(5).max(1); // >= 60% of owned titles
    let mut best: Option<AlbumCompletion> = None;
    let mut best_have = 0usize;
    for album in candidates {
        if album.tracks.len() <= owned.len() {
            continue; // not more complete than what is already on disk
        }
        let norm: Vec<String> = album.tracks.iter().map(|t| norm_title(&t.title)).collect();
        let have = owned
            .iter()
            .filter(|o| norm.iter().any(|n| n == *o))
            .count();
        if have < need {
            continue;
        }
        let missing: Vec<Acquire> = album
            .tracks
            .iter()
            .zip(&norm)
            .filter(|(_, n)| !owned.iter().any(|o| o == *n))
            .map(|(t, _)| t.clone())
            .collect();
        if missing.is_empty() {
            continue;
        }
        if have > best_have {
            best_have = have;
            best = Some(AlbumCompletion {
                album,
                missing,
                have,
            });
        }
    }
    best
}

/// Normalise a track title for cross-source matching: lowercase, drop
/// parenthetical noise like "(Remastered 2011)" or "(feat. X)", drop
/// punctuation, collapse whitespace.
fn norm_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut depth = 0i32;
    for ch in title.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = (depth - 1).max(0),
            _ if depth > 0 => {}
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            c if c.is_whitespace() => out.push(' '),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Run the Albums-filtered search once, returning album browse ids plus the id
/// of the top loose SONG hit (the seed for the artist search).
async fn album_search(q: &str) -> Result<(Vec<String>, String)> {
    // The query rides in as a URL parameter, so it must be escaped — an
    // unescaped "&" would truncate the search silently.
    let search_url = format!(
        "https://music.youtube.com/search?q={}&params={}",
        query_escape(q),
        YT_MUSIC_ALBUMS_FILTER
    );
    let depth = ALBUM_SEARCH_DEPTH.to_string();
    let out = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--flat-playlist",
            "--dump-json",
            "--playlist-end",
            &depth,
            "--",
            &search_url,
        ],
        SEARCH_LIMIT,
    )
    .await?;
    Ok(album_ids_and_seed(&out, MAX_RESOLVE))
}

fn album_ids_and_seed(out: &[u8], max: usize) -> (Vec<String>, String) {
    let text = String::from_utf8_lossy(out);
    let mut ids = Vec::new();
    let mut seed = String::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(e) = serde_json::from_str::<YtEntry>(line) else {
            continue;
        };
        if e.id.starts_with(ALBUM_BROWSE_PREFIX) {
            if ids.len() < max {
                ids.push(e.id);
            }
        } else if seed.is_empty() && valid_source_id(&e.id) {
            seed = e.id;
        }
    }
    (ids, seed)
}

/// Turn browse ids into albums, concurrently but bounded, dropping failures and
/// preserving input order so ranking can rely on YouTube Music's relevance as
/// the tiebreak.
async fn resolve_albums(ids: Vec<String>) -> Vec<AcquireAlbum> {
    // Inline, not `tokio::spawn`: a spawned task outlives the request that wanted
    // it, so an abandoned search (every keystroke abandons the previous one) went
    // on to start every remaining yt-dlp lookup for nobody. Awaited here, the
    // lookups are dropped with the request and `kill_on_drop` stops the children.
    // `join_all` keeps input order; the global `LOOKUP_SLOTS` bounds the fan-out.
    let sem = Semaphore::new(MAX_ALBUM_RESOLVES);
    let resolved = futures::future::join_all(ids.iter().map(|id| {
        let sem = &sem;
        async move {
            let _permit = sem.acquire().await.ok()?;
            resolve_album(id).await.ok()
        }
    }))
    .await;
    resolved.into_iter().flatten().collect()
}

async fn resolve_album(browse_id: &str) -> Result<AcquireAlbum> {
    let url = format!("https://music.youtube.com/browse/{browse_id}");
    let out = run(
        "yt-dlp",
        &[
            "--ignore-config",
            "--no-warnings",
            "--flat-playlist",
            "--dump-single-json",
            "--",
            &url,
        ],
        SEARCH_LIMIT,
    )
    .await?;
    parse_album_json(&out)
}

fn parse_album_json(out: &[u8]) -> Result<AcquireAlbum> {
    let al: YtAlbum =
        serde_json::from_slice(out).map_err(|e| MediaError::Parse(format!("parse album: {e}")))?;
    if al.id.is_empty() {
        return Err(MediaError::Parse("album has no playlist id".to_owned()));
    }
    let mut album = AcquireAlbum {
        playlist_id: al.id,
        title: al
            .title
            .strip_prefix(YT_MUSIC_ALBUM_TITLE_PREFIX)
            .unwrap_or(&al.title)
            .to_owned(),
        track_count: al.playlist_count,
        views: al.view_count,
        ..AcquireAlbum::default()
    };
    for e in al.entries {
        if album.artist.is_empty() {
            album.artist = first_non_empty(&[&e.artist, &e.channel, &e.uploader]);
        }
        if album.thumb.is_empty() && !e.id.is_empty() {
            album.thumb = format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", e.id);
        }
        let d = duration_seconds(e.duration);
        album.duration_sec += d;
        album.tracks.push(Acquire {
            source_id: e.id,
            title: first_non_empty(&[&e.track, &e.title]),
            artist: first_non_empty(&[&e.artist, &e.channel, &e.uploader]),
            duration: d,
            thumb: String::new(),
        });
    }
    if album.track_count == 0 {
        album.track_count = i32::try_from(album.tracks.len()).unwrap_or(i32::MAX);
    }
    Ok(album)
}

/// Stably order albums by how many query words their title contains, most first.
fn rank_albums_by_title(albums: &mut [AcquireAlbum], q: &str) {
    let qw = title_words(q);
    albums.sort_by_key(|a| std::cmp::Reverse(title_overlap(&a.title, &qw)));
}

fn title_overlap(title: &str, query_words: &[String]) -> usize {
    let have: HashSet<String> = title_words(title).into_iter().collect();
    query_words.iter().filter(|w| have.contains(*w)).count()
}

/// Lowercase and split on anything that is not a letter or digit, so
/// "Brasilian Skies" and "brasilian-skies" tokenise the same.
fn title_words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Concatenate id lists, first occurrence wins, order preserved.
fn merge_unique(lists: &[Vec<String>]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for list in lists {
        for s in list {
            if seen.insert(s.clone()) {
                out.push(s.clone());
            }
        }
    }
    out
}

// --------------------------------------------------------------------- shared

/// Run a command with an argument slice and never a shell string.
async fn run(tool: &'static str, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    // Waiting for a slot is not part of the command's own time budget, and a
    // caller that gives up while waiting (a dropped request) never starts the
    // process at all, instead of starting it for nobody.
    let _slot = LOOKUP_SLOTS
        .acquire()
        .await
        .map_err(|_| MediaError::Io(std::io::Error::other("lookup semaphore closed")))?;
    let child = Command::new(tool).args(args).kill_on_drop(true).output();
    match tokio::time::timeout(timeout, child).await {
        Err(_) => Err(MediaError::Tool {
            tool,
            message: format!("{tool} timed out"),
        }),
        Ok(Err(e)) => Err(MediaError::Io(e)),
        Ok(Ok(out)) if out.status.success() => Ok(out.stdout),
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let message = last_line(&stderr);
            let message = if message.is_empty() {
                format!("exited with {}", out.status)
            } else {
                message
            };
            Err(MediaError::Tool { tool, message })
        }
    }
}

fn watch_url(source_id: &str) -> String {
    // source_id is validated, so this cannot become another host or smuggle a
    // query parameter.
    format!("https://www.youtube.com/watch?v={source_id}")
}

/// Reduce free text to one filesystem path component.
///
/// The output is checked to still be a bare basename, so no input turns an
/// artist name into a traversal. '%' is stripped with the shell-hostile set
/// because these strings also sit in yt-dlp `-o` templates, where '%' introduces
/// a field.
#[must_use]
pub fn safe_segment(s: &str) -> String {
    let mut b = String::with_capacity(s.len());
    for c in s.chars() {
        if (c as u32) < 0x20 || c as u32 == 0x7f {
            // control characters, including newline: dropped
        } else if "/\\:*?\"<>|%$`".contains(c) {
            b.push('_');
        } else {
            b.push(c);
        }
    }
    let mut out = b.trim().trim_matches(|c| c == ' ' || c == '.').to_owned();
    if out.chars().count() > SEGMENT_RUNES {
        out = out.chars().take(SEGMENT_RUNES).collect::<String>();
    }
    let out = out.trim().trim_matches(|c| c == ' ' || c == '.');
    if out.is_empty() || out.contains('/') || out.contains('\\') {
        return "unknown".to_owned();
    }
    out.to_owned()
}

/// Locate the file yt-dlp produced, matching by prefix (the container may not be
/// `.opus`) and skipping the thumbnail it writes beside the audio.
fn find_temp(dir: &Path, stem: &str) -> Result<PathBuf> {
    let prefix = format!("{stem}.");
    let entries = std::fs::read_dir(dir)?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(&prefix) {
            continue;
        }
        if entry.metadata().map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        if is_thumb_ext(ext_of(name)) {
            continue;
        }
        return Ok(entry.path());
    }
    Err(MediaError::Tool {
        tool: "yt-dlp",
        message: format!(
            "reported success but produced no audio file in {}",
            dir.display()
        ),
    })
}

/// An existing library file for this title in whatever container, scoped to the
/// artist folder, case-insensitive (the SMB share and two taggers disagree on
/// case). Thumbnails excluded by denylist so an unfamiliar audio container still
/// counts as audio.
fn already_downloaded(dir: &Path, title: &str) -> Option<PathBuf> {
    let want = safe_segment(title);
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if entry.metadata().map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        let ext = ext_of(name);
        if is_thumb_ext(ext) {
            continue;
        }
        let stem = &name[..name.len() - ext.len()];
        if stem.eq_ignore_ascii_case(&want) {
            return Some(entry.path());
        }
    }
    None
}

/// The extension including the leading dot, e.g. `.opus`; empty when none.
fn ext_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i..],
        _ => "",
    }
}

fn is_thumb_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        ".jpg" | ".jpeg" | ".png" | ".webp" | ".gif"
    )
}

fn clean_channel(s: &str) -> String {
    s.trim()
        .strip_suffix("- Topic")
        .unwrap_or_else(|| s.trim())
        .trim()
        .to_owned()
}

fn first_non_empty(vals: &[&str]) -> String {
    for v in vals {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_owned();
        }
    }
    String::new()
}

fn duration_seconds(d: Option<f64>) -> i32 {
    match d {
        // Duration in seconds; fits i32 for any real track. Truncates the fraction.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(v) if v > 0.0 => v as i32,
        _ => 0,
    }
}

fn last_line(s: &str) -> String {
    for line in s.trim().lines().rev() {
        let line = line.trim();
        if !line.is_empty() {
            return trunc(line, 300);
        }
    }
    String::new()
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push_str("...");
    out
}

/// Percent-encode for a URL query component, matching Go's `url.QueryEscape`:
/// unreserved bytes kept, space becomes `+`, everything else `%XX`.
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

fn tmp_stem() -> String {
    format!("{TMP_PREFIX}{:016x}", rand::random::<u64>())
}

fn fresh(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn path_str(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_owned)
        .ok_or_else(|| MediaError::Parse("path is not valid UTF-8".to_owned()))
}

/// Removes anything left under a run's unique stem when it drops. Named after a
/// random stem, so a failed run cannot delete another run's work in progress.
struct TmpSweep {
    dir: PathBuf,
    stem: String,
}

impl TmpSweep {
    fn new(dir: &Path, stem: &str) -> Self {
        Self {
            dir: dir.to_owned(),
            stem: stem.to_owned(),
        }
    }
}

impl Drop for TmpSweep {
    fn drop(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(&self.stem) && !entry.path().is_dir() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::format_push_string)]
    use super::*;

    #[test]
    fn source_id_rejects_unicode_and_bad_length() {
        assert!(valid_source_id("dQw4w9WgXcQ"));
        assert!(valid_source_id("abc-_5"));
        assert!(!valid_source_id("abc")); // too short
        assert!(!valid_source_id("../etc/passwd"));
        assert!(!valid_source_id("dQw4w9WgXc\u{0435}")); // Cyrillic look-alike
        assert!(!valid_source_id("has space"));
    }

    #[test]
    fn playlist_id_accepts_a_release_playlist_and_rejects_a_video_id() {
        // The real id of a release playlist read off a channel's Releases shelf.
        // `valid_source_id` rejects this at 44 characters, and reusing that
        // validator made the whole YouTube source return nothing while still
        // logging healthy - so the length difference is pinned here.
        assert!(valid_playlist_id(
            "OLAK5uy_k8AY2QwXUJVs0q1PVxO96UBVX8gXya-cQ"
        ));
        assert!(!valid_source_id(
            "OLAK5uy_k8AY2QwXUJVs0q1PVxO96UBVX8gXya-cQ"
        ));
        // A plain user playlist is 34 characters and equally valid.
        assert!(valid_playlist_id("PLHXT-5Xp0QicNno4ScdENhBY2abcEFGHIJ"));
        // A video id is not a playlist id, and nothing that could rewrite the URL.
        assert!(!valid_playlist_id("dQw4w9WgXcQ"));
        assert!(!valid_playlist_id("../../etc/passwd"));
        assert!(!valid_playlist_id("https://evil.example/x"));
    }

    #[test]
    fn safe_segment_never_escapes_the_folder() {
        assert_eq!(safe_segment("../../etc"), "_.._etc"); // slashes gone, no traversal
        assert_eq!(safe_segment("A/B:C*"), "A_B_C_");
        assert_eq!(safe_segment("  ...  "), "unknown");
        assert_eq!(safe_segment("100% Real $$$"), "100_ Real ___");
        let long: String = "x".repeat(200);
        assert_eq!(safe_segment(&long).chars().count(), SEGMENT_RUNES);
    }

    #[test]
    fn album_ids_keeps_browse_ids_and_first_song_seed() {
        let dump = "\
{\"id\":\"song1234567\",\"title\":\"a loose track\"}
{\"id\":\"MPREb_first\",\"title\":\"Album - One\"}
{\"id\":\"MPREb_second\",\"title\":\"Album - Two\"}
{\"id\":\"song7654321\",\"title\":\"another loose track\"}";
        let (ids, seed) = album_ids_and_seed(dump.as_bytes(), 12);
        assert_eq!(ids, vec!["MPREb_first", "MPREb_second"]);
        assert_eq!(seed, "song1234567"); // first loose song only
    }

    #[test]
    fn album_ids_respects_the_resolve_cap() {
        let mut dump = String::new();
        for i in 0..20 {
            dump.push_str(&format!(
                "{{\"id\":\"MPREb_{i:03}\",\"title\":\"Album - {i}\"}}\n"
            ));
        }
        let (ids, _) = album_ids_and_seed(dump.as_bytes(), 3);
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn parse_album_derives_artist_thumb_and_duration() {
        let json = r#"{"id":"OLAK5uy_abc","title":"Album - Random Access Memories",
            "playlist_count":2,"view_count":9001,"entries":[
            {"id":"trackaaaaaa","track":"Give Life","artist":"Daft Punk","duration":100.0},
            {"id":"trackbbbbbb","title":"Get Lucky","channel":"Daft Punk","duration":50.5}]}"#;
        let album = parse_album_json(json.as_bytes()).unwrap();
        assert_eq!(album.playlist_id, "OLAK5uy_abc");
        assert_eq!(album.title, "Random Access Memories"); // prefix stripped
        assert_eq!(album.artist, "Daft Punk");
        assert_eq!(
            album.thumb,
            "https://i.ytimg.com/vi/trackaaaaaa/hqdefault.jpg"
        );
        assert_eq!(album.duration_sec, 150);
        assert_eq!(album.track_count, 2);
        assert_eq!(album.tracks.len(), 2);
    }

    #[test]
    fn parse_album_track_count_falls_back_to_length() {
        let json = r#"{"id":"OLAK5uy_x","title":"X","entries":[
            {"id":"trackaaaaaa","title":"One","duration":10.0}]}"#;
        let album = parse_album_json(json.as_bytes()).unwrap();
        assert_eq!(album.track_count, 1);
    }

    #[test]
    fn ranking_puts_the_named_album_first() {
        let mut albums = vec![
            AcquireAlbum {
                title: "Greatest Hits".to_owned(),
                ..Default::default()
            },
            AcquireAlbum {
                title: "Brasilian Skies".to_owned(),
                ..Default::default()
            },
            AcquireAlbum {
                title: "Live in Tokyo".to_owned(),
                ..Default::default()
            },
        ];
        rank_albums_by_title(&mut albums, "brasilian skies");
        assert_eq!(albums[0].title, "Brasilian Skies");
    }

    #[test]
    fn ranking_is_stable_for_an_artist_query() {
        // No album title contains the query words, so YouTube Music's order holds.
        let mut albums = vec![
            AcquireAlbum {
                title: "First".to_owned(),
                ..Default::default()
            },
            AcquireAlbum {
                title: "Second".to_owned(),
                ..Default::default()
            },
        ];
        rank_albums_by_title(&mut albums, "some artist name");
        assert_eq!(albums[0].title, "First");
    }

    #[test]
    fn merge_unique_first_occurrence_wins() {
        let a = vec!["x".to_owned(), "y".to_owned()];
        let b = vec!["y".to_owned(), "z".to_owned()];
        assert_eq!(merge_unique(&[a, b]), vec!["x", "y", "z"]);
    }

    fn acq(title: &str) -> Acquire {
        Acquire {
            source_id: "vid00000000".to_owned(),
            title: title.to_owned(),
            artist: String::new(),
            duration: 0,
            thumb: String::new(),
        }
    }

    #[test]
    fn best_completion_picks_the_verified_superset() {
        // Owned: 2 of the album's 4 tracks. The right album contains both and is
        // larger, so it wins; the decoy shares nothing.
        let owned = vec!["Song A".to_owned(), "Song B (Remastered)".to_owned()];
        let right = AcquireAlbum {
            title: "The Album".to_owned(),
            tracks: vec![acq("Song A"), acq("Song B"), acq("Song C"), acq("Song D")],
            ..Default::default()
        };
        let decoy = AcquireAlbum {
            title: "Other".to_owned(),
            tracks: vec![acq("Nope 1"), acq("Nope 2"), acq("Nope 3")],
            ..Default::default()
        };
        let got = best_completion(vec![decoy, right], &owned).expect("a confident match");
        assert_eq!(got.have, 2);
        let missing: Vec<&str> = got.missing.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(missing, vec!["Song C", "Song D"]);
    }

    #[test]
    fn best_completion_rejects_weak_and_already_complete() {
        let owned = vec!["A".to_owned(), "B".to_owned(), "C".to_owned()];
        // only 1 of 3 owned present -> below the 60% floor
        let weak = AcquireAlbum {
            tracks: vec![acq("A"), acq("X"), acq("Y"), acq("Z")],
            ..Default::default()
        };
        assert!(best_completion(vec![weak], &owned).is_none());
        // no larger than what we own -> nothing to complete
        let same = AcquireAlbum {
            tracks: vec![acq("A"), acq("B"), acq("C")],
            ..Default::default()
        };
        assert!(best_completion(vec![same], &owned).is_none());
    }

    #[test]
    fn norm_title_ignores_case_punctuation_and_parentheticals() {
        assert_eq!(norm_title("Song B (Remastered 2011)"), "song b");
        assert_eq!(norm_title("Déjà Vu!"), "déjà vu");
        assert_eq!(norm_title("A - B [Live]"), "a b");
    }

    #[test]
    fn is_unavailable_flags_ungettable_not_transient() {
        assert!(is_unavailable(
            "yt-dlp failed: ERROR: [youtube] x: This video is only available to Music Premium members"
        ));
        assert!(is_unavailable(
            "ERROR: Private video. Sign in if you've been granted access"
        ));
        assert!(is_unavailable("ERROR: Video unavailable"));
        assert!(!is_unavailable(
            "ERROR: unable to download webpage: The read operation timed out"
        ));
    }

    #[test]
    fn parse_source_meta_prefers_upload_date() {
        let json = r#"{"upload_date":"20250815","view_count":42,"channel":"Daft Punk - Topic"}"#;
        let m = parse_source_meta(json.as_bytes()).unwrap();
        assert_eq!(m.posted_at, "2025-08-15");
        assert_eq!(m.views, 42);
        assert_eq!(m.channel, "Daft Punk"); // "- Topic" stripped
    }

    #[test]
    fn parse_source_meta_falls_back_to_timestamp() {
        let json = r#"{"timestamp":1600000000,"uploader":"Someone"}"#;
        let m = parse_source_meta(json.as_bytes()).unwrap();
        assert_eq!(m.posted_at, "2020-09-13");
    }

    #[test]
    fn parse_search_skips_invalid_ids_and_empty_titles() {
        let dump = "\
{\"id\":\"validid1234\",\"title\":\"Real Song\",\"uploader\":\"Band - Topic\",\"duration\":200.0}
{\"id\":\"../bad\",\"title\":\"Injection\"}
{\"id\":\"notitle5678\",\"title\":\"\"}";
        let results = parse_search_entries(dump.as_bytes(), 8);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_id, "validid1234");
        assert_eq!(results[0].artist, "Band"); // "- Topic" cleaned
        assert_eq!(results[0].duration, 200);
    }

    /// However large the requested count, one search yields at most
    /// MAX_SEARCH_RESULTS results (the caller's n is user input).
    #[test]
    fn a_search_never_returns_more_than_the_cap() {
        let dump = (0..40)
            .map(|i| format!("{{\"id\":\"validid{i:04}\",\"title\":\"Song {i}\"}}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            parse_search_entries(dump.as_bytes(), usize::MAX).len(),
            MAX_SEARCH_RESULTS
        );
        assert_eq!(parse_search_entries(dump.as_bytes(), 3).len(), 3);
    }

    #[test]
    fn already_downloaded_matches_any_container_case_insensitively() {
        let dir = std::env::temp_dir().join(format!("mh-dedup-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Lemon Tree.mp3"), b"x").unwrap();
        std::fs::write(dir.join("Lemon Tree.jpg"), b"x").unwrap(); // thumbnail
        assert!(already_downloaded(&dir, "lemon tree").is_some());
        assert!(already_downloaded(&dir, "Other Song").is_none());
        // A thumbnail alone must not count as a downloaded track.
        std::fs::remove_file(dir.join("Lemon Tree.mp3")).unwrap();
        std::fs::write(dir.join("Cover.png"), b"x").unwrap();
        assert!(already_downloaded(&dir, "Cover").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn fetch_reuses_an_existing_file_without_downloading() {
        let root = std::env::temp_dir().join(format!("mh-reuse-{:016x}", rand::random::<u64>()));
        let dir = root.join("u/downloads").join("Art");
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("Lemon Tree.opus");
        std::fs::write(&existing, b"audio").unwrap();
        // A valid source id, but fetch must never reach yt-dlp because the track
        // is already on disk - so this passes with no network.
        let got = fetch(
            root.to_str().unwrap(),
            "u/downloads",
            "dQw4w9WgXcQ",
            "Lemon Tree",
            "Art",
            "Greatest Hits",
            "Art",
        )
        .await
        .unwrap();
        assert_eq!(
            got, existing,
            "returns the existing file instead of downloading"
        );
        assert!(existing.exists(), "the existing file is kept in place");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn find_temp_skips_the_thumbnail() {
        let dir = std::env::temp_dir().join(format!("mh-findtmp-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let stem = ".mh-abc";
        std::fs::write(dir.join(format!("{stem}.jpg")), b"x").unwrap();
        std::fs::write(dir.join(format!("{stem}.opus")), b"x").unwrap();
        let got = find_temp(&dir, stem).unwrap();
        assert_eq!(got.extension().unwrap(), "opus");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn query_escape_matches_go_semantics() {
        assert_eq!(query_escape("brasilian skies"), "brasilian+skies");
        assert_eq!(query_escape("a&b=c"), "a%26b%3Dc");
        assert_eq!(query_escape("keep-_.~"), "keep-_.~");
    }

    /// Every cover tile and every typed pause starts a lookup; unbounded, 30 at
    /// once held ~1 GB against a 768 MiB pod and the kernel killed it 8 times.
    #[tokio::test]
    async fn a_lookup_beyond_the_cap_waits_and_a_dropped_one_never_starts() {
        let marker = std::env::temp_dir().join(format!("mh-lookup-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let touch = format!("touch {}", marker.display());

        let held = LOOKUP_SLOTS
            .acquire_many(u32::try_from(MAX_LOOKUPS).unwrap())
            .await
            .unwrap();
        let waiting = tokio::time::timeout(
            Duration::from_millis(200),
            run("sh", &["-c", &touch], Duration::from_secs(5)),
        )
        .await;
        assert!(waiting.is_err(), "ran with every slot taken");

        drop(held);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !marker.exists(),
            "a caller that gave up while queued still started its process"
        );

        run("sh", &["-c", &touch], Duration::from_secs(5))
            .await
            .unwrap();
        assert!(marker.exists(), "a lookup with a free slot did not run");
        let _ = std::fs::remove_file(&marker);
    }
}
