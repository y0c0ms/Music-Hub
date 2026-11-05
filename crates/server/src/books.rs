//! The audiobook shelf and the book-download HTTP surface.
//!
//! A book is a folder under the books prefix. The library scan is the authority
//! on which files exist (rows with `is_book`); the on-disk `book.json` sidecar is
//! the authority on titles and chapter marks. Neither replaces the other: the DB
//! does not know a 14-hour file has 32 chapters, and the sidecar does not know
//! track ids. Per-user position, speed, and sleep timer live in `book_progress`.
//! Downloads reuse the shared 2-slot yt-dlp gate and the [`mh_media::bookfetch`]
//! engine. A faithful port of the Go `internal/books` + `internal/bookfetch` HTTP.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use mh_core::model::{Book, BookProgress, Chapter};
use mh_core::Config;
use mh_db::books::{BookTrack, Progress};
use mh_media::bookfetch::{self, SearchResult, Sidecar, SIDECAR_NAME};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::auth::Session;
use crate::AppState;

const MAX_JOBS: usize = 50;
const MIN_SPEED: f64 = 0.5;
const MAX_SPEED: f64 = 3.0;
const MAX_SLEEP: i32 = 24 * 60;

// ------------------------------------------------------------------ downloader

/// One book download in flight, polled by the app. Its own shape (not the music
/// `Job`): a book reports a percent and carries its source.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookJob {
    pub id: String,
    pub title: String,
    pub source: String,
    pub state: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub percent: i32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub book_id: String,
    pub at: i64,
}

/// The book-download job ring plus the in-flight guard against fetching one book
/// twice at once (two writers in one folder produce an interleaved mess).
pub struct Fetcher {
    config: Arc<Config>,
    jobs: Mutex<Vec<BookJob>>,
    inflight: Mutex<HashSet<String>>,
}

impl Fetcher {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            jobs: Mutex::new(Vec::new()),
            inflight: Mutex::new(HashSet::new()),
        }
    }

    fn lock_jobs(&self) -> MutexGuard<'_, Vec<BookJob>> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_inflight(&self) -> MutexGuard<'_, HashSet<String>> {
        self.inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Recent downloads, newest first.
    #[must_use]
    pub fn jobs(&self) -> Vec<BookJob> {
        self.lock_jobs().iter().rev().cloned().collect()
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut BookJob)) {
        if let Some(job) = self.lock_jobs().iter_mut().find(|j| j.id == id) {
            f(job);
        }
    }

    /// Validate and start a download, returning immediately with a queued job.
    ///
    /// # Errors
    /// Returns the message to show the user when the source is unknown, the id
    /// or title is empty, or that book is already downloading.
    pub fn enqueue(&self, app: &AppState, r: SearchResult) -> Result<BookJob, String> {
        if !bookfetch::source_names().contains(&r.source) {
            return Err(format!("unknown source {:?}", r.source));
        }
        if r.id.trim().is_empty() || r.title.trim().is_empty() {
            return Err("id and title are required".to_owned());
        }
        let key = format!("{}:{}", r.source, r.id);
        {
            let mut inflight = self.lock_inflight();
            if inflight.contains(&key) {
                return Err("that book is already downloading".to_owned());
            }
            inflight.insert(key.clone());
        }
        let job = BookJob {
            id: rand_hex(),
            title: r.title.clone(),
            source: r.source.clone(),
            state: "queued".to_owned(),
            detail: String::new(),
            percent: 0,
            book_id: String::new(),
            at: chrono::Utc::now().timestamp_millis(),
        };
        self.lock_jobs().push(job.clone());
        self.trim();

        let app = app.clone();
        let job_id = job.id.clone();
        tokio::spawn(async move {
            // One shared download slot across all sources; the job stays queued
            // until it frees.
            let _slot = mh_media::ytdlp::slot().await;
            let result = app.books.run(&app, &r, &job_id).await;
            app.books.lock_inflight().remove(&key);
            if let Err(e) = result {
                tracing::warn!(source = %r.source, title = %r.title, error = %e, "bookfetch failed");
                app.books.update(&job_id, |j| {
                    "failed".clone_into(&mut j.state);
                    j.detail = trunc(&e, 200);
                });
            }
        });
        Ok(job)
    }

    fn trim(&self) {
        let mut jobs = self.lock_jobs();
        if jobs.len() > MAX_JOBS {
            let excess = jobs.len() - MAX_JOBS;
            jobs.drain(0..excess);
        }
    }

    /// Download into a staging folder and move it into place atomically.
    ///
    /// A book written directly into `books/` would be indexed while half its
    /// chapters exist; the whole folder is assembled out of the way and moved in
    /// one rename.
    async fn run(&self, app: &AppState, r: &SearchResult, job_id: &str) -> Result<(), String> {
        self.update(job_id, |j| "downloading".clone_into(&mut j.state));

        let books_root = books_root(&self.config);
        tokio::fs::create_dir_all(&books_root)
            .await
            .map_err(|e| format!("create books root: {e}"))?;

        let folder = mh_media::acquire::safe_segment(&r.title);
        if folder == "unknown" || folder.is_empty() {
            return Err("title produces an empty folder name".to_owned());
        }
        let final_dir = books_root.join(&folder);
        if final_dir.exists() {
            return Err(format!("{:?} is already in the library", r.title));
        }

        let tmp = books_root.join(format!(".incoming-{}", rand_hex()));
        tokio::fs::create_dir_all(&tmp)
            .await
            .map_err(|e| format!("create staging dir: {e}"))?;
        let _cleanup = StagingDir(tmp.clone());

        let mut sidecar = bookfetch::fetch(r, &tmp).await.map_err(|e| e.to_string())?;
        if sidecar.chapters.is_empty() {
            return Err(format!("{} returned no chapters", r.source));
        }
        if sidecar.title.is_empty() {
            sidecar.title = r.title.clone();
        }
        if sidecar.author.is_empty() {
            sidecar.author = r.author.clone();
        }
        sidecar.source = r.source.clone();

        let blob =
            serde_json::to_string_pretty(&sidecar).map_err(|e| format!("encode sidecar: {e}"))?;
        tokio::fs::write(tmp.join(SIDECAR_NAME), blob)
            .await
            .map_err(|e| format!("write sidecar: {e}"))?;

        tokio::fs::rename(&tmp, &final_dir)
            .await
            .map_err(|e| format!("move into library: {e}"))?;
        tracing::info!(dir = %final_dir.display(), chapters = sidecar.chapters.len(), "bookfetch wrote");

        crate::scan::scan(app).await.map_err(|e| e.to_string())?;
        let book = book_id(&folder);
        self.update(job_id, |j| {
            "done".clone_into(&mut j.state);
            j.percent = 100;
            j.book_id = book;
        });
        Ok(())
    }
}

/// Removes a staging folder when dropped. After a successful rename the folder is
/// already gone, so the removal is a harmless no-op.
struct StagingDir(PathBuf);

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// --------------------------------------------------------------------- shelf

/// A folder of book files plus everything derived from it.
struct Assembled {
    book: Book,
    chapters: Vec<Chapter>,
    folder: String,
}

fn books_root(config: &Config) -> PathBuf {
    Path::new(config.music_root.trim_end_matches('/')).join(config.books_prefix.trim_matches('/'))
}

/// The book folder an absolute path belongs to, or `None` if it is not under the
/// books root. The book is the FIRST directory under the prefix, so per-chapter
/// files in subfolders still group as one book; a loose file is its own book.
fn folder_of(abs_path: &str, books_root: &Path) -> Option<String> {
    let root = books_root.to_str()?;
    let rest = abs_path.strip_prefix(root)?.trim_start_matches('/');
    if rest.is_empty() {
        return None;
    }
    match rest.split_once('/') {
        Some((folder, _)) if !folder.is_empty() => Some(folder.to_owned()),
        Some(_) => None,
        None => {
            // A loose file directly under books/: its name without extension.
            let stem = rest.rsplit_once('.').map_or(rest, |(s, _)| s);
            (!stem.is_empty()).then(|| stem.to_owned())
        }
    }
}

/// A stable, URL-safe id derived from the folder. Hashed so it does not change
/// when a title is edited; it DOES change if the folder is renamed, which is a
/// different book as far as the library is concerned.
fn book_id(folder: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let sum = Sha256::digest(folder.as_bytes());
    let mut id = String::with_capacity(19);
    id.push_str("bk_");
    for &b in &sum[..8] {
        id.push(HEX[(b >> 4) as usize] as char);
        id.push(HEX[(b & 0x0f) as usize] as char);
    }
    id
}

fn pretty_folder(s: &str) -> String {
    s.replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Sorts `chapter2` before `chapter10`: chapter order IS the book, and a wrong
/// order points a resume position at the wrong text.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    while let (Some(&ac), Some(&bc)) = (a.first(), b.first()) {
        let (ad, bd) = (ac.is_ascii_digit(), bc.is_ascii_digit());
        if ad != bd {
            return ac.cmp(&bc);
        }
        if !ad {
            if ac != bc {
                return ac.cmp(&bc);
            }
            a = &a[1..];
            b = &b[1..];
            continue;
        }
        let ai = a.iter().take_while(|c| c.is_ascii_digit()).count();
        let bi = b.iter().take_while(|c| c.is_ascii_digit()).count();
        let an = trim_leading_zeros(&a[..ai]);
        let bn = trim_leading_zeros(&b[..bi]);
        if an.len() != bn.len() {
            return an.len().cmp(&bn.len());
        }
        if an != bn {
            return an.cmp(bn);
        }
        a = &a[ai..];
        b = &b[bi..];
    }
    a.len().cmp(&b.len())
}

fn trim_leading_zeros(b: &[u8]) -> &[u8] {
    let start = b.iter().take_while(|&&c| c == b'0').count();
    &b[start..]
}

/// Build the chapter list for a book that ships marks, mapping each sidecar file
/// to its track and clamping the end to the file's real duration (a source's
/// claimed length runs past the audio otherwise, and a chapter can never finish).
fn chapters_from_sidecar(files: &[BookTrack], sidecar: &Sidecar) -> Vec<Chapter> {
    let mut by_file: HashMap<&str, &BookTrack> = HashMap::new();
    for f in files {
        by_file.insert(basename(&f.abs_path), f);
    }
    let mut chapters = Vec::new();
    for c in &sidecar.chapters {
        let Some(track) = by_file.get(basename(&c.file)) else {
            continue; // names a file not in the library: partial download or removal
        };
        let mut end = c.end_sec;
        let start = c.start_sec;
        if track.duration_sec > 0 {
            if end > track.duration_sec {
                end = track.duration_sec;
            }
            if start >= track.duration_sec {
                continue; // whole chapter past the end of the audio
            }
        }
        if end <= start {
            continue;
        }
        chapters.push(Chapter {
            title: c.title.clone(),
            track_id: track.id.to_string(),
            start_sec: start,
            end_sec: end,
        });
    }
    chapters
}

fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, b)| b)
}

/// Convert a position into "how far through the whole book". For a split book,
/// being 500s into chapter 40 means most of the book is behind you.
fn listened_sec(chapters: &[Chapter], track_id: &str, position_sec: i32) -> i32 {
    for (i, c) in chapters.iter().enumerate() {
        if c.track_id != track_id {
            continue;
        }
        if position_sec < c.start_sec || (c.end_sec > 0 && position_sec >= c.end_sec) {
            continue;
        }
        let mut total = position_sec - c.start_sec;
        for prev in &chapters[..i] {
            total += prev.end_sec - prev.start_sec;
        }
        return total;
    }
    0
}

/// Fold one user's stored state onto a book. `listened_sec` is computed, never
/// stored: it is a function of the position and layout.
fn apply_state(book: &mut Book, chapters: &[Chapter], progress: Option<&Progress>) {
    book.speed = progress
        .map(|p| p.speed)
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);
    book.sleep_minutes = progress.map_or(0, |p| p.sleep_minutes);
    // An empty track id means only settings were saved: not started, no bar.
    if let Some(p) = progress {
        if !p.track_id.is_empty() {
            book.listened_sec = listened_sec(chapters, &p.track_id, p.position_sec);
            book.progress = Some(BookProgress {
                track_id: p.track_id.clone(),
                position_sec: p.position_sec,
                updated_at: p.updated_ms,
            });
        }
    }
}

fn read_sidecar(dir: &Path) -> Sidecar {
    let path = dir.join(SIDECAR_NAME);
    // Absent is normal: a hand-dropped folder has no sidecar.
    std::fs::read(&path).map_or_else(
        |_| Sidecar::default(),
        |bytes| {
            serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "book.json invalid, deriving from files");
                Sidecar::default()
            })
        },
    )
}

/// Assemble every book from the library rows plus the sidecars on disk.
async fn assemble(app: &AppState) -> Result<Vec<Assembled>, crate::AppError> {
    let tracks = mh_db::books::book_tracks(&app.pool).await?;
    let root = books_root(&app.config);

    let mut folders: HashMap<String, Vec<BookTrack>> = HashMap::new();
    for track in tracks {
        if let Some(folder) = folder_of(&track.abs_path, &root) {
            folders.entry(folder).or_default().push(track);
        }
    }

    let mut out = Vec::with_capacity(folders.len());
    for (folder, mut files) in folders {
        files.sort_by(|a, b| natural_cmp(&a.abs_path, &b.abs_path));
        let dir = root.join(&folder);
        if !dir.is_dir() {
            continue; // files gone between a delete and the next scan
        }
        let Some(first) = files.first() else { continue };
        let sidecar = read_sidecar(&dir);

        let chapters = if sidecar.chapters.is_empty() {
            files
                .iter()
                .map(|f| Chapter {
                    title: chapter_title(f),
                    track_id: f.id.to_string(),
                    start_sec: 0,
                    end_sec: f.duration_sec,
                })
                .collect::<Vec<_>>()
        } else {
            chapters_from_sidecar(&files, &sidecar)
        };
        if chapters.is_empty() {
            continue;
        }

        let duration: i32 = chapters
            .iter()
            .map(|c| (c.end_sec - c.start_sec).max(0))
            .sum();
        let title = {
            let t = sidecar.title.trim();
            if t.is_empty() {
                pretty_folder(&folder)
            } else {
                t.to_owned()
            }
        };
        let author = {
            let a = sidecar.author.trim();
            if a.is_empty() {
                first.artist.clone()
            } else {
                a.to_owned()
            }
        };
        let book = Book {
            id: book_id(&folder),
            title,
            author,
            cover_url: format!("/api/cover/{}", first.id),
            chapter_count: i32::try_from(chapters.len()).unwrap_or(i32::MAX),
            duration_sec: duration,
            listened_sec: 0,
            progress: None,
            speed: 1.0,
            sleep_minutes: 0,
            chapters: Vec::new(),
        };
        out.push(Assembled {
            book,
            chapters,
            folder,
        });
    }
    out.sort_by(|a, b| a.book.title.cmp(&b.book.title));
    Ok(out)
}

fn chapter_title(f: &BookTrack) -> String {
    let t = f.title.trim();
    if !t.is_empty() {
        return t.to_owned();
    }
    let stem = basename(&f.abs_path);
    let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
    pretty_folder(stem)
}

// ------------------------------------------------------------------- handlers

/// `GET /api/books` — the shelf, without chapters. In-progress books first.
///
/// # Errors
/// Returns [`crate::AppError`] if the library or progress query fails.
pub async fn list(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Vec<Book>>, crate::AppError> {
    let assembled = assemble(&app).await?;
    let state = mh_db::books::all_progress(&app.pool, session.user_id).await?;
    let mut shelf: Vec<Book> = assembled
        .into_iter()
        .map(|a| {
            let mut b = a.book;
            let progress = state.get(&b.id).cloned();
            apply_state(&mut b, &a.chapters, progress.as_ref());
            b
        })
        .collect();
    shelf.sort_by(in_progress_first);
    Ok(Json(shelf))
}

fn in_progress_first(a: &Book, b: &Book) -> Ordering {
    match (a.progress.as_ref(), b.progress.as_ref()) {
        (Some(pa), Some(pb)) => pb.updated_at.cmp(&pa.updated_at),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// `GET /api/books/:id` — one book with its chapters and this user's progress.
pub async fn get(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match load_book(&app, session.user_id, &id).await {
        Ok(Some(book)) => Json(book).into_response(),
        Ok(None) => not_found(),
        Err(e) => e.into_response(),
    }
}

/// Assemble one book (with chapters + this user's state), or `None` if unknown.
async fn load_book(
    app: &AppState,
    user_id: uuid::Uuid,
    id: &str,
) -> Result<Option<Book>, crate::AppError> {
    let Some(found) = assemble(app).await?.into_iter().find(|a| a.book.id == id) else {
        return Ok(None);
    };
    let progress = mh_db::books::get_progress(&app.pool, user_id, id).await?;
    let mut book = found.book;
    book.chapters = found.chapters;
    let chapters = book.chapters.clone();
    apply_state(&mut book, &chapters, progress.as_ref());
    Ok(Some(book))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressReq {
    #[serde(default)]
    track_id: String,
    #[serde(default)]
    position_sec: i32,
}

/// `PUT /api/books/:id/progress` — the hottest write; validates rather than
/// trusts, so a bookmark never resolves to no chapter.
pub async fn save_progress(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<ProgressReq>,
) -> Response {
    let book = match load_book(&app, session.user_id, &id).await {
        Ok(Some(b)) => b,
        Ok(None) => return not_found(),
        Err(e) => return e.into_response(),
    };
    let track_id = req.track_id.trim();
    if req.position_sec < 0 {
        return err(StatusCode::BAD_REQUEST, "position cannot be negative");
    }
    if track_id.is_empty() || !book.chapters.iter().any(|c| c.track_id == track_id) {
        return err(
            StatusCode::BAD_REQUEST,
            "that track is not part of this book",
        );
    }
    if let Err(e) =
        mh_db::books::save_progress(&app.pool, session.user_id, &id, track_id, req.position_sec)
            .await
    {
        tracing::warn!(book = %id, error = %e, "save book progress");
        return err(StatusCode::INTERNAL_SERVER_ERROR, "could not save progress");
    }
    reload(&app, session.user_id, &id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsReq {
    speed: Option<f64>,
    sleep_minutes: Option<i32>,
}

/// `PUT /api/books/:id` — playback speed and the sleep timer.
pub async fn save_settings(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<SettingsReq>,
) -> Response {
    match load_book(&app, session.user_id, &id).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(e) => return e.into_response(),
    }
    if let Some(speed) = req.speed {
        if !(MIN_SPEED..=MAX_SPEED).contains(&speed) {
            return err(StatusCode::BAD_REQUEST, "speed out of range");
        }
    }
    if let Some(sleep) = req.sleep_minutes {
        if !(0..=MAX_SLEEP).contains(&sleep) {
            return err(StatusCode::BAD_REQUEST, "sleep timer out of range");
        }
    }
    if let Err(e) = mh_db::books::save_settings(
        &app.pool,
        session.user_id,
        &id,
        req.speed,
        req.sleep_minutes,
    )
    .await
    {
        tracing::warn!(book = %id, error = %e, "save book settings");
        return err(StatusCode::INTERNAL_SERVER_ERROR, "could not save settings");
    }
    reload(&app, session.user_id, &id).await
}

async fn reload(app: &AppState, user_id: uuid::Uuid, id: &str) -> Response {
    match load_book(app, user_id, id).await {
        Ok(Some(book)) => Json(book).into_response(),
        Ok(None) => not_found(),
        Err(e) => e.into_response(),
    }
}

/// `DELETE /api/books/:id` — remove the book for everyone; per-user progress is
/// kept so re-adding restores every listener's place.
pub async fn delete(State(app): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    let folder = match assemble(&app).await {
        Ok(books) => books
            .into_iter()
            .find(|a| a.book.id == id)
            .map(|a| a.folder),
        Err(e) => return e.into_response(),
    };
    let Some(folder) = folder else {
        return not_found();
    };
    // The id is a hash and the folder came from the index, but re-validate the
    // resolved path stays under the books root before removing anything.
    let root = books_root(&app.config);
    let dir = root.join(&folder);
    if !dir.starts_with(&root) || folder.contains('/') || folder == ".." {
        return err(StatusCode::BAD_REQUEST, "could not delete this book");
    }
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
        tracing::warn!(dir = %dir.display(), error = %e, "book delete");
        return err(StatusCode::BAD_REQUEST, "could not delete this book");
    }
    if let Err(e) = crate::scan::scan(&app).await {
        tracing::warn!(error = %e, "rescan after book delete");
    }
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

#[derive(Debug, Deserialize)]
pub struct SearchQ {
    #[serde(default)]
    q: String,
    n: Option<usize>,
    source: Option<String>,
}

/// `GET /api/books/search` — search online sources (YouTube, LibriVox, media.ccc.de).
pub async fn search_http(Query(p): Query<SearchQ>) -> Response {
    let q = p.q.trim();
    if q.is_empty() {
        return Json(Vec::<SearchResult>::new()).into_response();
    }
    let n = p.n.filter(|v| *v > 0).map_or(15, |v| v.min(30));
    let results = bookfetch::search(q, n, p.source.as_deref()).await;
    Json(results).into_response()
}

/// `GET /api/books/sources` — the source picker list.
pub async fn sources_http() -> Json<Vec<String>> {
    Json(bookfetch::source_names())
}

/// `GET /api/books/jobs` — in-flight and recent book downloads, newest first.
pub async fn jobs_http(State(app): State<AppState>) -> Json<Vec<BookJob>> {
    Json(app.books.jobs())
}

/// `POST /api/books/get` — queue a download from a whole search result.
pub async fn get_http(State(app): State<AppState>, Json(result): Json<SearchResult>) -> Response {
    match app.books.enqueue(&app, result) {
        Ok(job) => (StatusCode::ACCEPTED, Json(job)).into_response(),
        Err(msg) => err(StatusCode::BAD_REQUEST, &msg),
    }
}

/// `POST /api/books/chapters` — one result's contents, fetched on demand.
pub async fn chapters_http(Json(result): Json<SearchResult>) -> Response {
    match bookfetch::chapters(&result).await {
        Ok(chapters) => Json(chapters).into_response(),
        Err(e) => err(
            StatusCode::BAD_GATEWAY,
            &format!("could not read chapters: {e}"),
        ),
    }
}

// --------------------------------------------------------------------- shared

fn rand_hex() -> String {
    format!("{:016x}", rand::random::<u64>())
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push_str("...");
    out
}

fn not_found() -> Response {
    err(StatusCode::NOT_FOUND, "no such book")
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn track(abs: &str, dur: i32) -> BookTrack {
        BookTrack {
            id: uuid::Uuid::nil(),
            abs_path: abs.to_owned(),
            title: String::new(),
            artist: "Narrator".to_owned(),
            duration_sec: dur,
            has_cover: false,
        }
    }

    #[test]
    fn book_id_is_stable_and_prefixed() {
        assert_eq!(book_id("Dune"), book_id("Dune"));
        assert_ne!(book_id("Dune"), book_id("dune"));
        assert!(book_id("Dune").starts_with("bk_"));
        assert_eq!(book_id("Dune").len(), 19); // "bk_" + 16 hex
    }

    #[test]
    fn folder_of_groups_by_first_dir_and_names_loose_files() {
        let root = Path::new("/music/books");
        assert_eq!(
            folder_of("/music/books/Dune/ch1.mp3", root).as_deref(),
            Some("Dune")
        );
        assert_eq!(
            folder_of("/music/books/Dune/parts/2.mp3", root).as_deref(),
            Some("Dune")
        );
        assert_eq!(
            folder_of("/music/books/loose.opus", root).as_deref(),
            Some("loose")
        );
        assert_eq!(folder_of("/music/albums/x.flac", root), None);
    }

    #[test]
    fn natural_cmp_orders_chapters_numerically() {
        let mut v = vec!["chapter10.mp3", "chapter2.mp3", "chapter1.mp3"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["chapter1.mp3", "chapter2.mp3", "chapter10.mp3"]);
    }

    #[test]
    fn chapters_from_sidecar_clamps_end_to_real_duration() {
        // media.ccc.de reported 1660s for a file the scan measured at 1648s.
        let files = vec![track("/music/books/Talk/talk.mp3", 1648)];
        let sidecar = Sidecar {
            title: "Talk".to_owned(),
            author: String::new(),
            source: "media.ccc.de".to_owned(),
            chapters: vec![bookfetch::SidecarChapter {
                title: "Whole".to_owned(),
                file: "talk.mp3".to_owned(),
                start_sec: 0,
                end_sec: 1660,
            }],
        };
        let ch = chapters_from_sidecar(&files, &sidecar);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].end_sec, 1648); // clamped, so the chapter can finish
    }

    #[test]
    fn chapters_from_sidecar_drops_chapters_past_the_end() {
        let files = vec![track("/music/books/B/a.mp3", 100)];
        let sidecar = Sidecar {
            title: "B".to_owned(),
            author: String::new(),
            source: "x".to_owned(),
            chapters: vec![
                bookfetch::SidecarChapter {
                    title: "ok".to_owned(),
                    file: "a.mp3".to_owned(),
                    start_sec: 0,
                    end_sec: 50,
                },
                bookfetch::SidecarChapter {
                    title: "past".to_owned(),
                    file: "a.mp3".to_owned(),
                    start_sec: 200,
                    end_sec: 300,
                },
                bookfetch::SidecarChapter {
                    title: "missing".to_owned(),
                    file: "gone.mp3".to_owned(),
                    start_sec: 0,
                    end_sec: 10,
                },
            ],
        };
        let ch = chapters_from_sidecar(&files, &sidecar);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].title, "ok");
    }

    #[test]
    fn listened_sec_sums_prior_chapters_for_a_split_book() {
        let chapters = vec![
            Chapter {
                title: "1".to_owned(),
                track_id: "a".to_owned(),
                start_sec: 0,
                end_sec: 100,
            },
            Chapter {
                title: "2".to_owned(),
                track_id: "b".to_owned(),
                start_sec: 0,
                end_sec: 100,
            },
        ];
        // 30s into chapter 2 -> 100 (all of ch1) + 30.
        assert_eq!(listened_sec(&chapters, "b", 30), 130);
        // Position outside any chapter -> 0.
        assert_eq!(listened_sec(&chapters, "b", 500), 0);
    }

    #[test]
    fn apply_state_distinguishes_unstarted_from_position_zero() {
        let chapters = vec![Chapter {
            title: "1".to_owned(),
            track_id: "tr".to_owned(),
            start_sec: 0,
            end_sec: 1000,
        }];

        // Settings-only row (empty track id): speed applies, no progress bar.
        let mut b = Book::default();
        let settings = Progress {
            book_id: "x".to_owned(),
            track_id: String::new(),
            position_sec: 0,
            speed: 1.5,
            sleep_minutes: 10,
            updated_ms: 1,
        };
        apply_state(&mut b, &chapters, Some(&settings));
        assert!(b.progress.is_none());
        assert!((b.speed - 1.5).abs() < 1e-9);
        assert_eq!(b.sleep_minutes, 10);

        // Rewound to zero (track id present): a progress bar at zero.
        let mut b = Book::default();
        let zeroed = Progress {
            book_id: "x".to_owned(),
            track_id: "tr".to_owned(),
            position_sec: 0,
            speed: 1.0,
            sleep_minutes: 0,
            updated_ms: 2,
        };
        apply_state(&mut b, &chapters, Some(&zeroed));
        assert!(b.progress.is_some());

        // No row at all: default speed, unstarted.
        let mut b = Book::default();
        apply_state(&mut b, &chapters, None);
        assert!(b.progress.is_none());
        assert!((b.speed - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn assemble_builds_a_book_from_tracks_and_its_sidecar() {
        use mh_db::library::NewTrack;
        use sqlx::postgres::PgPoolOptions;
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(pool) = PgPoolOptions::new().connect(&url).await else {
            return;
        };

        // A book folder on disk with two chapter files and a sidecar.
        let root = std::env::temp_dir().join(format!("mh-shelf-{:016x}", rand::random::<u64>()));
        let dir = root.join("books").join("Dune");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ch1.mp3"), b"a").unwrap();
        std::fs::write(dir.join("ch2.mp3"), b"b").unwrap();
        let sidecar = Sidecar {
            title: "Dune".to_owned(),
            author: "Frank Herbert".to_owned(),
            source: "librivox".to_owned(),
            chapters: vec![
                bookfetch::SidecarChapter {
                    title: "One".to_owned(),
                    file: "ch1.mp3".to_owned(),
                    start_sec: 0,
                    end_sec: 100,
                },
                bookfetch::SidecarChapter {
                    title: "Two".to_owned(),
                    file: "ch2.mp3".to_owned(),
                    start_sec: 0,
                    end_sec: 200,
                },
            ],
        };
        std::fs::write(
            dir.join(SIDECAR_NAME),
            serde_json::to_vec(&sidecar).unwrap(),
        )
        .unwrap();

        // Seed the two chapter tracks (is_book) at their real absolute paths.
        let lib = mh_db::library::get_or_create_library(
            &pool,
            root.join("books").to_str().unwrap(),
            "books",
            "shared",
            "",
        )
        .await
        .unwrap();
        for (n, dur) in [("ch1", 100), ("ch2", 200)] {
            let id = uuid::Uuid::new_v4();
            let abs = dir.join(format!("{n}.mp3")).to_string_lossy().into_owned();
            let track = NewTrack {
                id,
                library_id: lib,
                rel_path: format!("Dune/{n}.mp3"),
                abs_path: abs,
                title: n.to_owned(),
                artist: "Frank Herbert".to_owned(),
                artist_id: uuid::Uuid::new_v4(),
                album_artist: String::new(),
                album: "Dune".to_owned(),
                album_id: None,
                track_no: 0,
                disc_no: 0,
                year: 0,
                genre: String::new(),
                duration_sec: dur,
                size_bytes: 1,
                has_cover: false,
                is_book: true,
                mtime: None,
            };
            mh_db::library::upsert_track(&pool, &track).await.unwrap();
        }

        let env: std::collections::HashMap<String, String> = [
            ("OIDC_ISSUER", "https://kc"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("DATABASE_URL", url.as_str()),
            ("MUSIC_ROOT", root.to_str().unwrap()),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let cfg = Config::load(|k| env.get(k).cloned()).unwrap();
        let app = crate::AppState::for_test(cfg, pool.clone());

        let books = assemble(&app).await.unwrap();
        let book = books
            .iter()
            .find(|a| a.book.title == "Dune")
            .expect("Dune assembled");
        assert_eq!(book.book.author, "Frank Herbert");
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(book.book.duration_sec, 300); // 100 + 200 from the sidecar
        assert_eq!(book.book.id, book_id("Dune"));

        // Cleanup: the two tracks and the library row, then the temp tree.
        sqlx::query("DELETE FROM tracks WHERE library_id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM libraries WHERE id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
}
