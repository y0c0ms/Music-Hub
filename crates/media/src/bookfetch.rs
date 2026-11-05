//! Finds and downloads audiobooks from three unlike sources.
//!
//! The three places worth getting audiobooks from behave nothing alike, so each
//! is a [`Source`] implementation dispatched by name:
//!
//! - `youtube` — one long file plus chapter marks, fetched with yt-dlp.
//! - `librivox` — already split, one MP3 per chapter, plain HTTP from archive.org.
//! - `media.ccc.de` — conference talks with a published audio-only MP3.
//!
//! Each produces the same thing: audio in a folder plus a [`Sidecar`] describing
//! its chapters. Unlike the Go original, [`fetch`] only writes audio into `dir`
//! and returns the sidecar; the server persists `book.json`, stages, and
//! rescans. State (jobs, the fetch queue, the download slot) also lives in the
//! server; this module is the stateless search/download engine plus its pure,
//! testable helpers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::join_all;
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::acquire::valid_source_id;
use crate::{ytdlp, MediaError};

type Result<T, E = MediaError> = std::result::Result<T, E>;

/// The filename the server writes a book's [`Sidecar`] to.
pub const SIDECAR_NAME: &str = "book.json";

/// The User-Agent every HTTP request to a book source carries.
const USER_AGENT: &str = "music-hub/1.0 (+https://github.com/y0c0ms/Music-Hub; audiobook fetcher)";

/// A search result of an audiobook search that returns nothing gets 10 hits.
const DEFAULT_HITS: usize = 10;
/// Below this a YouTube hit is a trailer or a clip, not a book; talks stay in.
const MIN_BOOK_SEC: i32 = 20 * 60;
/// Below this a media.ccc.de talk is a lightning talk, not long-form audio.
const MIN_TALK_SEC: i32 = 10 * 60;

const SEARCH_LIMIT: Duration = Duration::from_secs(45);
const META_LIMIT: Duration = Duration::from_secs(45);
/// A fourteen-hour YouTube file is a long transcode; the bound only releases a
/// wedged download, it is not meant to be tight.
const FETCH_LIMIT: Duration = Duration::from_secs(3 * 60 * 60);
const JSON_LIMIT: Duration = Duration::from_secs(60);
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(30 * 60);
/// Cap on a source's JSON body, so a hostile or broken feed cannot exhaust the
/// pod's memory. Matches the Go `io.LimitReader(_, 8<<20)`.
const MAX_JSON: usize = 8 << 20;

const LIBRIVOX_API: &str = "https://librivox.org/api/feed/audiobooks/";
const MEDIA_CCC_API: &str = "https://api.media.ccc.de/public";

const HEX: &[u8; 16] = b"0123456789ABCDEF";

// ------------------------------------------------------------------- contract

/// Something findable that is not in the library yet.
///
/// Serialises byte-for-byte as the Go `bookfetch.Result`: `durationSec`,
/// `chapters`, and `views` are dropped when zero (the source gave no signal),
/// and the free-text fields when empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchResult {
    /// `youtube` | `librivox` | `media.ccc.de`.
    pub source: String,
    /// Opaque to the caller; meaningful to its source.
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// Zero means the source did not say.
    #[serde(skip_serializing_if = "is_zero_i32")]
    pub duration_sec: i32,
    /// A COUNT, not a list; the search feeds omit the chapter list.
    #[serde(skip_serializing_if = "is_zero_i32")]
    pub chapters: i32,
    /// Ranks near-identical titles; only YouTube reports it, 0 elsewhere.
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub views: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub thumb: String,
    /// Conference, narrator, etc.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// One entry in a book's contents, for the detail sheet.
///
/// A title and a length, derived from metadata with nothing downloaded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Chapter {
    pub title: String,
    pub start_sec: i32,
    pub end_sec: i32,
}

/// One chapter as written into a book's on-disk [`Sidecar`].
///
/// `file` is the audio file the chapter lives in; for a single-file book every
/// chapter names the same file. Serialises identically to the Go
/// `books.SidecarChapter`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SidecarChapter {
    pub title: String,
    pub file: String,
    pub start_sec: i32,
    pub end_sec: i32,
}

/// The on-disk description of a book, returned by [`fetch`].
///
/// Chapter marks for a single-file book exist nowhere else. Serialises
/// byte-for-byte as the Go `books.Sidecar`, because the cutover reads
/// Go-written `book.json` files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Sidecar {
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// `youtube` | `librivox` | `media.ccc.de`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Chapters are relative to the book folder.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<SidecarChapter>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip_serializing_if requires &T
const fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip_serializing_if requires &T
const fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

// --------------------------------------------------------------- public entry

/// The searchable sources, sorted, for the app's source picker.
#[must_use]
pub fn source_names() -> Vec<String> {
    let mut names = vec![
        LibriVox::NAME.to_owned(),
        MediaCcc::NAME.to_owned(),
        YouTube::NAME.to_owned(),
    ];
    names.sort();
    names
}

/// Ask every source at once (or the one named by `only`) and merge the hits.
///
/// Concurrent because the calls are independent and the slowest would otherwise
/// set the latency for all three. A source that fails is logged and skipped, so
/// LibriVox being down does not stop a media.ccc.de talk being found. Results
/// come back ranked best-first (see [`rank_results`]).
#[must_use]
pub async fn search(q: &str, per: usize, only: Option<&str>) -> Vec<SearchResult> {
    let q = q.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let sources = all_sources();
    let jobs = sources
        .iter()
        .filter(|s| only.is_none_or(|name| name == s.name()))
        .map(|s| async move {
            match s.search(q, per).await {
                Ok(hits) => hits,
                Err(err) => {
                    tracing::warn!(source = s.name(), %err, "bookfetch source search failed");
                    Vec::new()
                }
            }
        });
    let mut merged: Vec<SearchResult> = join_all(jobs).await.into_iter().flatten().collect();
    rank_results(q, &mut merged);
    merged
}

/// Return one book's contents from metadata alone, dispatching to its source.
///
/// Downloads nothing, so it is safe to call the moment a result is tapped.
///
/// # Errors
/// Fails when the source is unknown, the id is empty, or the source lookup does.
pub async fn chapters(r: &SearchResult) -> Result<Vec<Chapter>> {
    let source = source_for(&r.source)
        .ok_or_else(|| MediaError::Parse(format!("unknown source {:?}", r.source)))?;
    if r.id.trim().is_empty() {
        return Err(MediaError::Parse("id is required".to_owned()));
    }
    source.chapters(r).await
}

/// Download a book into `dir` and return the sidecar describing what was written.
///
/// `dir` must exist and be empty. The returned sidecar's chapter list is
/// non-empty on success. This does NOT write `book.json`, stage, rename, or
/// rescan — the server does that.
///
/// # Errors
/// Fails when the source is unknown or the source's download does.
pub async fn fetch(r: &SearchResult, dir: &Path) -> Result<Sidecar> {
    let source = source_for(&r.source)
        .ok_or_else(|| MediaError::Parse(format!("unknown source {:?}", r.source)))?;
    source.fetch(r, dir).await
}

// -------------------------------------------------------------------- sources

/// One place books can come from.
#[async_trait]
trait Source: Send + Sync {
    fn name(&self) -> &'static str;
    async fn search(&self, q: &str, per: usize) -> Result<Vec<SearchResult>>;
    async fn fetch(&self, r: &SearchResult, dir: &Path) -> Result<Sidecar>;
    async fn chapters(&self, r: &SearchResult) -> Result<Vec<Chapter>>;
}

fn all_sources() -> Vec<Box<dyn Source>> {
    let hc = http_client();
    vec![
        Box::new(YouTube),
        Box::new(LibriVox { hc: hc.clone() }),
        Box::new(MediaCcc { hc }),
    ]
}

fn source_for(name: &str) -> Option<Box<dyn Source>> {
    match name {
        YouTube::NAME => Some(Box::new(YouTube)),
        LibriVox::NAME => Some(Box::new(LibriVox { hc: http_client() })),
        MediaCcc::NAME => Some(Box::new(MediaCcc { hc: http_client() })),
        _ => None,
    }
}

// -------------------------------------------------------------------- youtube

/// Fetches an audiobook as ONE file plus chapter marks.
///
/// A YouTube audiobook is one upload with chapter markers, so splitting it would
/// re-encode a fourteen-hour file to reproduce what the source already
/// describes. Keeping one file makes the saved position a plain offset into an
/// immutable file. The marks come from yt-dlp's `chapters` array; an upload with
/// none becomes a single chapter over the whole file.
struct YouTube;

impl YouTube {
    const NAME: &'static str = "youtube";
    const AUDIO_NAME: &'static str = "audiobook.opus";
}

/// One chapter mark from yt-dlp. Partial on purpose: only decoded fields.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct YtChapter {
    start_time: f64,
    end_time: f64,
    title: String,
}

/// A YouTube info blob from `--dump-json` / `-J`. Partial on purpose.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct YtInfo {
    id: String,
    title: String,
    uploader: String,
    channel: String,
    duration: f64,
    thumbnail: String,
    view_count: i64,
    chapters: Vec<YtChapter>,
}

#[async_trait]
impl Source for YouTube {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn search(&self, q: &str, per: usize) -> Result<Vec<SearchResult>> {
        let n = hits(per);
        // Over-fetch: the duration filter drops trailers and clips, so asking for
        // exactly n returns far fewer than n real books. Ask for a multiple.
        let want = n * 2 + 8;
        let owned = yt_search_argv(want, q);
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let out = run("yt-dlp", &args, SEARCH_LIMIT).await?;
        Ok(parse_youtube_search(&out, n))
    }

    async fn fetch(&self, r: &SearchResult, dir: &Path) -> Result<Sidecar> {
        let info = self.metadata(r).await?;
        if info.duration <= 0.0 {
            return Err(MediaError::Parse(
                "source reports no duration; refusing to guess chapter bounds".to_owned(),
            ));
        }
        // Download after the metadata succeeds, so a book with no usable metadata
        // fails in seconds rather than after an hour of transcoding.
        let url = watch_url(&r.id);
        let out_tmpl = path_str(&dir.join("audiobook.%(ext)s"))?;
        let dl_args = ytdlp::media(&[
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
            &out_tmpl,
            "--",
            &url,
        ]);
        run("yt-dlp", &dl_args, FETCH_LIMIT).await?;
        let audio = dir.join(Self::AUDIO_NAME);
        if tokio::fs::metadata(&audio).await.is_err() {
            return Err(MediaError::Tool {
                tool: "yt-dlp",
                message: format!("reported success but {} is not there", Self::AUDIO_NAME),
            });
        }
        let title = first_non_empty(&[&info.title, &r.title]);
        Ok(Sidecar {
            author: first_non_empty(&[&info.uploader, &info.channel, &r.author]),
            source: Self::NAME.to_owned(),
            chapters: chapters_from(
                &info.chapters,
                duration_seconds(Some(info.duration)),
                Self::AUDIO_NAME,
                &title,
            ),
            title,
        })
    }

    async fn chapters(&self, r: &SearchResult) -> Result<Vec<Chapter>> {
        let info = self.metadata(r).await?;
        let title = first_non_empty(&[&info.title, &r.title]);
        Ok(from_sidecar(&chapters_from(
            &info.chapters,
            duration_seconds(Some(info.duration)),
            Self::AUDIO_NAME,
            &title,
        )))
    }
}

impl YouTube {
    /// Read a video's metadata with the pinned player client, without downloading.
    async fn metadata(&self, r: &SearchResult) -> Result<YtInfo> {
        if !valid_source_id(&r.id) {
            return Err(MediaError::Parse(format!("invalid youtube id {:?}", r.id)));
        }
        let url = watch_url(&r.id);
        let args = yt_meta_argv(&url);
        let raw = run("yt-dlp", &args, META_LIMIT).await?;
        serde_json::from_slice(&raw).map_err(|e| MediaError::Parse(format!("parse metadata: {e}")))
    }
}

/// The unpinned search argv: `--flat-playlist` never touches a media URL, so it
/// must NOT carry the player-client pin (see [`ytdlp`]).
fn yt_search_argv(want: usize, q: &str) -> Vec<String> {
    vec![
        "--ignore-config".to_owned(),
        "--no-warnings".to_owned(),
        "--flat-playlist".to_owned(),
        "--dump-json".to_owned(),
        format!("ytsearch{want}:{q} audiobook"),
    ]
}

/// The pinned metadata argv: a media resolve, so it leads with the player-client
/// pin via [`ytdlp::media`].
fn yt_meta_argv(url: &str) -> Vec<&str> {
    ytdlp::media(&[
        "--ignore-config",
        "--no-warnings",
        "--no-playlist",
        "-J",
        "--",
        url,
    ])
}

/// Turn a batch of `--dump-json` lines into results, filtered and capped.
///
/// Pure so the awkward parts — duration filtering, the id-derived thumbnail
/// fallback, the cap — are testable without yt-dlp.
fn parse_youtube_search(out: &[u8], n: usize) -> Vec<SearchResult> {
    let text = String::from_utf8_lossy(out);
    let mut kept = Vec::new();
    for line in text.trim().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(e) = serde_json::from_str::<YtInfo>(line) else {
            continue;
        };
        if e.id.is_empty() {
            continue;
        }
        let duration_sec = duration_seconds(Some(e.duration));
        // A three-minute result from an audiobook search is a trailer or clip.
        // Twenty minutes is under any real book and over any excerpt, and keeps
        // legitimate 30-120 min talks in.
        if duration_sec != 0 && duration_sec < MIN_BOOK_SEC {
            continue;
        }
        // --flat-playlist returns an empty thumbnail; the id-derived URL is stable
        // and needs no per-result extraction.
        let derived = format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", e.id);
        let thumb = first_non_empty(&[&e.thumbnail, &derived]);
        kept.push(SearchResult {
            source: YouTube::NAME.to_owned(),
            id: e.id,
            title: e.title.trim().to_owned(),
            author: first_non_empty(&[&e.uploader, &e.channel]),
            duration_sec,
            chapters: 0,
            views: e.view_count,
            thumb,
            detail: String::new(),
        });
        if kept.len() == n {
            break;
        }
    }
    kept
}

/// Convert YouTube's chapter list into sidecar chapters.
///
/// Pure so the awkward cases can be tested without a network: no chapters at
/// all, marks that overlap, marks that run past the end, and a final chapter
/// with a zero end time. Each would otherwise put a resume position in the wrong
/// place, invisibly.
#[allow(clippy::cast_possible_truncation)] // seconds truncate toward zero, as Go's int(f)
fn chapters_from(
    marks: &[YtChapter],
    duration_sec: i32,
    file: &str,
    book_title: &str,
) -> Vec<SidecarChapter> {
    if duration_sec <= 0 {
        return Vec::new();
    }
    let whole = || {
        vec![SidecarChapter {
            title: book_title.to_owned(),
            file: file.to_owned(),
            start_sec: 0,
            end_sec: duration_sec,
        }]
    };
    if marks.is_empty() {
        // No marks published. One chapter spanning the file is honest; inventing
        // divisions would put breaks where the book has none.
        return whole();
    }

    let mut sorted = marks.to_vec();
    sorted.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));

    let mut out: Vec<SidecarChapter> = Vec::new();
    for (i, c) in sorted.iter().enumerate() {
        let start = (c.start_time as i32).max(0);
        if start >= duration_sec {
            continue; // a mark past the end of the file describes nothing
        }
        // Trust the NEXT chapter's start over this one's end: YouTube end times
        // are frequently absent or overlapping, and a gap would leave positions
        // belonging to no chapter, which reads as the app forgetting the spot.
        let mut end = duration_sec;
        if let Some(next) = sorted.get(i + 1) {
            let next = next.start_time as i32;
            if next > start {
                end = next;
            }
        } else {
            let e = c.end_time as i32;
            if e > start && e < duration_sec {
                end = e;
            }
        }
        // The next mark can itself be past the end (uploads publish marks for
        // trimmed material), so clamp to the file.
        if end > duration_sec {
            end = duration_sec;
        }
        if end <= start {
            continue;
        }
        let title = c.title.trim();
        let title = if title.is_empty() {
            format!("Chapter {}", out.len() + 1)
        } else {
            title.to_owned()
        };
        out.push(SidecarChapter {
            title,
            file: file.to_owned(),
            start_sec: start,
            end_sec: end,
        });
    }
    if out.is_empty() {
        return whole();
    }
    // The first mark is often not at zero. Without this the opening minutes
    // belong to no chapter and starting the book resolves to nothing.
    if out[0].start_sec > 0 {
        out[0].start_sec = 0;
    }
    out
}

/// Reduce download-time chapters to the preview shape, so YouTube's two paths
/// cannot disagree about where a chapter starts.
fn from_sidecar(sc: &[SidecarChapter]) -> Vec<Chapter> {
    sc.iter()
        .map(|c| Chapter {
            title: c.title.clone(),
            start_sec: c.start_sec,
            end_sec: c.end_sec,
        })
        .collect()
}

fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

// ------------------------------------------------------------------- librivox

/// Fetches public-domain audiobooks, already split into chapters.
///
/// Free and legal: every recording is public domain, served from archive.org
/// with no key. The limit is the same thing that makes it free — public domain
/// only, so classics and almost nothing technical. Each chapter is its own MP3
/// starting at zero, so a chapter's identity is its track, not an offset.
struct LibriVox {
    hc: reqwest::Client,
}

impl LibriVox {
    const NAME: &'static str = "librivox";
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct LvAuthor {
    first_name: String,
    last_name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct LvSection {
    section_number: String,
    title: String,
    playtime: String, // seconds, as a string
    listen_url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct LvBook {
    id: String,
    title: String,
    num_sections: String,
    totaltime: String, // "8:16:48"
    authors: Vec<LvAuthor>,
    sections: Vec<LvSection>,
    // Present in the BASIC feed (sections are not) and embeds the archive.org
    // item id, the only route to a cover without a heavier extended=1 request.
    url_zip_file: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LvResponse {
    books: Vec<LvBook>,
}

#[async_trait]
impl Source for LibriVox {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn search(&self, q: &str, per: usize) -> Result<Vec<SearchResult>> {
        let n = hits(per);
        // The leading "^" is required, not politeness: without it the title
        // filter is an EXACT match, so "frankenstein" returns nothing while
        // "^frankenstein" returns three.
        let needle = query_escape(&format!("^{q}"));
        let title_url = format!("{LIBRIVOX_API}?title={needle}&format=json&limit={n}");
        let mut resp: LvResponse = get_json(&self.hc, &title_url).await?;
        // LibriVox 404s a no-match search rather than returning an empty list.
        // Retrying by author covers someone typing a writer's name.
        if resp.books.is_empty() {
            let author_url = format!("{LIBRIVOX_API}?author={needle}&format=json&limit={n}");
            match get_json::<LvResponse>(&self.hc, &author_url).await {
                Ok(r) => resp = r,
                Err(_) => return Ok(Vec::new()), // genuinely nothing
            }
        }
        Ok(lv_results(resp))
    }

    async fn fetch(&self, r: &SearchResult, dir: &Path) -> Result<Sidecar> {
        let resp = self.extended(&r.id).await?;
        let (sections, book) = sorted_sections(resp)?;
        let mut side = Sidecar {
            title: book.title.trim().to_owned(),
            author: author_name(&book.authors),
            source: Self::NAME.to_owned(),
            chapters: Vec::with_capacity(sections.len()),
        };
        for (i, s) in sections.iter().enumerate() {
            // Zero-padded so on-disk order matches chapter order for anything
            // reading the folder directly, ffprobe included.
            let name = format!("{:03}.mp3", i + 1);
            download(&self.hc, &s.listen_url, &dir.join(&name))
                .await
                .map_err(|e| MediaError::Tool {
                    tool: "http",
                    message: format!("chapter {} of {}: {e}", i + 1, sections.len()),
                })?;
            let title = section_title(&s.title, i);
            side.chapters.push(SidecarChapter {
                title,
                file: name,
                start_sec: 0, // each chapter is its own file
                end_sec: atoi_safe(&s.playtime),
            });
        }
        Ok(side)
    }

    async fn chapters(&self, r: &SearchResult) -> Result<Vec<Chapter>> {
        let resp = self.extended(&r.id).await?;
        let (sections, _) = sorted_sections(resp)?;
        Ok(sections
            .iter()
            .enumerate()
            .map(|(i, s)| Chapter {
                title: section_title(&s.title, i),
                start_sec: 0,
                // Each section is its own file starting at zero, so its length is
                // simply its playtime.
                end_sec: atoi_safe(&s.playtime),
            })
            .collect())
    }
}

impl LibriVox {
    /// Fetch a book with its per-section file list (`extended=1`).
    async fn extended(&self, id: &str) -> Result<LvResponse> {
        let id = query_escape(id);
        // extended=1 is the only way to get the per-section file list.
        let url = format!("{LIBRIVOX_API}?id={id}&format=json&extended=1");
        get_json(&self.hc, &url).await
    }
}

fn lv_results(resp: LvResponse) -> Vec<SearchResult> {
    resp.books
        .into_iter()
        .map(|b| SearchResult {
            source: LibriVox::NAME.to_owned(),
            title: b.title.trim().to_owned(),
            author: author_name(&b.authors),
            duration_sec: hms_to_seconds(&b.totaltime),
            chapters: atoi_safe(&b.num_sections),
            views: 0,
            thumb: archive_cover(&b.url_zip_file),
            detail: "LibriVox · public domain".to_owned(),
            id: b.id,
        })
        .collect()
}

/// Take the downloadable sections in chapter order, plus the book they belong to.
///
/// Shared by fetch and chapters so the two cannot disagree about what a chapter
/// is or its order. Pure over an already-fetched response.
fn sorted_sections(mut resp: LvResponse) -> Result<(Vec<LvSection>, LvBook)> {
    if resp.books.is_empty() {
        return Err(MediaError::Parse(
            "librivox returned no such book".to_owned(),
        ));
    }
    let book = resp.books.remove(0);
    let mut sections: Vec<LvSection> = book
        .sections
        .iter()
        .filter(|s| !s.listen_url.trim().is_empty())
        .cloned()
        .collect();
    if sections.is_empty() {
        return Err(MediaError::Parse(format!(
            "no downloadable sections for {:?}",
            book.title
        )));
    }
    // By section number, not the order the API returned. Chapter order IS the book.
    sections.sort_by(|a, b| atoi_safe(&a.section_number).cmp(&atoi_safe(&b.section_number)));
    Ok((sections, book))
}

/// Derive an archive.org cover URL from a book's zip-file link.
///
/// `url_zip_file` embeds the archive.org item id, and `/services/img/<id>` is a
/// stable thumbnail for it — no extra request. Returns "" when unreadable.
fn archive_cover(zip_url: &str) -> String {
    for marker in ["/compress/", "/download/"] {
        if let Some(i) = zip_url.find(marker) {
            let rest = &zip_url[i + marker.len()..];
            if let Some(j) = rest.find('/') {
                if j > 0 {
                    return format!("https://archive.org/services/img/{}", &rest[..j]);
                }
            }
        }
    }
    String::new()
}

fn author_name(authors: &[LvAuthor]) -> String {
    authors.first().map_or_else(String::new, |a| {
        format!("{} {}", a.first_name.trim(), a.last_name.trim())
            .trim()
            .to_owned()
    })
}

fn section_title(title: &str, i: usize) -> String {
    let title = title.trim();
    if title.is_empty() {
        format!("Section {}", i + 1)
    } else {
        title.to_owned()
    }
}

/// Parse LibriVox's "8:16:48" and its shorter "16:48".
fn hms_to_seconds(s: &str) -> i32 {
    s.trim()
        .split(':')
        .fold(0, |total, p| total * 60 + atoi_safe(p))
}

fn atoi_safe(s: &str) -> i32 {
    s.trim().parse::<i32>().unwrap_or(0)
}

// ---------------------------------------------------------------- media.ccc.de

/// Fetches conference talks as audio.
///
/// Technical audiobooks are almost all paid, so the free supply for networking
/// and programming turns out to be conference recordings. media.ccc.de indexes
/// hundreds of conferences, publishes an AUDIO-ONLY MP3 per talk, and has a
/// public API with no auth. A talk has no chapter marks, so it lands as a
/// single-chapter book — the same shape as a markless YouTube upload.
struct MediaCcc {
    hc: reqwest::Client,
}

impl MediaCcc {
    const NAME: &'static str = "media.ccc.de";
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct CccRecording {
    mime_type: String,
    folder: String,
    language: String,
    recording_url: String,
    size: i32, // MB
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct CccEvent {
    guid: String,
    title: String,
    subtitle: String,
    persons: Vec<String>,
    conference_title: String,
    length: i32, // seconds
    thumb_url: String,
    recordings: Vec<CccRecording>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CccSearch {
    events: Vec<CccEvent>,
}

#[async_trait]
impl Source for MediaCcc {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn search(&self, q: &str, per: usize) -> Result<Vec<SearchResult>> {
        let n = hits(per);
        let url = format!("{MEDIA_CCC_API}/events/search?q={}", query_escape(q));
        let resp: CccSearch = get_json(&self.hc, &url).await?;
        Ok(ccc_results(resp, n))
    }

    async fn fetch(&self, r: &SearchResult, dir: &Path) -> Result<Sidecar> {
        let e: CccEvent = get_json(&self.hc, &ccc_event_url(&r.id)).await?;
        let rec = pick_audio(&e.recordings).ok_or_else(|| {
            // A few very old talks are video only. Saying so beats downloading a
            // video into a music library.
            MediaError::Parse(format!("{:?} has no audio-only recording", e.title))
        })?;
        let name = format!("talk{}", ext_for(&rec.mime_type));
        download(&self.hc, &rec.recording_url, &dir.join(&name)).await?;

        let length = first_positive(&[e.length, r.duration_sec]);
        if length <= 0 {
            return Err(MediaError::Parse(format!(
                "no duration for {:?}; cannot describe a chapter",
                e.title
            )));
        }
        let title = first_non_empty(&[&e.title, &r.title]);
        Ok(Sidecar {
            author: first_non_empty(&[&e.persons.join(", "), &r.author]),
            source: Self::NAME.to_owned(),
            // One chapter: a talk has no marks, and inventing them would put
            // breaks where the speaker did not pause.
            chapters: vec![SidecarChapter {
                title: title.clone(),
                file: name,
                start_sec: 0,
                end_sec: length,
            }],
            title,
        })
    }

    async fn chapters(&self, r: &SearchResult) -> Result<Vec<Chapter>> {
        // Fetch the event for an authoritative length and title, falling back to
        // the search result so the sheet still shows something if it fails.
        let mut title = r.title.trim().to_owned();
        let mut length = r.duration_sec;
        if let Ok(e) = get_json::<CccEvent>(&self.hc, &ccc_event_url(&r.id)).await {
            title = first_non_empty(&[&e.title, &title]);
            if e.length > 0 {
                length = e.length;
            }
        }
        if title.is_empty() {
            "Full talk".clone_into(&mut title);
        }
        Ok(vec![Chapter {
            title,
            start_sec: 0,
            end_sec: length,
        }])
    }
}

fn ccc_event_url(id: &str) -> String {
    format!("{MEDIA_CCC_API}/events/{}", path_escape(id))
}

fn ccc_results(resp: CccSearch, n: usize) -> Vec<SearchResult> {
    let mut out = Vec::with_capacity(n.min(resp.events.len()));
    for e in resp.events {
        if out.len() >= n {
            break;
        }
        // Under ten minutes is a lightning talk: not useless, but not long-form
        // audio, and it crowds out the hour-long talks that are.
        if e.length > 0 && e.length < MIN_TALK_SEC {
            continue;
        }
        out.push(SearchResult {
            source: MediaCcc::NAME.to_owned(),
            id: e.guid,
            title: e.title.trim().to_owned(),
            author: e.persons.join(", "),
            duration_sec: e.length,
            chapters: 0,
            views: 0,
            thumb: e.thumb_url,
            detail: e.conference_title.trim().to_owned(),
        });
    }
    out
}

/// Find the audio-only recording, preferring MP3 and the original-language track.
///
/// Pure so it can be tested against the real response shape: the list mixes
/// video and audio, and picking wrong means a 2 GB video lands in the library.
fn pick_audio(recs: &[CccRecording]) -> Option<&CccRecording> {
    let mut mp3: Option<&CccRecording> = None;
    let mut other: Option<&CccRecording> = None;
    for r in recs {
        if !r.mime_type.starts_with("audio/") || r.recording_url.is_empty() {
            continue;
        }
        if r.mime_type.contains("mpeg") {
            if mp3.is_none() || (r.language == "eng" && mp3.is_some_and(|m| m.language != "eng")) {
                mp3 = Some(r);
            }
        } else if other.is_none() {
            other = Some(r);
        }
    }
    mp3.or(other)
}

fn ext_for(mime: &str) -> &'static str {
    if mime.contains("mpeg") {
        ".mp3"
    } else if mime.contains("opus") {
        ".opus"
    } else if mime.contains("ogg") {
        ".ogg"
    } else if mime.contains("mp4") || mime.contains("m4a") {
        ".m4a"
    } else {
        ".mp3"
    }
}

// -------------------------------------------------------------------- ranking

/// Order merged hits best-first, in place.
///
/// The model, in priority order: title relevance (dominant, so a popular but
/// unrelated video cannot bury the asked-for book), then log-scaled popularity,
/// then chapter count, then duration as the final tiebreaker. Replaced a plain
/// longest-first sort that put a 17-hour sleep reading above the 7.6M-view full
/// audiobook of the same book.
fn rank_results(q: &str, results: &mut [SearchResult]) {
    results.sort_by(|a, b| {
        score(q, b)
            .total_cmp(&score(q, a))
            .then_with(|| b.duration_sec.cmp(&a.duration_sec))
    });
}

fn is_stop_word(w: &str) -> bool {
    matches!(
        w,
        "the" | "a" | "an" | "and" | "of" | "by" | "audiobook" | "audio" | "book" | "full"
    )
}

#[allow(clippy::cast_precision_loss)] // view counts are far below f64's exact-integer range
fn score(q: &str, r: &SearchResult) -> f64 {
    let mut s = title_relevance(q, &r.title) * 3.0;
    if r.views > 0 {
        s += (r.views as f64 + 1.0).log10() / 10.0; // 100M -> ~0.8, 1k -> ~0.3
    }
    s += f64::from(r.chapters.min(40)) / 40.0 * 0.5;
    if r.duration_sec >= MIN_BOOK_SEC {
        s += 0.15; // a real book, not an excerpt
    }
    s
}

/// The fraction of the query's meaningful words present in the title, with a
/// bonus when the whole query appears as a phrase.
///
/// Stopwords are dropped so "Pride and Prejudice" is not penalised against a
/// title omitting "and", and "audiobook" does not count as a match against
/// itself.
#[allow(clippy::cast_precision_loss)] // word counts are tiny, exact in f64
fn title_relevance(q: &str, title: &str) -> f64 {
    let t = title.to_lowercase();
    let ql = q.to_lowercase();
    let toks: Vec<&str> = ql.split_whitespace().filter(|w| !is_stop_word(w)).collect();
    if toks.is_empty() {
        return 0.0;
    }
    let hit = toks.iter().filter(|tok| t.contains(**tok)).count();
    let mut rel = hit as f64 / toks.len() as f64;
    let q2 = ql.trim();
    if !q2.is_empty() && t.contains(q2) {
        rel = f64::min(1.0, rel + 0.25);
    }
    rel
}

// --------------------------------------------------------------------- shared

const fn hits(per: usize) -> usize {
    if per == 0 {
        DEFAULT_HITS
    } else {
        per
    }
}

fn http_client() -> reqwest::Client {
    mh_core::tls::client_builder()
        .user_agent(USER_AGENT)
        .build()
        .unwrap_or_default()
}

/// Fetch and decode JSON, bounded to [`MAX_JSON`] bytes.
async fn get_json<T: DeserializeOwned>(hc: &reqwest::Client, url: &str) -> Result<T> {
    let resp = hc
        .get(url)
        .timeout(JSON_LIMIT)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| http_err(&e))?;
    if !resp.status().is_success() {
        return Err(MediaError::Tool {
            tool: "http",
            message: format!("GET {}: {}", trunc(url, 80), resp.status()),
        });
    }
    let bytes = resp.bytes().await.map_err(|e| http_err(&e))?;
    let slice = &bytes[..bytes.len().min(MAX_JSON)];
    serde_json::from_slice(slice).map_err(|e| MediaError::Parse(e.to_string()))
}

/// Stream a URL to `dest`, via a `.part` file renamed on success.
///
/// Streamed to disk rather than buffered: a talk is tens of megabytes and
/// holding one in memory per concurrent download OOM-kills a small pod.
async fn download(hc: &reqwest::Client, url: &str, dest: &Path) -> Result<()> {
    let resp = hc
        .get(url)
        .timeout(DOWNLOAD_LIMIT)
        .send()
        .await
        .map_err(|e| http_err(&e))?;
    if !resp.status().is_success() {
        return Err(MediaError::Tool {
            tool: "http",
            message: format!("GET {}: {}", trunc(url, 80), resp.status()),
        });
    }
    // Written under a temporary name and renamed, so a download killed midway
    // never leaves a truncated file that looks like a complete chapter.
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    if let Err(e) = stream_to_file(&part, resp).await {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

async fn stream_to_file(part: &Path, resp: reqwest::Response) -> Result<()> {
    let mut file = tokio::fs::File::create(part).await?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| http_err(&e))?;
        file.write_all(&bytes).await?;
    }
    file.flush().await?;
    Ok(())
}

fn http_err(e: &reqwest::Error) -> MediaError {
    MediaError::Tool {
        tool: "http",
        message: trunc(&e.to_string(), 300),
    }
}

/// Run a command with an argument slice and never a shell string. Copied from
/// [`crate::acquire`]: same kill-on-drop, timeout, and stderr-last-line rule.
async fn run(tool: &'static str, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
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

fn first_non_empty(vals: &[&str]) -> String {
    for v in vals {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_owned();
        }
    }
    String::new()
}

const fn first_positive(vals: &[i32]) -> i32 {
    let mut i = 0;
    while i < vals.len() {
        if vals[i] > 0 {
            return vals[i];
        }
        i += 1;
    }
    0
}

fn duration_seconds(d: Option<f64>) -> i32 {
    match d {
        // Duration in seconds; fits i32 for any real book. Truncates the fraction.
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

fn path_str(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_owned)
        .ok_or_else(|| MediaError::Parse("path is not valid UTF-8".to_owned()))
}

/// Percent-encode a URL query component, matching Go's `url.QueryEscape`: space
/// becomes `+`, unreserved bytes stay, everything else `%XX`.
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => push_hex(&mut out, b),
        }
    }
    out
}

/// Percent-encode a URL path segment, matching Go's `url.PathEscape`.
fn path_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            _ => push_hex(&mut out, b),
        }
    }
    out
}

fn push_hex(out: &mut String, b: u8) {
    out.push('%');
    out.push(char::from(HEX[(b >> 4) as usize]));
    out.push(char::from(HEX[(b & 0x0f) as usize]));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn mark(start: f64, end: f64, title: &str) -> YtChapter {
        YtChapter {
            start_time: start,
            end_time: end,
            title: title.to_owned(),
        }
    }

    // --------------------------------------------------------- chapters_from

    #[test]
    fn no_marks_yields_one_whole_file_chapter() {
        let out = chapters_from(&[], 3600, "audiobook.opus", "Frankenstein");
        assert_eq!(
            out,
            vec![SidecarChapter {
                title: "Frankenstein".to_owned(),
                file: "audiobook.opus".to_owned(),
                start_sec: 0,
                end_sec: 3600,
            }]
        );
    }

    #[test]
    fn zero_duration_yields_no_chapters() {
        assert!(chapters_from(&[mark(0.0, 10.0, "A")], 0, "f", "T").is_empty());
    }

    #[test]
    fn overlapping_marks_trust_the_next_start_and_force_first_to_zero() {
        // Marks unsorted, first not at zero, second overlaps the first's end.
        let marks = vec![
            mark(30.0, 999.0, "One"),
            mark(120.0, 60.0, "Two"),
            mark(200.0, 0.0, "Three"),
        ];
        let out = chapters_from(&marks, 300, "audiobook.opus", "Book");
        assert_eq!(out.len(), 3);
        // First forced to zero; ends at the next mark's start, not its own end.
        assert_eq!((out[0].start_sec, out[0].end_sec), (0, 120));
        assert_eq!((out[1].start_sec, out[1].end_sec), (120, 200));
        // Final chapter has a zero end time -> runs to the file duration.
        assert_eq!((out[2].start_sec, out[2].end_sec), (200, 300));
    }

    #[test]
    fn marks_past_the_end_are_dropped_and_ends_clamped() {
        let marks = vec![
            mark(10.0, 0.0, "Real"),
            mark(5000.0, 6000.0, "Past the end"),
        ];
        let out = chapters_from(&marks, 600, "f", "Book");
        // The past-the-end mark describes nothing and is dropped; the survivor is
        // forced to start at zero and clamped to the file duration.
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].start_sec, out[0].end_sec), (0, 600));
    }

    #[test]
    fn empty_chapter_titles_are_numbered() {
        let out = chapters_from(&[mark(0.0, 100.0, "  ")], 200, "f", "Book");
        assert_eq!(out[0].title, "Chapter 1");
    }

    // -------------------------------------------------------- youtube search

    #[test]
    fn youtube_search_filters_short_clips_and_caps() {
        let lines = [
            r#"{"id":"aaaaaaaaaaa","title":"Trailer","duration":180.0,"view_count":10}"#,
            r#"{"id":"bbbbbbbbbbb","title":" Real Book ","duration":3600.0,"view_count":5000,"uploader":"Reader"}"#,
            r#"{"id":"ccccccccccc","title":"Live stream","duration":0.0}"#,
            "",
            r#"{"title":"no id, dropped","duration":9999.0}"#,
            r#"{"id":"ddddddddddd","title":"Another","duration":2400.0}"#,
        ]
        .join("\n");
        let out = parse_youtube_search(lines.as_bytes(), 2);
        assert_eq!(out.len(), 2); // capped
        assert_eq!(out[0].id, "bbbbbbbbbbb");
        assert_eq!(out[0].title, "Real Book");
        assert_eq!(out[0].author, "Reader");
        assert_eq!(out[0].duration_sec, 3600);
        // zero-duration hit is kept (source did not say), short trailer dropped.
        assert_eq!(out[1].id, "ccccccccccc");
    }

    #[test]
    fn youtube_search_falls_back_to_an_id_derived_thumbnail() {
        let line = r#"{"id":"eeeeeeeeeee","title":"X","duration":1500.0}"#;
        let out = parse_youtube_search(line.as_bytes(), 10);
        assert_eq!(
            out[0].thumb,
            "https://i.ytimg.com/vi/eeeeeeeeeee/hqdefault.jpg"
        );
    }

    // ---------------------------------------------------------------- pins

    #[test]
    fn media_metadata_call_includes_the_player_pin() {
        let url = watch_url("dQw4w9WgXcQ");
        let args = yt_meta_argv(&url);
        assert_eq!(args[0], "--extractor-args");
        assert_eq!(args[1], ytdlp::PLAYER_CLIENT);
        assert!(args.contains(&"-J"));
    }

    #[test]
    fn search_call_is_unpinned() {
        let args = yt_search_argv(28, "frankenstein");
        assert!(!args.iter().any(|a| a.as_str() == ytdlp::PLAYER_CLIENT));
        assert!(args.iter().any(|a| a == "--flat-playlist"));
        assert_eq!(args.last().unwrap(), "ytsearch28:frankenstein audiobook");
    }

    // -------------------------------------------------------------- ranking

    #[test]
    fn popular_full_audiobook_outranks_a_longer_sleep_reading() {
        // Same title, so title relevance ties; the 7.6M-view reading must win
        // over the 17-hour one on views, not lose on duration.
        let mut hits = vec![
            SearchResult {
                source: "youtube".into(),
                id: "sleep".into(),
                title: "Pride and Prejudice".into(),
                duration_sec: 17 * 3600,
                views: 0,
                ..SearchResult::default()
            },
            SearchResult {
                source: "youtube".into(),
                id: "full".into(),
                title: "Pride and Prejudice".into(),
                duration_sec: 11 * 3600,
                views: 7_600_000,
                ..SearchResult::default()
            },
        ];
        rank_results("pride and prejudice", &mut hits);
        assert_eq!(hits[0].id, "full");
    }

    #[test]
    fn relevance_dominates_popularity() {
        let mut hits = vec![
            SearchResult {
                source: "youtube".into(),
                id: "viral".into(),
                title: "Funny cat compilation".into(),
                duration_sec: 30 * 60,
                views: 50_000_000,
                ..SearchResult::default()
            },
            SearchResult {
                source: "librivox".into(),
                id: "book".into(),
                title: "Frankenstein".into(),
                duration_sec: 30 * 60,
                views: 0,
                ..SearchResult::default()
            },
        ];
        rank_results("frankenstein", &mut hits);
        assert_eq!(hits[0].id, "book");
    }

    // ------------------------------------------------------- title relevance

    #[test]
    fn title_relevance_ignores_stopwords() {
        // Only "pride" and "prejudice" count; "the"/"and"/"audiobook" are dropped,
        // so a title carrying both meaningful words scores a full match.
        let rel = title_relevance("The Pride and Prejudice audiobook", "Pride and Prejudice");
        assert!((rel - 1.0).abs() < 1e-9, "got {rel}");
    }

    #[test]
    fn a_query_of_only_stopwords_is_irrelevant() {
        assert!(title_relevance("audiobook", "Some Audiobook") < 1e-9);
    }

    #[test]
    fn a_partial_title_match_scores_a_fraction() {
        // One of two meaningful words present, no phrase bonus.
        let rel = title_relevance("moby dick", "Moby the whale");
        assert!((rel - 0.5).abs() < 1e-9, "got {rel}");
    }

    // ---------------------------------------------------------- source names

    #[test]
    fn source_names_are_sorted() {
        assert_eq!(source_names(), vec!["librivox", "media.ccc.de", "youtube"]);
    }

    // ------------------------------------------------------------ media.ccc.de

    #[test]
    fn pick_audio_prefers_mp3_then_english() {
        let recs = vec![
            CccRecording {
                mime_type: "video/mp4".into(),
                recording_url: "v".into(),
                ..CccRecording::default()
            },
            CccRecording {
                mime_type: "audio/opus".into(),
                recording_url: "o".into(),
                ..CccRecording::default()
            },
            CccRecording {
                mime_type: "audio/mpeg".into(),
                language: "deu".into(),
                recording_url: "de".into(),
                ..CccRecording::default()
            },
            CccRecording {
                mime_type: "audio/mpeg".into(),
                language: "eng".into(),
                recording_url: "en".into(),
                ..CccRecording::default()
            },
        ];
        let picked = pick_audio(&recs).unwrap();
        assert_eq!(picked.recording_url, "en"); // mp3 beats opus, eng beats deu
    }

    #[test]
    fn pick_audio_falls_back_to_non_mp3_audio() {
        let recs = vec![
            CccRecording {
                mime_type: "video/mp4".into(),
                recording_url: "v".into(),
                ..CccRecording::default()
            },
            CccRecording {
                mime_type: "audio/opus".into(),
                recording_url: "o".into(),
                ..CccRecording::default()
            },
        ];
        assert_eq!(pick_audio(&recs).unwrap().recording_url, "o");
    }

    #[test]
    fn ccc_results_drop_lightning_talks_and_cap() {
        let resp = CccSearch {
            events: vec![
                CccEvent {
                    guid: "short".into(),
                    title: "Lightning".into(),
                    length: 300,
                    ..CccEvent::default()
                },
                CccEvent {
                    guid: "talk".into(),
                    title: "Real Talk".into(),
                    length: 3600,
                    conference_title: "DENOG".into(),
                    persons: vec!["A".into(), "B".into()],
                    ..CccEvent::default()
                },
                CccEvent {
                    guid: "unknown-len".into(),
                    title: "No length given".into(),
                    length: 0,
                    ..CccEvent::default()
                },
            ],
        };
        let out = ccc_results(resp, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "talk");
        assert_eq!(out[0].author, "A, B");
        assert_eq!(out[0].detail, "DENOG");
        assert_eq!(out[1].id, "unknown-len"); // zero length is kept
    }

    #[test]
    fn ext_for_maps_mime_types() {
        assert_eq!(ext_for("audio/mpeg"), ".mp3");
        assert_eq!(ext_for("audio/opus"), ".opus");
        assert_eq!(ext_for("audio/ogg"), ".ogg");
        assert_eq!(ext_for("audio/mp4"), ".m4a");
        assert_eq!(ext_for("audio/weird"), ".mp3");
    }

    // ----------------------------------------------------------------- librivox

    #[test]
    fn archive_cover_reads_the_item_id() {
        assert_eq!(
            archive_cover("https://archive.org/compress/frankenstein_shelley/formats=X"),
            "https://archive.org/services/img/frankenstein_shelley"
        );
        assert_eq!(
            archive_cover("https://archive.org/download/mobydick_librivox/x.zip"),
            "https://archive.org/services/img/mobydick_librivox"
        );
        assert_eq!(archive_cover("https://example.com/nope.zip"), "");
    }

    #[test]
    fn hms_parses_both_shapes() {
        assert_eq!(hms_to_seconds("8:16:48"), 8 * 3600 + 16 * 60 + 48);
        assert_eq!(hms_to_seconds("16:48"), 16 * 60 + 48);
        assert_eq!(hms_to_seconds(""), 0);
    }

    #[test]
    fn lv_results_and_sorted_sections_use_section_order() {
        let json = r#"{"books":[{
            "id":"123","title":" Frankenstein ","num_sections":"3","totaltime":"1:00:00",
            "authors":[{"first_name":"Mary","last_name":"Shelley"}],
            "url_zip_file":"https://archive.org/compress/frank/formats=X",
            "sections":[
                {"section_number":"2","title":"Two","playtime":"120","listen_url":"u2"},
                {"section_number":"1","title":"","playtime":"60","listen_url":"u1"},
                {"section_number":"3","title":"Three","playtime":"90","listen_url":""}
            ]
        }]}"#;
        let resp: LvResponse = serde_json::from_str(json).unwrap();
        let results = lv_results(serde_json::from_str(json).unwrap());
        assert_eq!(results[0].title, "Frankenstein");
        assert_eq!(results[0].author, "Mary Shelley");
        assert_eq!(results[0].chapters, 3);
        assert_eq!(results[0].duration_sec, 3600);
        assert_eq!(results[0].thumb, "https://archive.org/services/img/frank");

        let (sections, book) = sorted_sections(resp).unwrap();
        // Section 3 has no listen_url and is dropped; the rest sort by number.
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].section_number, "1");
        assert_eq!(sections[1].section_number, "2");
        assert_eq!(book.title.trim(), "Frankenstein");
        // An empty section title becomes a numbered fallback.
        assert_eq!(section_title(&sections[0].title, 0), "Section 1");
    }

    // -------------------------------------------------------------- serde JSON

    #[test]
    fn sidecar_serialises_exactly_like_the_go_shape() {
        let side = Sidecar {
            title: "Frankenstein".into(),
            author: "Mary Shelley".into(),
            source: "librivox".into(),
            chapters: vec![SidecarChapter {
                title: "Chapter 1".into(),
                file: "001.mp3".into(),
                start_sec: 0,
                end_sec: 60,
            }],
        };
        assert_eq!(
            serde_json::to_string(&side).unwrap(),
            r#"{"title":"Frankenstein","author":"Mary Shelley","source":"librivox","chapters":[{"title":"Chapter 1","file":"001.mp3","startSec":0,"endSec":60}]}"#
        );
    }

    #[test]
    fn sidecar_drops_empty_optional_fields() {
        let side = Sidecar {
            title: "T".into(),
            ..Sidecar::default()
        };
        assert_eq!(serde_json::to_string(&side).unwrap(), r#"{"title":"T"}"#);
    }

    #[test]
    fn search_result_serialises_camelcase_and_drops_zero_and_empty() {
        let r = SearchResult {
            source: "youtube".into(),
            id: "abc".into(),
            title: "T".into(),
            ..SearchResult::default()
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"source":"youtube","id":"abc","title":"T"}"#
        );

        let full = SearchResult {
            source: "youtube".into(),
            id: "abc".into(),
            title: "T".into(),
            author: "A".into(),
            duration_sec: 3600,
            chapters: 12,
            views: 5000,
            thumb: "http://t".into(),
            detail: "d".into(),
        };
        assert_eq!(
            serde_json::to_string(&full).unwrap(),
            r#"{"source":"youtube","id":"abc","title":"T","author":"A","durationSec":3600,"chapters":12,"views":5000,"thumb":"http://t","detail":"d"}"#
        );
    }

    #[test]
    fn chapter_serialises_camelcase() {
        let c = Chapter {
            title: "One".into(),
            start_sec: 0,
            end_sec: 120,
        };
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"title":"One","startSec":0,"endSec":120}"#
        );
    }
}
