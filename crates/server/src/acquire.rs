//! HTTP surface for the downloader: search, album search, download, jobs.
//!
//! The subprocess engine lives in `mh_media::acquire`; this module owns the
//! runtime state the Go `Acquire` struct carried — the job list, the source-meta
//! cache, per-id preview locks, and operational alerting — and wires it to HTTP.
//! Error responses use the Go downloader's `{"error": <message>}` shape.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use mh_core::model::{Acquire as AcquireModel, AcquireAlbum, Job, SourceMeta};
use mh_core::Config;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::alert::Notifier;
use crate::auth::Session;
use crate::{AppError, AppState};

const MAX_JOBS: usize = 50;
const MAX_META: usize = 500;
/// One dedup key for "the downloader is broken" — download and preview fail
/// together and for the same reason.
const ALERT_YTDLP: &str = "ytdlp";

/// yt-dlp error signatures that mean the extractor is blocked (worth waking
/// someone), as opposed to one unavailable video. Every string has been observed
/// in practice.
const BREAKAGE_SIGNATURES: [&str; 6] = [
    "HTTP Error 403",
    "Requested format is not available",
    "unable to download video data",
    "Sign in to confirm",
    "nsig extraction failed",
    "player response",
];

/// Runtime state for the downloader, shared via `Arc` in [`AppState`].
pub struct Downloader {
    config: Arc<Config>,
    pool: mh_db::Pool,
    alerts: Notifier,
    /// Oldest first; [`Downloader::jobs`] reverses on the way out.
    jobs: Mutex<Vec<Job>>,
    seq: AtomicI64,
    meta_cache: Mutex<HashMap<String, SourceMeta>>,
    /// Serialises preview downloads per source id so two swipes on one card do
    /// not race two yt-dlp runs into the same cache file.
    clips: KeyedLock,
}

impl Downloader {
    #[must_use]
    pub fn new(config: Arc<Config>, pool: mh_db::Pool, alerts: Notifier) -> Self {
        Self {
            config,
            pool,
            alerts,
            jobs: Mutex::new(Vec::new()),
            seq: AtomicI64::new(0),
            meta_cache: Mutex::new(HashMap::new()),
            clips: KeyedLock::default(),
        }
    }

    /// The directory a download for `user` lands in, relative to the music root.
    /// A user's own library is `users/<user>/downloads` (what the scanner
    /// attributes to them); an empty user falls back to the shared requests dir.
    fn dest_dir(&self, user: &str) -> String {
        if user.is_empty() {
            self.config.requests_dir.clone()
        } else {
            format!("users/{user}/downloads")
        }
    }

    fn lock_jobs(&self) -> MutexGuard<'_, Vec<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn add_job(&self, title: String) -> Job {
        let id = format!("job-{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1);
        let job = Job {
            id,
            title,
            state: "queued".to_owned(),
            ..Job::default()
        };
        let mut jobs = self.lock_jobs();
        jobs.push(job.clone());
        if jobs.len() > MAX_JOBS {
            let excess = jobs.len() - MAX_JOBS;
            jobs.drain(0..excess);
        }
        job
    }

    fn update_job(&self, id: &str, state: &str, detail: &str) {
        for job in self.lock_jobs().iter_mut() {
            if job.id == id {
                state.clone_into(&mut job.state);
                detail.clone_into(&mut job.detail);
                return;
            }
        }
    }

    fn set_job_song(&self, id: &str, song_id: &str) {
        for job in self.lock_jobs().iter_mut() {
            if job.id == id {
                song_id.clone_into(&mut job.song_id);
                return;
            }
        }
    }

    /// Recent downloads, newest first.
    #[must_use]
    pub fn jobs(&self) -> Vec<Job> {
        self.lock_jobs().iter().rev().cloned().collect()
    }

    /// Validate a request, register a visible job, and download in the
    /// background. Every download the service performs is auditable in one list.
    ///
    /// # Errors
    /// Returns the message to show the user when the id is invalid or the title
    /// is empty.
    // Seven distinct descriptive fields for one download; bundling them into a
    // struct only to satisfy the lint would add indirection, not clarity.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue(
        &self,
        app: &AppState,
        user: &str,
        source_id: &str,
        title: &str,
        artist: &str,
        album: &str,
        album_artist: &str,
    ) -> Result<Job, String> {
        if !mh_media::acquire::valid_source_id(source_id) {
            return Err(format!("invalid sourceId {:?}", trunc(source_id, 40)));
        }
        let title = title.trim().to_owned();
        let artist = artist.trim().to_owned();
        let album = album.trim().to_owned();
        let album_artist = album_artist.trim().to_owned();
        if title.is_empty() {
            return Err("title is required".to_owned());
        }
        let label = if artist.is_empty() {
            title.clone()
        } else {
            format!("{artist} - {title}")
        };
        let job = self.add_job(label);

        let app = app.clone();
        let job_id = job.id.clone();
        let user = user.to_owned();
        let source_id = source_id.to_owned();
        tokio::spawn(async move {
            let acq = &app.acquire;
            // The download holds a shared slot; the scan afterwards does not, so
            // a job stays truthfully "queued" until a slot frees rather than
            // starting a dozen transcodes at once and OOMing the pod.
            let fetched = {
                let _slot = mh_media::ytdlp::slot().await;
                acq.update_job(&job_id, "downloading", "");
                acq.fetch_track(&user, &source_id, &title, &artist, &album, &album_artist)
                    .await
            };
            match fetched {
                Err(e) => {
                    tracing::warn!(source = %source_id, error = %e, "acquire failed");
                    acq.update_job(&job_id, "failed", &trunc(&e.to_string(), 300));
                }
                Ok(rel) => {
                    // Done the moment the file is on disk — that is what the user
                    // asked for, and must not depend on the scan succeeding.
                    acq.update_job(&job_id, "done", "");
                    match acq.resolve_song(&app, &rel).await {
                        Ok(song_id) => acq.set_job_song(&job_id, &song_id),
                        Err(e) => {
                            tracing::warn!(rel = %rel, error = %e, "acquire downloaded but not indexed yet");
                        }
                    }
                }
            }
        });

        Ok(job)
    }

    /// Download one track and return its path relative to the music root.
    async fn fetch_track(
        &self,
        user: &str,
        source_id: &str,
        title: &str,
        artist: &str,
        album: &str,
        album_artist: &str,
    ) -> Result<String, mh_media::MediaError> {
        // Library-wide dedup first: if this song is already on the server, reuse
        // that file (re-tagged into the album) instead of downloading it again.
        if let Some(abs) = self
            .reuse_existing(user, title, artist, album, album_artist)
            .await
        {
            return Ok(rel_path(&self.config.music_root, &abs));
        }
        let dest = self.dest_dir(user);
        let abs = match mh_media::acquire::fetch(
            &self.config.music_root,
            &dest,
            source_id,
            title,
            artist,
            album,
            album_artist,
        )
        .await
        {
            Ok(path) => path,
            Err(e) => {
                self.note_failure(&e, source_id);
                return Err(e);
            }
        };
        // A success means whatever blocked us is not blocking us now, so the next
        // outage alerts immediately instead of waiting out a cooldown.
        self.alerts.clear(ALERT_YTDLP);
        Ok(rel_path(&self.config.music_root, &abs))
    }

    /// The path of an already-owned copy of (title, artist) the user can see,
    /// re-tagged into `album` when given - the library-wide download dedup.
    ///
    /// `None` when nothing matches or the indexed file has vanished, so the
    /// caller downloads instead.
    async fn reuse_existing(
        &self,
        user: &str,
        title: &str,
        artist: &str,
        album: &str,
        album_artist: &str,
    ) -> Option<PathBuf> {
        let (title, artist) = mh_media::acquire::clean_identity(title, artist);
        if title.is_empty() {
            return None;
        }
        let copies = mh_db::browse::track_copies_by_title(&self.pool, &title, user)
            .await
            .ok()?;
        let (path, _) = copies
            .into_iter()
            .find(|(_, have)| crate::discovery::same_artist(have, &artist))?;
        let abs = PathBuf::from(path);
        if !tokio::fs::try_exists(&abs).await.unwrap_or(false) {
            return None; // indexed but the file is gone - fetch a fresh copy
        }
        if !album.trim().is_empty() {
            if let Err(e) =
                mh_media::acquire::retag_path(&abs, &title, &artist, album, album_artist).await
            {
                tracing::warn!(path = %abs.display(), error = %e, "reuse could not re-tag album");
            }
        }
        tracing::info!(path = %abs.display(), "acquire reused an existing library copy");
        Some(abs)
    }

    /// Wait for the freshly written file to be indexed and return its library id.
    /// Our scanner is synchronous, so one scan suffices where the Go path polled
    /// Navidrome.
    async fn resolve_song(&self, app: &AppState, rel: &str) -> Result<String, AppError> {
        if rel.is_empty() {
            return Err(AppError::Internal("no path to look up".to_owned()));
        }
        let want_abs = format!("{}/{rel}", self.config.music_root.trim_end_matches('/'));
        crate::scan::scan(app).await?;
        let id: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT id FROM tracks WHERE abs_path = $1")
                .bind(&want_abs)
                .fetch_optional(&self.pool)
                .await
                .map_err(mh_db::DbError::from)?;
        id.map(|u| u.to_string()).ok_or(AppError::NotFound)
    }

    /// Published date and play count for a discovery card, cached per source id.
    ///
    /// # Errors
    /// Fails when yt-dlp cannot be run or the metadata is unparseable.
    pub async fn source_meta(&self, source_id: &str) -> Result<SourceMeta, mh_media::MediaError> {
        let cached = self.lock_meta().get(source_id).cloned();
        if let Some(m) = cached {
            return Ok(m);
        }
        let m = mh_media::acquire::source_meta(source_id).await?;
        {
            let mut cache = self.lock_meta();
            // Dropped wholesale rather than evicted one by one: a convenience
            // cache for a browsing session, an LRU here would be machinery for
            // nothing.
            if cache.len() >= MAX_META {
                cache.clear();
            }
            cache.insert(source_id.to_owned(), m.clone());
        }
        Ok(m)
    }

    fn lock_meta(&self) -> MutexGuard<'_, HashMap<String, SourceMeta>> {
        self.meta_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A local ~30s preview clip for `source_id`, downloading on first use and
    /// serialising concurrent requests for the same id.
    ///
    /// # Errors
    /// Fails when yt-dlp cannot be run or the id is invalid.
    pub async fn preview_clip(&self, source_id: &str) -> Result<PathBuf, mh_media::MediaError> {
        let _guard = self.clips.lock(source_id).await;
        match mh_media::acquire::preview_clip(Path::new(&self.config.cache_dir), source_id).await {
            Ok(path) => {
                self.alerts.clear(ALERT_YTDLP);
                Ok(path)
            }
            Err(e) => {
                self.note_failure(&e, source_id);
                Err(e)
            }
        }
    }

    /// Alert on the first systemic failure and say what to do about it.
    fn note_failure(&self, err: &mh_media::MediaError, source_id: &str) {
        let message = err.to_string();
        if !BREAKAGE_SIGNATURES.iter().any(|sig| message.contains(sig)) {
            return;
        }
        self.alerts.fire(
            ALERT_YTDLP,
            "music-hub: the downloader is blocked",
            &format!(
                "yt-dlp could not fetch audio for {source_id}.\n\n{}\n\nEvery download and \
                 preview is failing until this is fixed. YouTube has almost certainly blocked \
                 the pinned player client: re-run the per-client probe and re-pin whichever \
                 single client still serves media.",
                trunc(&message, 240)
            ),
        );
    }
}

/// Express an absolute library path relative to the music root.
fn rel_path(root: &str, abs: &Path) -> String {
    abs.strip_prefix(root).map_or_else(
        |_| abs.to_string_lossy().into_owned(),
        |p| p.to_string_lossy().into_owned(),
    )
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push_str("...");
    out
}

/// Serialises work per key. Idle entries (no active holder) are swept on each
/// lock, so the map stays about the size of the concurrently-active set.
#[derive(Default)]
struct KeyedLock {
    map: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl KeyedLock {
    async fn lock(&self, key: &str) -> OwnedMutexGuard<()> {
        let mutex = {
            let mut map = self
                .map
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.retain(|_, m| Arc::strong_count(m) > 1);
            Arc::clone(
                map.entry(key.to_owned())
                    .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
            )
        };
        mutex.lock_owned().await
    }
}

// ------------------------------------------------------------------- handlers

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    n: Option<usize>,
}

/// `GET /api/acquire/search` — what yt-dlp can find for a query. A separate
/// endpoint from `/api/search` because the library answers in milliseconds and
/// this takes seconds.
pub async fn search(Query(p): Query<SearchQuery>) -> Response {
    let q = p.q.trim();
    if q.is_empty() {
        return Json(Vec::<AcquireModel>::new()).into_response();
    }
    let n = p.n.filter(|v| *v > 0).map_or(8, |v| v.min(20));
    match mh_media::acquire::search_source(q, n).await {
        Ok(results) => Json(results).into_response(),
        Err(e) => {
            tracing::warn!(query = %q, error = %e, "acquire search");
            err_response(
                StatusCode::BAD_GATEWAY,
                &format!("could not reach the download source: {e}"),
            )
        }
    }
}

/// `GET /api/acquire/albums` — whole albums from YouTube Music, which a plain
/// video search cannot find. Slower than track search, so the UI asks for it
/// alongside rather than before showing anything.
pub async fn albums(Query(p): Query<SearchQuery>) -> Response {
    let q = p.q.trim();
    if q.is_empty() {
        return Json(Vec::<AcquireAlbum>::new()).into_response();
    }
    let n = p.n.filter(|v| *v > 0).map_or(4, |v| v.min(10));
    match mh_media::acquire::search_albums(q, n).await {
        Ok(albums) => Json(albums).into_response(),
        Err(e) => {
            tracing::warn!(query = %q, error = %e, "acquire album search");
            err_response(
                StatusCode::BAD_GATEWAY,
                &format!("could not reach the download source: {e}"),
            )
        }
    }
}

/// `GET /api/acquire/album?list=OLAK5uy_...` — the tracklist of one release
/// playlist, so a release can be opened and picked apart before downloading it.
///
/// The New Releases list is the caller: it already knows each release's playlist
/// id, and the sweep resolved the tracklist to decide ownership but threw it
/// away afterwards. This re-resolves it on demand instead, which costs a few
/// seconds once and only when someone actually asks to see the tracks.
pub async fn album(Query(p): Query<AlbumQuery>) -> Response {
    let list = p.list.trim();
    if list.is_empty() {
        return err_response(StatusCode::BAD_REQUEST, "list is required");
    }
    match mh_media::acquire::resolve_playlist(list).await {
        Ok(album) => Json(album).into_response(),
        Err(e) => {
            tracing::warn!(list, error = %e, "acquire album resolve");
            err_response(
                StatusCode::BAD_GATEWAY,
                &format!("could not reach the download source: {e}"),
            )
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AlbumQuery {
    #[serde(default)]
    list: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadBody {
    #[serde(default)]
    source_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    album: String,
    #[serde(default)]
    album_artist: String,
}

/// `POST /api/acquire` — answer immediately with a job, because a download takes
/// tens of seconds and a phone on mobile data will not hold the request open.
pub async fn download(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Json(body): Json<DownloadBody>,
) -> Response {
    match app.acquire.enqueue(
        &app,
        &session.username,
        &body.source_id,
        &body.title,
        &body.artist,
        &body.album,
        &body.album_artist,
    ) {
        Ok(job) => {
            tracing::info!(
                user = %session.username,
                source = %body.source_id,
                job = %job.id,
                "acquire enqueued"
            );
            (StatusCode::ACCEPTED, Json(job)).into_response()
        }
        Err(msg) => err_response(StatusCode::BAD_REQUEST, &msg),
    }
}

/// `GET /api/acquire/jobs` — recent downloads, newest first.
pub async fn jobs(State(app): State<AppState>) -> Json<Vec<Job>> {
    Json(app.acquire.jobs())
}

#[derive(Debug, Deserialize)]
pub struct AlbumMissingQuery {
    #[serde(rename = "albumId")]
    album_id: String,
}

/// `GET /api/acquire/album-missing` — the online album that completes a library one.
///
/// Returns the matched album and the tracks it is missing. Never downloads; the
/// PWA shows the missing names for the user to confirm before any download.
pub async fn album_missing(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Query(p): Query<AlbumMissingQuery>,
) -> Response {
    let Ok(album_id) = uuid::Uuid::parse_str(p.album_id.trim()) else {
        return err_response(StatusCode::BAD_REQUEST, "invalid albumId");
    };
    let album = match mh_db::browse::album_single(&app.pool, &session.username, album_id).await {
        Ok(Some(a)) => a,
        Ok(None) => return err_response(StatusCode::NOT_FOUND, "album not found"),
        Err(e) => {
            tracing::warn!(error = %e, "album_missing: album lookup");
            return err_response(StatusCode::INTERNAL_SERVER_ERROR, "album lookup failed");
        }
    };
    let owned =
        match mh_db::browse::album_tracks(&app.pool, session.user_id, &session.username, album_id)
            .await
        {
            Ok(tracks) => tracks.into_iter().map(|t| t.title).collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!(error = %e, "album_missing: track lookup");
                return err_response(StatusCode::INTERNAL_SERVER_ERROR, "album lookup failed");
            }
        };
    match mh_media::acquire::find_album_completion(&album.name, &album.artist, &owned).await {
        Ok(Some(c)) => Json(json!({
            "found": true,
            "album": {
                "playlistId": c.album.playlist_id,
                "title": c.album.title,
                "artist": c.album.artist,
                "thumb": c.album.thumb,
                "trackCount": c.album.track_count,
            },
            "have": c.have,
            "missing": c.missing.iter().map(|t| json!({
                "sourceId": t.source_id,
                "title": t.title,
                "artist": t.artist,
                "duration": t.duration,
            })).collect::<Vec<_>>(),
        }))
        .into_response(),
        Ok(None) => Json(json!({ "found": false })).into_response(),
        Err(e) => {
            tracing::warn!(album = %album.name, error = %e, "album_missing: search");
            err_response(StatusCode::BAD_GATEWAY, "album search failed")
        }
    }
}

fn err_response(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    fn downloader() -> Downloader {
        let env: HashMap<String, String> = [
            ("OIDC_ISSUER", "https://kc"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("DATABASE_URL", "postgres://x/y"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let config = Arc::new(Config::load(|k| env.get(k).cloned()).unwrap());
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://mh:mh@localhost/musichub")
            .unwrap();
        Downloader::new(config, pool, Notifier::disabled())
    }

    #[tokio::test]
    async fn dest_dir_uses_the_user_downloads_convention() {
        let d = downloader();
        assert_eq!(d.dest_dir("sam"), "users/sam/downloads");
        assert_eq!(d.dest_dir(""), "requests"); // shared fallback
    }

    #[tokio::test]
    async fn jobs_are_returned_newest_first_and_capped() {
        let d = downloader();
        for i in 0..(MAX_JOBS + 5) {
            d.add_job(format!("track {i}"));
        }
        let jobs = d.jobs();
        assert_eq!(jobs.len(), MAX_JOBS); // ring bounded
                                          // Newest first: the last-added title leads.
        assert_eq!(jobs[0].title, format!("track {}", MAX_JOBS + 4));
    }

    #[tokio::test]
    async fn job_ids_are_sequential_from_one() {
        let d = downloader();
        assert_eq!(d.add_job("a".to_owned()).id, "job-1");
        assert_eq!(d.add_job("b".to_owned()).id, "job-2");
    }

    #[tokio::test]
    async fn update_and_song_id_mutate_the_named_job() {
        let d = downloader();
        let job = d.add_job("t".to_owned());
        d.update_job(&job.id, "done", "");
        d.set_job_song(&job.id, "track-42");
        let stored = d.jobs().into_iter().find(|j| j.id == job.id).unwrap();
        assert_eq!(stored.state, "done");
        assert_eq!(stored.song_id, "track-42");
    }

    #[test]
    fn rel_path_strips_the_music_root() {
        assert_eq!(
            rel_path("/music", Path::new("/music/users/sam/downloads/x.opus")),
            "users/sam/downloads/x.opus"
        );
    }

    #[tokio::test]
    async fn empty_query_returns_an_empty_array_not_an_error() {
        let resp = search(Query(SearchQuery {
            q: "   ".to_owned(),
            n: None,
        }))
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_album_request_without_a_list_is_a_bad_request() {
        // A blank playlist id has nothing to resolve, and going ahead anyway
        // would hand the source an empty id rather than saying what is wrong.
        for list in ["", "   "] {
            let resp = album(Query(AlbumQuery {
                list: list.to_owned(),
            }))
            .await;
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "list={list:?} should be rejected"
            );
        }
    }

    #[tokio::test]
    async fn an_unresolvable_album_is_a_bad_gateway_not_a_panic() {
        // Reaches the real resolve, which cannot reach YouTube from a test. The
        // point is that an upstream failure is reported as one - a 502 with the
        // reason - rather than surfacing as a 500 or, worse, an empty tracklist
        // that looks like a release with no songs.
        let resp = album(Query(AlbumQuery {
            list: "OLAK5uy_not_a_real_playlist".to_owned(),
        }))
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
