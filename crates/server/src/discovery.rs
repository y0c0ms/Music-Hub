//! The swipe deck: owned tracks under the discovery prefix to keep or delete,
//! and candidates from ListenBrainz + Last.fm to download or skip.
//!
//! The two kinds are not cosmetic. A left swipe on an owned card deletes a real
//! file from a library that has lost data before; a left swipe on a candidate
//! costs nothing. Everything here that looks over-careful is over-careful about
//! exactly that one operation — see [`guard_delete`]. A faithful port of the Go
//! `internal/discovery` package, re-architected onto the owned Postgres library
//! (no Navidrome: the track's stored `abs_path` is the truth).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use mh_core::model::{Acquire, AcquireAlbum, DiscoveryCard, SwipeResult};
use mh_core::Config;
use rand::seq::SliceRandom;
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::auth::Session;
use crate::AppState;

const DEFAULT_DECK: usize = 20;
const MAX_DECK: usize = 50;
const SEED_ARTISTS: i64 = 6;
const LB_FRESH_DAYS: u32 = 90;

const LASTFM_BASE: &str = "https://ws.audioscrobbler.com/2.0/";
const LB_BASE: &str = "https://api.listenbrainz.org";
const LB_WEEKLY_PREFIX: &str = "Weekly Exploration";
const USER_AGENT: &str = "music-hub/1.0 (self-hosted)";

const MB_BASE: &str = "https://musicbrainz.org/ws/2";
/// MusicBrainz wants a descriptive UA naming a contact URL, unlike ListenBrainz.
const MB_USER_AGENT: &str = "music-hub/1.0 ( https://github.com/y0c0ms/Music-Hub )";
/// MusicBrainz asks for at most one request per second.
const MB_RATE: Duration = Duration::from_secs(1);
/// How far back a release still counts as "new" for the top-artist sweep.
const NEW_RELEASE_DAYS: i64 = 120;

/// Runtime state for the deck: just the outbound HTTP client and config. The
/// pool and downloader come from [`AppState`].
pub struct Discovery {
    config: Arc<Config>,
    http: reqwest::Client,
}

impl Discovery {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        let http = mh_core::tls::client_builder()
            .timeout(Duration::from_secs(12))
            .build()
            .unwrap_or_default();
        Self { config, http }
    }

    fn abs_prefix(&self) -> String {
        format!(
            "{}/{}",
            self.config.music_root.trim_end_matches('/'),
            self.config.discovery_prefix
        )
    }
}

// ------------------------------------------------------------------- handlers

#[derive(Debug, Deserialize)]
pub struct DeckQuery {
    n: Option<usize>,
}

/// `GET /api/discovery/deck` — a mixed stack of owned + candidate cards.
pub async fn deck(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Query(q): Query<DeckQuery>,
) -> Response {
    let n =
        q.n.filter(|v| *v > 0)
            .map_or(DEFAULT_DECK, |v| v.min(MAX_DECK));
    let judged = match mh_db::discovery::judged(&app.pool, session.user_id).await {
        Ok(j) => j,
        Err(e) => return crate::AppError::from(e).into_response(),
    };

    // Cap the stale explo "owned" half low (Explo is dead upstream); Discover
    // should lead with new music, not surface the library.
    let owned_fut = owned_cards(&app, &judged, n / 4);
    let cands_fut = candidate_cards(&app, &session, &judged, n.div_ceil(2));
    let (mut owned, (cands, problem)) = tokio::join!(owned_fut, cands_fut);

    // Last.fm/ListenBrainz being down costs suggestions, not the deck: say so on
    // the owned cards rather than failing.
    if let Some(note) = problem {
        let note = format!("new suggestions unavailable ({note})");
        for card in &mut owned {
            if card.reason.is_empty() {
                card.reason.clone_from(&note);
            } else {
                card.reason = format!("{} - {note}", card.reason);
            }
        }
    }

    Json(interleave(&owned, &cands, n)).into_response()
}

/// `GET /api/discovery/meta/:id` — published date + views for a candidate.
/// A failure answers 200 with an empty object: the detail is a nicety.
pub async fn meta(State(app): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    if !mh_media::acquire::valid_source_id(&id) {
        return err(StatusCode::BAD_REQUEST, "invalid source id");
    }
    let meta = app.acquire.source_meta(&id).await.unwrap_or_default();
    Json(meta).into_response()
}

/// `GET /api/discovery/cover/:playlist` — the artwork of one release playlist.
///
/// Signed YouTube thumbnail URLs expire, so none are stored: the cache keeps only
/// the playlist id, and the image is fetched on first request, written beside the
/// other state files, and served from there. A playlist whose cover cannot be
/// fetched answers 404 and the client shows its placeholder.
pub async fn playlist_cover(
    State(app): State<AppState>,
    AxumPath(playlist): AxumPath<String>,
) -> Response {
    let valid = playlist.starts_with("OLAK5uy_")
        && playlist.len() <= 50
        && playlist
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !valid {
        return StatusCode::NOT_FOUND.into_response();
    }
    let dir = Path::new(&app.config.state_dir).join("release-covers");
    let file = dir.join(format!("{playlist}.jpg"));
    if let Ok(bytes) = tokio::fs::read(&file).await {
        return cover_response(bytes);
    }
    let Ok(url) = mh_media::acquire::playlist_cover_url(&playlist).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(resp) = app.discovery.http.get(&url).send().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    if !resp.status().is_success() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(bytes) = resp.bytes().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    if tokio::fs::create_dir_all(&dir).await.is_ok() {
        let tmp = dir.join(format!("{playlist}.jpg.tmp"));
        if tokio::fs::write(&tmp, &bytes).await.is_ok() {
            let _ = tokio::fs::rename(&tmp, &file).await;
        }
    }
    cover_response(bytes.to_vec())
}

fn cover_response(bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        bytes,
    )
        .into_response()
}

/// `GET /api/discovery/preview/:kind/:id` — an audio preview for one card.
pub async fn preview(
    State(app): State<AppState>,
    AxumPath((kind, id)): AxumPath<(String, String)>,
    request: Request,
) -> Response {
    match kind.as_str() {
        "owned" => match uuid::Uuid::parse_str(&id) {
            Ok(uuid) => match mh_db::discovery::track_abs_path(&app.pool, uuid).await {
                Ok(Some(path)) => {
                    serve_file(request, Path::new(&path), None, "private, max-age=3600").await
                }
                Ok(None) => err(StatusCode::NOT_FOUND, "no such track"),
                Err(e) => crate::AppError::from(e).into_response(),
            },
            Err(_) => err(StatusCode::BAD_REQUEST, "invalid track id"),
        },
        "candidate" => {
            if !mh_media::acquire::valid_source_id(&id) {
                return err(StatusCode::BAD_REQUEST, "invalid source id");
            }
            match app.acquire.preview_clip(&id).await {
                Ok(path) => {
                    serve_file(request, &path, Some("audio/ogg"), "private, max-age=3600").await
                }
                Err(e) => err(
                    StatusCode::BAD_GATEWAY,
                    &format!("could not fetch a preview: {e}"),
                ),
            }
        }
        _ => err(StatusCode::BAD_REQUEST, "unknown preview kind"),
    }
}

#[derive(Debug, Deserialize)]
pub struct SwipeReq {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
}

/// `POST /api/discovery/like`.
pub async fn like(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Json(req): Json<SwipeReq>,
) -> Response {
    if req.id.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "missing id");
    }
    match req.kind.as_str() {
        "owned" => {
            if let Err(e) = mh_db::discovery::judge(&app.pool, session.user_id, &req.id, true).await
            {
                return swipe(
                    false,
                    "kept",
                    &format!("kept the file but could not save the verdict: {e}"),
                );
            }
            swipe(true, "kept", "kept in your library")
        }
        "candidate" => {
            if !mh_media::acquire::valid_source_id(&req.id) {
                return err(StatusCode::BAD_REQUEST, "invalid source id");
            }
            match app.acquire.enqueue(
                &app,
                &session.username,
                &req.id,
                &req.title,
                &req.artist,
                "",
                "",
            ) {
                Ok(job) => {
                    if let Err(e) =
                        mh_db::discovery::judge(&app.pool, session.user_id, &req.id, true).await
                    {
                        tracing::warn!(source = %req.id, error = %e, "discovery could not record like");
                    }
                    swipe(true, "queued", &format!("downloading {}", job.title))
                }
                Err(msg) => swipe(false, "queued", &msg),
            }
        }
        _ => err(StatusCode::BAD_REQUEST, "unknown card kind"),
    }
}

/// `POST /api/discovery/dislike`.
pub async fn dislike(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Json(req): Json<SwipeReq>,
) -> Response {
    if req.id.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "missing id");
    }
    match req.kind.as_str() {
        "candidate" => {
            // Nothing was downloaded; remembering the verdict is the whole op.
            if let Err(e) =
                mh_db::discovery::judge(&app.pool, session.user_id, &req.id, false).await
            {
                return swipe(
                    false,
                    "skipped",
                    &format!("could not save the verdict: {e}"),
                );
            }
            swipe(true, "skipped", "won't suggest that again")
        }
        "owned" => delete_owned(&app, session.user_id, &req.id)
            .await
            .into_response(),
        _ => err(StatusCode::BAD_REQUEST, "unknown card kind"),
    }
}

#[derive(Debug, Deserialize)]
pub struct NewReleasesQuery {
    days: Option<u32>,
}

/// `GET /api/discovery/new-releases` — recent releases from artists you listen to.
///
/// From ListenBrainz fresh releases, minus what you own and minus live/bootleg
/// noise. A list to browse and download, not a swipe deck.
pub async fn new_releases(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Query(q): Query<NewReleasesQuery>,
) -> Response {
    let judged = mh_db::discovery::judged(&app.pool, session.user_id)
        .await
        .unwrap_or_default();

    // Prefer the nightly precompute: instant, and each row already carries a
    // resolved preview source so the first tap plays with no live yt-dlp resolve.
    // `judged` is still applied here so a dismissal since last night takes effect.
    let cached = mh_db::discovery::cached_new_releases(&app.pool, session.user_id)
        .await
        .unwrap_or_default();
    if !cached.is_empty() {
        let out: Vec<Value> = cached
            .into_iter()
            .filter(|r| !judged.contains(&release_key(&r.mbid, &r.artist, &r.title)))
            .map(|r| cached_release_json(&r))
            .collect();
        return Json(json!({ "releases": out, "cached": true })).into_response();
    }

    // Live fallback: a cold cache (a new user, or before the first nightly run).
    let days = q.days.unwrap_or(LB_FRESH_DAYS).clamp(1, LB_FRESH_DAYS);
    let lb = lb_user(&app, &session).await;
    if lb.is_empty() {
        return Json(json!({ "releases": [], "note": "no ListenBrainz user configured" }))
            .into_response();
    }
    let releases = match fresh_releases(&app.discovery.http, &lb, days).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "new releases");
            return Json(json!({ "releases": [], "note": e })).into_response();
        }
    };
    let owned = mh_db::discovery::owned_tracks(&app.pool, session.user_id)
        .await
        .unwrap_or_default();
    let owned_albums = mh_db::discovery::owned_albums(&app.pool, session.user_id)
        .await
        .unwrap_or_default();
    let out: Vec<Value> = releases
        .into_iter()
        .filter(|r| {
            // A release with a tracklist is compared track by track, so owning
            // one of six never hides the other five. Without a tracklist there
            // is nothing finer to compare than the title.
            let have_it = if r.tracks.is_empty() {
                owned_track(&owned, &r.artist, &r.title)
                    || release_owned_by_album(&owned_albums, &r.artist, &r.title, r.track_count)
            } else {
                release_fully_owned(&owned, &r.artist, &r.tracks)
                    || release_owned_by_album(&owned_albums, &r.artist, &r.title, r.track_count)
            };
            !have_it && !judged.contains(&release_key(&r.mbid, &r.artist, &r.title))
        })
        .map(|r| {
            json!({
                "artist": r.artist,
                "title": r.title,
                "date": r.date,
                "kind": r.kind,
                "coverUrl": r.cover,
                "mbid": r.mbid,
            })
        })
        .collect();
    Json(json!({ "releases": out })).into_response()
}

/// Shape one cached release for the New Releases list, carrying its resolved
/// preview source so the client can play with no live resolve.
/// The cover a cached row is served with.
///
/// A YouTube row's stored cover is a signed URL that goes stale, so it is never
/// sent to the client; the playlist's own artwork route is. Other sources keep
/// their stored URL.
fn cover_for(r: &mh_db::discovery::CachedRelease) -> String {
    if r.source == "youtube" && r.mbid.starts_with("OLAK5uy_") {
        format!("/api/discovery/cover/{}", r.mbid)
    } else {
        r.cover_url.clone()
    }
}

fn cached_release_json(r: &mh_db::discovery::CachedRelease) -> Value {
    let preview = if r.source_id.is_empty() {
        Value::Null
    } else {
        json!(format!("/api/discovery/preview/candidate/{}", r.source_id))
    };
    json!({
        "artist": r.artist,
        "title": r.title,
        // Blank when the date is not a real release date, so the client cannot
        // file the row into a week it does not belong to.
        "date": if r.date_uncertain { "" } else { r.release_date.as_str() },
        "kind": r.kind,
        "coverUrl": cover_for(r),
        "mbid": r.mbid,
        "sourceId": r.source_id,
        "previewUrl": preview,
        "source": r.source,
        "dateUncertain": r.date_uncertain,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquireReleaseReq {
    artist: String,
    title: String,
    kind: Option<String>,
    /// The release's own YouTube Music playlist (`OLAK5uy_...`), which the New
    /// Releases row already carries as its `mbid`. When present it is the
    /// tracklist, and no search is run.
    #[serde(default)]
    playlist_id: String,
}

/// `POST /api/discovery/acquire-release` — resolve a release and enqueue it.
///
/// An album/EP downloads the exact playlist the row was built from. Searching by
/// name was the old way, and for a day-old album the search found nothing and
/// fell through to the artist's previous record: VENDETTA (25 new songs) queued
/// Carta de Alforria (22 owned ones) on 2026-11-05. A search result is now
/// accepted only when its title is the release's title; otherwise the request
/// fails and nothing is queued. A single is one public upload.
pub async fn acquire_release(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Json(req): Json<AcquireReleaseReq>,
) -> Response {
    let artist = req.artist.trim();
    let title = req.title.trim();
    if artist.is_empty() || title.is_empty() {
        return err(StatusCode::BAD_REQUEST, "artist and title are required");
    }
    let single = req
        .kind
        .as_deref()
        .is_some_and(|k| k.eq_ignore_ascii_case("single"));
    let mut queued = 0usize;
    if single {
        let query = format!("{artist} {title}");
        if let Ok(hits) = mh_media::acquire::search_source(&query, 3).await {
            if let Some(hit) = pick_hit(&hits) {
                if app
                    .acquire
                    .enqueue(
                        &app,
                        &session.username,
                        &hit.source_id,
                        title,
                        artist,
                        "",
                        "",
                    )
                    .is_ok()
                {
                    queued = 1;
                }
            }
        }
    } else if let Some(album) = release_album(req.playlist_id.trim(), artist, title).await {
        for tr in &album.tracks {
            if app
                .acquire
                .enqueue(
                    &app,
                    &session.username,
                    &tr.source_id,
                    &tr.title,
                    &tr.artist,
                    title,
                    artist,
                )
                .is_ok()
            {
                queued += 1;
            }
        }
    }
    if queued == 0 {
        return err(
            StatusCode::BAD_GATEWAY,
            "could not find that release to download",
        );
    }
    tracing::info!(user = %session.username, %artist, %title, queued, "acquire release");
    Json(json!({ "queued": queued })).into_response()
}

/// The tracklist to download for an album release: its own playlist when the
/// row carries one, else the first search result whose title IS the release.
async fn release_album(playlist_id: &str, artist: &str, title: &str) -> Option<AcquireAlbum> {
    if playlist_id.starts_with("OLAK5uy_") {
        return match mh_media::acquire::resolve_playlist(playlist_id).await {
            Ok(album) if !album.tracks.is_empty() => Some(album),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(playlist_id, error = %e, "acquire release: playlist resolve");
                None
            }
        };
    }
    let albums = mh_media::acquire::search_albums(&format!("{artist} {title}"), 4)
        .await
        .ok()?;
    let pick = pick_release_album(albums, artist, title);
    if pick.is_none() {
        tracing::warn!(%artist, %title, "acquire release: no album with that title");
    }
    pick
}

/// The first album whose title is the release title (accent- and
/// punctuation-folded) by the same artist. Never a "closest" album: a wrong
/// album downloads songs the user did not ask for.
fn pick_release_album(
    albums: Vec<AcquireAlbum>,
    artist: &str,
    title: &str,
) -> Option<AcquireAlbum> {
    let want = norm(title);
    albums.into_iter().find(|a| {
        norm(&a.title) == want
            && !a.tracks.is_empty()
            && (a.artist.is_empty() || same_artist(&a.artist, artist))
    })
}

#[derive(Debug, Deserialize)]
pub struct ReleaseSourceQuery {
    artist: String,
    title: String,
}

/// `GET /api/discovery/release-source` — a public upload's id for a release, so
/// the New Releases list can play a 30s preview before you decide.
pub async fn release_source(Query(q): Query<ReleaseSourceQuery>) -> Response {
    let query = format!("{} {}", q.artist.trim(), q.title.trim());
    if query.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "artist and title are required");
    }
    let source = mh_media::acquire::search_source(&query, 3)
        .await
        .ok()
        .and_then(|hits| pick_hit(&hits))
        .map(|h| h.source_id)
        .unwrap_or_default();
    Json(json!({ "sourceId": source })).into_response()
}

#[derive(Debug, Deserialize)]
pub struct DismissReleaseReq {
    artist: String,
    title: String,
    #[serde(default)]
    mbid: String,
}

/// `POST /api/discovery/dismiss-release` — record that the user is not interested
/// in a release, so it leaves the New Releases list and does not return.
pub async fn dismiss_release(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Json(req): Json<DismissReleaseReq>,
) -> Response {
    let key = release_key(req.mbid.trim(), req.artist.trim(), req.title.trim());
    match mh_db::discovery::judge(&app.pool, session.user_id, &key, false).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => crate::AppError::from(e).into_response(),
    }
}

/// The irreversible half of the deck: remove one owned file, guarded.
async fn delete_owned(app: &AppState, user_id: uuid::Uuid, id: &str) -> Json<SwipeResult> {
    let Ok(uuid) = uuid::Uuid::parse_str(id) else {
        return Json(fail_delete("invalid track id"));
    };
    let abs = match mh_db::discovery::track_abs_path(&app.pool, uuid).await {
        Ok(Some(p)) => p,
        Ok(None) => return Json(fail_delete("no such track")),
        Err(e) => return Json(fail_delete(&e.to_string())),
    };
    let path = match guard_delete(&abs, &app.config.music_root, &app.config.discovery_prefix) {
        Ok(p) => p,
        Err(reason) => {
            tracing::warn!(track = %id, path = %abs, %reason, "discovery delete refused");
            return Json(fail_delete(&reason));
        }
    };
    // The audit line is the only record the file existed; written before removal.
    tracing::info!(track = %id, path = %path.display(), "discovery delete");
    if let Err(e) = tokio::fs::remove_file(&path).await {
        tracing::error!(track = %id, path = %path.display(), error = %e, "discovery delete failed");
        return Json(fail_delete(&format!(
            "could not delete {}: {e}",
            path.display()
        )));
    }
    if let Err(e) = mh_db::discovery::judge(&app.pool, user_id, id, false).await {
        tracing::warn!(track = %id, error = %e, "discovery could not record dislike");
    }
    let message = match crate::scan::scan(app).await {
        Ok(_) => "deleted".to_owned(),
        Err(e) => {
            tracing::warn!(error = %e, "discovery rescan after delete failed");
            "deleted, but the library rescan could not be triggered - it may take a few minutes to disappear".to_owned()
        }
    };
    Json(SwipeResult {
        ok: true,
        action: "deleted".to_owned(),
        message,
    })
}

/// `DELETE /api/library/song/{id}` — remove a track the caller downloaded.
///
/// Guarded to the caller's own downloads folder, so a shared-library file or
/// another user's download comes back 403 and is never touched. This is the Go
/// handler the Rust cutover dropped - which is why deleting a song answered 405.
pub async fn delete_song(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let Ok(uuid) = uuid::Uuid::parse_str(&id) else {
        return err(StatusCode::NOT_FOUND, "no such track");
    };
    let abs = match mh_db::discovery::track_abs_path(&app.pool, uuid).await {
        Ok(Some(p)) => p,
        Ok(None) => return err(StatusCode::NOT_FOUND, "no such track"),
        Err(e) => return crate::AppError::from(e).into_response(),
    };
    // The caller may only delete inside their own downloads; the same symlink and
    // regular-file safety as the discovery guard, scoped per user.
    let prefix = format!("users/{}/downloads/", session.username);
    let path = match guard_delete(&abs, &app.config.music_root, &prefix) {
        Ok(p) => p,
        Err(reason) => {
            tracing::warn!(track = %id, user = %session.username, %reason, "library delete refused");
            return err(StatusCode::FORBIDDEN, "that track is not in your downloads");
        }
    };
    tracing::info!(track = %id, user = %session.username, path = %path.display(), "library delete");
    if let Err(e) = tokio::fs::remove_file(&path).await {
        tracing::error!(track = %id, error = %e, "library delete failed");
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not delete the file",
        );
    }
    // Best-effort rescan so the row disappears now rather than on the next sweep.
    if let Err(e) = crate::scan::scan(&app).await {
        tracing::warn!(error = %e, "rescan after delete failed");
    }
    Json(json!({ "deleted": id })).into_response()
}

// ------------------------------------------------------------- owned tracks

async fn owned_cards(app: &AppState, judged: &HashSet<String>, limit: usize) -> Vec<DiscoveryCard> {
    let cap = i64::try_from(limit.saturating_mul(4)).unwrap_or(i64::MAX);
    let tracks =
        match mh_db::discovery::owned_under_prefix(&app.pool, &app.discovery.abs_prefix(), cap)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "discovery owned query");
                return Vec::new();
            }
        };
    let mut cards = Vec::with_capacity(tracks.len());
    for t in tracks {
        let id = t.id.to_string();
        if judged.contains(&id) {
            continue;
        }
        let genres = mh_db::genre::genres_for(&app.pool, t.id)
            .await
            .unwrap_or_default();
        let cover_url = if t.has_cover {
            format!("/api/cover/{id}")
        } else {
            String::new()
        };
        cards.push(DiscoveryCard {
            kind: "owned".to_owned(),
            id: id.clone(),
            title: t.title,
            artist: t.artist,
            album: t.album,
            duration: t.duration_sec,
            cover_url,
            preview_url: format!("/api/discovery/preview/owned/{id}"),
            reason: "downloaded for you".to_owned(),
            genres: cap3(genres),
        });
    }
    let mut rng = rand::thread_rng();
    cards.shuffle(&mut rng);
    cards.truncate(limit);
    cards
}

// --------------------------------------------------------------- candidates

struct Suggestion {
    artist: String,
    title: String,
    album: String,
    dur: i32,
    reason: String,
}

/// Suggest music that is not in the library at all: ListenBrainz weekly
/// exploration first, Last.fm similar-artists top-up. Returns the cards and, on
/// total failure, a short reason for the UI.
async fn candidate_cards(
    app: &AppState,
    session: &Session,
    judged: &HashSet<String>,
    want: usize,
) -> (Vec<DiscoveryCard>, Option<String>) {
    if want == 0 {
        return (Vec::new(), None);
    }
    let lib: LibraryNames = mh_db::discovery::library_names(&app.pool)
        .await
        .unwrap_or_default()
        .into();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<DiscoveryCard> = Vec::with_capacity(want);
    let mut problems: Vec<String> = Vec::new();

    let lb_user = lb_user(app, session).await;
    match listenbrainz_suggestions(&app.discovery.http, &lb_user).await {
        Ok(sugg) => {
            let cards = resolve_candidates(app, judged, &mut seen, sugg, want, &lib).await;
            out.extend(cards);
        }
        Err(e) => problems.push(e),
    }

    if out.len() < want {
        match lastfm_suggestions(app, session, want - out.len(), &lib).await {
            Ok(sugg) => {
                let remaining = want - out.len();
                let cards = resolve_candidates(app, judged, &mut seen, sugg, remaining, &lib).await;
                out.extend(cards);
            }
            Err(e) => problems.push(e),
        }
    }

    if out.is_empty() && !problems.is_empty() {
        return (out, Some(short_reason(&problems)));
    }
    (out, None)
}

async fn resolve_candidates(
    app: &AppState,
    judged: &HashSet<String>,
    seen: &mut HashSet<String>,
    mut pool: Vec<Suggestion>,
    want: usize,
    lib: &LibraryNames,
) -> Vec<DiscoveryCard> {
    if want == 0 || pool.is_empty() {
        return Vec::new();
    }
    // Over-fetch: library hits, judged tracks and dead searches thin this out.
    if pool.len() > want + 8 {
        pool.truncate(want + 8);
    }

    let mut cards = Vec::new();
    for s in pool {
        if cards.len() >= want {
            break;
        }
        if library_has_track(lib, &s.artist, &s.title) {
            continue;
        }
        let Ok(hits) =
            mh_media::acquire::search_source(&format!("{} {}", s.artist, s.title), 3).await
        else {
            continue;
        };
        let Some(hit) = pick_hit(&hits) else { continue };
        if judged.contains(&hit.source_id) || seen.contains(&hit.source_id) {
            continue;
        }
        seen.insert(hit.source_id.clone());
        let dur = if s.dur == 0 && hit.duration > 0 {
            hit.duration
        } else {
            s.dur
        };
        let genres = mh_db::genre::artist_genres(&app.pool, &s.artist)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        cards.push(DiscoveryCard {
            kind: "candidate".to_owned(),
            id: hit.source_id.clone(),
            title: s.title,
            artist: s.artist,
            album: s.album,
            duration: dur,
            cover_url: hit.thumb.clone(),
            preview_url: format!("/api/discovery/preview/candidate/{}", hit.source_id),
            reason: s.reason,
            genres: cap3(genres),
        });
    }
    cards
}

// ------------------------------------------------------------ listenbrainz

async fn lb_user(app: &AppState, session: &Session) -> String {
    if let Ok(Some(u)) = mh_db::discovery::listenbrainz_user(&app.pool, session.user_id).await {
        if !u.trim().is_empty() {
            return u;
        }
    }
    app.config
        .server_listenbrainz_user(&session.username)
        .to_owned()
}

async fn listenbrainz_suggestions(
    http: &reqwest::Client,
    lb_user: &str,
) -> Result<Vec<Suggestion>, String> {
    if lb_user.is_empty() {
        return Err("LISTENBRAINZ_USER is not set".to_owned());
    }
    let listing = lb_get(
        http,
        &format!(
            "{LB_BASE}/1/user/{}/playlists/createdfor?count=10",
            enc(lb_user)
        ),
    )
    .await
    .map_err(|e| format!("listenbrainz playlists: {e}"))?;

    let mut mbids: Vec<(String, String)> = Vec::new();
    if let Some(items) = listing["playlists"].as_array() {
        for item in items {
            let pl = &item["playlist"];
            let title = pl["title"].as_str().unwrap_or("");
            if !title.starts_with(LB_WEEKLY_PREFIX) {
                continue;
            }
            let ident = pl["identifier"].as_str().unwrap_or("");
            let id = ident.rsplit('/').next().unwrap_or("");
            if !is_mbid(id) {
                continue;
            }
            let week = pl["date"].as_str().unwrap_or("").chars().take(10).collect();
            mbids.push((id.to_owned(), week));
        }
    }
    if mbids.is_empty() {
        return Err("listenbrainz has no weekly exploration playlist".to_owned());
    }
    mbids.truncate(2);

    let mut seen: HashSet<String> = HashSet::new();
    let mut pool = Vec::new();
    for (mbid, week) in mbids {
        let Ok(body) = lb_get(http, &format!("{LB_BASE}/1/playlist/{mbid}")).await else {
            continue;
        };
        let Some(tracks) = body["playlist"]["track"].as_array() else {
            continue;
        };
        for t in tracks {
            let artist = t["creator"].as_str().unwrap_or("").trim().to_owned();
            let title = t["title"].as_str().unwrap_or("").trim().to_owned();
            if artist.is_empty() || title.is_empty() {
                continue;
            }
            let key = format!("{}\u{0}{}", norm(&artist), norm(&title));
            if !seen.insert(key) {
                continue;
            }
            pool.push(Suggestion {
                dur: i32::try_from(
                    (t["duration"].as_i64().unwrap_or(0) / 1000).clamp(0, i64::from(i32::MAX)),
                )
                .unwrap_or(0),
                album: t["album"].as_str().unwrap_or("").trim().to_owned(),
                reason: format!("ListenBrainz Weekly Exploration, week of {week}"),
                artist,
                title,
            });
        }
    }
    if pool.is_empty() {
        return Err("listenbrainz weekly exploration was empty".to_owned());
    }
    Ok(pool)
}

/// One fresh release from ListenBrainz, pre-filtered and cover-enriched.
pub(crate) struct Release {
    pub artist: String,
    pub title: String,
    pub date: String,
    pub kind: String,
    pub cover: String,
    pub mbid: String,
    pub confidence: i64,
    /// Track titles when the source knows them. Empty means "no tracklist",
    /// which is not "an album with no tracks": ownership then falls back to
    /// comparing the release title, since there is nothing finer to compare.
    pub tracks: Vec<String>,
    /// Which source produced this row; empty means MusicBrainz/ListenBrainz.
    /// Carried through to the client so a YouTube album whose date could not be
    /// established is labelled as such instead of being given a date it lacks.
    pub source: String,
    /// How many tracks the release holds, when the source knows. Equal to
    /// `tracks.len()` after a resolve; a cached YouTube row keeps the count
    /// without the tracklist, and album ownership is judged by the count.
    pub track_count: usize,
    /// The date is the playlist's own `modified_date` rather than a release date,
    /// so the row must not be sorted into a week on it.
    pub date_uncertain: bool,
}

/// Recent releases from artists the user listens to, via ListenBrainz.
///
/// Drops live/bootleg/compilation noise and weak-confidence matches; newest first.
async fn fresh_releases(
    http: &reqwest::Client,
    lb_user: &str,
    days: u32,
) -> Result<Vec<Release>, String> {
    let url = format!(
        "{LB_BASE}/1/user/{}/fresh_releases?days={days}&future=false",
        enc(lb_user)
    );
    let body = lb_get(http, &url)
        .await
        .map_err(|e| format!("listenbrainz fresh releases: {e}"))?;
    let items = body["payload"]["releases"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let parsed = parse_fresh(&items);
    tracing::info!(user = %lb_user, raw = items.len(), kept = parsed.len(), "new-releases: listenbrainz parsed");
    Ok(parsed)
}

/// yt-dlp resolves are heavy; space them so the nightly sweep never spikes.
const RESOLVE_RATE: Duration = Duration::from_secs(2);
/// Gap between YouTube channel reads. Each is a yt-dlp invocation, far heavier
/// than a MusicBrainz request, so they are spaced to keep a 50-artist nightly
/// sweep from hammering YouTube.
const YT_RATE: Duration = Duration::from_secs(3);

/// Spawn the nightly "new releases" precompute.
///
/// The live path runs yt-dlp once per release to resolve a preview - seconds of
/// wait the first time a user opens Discover. This rebuilds `new_release_cache`
/// so the morning list is instant and the first preview tap plays with no live
/// resolve. Warms once shortly after startup (a fresh deploy is instant too),
/// then runs nightly at 03:00 UTC.
pub fn run_precompute(pool: mh_db::Pool, discovery: Arc<Discovery>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            match precompute_once(&pool, &discovery).await {
                Ok(n) => tracing::info!(users = n, "new-releases precompute"),
                Err(e) => tracing::warn!(error = %e, "new-releases precompute"),
            }
            tokio::time::sleep(until_next_run()).await;
        }
    });
}

/// One sweep: rebuild every configured user's cached new releases.
async fn precompute_once(
    pool: &mh_db::Pool,
    discovery: &Discovery,
) -> Result<usize, mh_db::DbError> {
    let users = mh_db::discovery::all_users_with_lb(pool).await?;
    let since = new_release_since();
    let today = today_str();
    let mut done = 0;
    for (user_id, lb) in users {
        let judged = mh_db::discovery::judged(pool, user_id)
            .await
            .unwrap_or_default();
        // Per user, not global: what one listener owns must not suppress the
        // same release for another. The shared library (blank owner) counts for
        // everyone.
        let owned = mh_db::discovery::owned_tracks(pool, user_id)
            .await
            .unwrap_or_default();
        let owned_albums = mh_db::discovery::owned_albums(pool, user_id)
            .await
            .unwrap_or_default();

        // Two sources, merged: ListenBrainz fresh releases (only for a user who
        // scrobbles) and recent MusicBrainz releases from the artists this user
        // actually plays - which is what makes the list work without ListenBrainz
        // and why an account that never scrobbled still gets a list.
        let mut releases: Vec<Release> = Vec::new();
        let mut lb_kept = 0usize;
        if lb.trim().is_empty() {
            tracing::info!("new-releases: no ListenBrainz user configured");
        } else {
            match fresh_releases(&discovery.http, &lb, LB_FRESH_DAYS).await {
                Ok(r) => {
                    lb_kept = r.len();
                    releases.extend(r);
                }
                Err(e) => {
                    // Swallowing this used to make a dead ListenBrainz source
                    // look identical to "this user simply has no new music".
                    tracing::warn!(user = %lb, error = %e, "new-releases: listenbrainz fetch failed");
                }
            }
        }
        for artist in top_artists(pool, user_id).await {
            tokio::time::sleep(MB_RATE).await;
            releases.extend(artist_new_releases(&discovery.http, &artist, &since, &today).await);
        }

        // Third source: the artists' own YouTube channels. MusicBrainz and
        // ListenBrainz only see a record once a distributor has it, so anything
        // released on the artist's channel alone is otherwise invisible - which is
        // exactly how a six-song album can go missing while its singles appear.
        // Merged into the same list rather than shown separately: the point is
        // that the listener does not have to care where a release came from.
        let mut yt_kept = 0usize;
        // Read once per sweep and shared by every user: a playlist's date is a
        // property of the playlist, not of who is looking at it.
        let checked = mh_db::discovery::checked_playlists(pool)
            .await
            .unwrap_or_default();
        for artist in top_artists(pool, user_id).await {
            tokio::time::sleep(YT_RATE).await;
            let found =
                crate::youtube::artist_releases(pool, user_id, &artist, &since, &checked).await;
            yt_kept += found.len();
            releases.extend(found);
        }

        // Newest first, then de-duplicate by normalised artist+title. A row whose
        // date is uncertain sorts *below* every dated row whatever its date says:
        // that date is the playlist's last-modified stamp, so sorting by it would
        // file an older album into this week on the strength of a later edit.
        let before_dedupe = releases.len();
        releases.sort_by(release_order);
        let mut seen: HashSet<String> = HashSet::new();
        releases.retain(|r| seen.insert(format!("{}\u{0}{}", norm(&r.artist), norm(&r.title))));
        let dupe_dropped = before_dedupe - releases.len();

        let mut cached = Vec::new();
        let mut owned_dropped = 0usize;
        let mut judged_dropped = 0usize;
        for r in releases {
            // A cached YouTube row has no tracklist but does know its track
            // count, so the album-name rule runs on every night, not only on
            // the night the playlist was first resolved.
            let have_it = if r.tracks.is_empty() {
                owned_track(&owned, &r.artist, &r.title)
                    || release_owned_by_album(&owned_albums, &r.artist, &r.title, r.track_count)
            } else {
                release_fully_owned(&owned, &r.artist, &r.tracks)
                    || release_owned_by_album(&owned_albums, &r.artist, &r.title, r.track_count)
            };
            if have_it {
                owned_dropped += 1;
                continue;
            }
            if judged.contains(&release_key(&r.mbid, &r.artist, &r.title)) {
                judged_dropped += 1;
                continue;
            }
            // Resolve the preview source now, so the morning tap is instant.
            tokio::time::sleep(RESOLVE_RATE).await;
            let source_id =
                mh_media::acquire::search_source(&format!("{} {}", r.artist, r.title), 3)
                    .await
                    .ok()
                    .and_then(|hits| pick_hit(&hits))
                    .map(|h| h.source_id)
                    .unwrap_or_default();
            let position = i32::try_from(cached.len()).unwrap_or(i32::MAX);
            cached.push(mh_db::discovery::CachedRelease {
                mbid: r.mbid,
                artist: r.artist,
                title: r.title,
                kind: r.kind,
                cover_url: r.cover,
                release_date: r.date,
                source_id,
                position,
                source: r.source,
                date_uncertain: r.date_uncertain,
            });
        }
        mh_db::discovery::replace_new_releases(pool, user_id, &cached).await?;
        // Every stage that can drop a row, counted. A silently short list used
        // to be indistinguishable from a user who simply has no new music.
        tracing::info!(
            user = %lb,
            lb_kept,
            yt_kept,
            dupes_dropped = dupe_dropped,
            owned_dropped,
            judged_dropped,
            cached = cached.len(),
            "new-releases: sweep summary"
        );
        done += 1;
    }
    Ok(done)
}

/// The user's most-played artists, reduced to lead names and de-duplicated, for
/// the MusicBrainz sweep. Capped at 50 so one nightly sweep stays bounded while
/// still reaching well past the handful of top favourites.
async fn top_artists(pool: &mh_db::Pool, user_id: uuid::Uuid) -> Vec<String> {
    let raw = mh_db::discovery::seed_artists(pool, user_id, 500)
        .await
        .unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for credit in raw {
        let lead = crate::genre::primary_artist(&credit);
        if lead.is_empty() {
            continue;
        }
        if seen.insert(norm(&lead)) {
            out.push(lead);
            if out.len() >= 50 {
                break;
            }
        }
    }
    out
}

/// Recent release-groups from MusicBrainz for one artist (studio releases only).
async fn artist_new_releases(
    http: &reqwest::Client,
    artist: &str,
    since: &str,
    today: &str,
) -> Vec<Release> {
    let query = format!(
        "artist:\"{}\" AND firstreleasedate:[{since} TO *]",
        lucene_escape(artist)
    );
    let resp = http
        .get(format!("{MB_BASE}/release-group"))
        .query(&[("query", query.as_str()), ("fmt", "json"), ("limit", "25")])
        .header(header::USER_AGENT, MB_USER_AGENT)
        .send()
        .await;
    let Ok(resp) = resp else {
        return Vec::new();
    };
    let Ok(body) = resp.json::<Value>().await else {
        return Vec::new();
    };
    parse_release_groups(&body, artist, since, today)
}

/// Shape MusicBrainz release-groups into [`Release`]s: music types only (no
/// spoken word or audiobooks), the credited lead must be the artist searched, and
/// the date must be out already, not in the future, and within the window.
fn parse_release_groups(body: &Value, artist: &str, since: &str, today: &str) -> Vec<Release> {
    let groups = body["release-groups"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let want = norm(artist);
    let mut out = Vec::new();
    for g in &groups {
        // Music release types only: an "Other"/"Broadcast" group is usually a
        // spoken-word piece or an audiobook, not a record.
        let primary = g["primary-type"].as_str().unwrap_or("");
        if !matches!(primary, "Album" | "Single" | "EP") {
            continue;
        }
        let secondary: Vec<&str> = g["secondary-types"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if secondary.iter().any(|s| {
            matches!(
                *s,
                "Live"
                    | "Compilation"
                    | "Demo"
                    | "Bootleg"
                    | "Interview"
                    | "Soundtrack"
                    | "Remix"
                    | "DJ-mix"
                    | "Mixtape/Street"
                    | "Audiobook"
                    | "Audio drama"
                    | "Spokenword"
                    | "Field recording"
            )
        }) {
            continue;
        }
        // Out already, not announced for the future, and inside the window.
        let date = g["first-release-date"].as_str().unwrap_or("");
        if date.is_empty() || date < since || date > today {
            continue;
        }
        let credit = g["artist-credit"][0]["name"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_owned();
        let title = g["title"].as_str().unwrap_or("").trim().to_owned();
        // Exact lead-artist match: the loose containment test let "Doktor Froid"
        // through on a search for "Froid".
        if title.is_empty() || norm(&crate::genre::primary_artist(&credit)) != want {
            continue;
        }
        let mbid = g["id"].as_str().unwrap_or("").to_owned();
        let cover = if mbid.is_empty() {
            String::new()
        } else {
            format!("https://coverartarchive.org/release-group/{mbid}/front-250")
        };
        out.push(Release {
            artist: credit,
            title,
            date: date.to_owned(),
            kind: primary.to_owned(),
            cover,
            mbid,
            confidence: 100,
            // MusicBrainz is queried per release-group and returns no tracklist
            // here, so ownership falls back to comparing the release title.
            tracks: Vec::new(),
            track_count: 0,
            source: String::new(),
            date_uncertain: false,
        });
    }
    out
}

/// The earliest date a release still counts as "new": today minus [`NEW_RELEASE_DAYS`].
fn new_release_since() -> String {
    (chrono::Utc::now() - chrono::Duration::days(NEW_RELEASE_DAYS))
        .format("%Y-%m-%d")
        .to_string()
}

/// Today as `YYYY-MM-DD`: the upper bound, so an announced-but-unreleased album
/// never shows up with a future date.
fn today_str() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Neutralise Lucene metacharacters in a value going into a quoted query term.
fn lucene_escape(s: &str) -> String {
    s.replace(['"', '\\'], " ").trim().to_owned()
}

/// Seconds until the next 03:00 UTC, so the sweep runs at night.
fn until_next_run() -> Duration {
    const DAY: u64 = 86_400;
    const TARGET: u64 = 3 * 3600; // 03:00 UTC
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let into_day = now % DAY;
    let delay = if into_day < TARGET {
        TARGET - into_day
    } else {
        DAY - into_day + TARGET
    };
    Duration::from_secs(delay)
}

/// Filter and shape the fresh-releases array: drop live/bootleg/compilation
/// noise and weak-confidence matches, cover-enrich, newest first.
fn parse_fresh(items: &[Value]) -> Vec<Release> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for r in items {
        let secondary = r["release_group_secondary_type"].as_str().unwrap_or("");
        if matches!(
            secondary,
            "Live" | "Compilation" | "Demo" | "Bootleg" | "Interview" | "Soundtrack" | "Remix"
        ) {
            continue;
        }
        let confidence = r["confidence"].as_i64().unwrap_or(0);
        if confidence < 10 {
            continue;
        }
        let artist = r["artist_credit_name"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_owned();
        let title = r["release_name"].as_str().unwrap_or("").trim().to_owned();
        if artist.is_empty() || title.is_empty() {
            continue;
        }
        if !seen.insert(format!("{}\u{0}{}", norm(&artist), norm(&title))) {
            continue;
        }
        let caa_mbid = r["caa_release_mbid"].as_str().unwrap_or("");
        // A release with no Cover Art Archive entry of its own still belongs to a
        // release group that usually does, so fall back to the group's front cover -
        // the same URL the MusicBrainz path builds. Without this a ListenBrainz row
        // whose release has no CAA entry renders with no artwork at all.
        let rg_mbid = r["release_group_mbid"].as_str().unwrap_or("");
        let cover = match (r["caa_id"].as_i64(), caa_mbid.is_empty()) {
            (Some(id), false) => format!(
                "https://archive.org/download/mbid-{caa_mbid}/mbid-{caa_mbid}-{id}_thumb250.jpg"
            ),
            _ if !rg_mbid.is_empty() => {
                format!("https://coverartarchive.org/release-group/{rg_mbid}/front-250")
            }
            _ => String::new(),
        };
        out.push(Release {
            artist,
            title,
            date: r["release_date"].as_str().unwrap_or("").to_owned(),
            kind: r["release_group_primary_type"]
                .as_str()
                .unwrap_or("Album")
                .to_owned(),
            cover,
            mbid: r["release_mbid"].as_str().unwrap_or("").to_owned(),
            confidence,
            // ListenBrainz names the release, not its tracks. Left empty so the
            // ownership check falls back to the release title - seeding it with
            // that same title would only fake a tracklist that does not exist.
            tracks: Vec::new(),
            track_count: 0,
            source: String::new(),
            date_uncertain: false,
        });
    }
    out.sort_by(|a, b| b.date.cmp(&a.date).then(b.confidence.cmp(&a.confidence)));
    out
}

/// Whether this user already has every track of a release, so it should not be
/// offered again.
///
/// Per track, not per album. Album-title matching was wrong twice over: a single
/// downloaded on its own carries no album name, so it never matched and the same
/// song came back every sweep; and an album released after one of its singles
/// would have been suppressed wholesale by that single's album name, hiding the
/// tracks the listener does not have. A release is only "owned" when the user
/// has all of it; a partial one stays visible so the rest can be fetched.
fn release_fully_owned(owned: &[(String, String)], artist: &str, tracks: &[String]) -> bool {
    !tracks.is_empty()
        && tracks.iter().all(|t| {
            let want = norm(t);
            owned
                .iter()
                .any(|(a, ti)| norm(ti) == want && same_artist(a, artist))
        })
}

/// Whether this user already has a track with exactly this title by this artist.
///
/// The single-download case: a track fetched on its own carries no album, so the
/// release it belongs to is recognised by its title matching a track the user
/// owns, rather than by an album name that was never written.
fn owned_track(owned: &[(String, String)], artist: &str, title: &str) -> bool {
    let want = norm(title);
    owned
        .iter()
        .any(|(a, ti)| norm(ti) == want && same_artist(a, artist))
}

/// Whether this user already owns the record by name.
///
/// Track-title matching fails on records the user has in full:
/// YouTube writes some titles as `Artist - Song` and appends
/// `(Official Music Video)`, so a track the user owns can carry a
/// title the release's own tracklist never uses. The album name is
/// the stable half of the pair - it is what the user's library calls
/// the record, and what the release is called.
///
/// The count is what keeps a partly-owned album visible: owning one track
/// of eight is not owning the record, so the user must own at least as many
/// tracks from that album as the release has. A release whose track count is
/// unknown (0 - MusicBrainz and ListenBrainz rows) cannot be judged this way
/// and returns false, leaving the title comparison to decide.
fn release_owned_by_album(
    owned_albums: &[(String, String, usize)],
    artist: &str,
    title: &str,
    track_count: usize,
) -> bool {
    if track_count == 0 {
        return false;
    }
    let want = norm(title);
    owned_albums
        .iter()
        .any(|(a, al, n)| norm(al) == want && same_artist(a, artist) && *n >= track_count)
}

/// The sweep's ordering before de-duplication: dated rows first, newest first,
/// then the more confident match.
///
/// De-duplication keeps the first row of each artist+title, so this order also
/// decides which source's copy survives. An uncertain date is the playlist's
/// last-modified stamp, not a release date, so it must lose to a real date. The
/// comparator used to read `b.date_uncertain.cmp(&a.date_uncertain)`, which put
/// every uncertain row first - live, all 175 uncertain rows sat at positions
/// 0-174 - so an undated YouTube copy beat a dated MusicBrainz copy of the same
/// record.
fn release_order(a: &Release, b: &Release) -> std::cmp::Ordering {
    a.date_uncertain
        .cmp(&b.date_uncertain)
        .then(b.date.cmp(&a.date))
        .then(b.confidence.cmp(&a.confidence))
}

/// A stable id for a release in the `judged` set: the MusicBrainz release id when
/// present, else a normalised artist+title so a dismissal survives across sweeps.
fn release_key(mbid: &str, artist: &str, title: &str) -> String {
    if mbid.is_empty() {
        format!("release:{}\u{0}{}", norm(artist), norm(title))
    } else {
        format!("release:{mbid}")
    }
}

async fn lb_get(http: &reqwest::Client, url: &str) -> Result<Value, String> {
    http.get(url)
        .header(header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Value>()
        .await
        .map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ last.fm

async fn lastfm_suggestions(
    app: &AppState,
    session: &Session,
    want: usize,
    lib: &LibraryNames,
) -> Result<Vec<Suggestion>, String> {
    if app.config.lastfm_api_key.is_empty() {
        return Err("LASTFM_API_KEY is not set".to_owned());
    }
    let seeds = mh_db::discovery::seed_artists(&app.pool, session.user_id, SEED_ARTISTS)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|a| crate::genre::primary_artist(&a))
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>();
    if seeds.is_empty() {
        return Err("no listening history to suggest from".to_owned());
    }

    // Similar artists per seed, round-robined so one prolific seed cannot own it.
    let mut per_seed: Vec<(String, Vec<String>)> = Vec::new();
    for seed in &seeds {
        if let Ok(names) =
            similar_artists(&app.discovery.http, &app.config.lastfm_api_key, seed).await
        {
            if !names.is_empty() {
                per_seed.push((seed.clone(), names));
            }
        }
    }
    if per_seed.is_empty() {
        return Err("last.fm returned no similar artists".to_owned());
    }
    let mut origin: Vec<(String, String)> = Vec::new(); // (name, seed)
    let mut seen_names: HashSet<String> = HashSet::new();
    for round in 0.. {
        let mut added = false;
        for (seed, names) in &per_seed {
            if let Some(name) = names.get(round) {
                if seen_names.insert(name.to_lowercase()) {
                    origin.push((name.clone(), seed.clone()));
                    added = true;
                }
            }
        }
        if !added || origin.len() >= 40 {
            break;
        }
    }

    // Drop artists already in the library, then two top tracks each.
    let mut pool = Vec::with_capacity(want * 3);
    for (name, seed) in origin {
        if pool.len() >= want * 3 {
            break;
        }
        if library_has_artist(lib, &name) {
            continue;
        }
        let Ok(tracks) = top_tracks(&app.discovery.http, &app.config.lastfm_api_key, &name).await
        else {
            continue;
        };
        for (title, dur) in tracks.into_iter().take(2) {
            pool.push(Suggestion {
                artist: name.clone(),
                title,
                album: String::new(),
                dur,
                reason: format!("because you listen to {seed}"),
            });
        }
    }
    if pool.is_empty() {
        return Err("last.fm returned no playable suggestions".to_owned());
    }
    Ok(pool)
}

async fn similar_artists(
    http: &reqwest::Client,
    key: &str,
    artist: &str,
) -> Result<Vec<String>, String> {
    let body = lastfm(
        http,
        key,
        "artist.getsimilar",
        &[("artist", artist), ("limit", "15"), ("autocorrect", "1")],
    )
    .await?;
    Ok(flex_list(&body["similarartists"]["artist"])
        .iter()
        .filter_map(|a| {
            a["name"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .collect())
}

async fn top_tracks(
    http: &reqwest::Client,
    key: &str,
    artist: &str,
) -> Result<Vec<(String, i32)>, String> {
    let body = lastfm(
        http,
        key,
        "artist.gettoptracks",
        &[("artist", artist), ("limit", "5"), ("autocorrect", "1")],
    )
    .await?;
    Ok(flex_list(&body["toptracks"]["track"])
        .iter()
        .filter_map(|t| {
            let name = t["name"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            Some((name.to_owned(), lfm_int(&t["duration"])))
        })
        .collect())
}

async fn lastfm(
    http: &reqwest::Client,
    key: &str,
    method: &str,
    params: &[(&str, &str)],
) -> Result<Value, String> {
    let mut url = format!("{LASTFM_BASE}?method={method}&api_key={key}&format=json");
    for (k, v) in params {
        url.push('&');
        url.push_str(k);
        url.push('=');
        url.push_str(&enc(v));
    }
    let body = http
        .get(&url)
        .header(header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Value>()
        .await
        .map_err(|e| e.to_string())?;
    // Last.fm reports API errors in the body, sometimes with HTTP 200.
    if let Some(code) = body["error"].as_i64() {
        if code != 0 {
            return Err(format!(
                "last.fm {method}: {} (code {code})",
                body["message"].as_str().unwrap_or("")
            ));
        }
    }
    Ok(body)
}

// ---------------------------------------------------------- the delete guard

/// Map an owned track's absolute path to the one path we may unlink, or refuse.
///
/// Ported from the Go `internal/safepath.Resolve` rules: the path must sit under
/// the music root and under the discovery prefix (string tests first, before any
/// filesystem call); the parent, resolved through symlinks, must stay under the
/// resolved prefix root (defeats a symlinked directory); and only a regular
/// non-symlink file is ever returned.
fn guard_delete(
    abs_path: &str,
    music_root: &str,
    discovery_prefix: &str,
) -> Result<PathBuf, String> {
    let root = Path::new(music_root);
    let abs = Path::new(abs_path);

    // 1. Under the music root — also blocks a "../.." climbing out.
    let rel = abs
        .strip_prefix(root)
        .map_err(|_| format!("{abs_path:?} is not under {music_root:?}; refusing to delete"))?;
    let rel = rel.to_string_lossy().replace('\\', "/");

    // 2. Under the discovery prefix, on a directory boundary (prefix ends "/").
    if !rel.starts_with(discovery_prefix) {
        return Err(format!(
            "{rel:?} is outside {discovery_prefix}; refusing to delete"
        ));
    }

    // 3. Resolve the prefix root and the parent through symlinks, and require the
    //    parent to stay inside — a symlinked dir under explo/ would escape.
    let guard = root.join(discovery_prefix.trim_end_matches('/'));
    let real_guard = std::fs::canonicalize(&guard)
        .map_err(|e| format!("cannot resolve {}: {e}", guard.display()))?;
    let parent = abs
        .parent()
        .ok_or_else(|| format!("{abs_path:?} has no parent; refusing to delete"))?;
    let real_parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("cannot resolve {}: {e}", parent.display()))?;
    if real_parent != real_guard && !real_parent.starts_with(&real_guard) {
        return Err(format!(
            "{abs_path:?} resolves to {}, outside {}; refusing to delete",
            real_parent.display(),
            real_guard.display()
        ));
    }
    let name = abs
        .file_name()
        .ok_or_else(|| format!("{abs_path:?} has no file name; refusing to delete"))?;
    let resolved = real_parent.join(name);

    // 4. Only a regular, non-symlink file is ever removed.
    let meta = std::fs::symlink_metadata(&resolved)
        .map_err(|e| format!("cannot stat {}: {e}", resolved.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink; refusing to delete",
            resolved.display()
        ));
    }
    if !meta.is_file() {
        return Err(format!(
            "{} is not a regular file; refusing to delete",
            resolved.display()
        ));
    }
    Ok(resolved)
}

// --------------------------------------------------------------------- pure

/// Alternate the two kinds so the first screen shows both, then top up.
fn interleave(owned: &[DiscoveryCard], cands: &[DiscoveryCard], n: usize) -> Vec<DiscoveryCard> {
    let mut take_o = owned.len().min(n / 2);
    let take_c = cands.len().min(n - take_o);
    if take_o + take_c < n {
        take_o = owned.len().min(n - take_c);
    }
    let mut out = Vec::with_capacity(take_o + take_c);
    for i in 0..take_o.max(take_c) {
        if i < take_o {
            out.push(owned[i].clone());
        }
        if i < take_c {
            out.push(cands[i].clone());
        }
    }
    out
}

/// Drop the hour-long mixes and ten-second clips a text search returns.
fn pick_hit(hits: &[Acquire]) -> Option<Acquire> {
    hits.iter()
        .find(|h| h.duration == 0 || (h.duration >= 45 && h.duration <= 15 * 60))
        .cloned()
}

#[derive(Default)]
struct LibraryNames {
    artists: Vec<String>,
    tracks: Vec<(String, String)>,
}

impl From<(Vec<String>, Vec<(String, String)>)> for LibraryNames {
    fn from((artists, tracks): (Vec<String>, Vec<(String, String)>)) -> Self {
        Self { artists, tracks }
    }
}

fn library_has_artist(lib: &LibraryNames, name: &str) -> bool {
    let want = norm(name);
    lib.artists.iter().any(|a| norm(a) == want)
}

fn library_has_track(lib: &LibraryNames, artist: &str, title: &str) -> bool {
    let want = norm(title);
    lib.tracks
        .iter()
        .any(|(t, a)| norm(t) == want && same_artist(a, artist))
}

/// Whether two artist strings name the same act. Containment both ways, floored
/// at four chars so a two-letter band cannot swallow everything.
pub(crate) fn same_artist(a: &str, b: &str) -> bool {
    let (na, nb) = (norm(a), norm(b));
    if na == nb {
        return true;
    }
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    let (short, long) = if na.len() > nb.len() {
        (nb, na)
    } else {
        (na, nb)
    };
    short.len() >= 4 && long.contains(&short)
}

/// Fold a title or artist to a comparable form. Accent folding is load-bearing:
/// "Motörhead" must match "Motorhead", not become "motrhead".
fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push_str(fold_accent(c));
        }
    }
    out
}

/// Map a lowercase accented Latin rune to its ASCII skeleton; everything else to
/// nothing. Covers Latin-1 + Latin Extended-A, the whole of Western music.
const fn fold_accent(c: char) -> &'static str {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'š' | 'ś' | 'ŝ' | 'ş' => "s",
        'ž' | 'ź' | 'ż' => "z",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĺ' | 'ļ' | 'ľ' | 'ł' => "l",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ţ' | 'ť' | 'ŧ' => "t",
        'ď' | 'đ' | 'ð' => "d",
        'ĥ' | 'ħ' => "h",
        'ĵ' => "j",
        'ķ' => "k",
        'ŵ' => "w",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'þ' => "th",
        _ => "",
    }
}

fn short_reason(problems: &[String]) -> String {
    let real: Vec<&str> = problems
        .iter()
        .map(String::as_str)
        .filter(|p| !p.contains("is not set"))
        .collect();
    let msg = if real.is_empty() {
        if problems.iter().all(|p| p.contains("is not set")) {
            "no suggestion source is configured".to_owned()
        } else {
            problems.join("; ")
        }
    } else {
        real.join("; ")
    };
    if msg.chars().count() > 120 {
        msg.chars().take(120).chain("...".chars()).collect()
    } else {
        msg
    }
}

fn cap3(mut genres: Vec<String>) -> Vec<String> {
    genres.truncate(3);
    genres
}

fn flex_list(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

fn lfm_int(v: &Value) -> i32 {
    match v {
        Value::Number(n) => {
            i32::try_from(n.as_i64().unwrap_or(0).clamp(0, i64::from(i32::MAX))).unwrap_or(0)
        }
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn is_mbid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn enc(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

async fn serve_file(
    request: Request,
    path: &Path,
    content_type: Option<&str>,
    cache: &str,
) -> Response {
    let mut response = ServeFile::new(path).oneshot(request).await.map_or_else(
        |_| err(StatusCode::INTERNAL_SERVER_ERROR, "could not serve file"),
        IntoResponse::into_response,
    );
    if let Some(ct) = content_type {
        if let Ok(v) = header::HeaderValue::from_str(ct) {
            response.headers_mut().insert(header::CONTENT_TYPE, v);
        }
    }
    if let Ok(v) = header::HeaderValue::from_str(cache) {
        response.headers_mut().insert(header::CACHE_CONTROL, v);
    }
    response
}

fn swipe(ok: bool, action: &str, message: &str) -> Response {
    Json(SwipeResult {
        ok,
        action: action.to_owned(),
        message: message.to_owned(),
    })
    .into_response()
}

fn fail_delete(message: &str) -> SwipeResult {
    SwipeResult {
        ok: false,
        action: "deleted".to_owned(),
        message: message.to_owned(),
    }
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn card(kind: &str, id: &str) -> DiscoveryCard {
        DiscoveryCard {
            kind: kind.to_owned(),
            id: id.to_owned(),
            ..DiscoveryCard::default()
        }
    }

    #[test]
    fn norm_folds_accents_so_owned_music_is_not_re_offered() {
        assert_eq!(norm("Motörhead"), norm("Motorhead"));
        assert_eq!(norm("Sigur Rós"), "sigurros");
        assert_eq!(norm("Björk"), "bjork");
        assert_eq!(norm("Beyoncé!"), "beyonce");
    }

    #[test]
    fn same_artist_matches_credit_lists_but_not_two_letter_bands() {
        assert!(same_artist("J. Cole", "J. Cole with PJ"));
        assert!(same_artist("Eurythmics", "Eurythmics, Annie Lennox"));
        assert!(!same_artist("U2", "Blur")); // too short to contain
        assert!(!same_artist("", "Drake"));
    }

    #[test]
    fn parse_fresh_drops_live_and_weak_and_dates_newest_first() {
        let items = vec![
            json!({
                "artist_credit_name": "Ye", "release_name": "BULLY", "release_date": "2026-07-02",
                "release_group_primary_type": "Album", "confidence": 16,
                "release_mbid": "aaaaaaaa-2222-3333-4444-555555555555",
                "caa_id": 12345, "caa_release_mbid": "aaaaaaaa-2222-3333-4444-555555555555"
            }),
            json!({
                "artist_credit_name": "Bob Dylan", "release_name": "Live Bootleg",
                "release_date": "2026-06-21", "release_group_primary_type": "Album",
                "release_group_secondary_type": "Live", "confidence": 12
            }),
            json!({
                "artist_credit_name": "Nobody", "release_name": "Faint",
                "release_date": "2026-08-01", "release_group_primary_type": "Single",
                "confidence": 3
            }),
            json!({
                "artist_credit_name": "Central Cee", "release_name": "The Team",
                "release_date": "2026-08-18", "release_group_primary_type": "Single",
                "confidence": 18
            }),
        ];
        let out = parse_fresh(&items);
        let names: Vec<&str> = out.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(names, vec!["The Team", "BULLY"]); // newest first; live + weak dropped
        assert_eq!(out[0].kind, "Single");
        assert!(out[1].cover.contains("archive.org")); // caa-enriched
    }

    fn album(title: &str, artist: &str, tracks: &[&str]) -> AcquireAlbum {
        AcquireAlbum {
            playlist_id: format!("OLAK5uy_{}", norm(title)),
            title: title.to_owned(),
            artist: artist.to_owned(),
            tracks: tracks
                .iter()
                .map(|t| Acquire {
                    source_id: (*t).to_owned(),
                    title: (*t).to_owned(),
                    artist: artist.to_owned(),
                    ..Acquire::default()
                })
                .collect(),
            ..AcquireAlbum::default()
        }
    }

    /// 2026-11-05: a day-old album was not in the search index yet, so the search
    /// returned the artist's older records and the first one was downloaded.
    #[test]
    fn a_release_is_never_substituted_by_another_album_of_the_artist() {
        let older = vec![
            album("Preto & Vermelho", "Plutonio 2765", &["a"]),
            album(
                "Carta de Alforria",
                "Plutonio 2765",
                &["Deserto", "Multiplica"],
            ),
        ];
        assert!(pick_release_album(older, "Plutonio 2765", "VENDETTA: Veni, Vidi, Vici").is_none());
    }

    #[test]
    fn the_release_is_picked_by_title_despite_punctuation_and_accents() {
        let found = vec![
            album("Carta de Alforria", "Plutonio", &["Deserto"]),
            album("Vendetta - Veni Vidi Vici", "Plutónio", &["Thank God"]),
        ];
        let got = pick_release_album(found, "Plutonio 2765", "VENDETTA: Veni, Vidi, Vici")
            .expect("the release itself was found");
        assert_eq!(got.tracks[0].title, "Thank God");
    }

    #[test]
    fn a_same_title_album_by_another_artist_is_not_the_release() {
        let found = vec![album("Vendetta", "Someone Else", &["x"])];
        assert!(pick_release_album(found, "Plutonio 2765", "Vendetta").is_none());
    }

    /// The owned "Multiplica" is credited "Plutonio"; the release credits
    /// "Plutonio 2765". Download dedup must see them as one act.
    #[test]
    fn same_artist_matches_a_name_with_a_suffix() {
        assert!(same_artist("Plutonio", "Plutonio 2765"));
        assert!(same_artist("Plutónio", "Plutonio 2765"));
    }

    #[test]
    fn parse_fresh_falls_back_to_the_release_group_cover_when_the_release_has_none() {
        // A ListenBrainz row whose release has no Cover Art Archive entry of its own
        // still belongs to a release group that usually does. Falling back to the
        // group's front cover is what keeps such a row from rendering blank.
        let items = vec![json!({
            "artist_credit_name": "Various", "release_name": "FOMO (Original Soundtrack)",
            "release_date": "2026-09-01", "release_group_primary_type": "Album",
            "confidence": 20, "release_mbid": "bbbbbbbb-2222-3333-4444-555555555555",
            "release_group_mbid": "cccccccc-2222-3333-4444-555555555555",
            "caa_id": null, "caa_release_mbid": null
        })];
        let out = parse_fresh(&items);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].cover,
            "https://coverartarchive.org/release-group/cccccccc-2222-3333-4444-555555555555/front-250"
        );
    }

    #[test]
    fn parse_fresh_still_prefers_the_release_cover_when_both_exist() {
        // The release-level art is the better picture when it exists, so the
        // fallback must not shadow it.
        let items = vec![json!({
            "artist_credit_name": "Ye", "release_name": "BULLY", "release_date": "2026-07-02",
            "release_group_primary_type": "Album", "confidence": 16,
            "release_mbid": "aaaaaaaa-2222-3333-4444-555555555555",
            "release_group_mbid": "cccccccc-2222-3333-4444-555555555555",
            "caa_id": 12345, "caa_release_mbid": "aaaaaaaa-2222-3333-4444-555555555555"
        })];
        let out = parse_fresh(&items);
        assert!(out[0].cover.contains("archive.org"));
    }

    #[test]
    fn parse_release_groups_keeps_only_real_new_music_by_the_artist() {
        let body = json!({ "release-groups": [
            { "id": "rg1", "title": "New One", "first-release-date": "2026-08-01",
              "primary-type": "Album", "secondary-types": [],
              "artist-credit": [{ "name": "Froid" }] },
            { "id": "rg2", "title": "Live Set", "first-release-date": "2026-08-01",
              "primary-type": "Album", "secondary-types": ["Live"],
              "artist-credit": [{ "name": "Froid" }] },
            { "id": "rg3", "title": "Old One", "first-release-date": "2024-01-01",
              "primary-type": "Album", "secondary-types": [],
              "artist-credit": [{ "name": "Froid" }] },
            { "id": "rg4", "title": "Experiments", "first-release-date": "2026-08-01",
              "primary-type": "Other", "secondary-types": ["Spokenword"],
              "artist-credit": [{ "name": "Doktor Froid" }] },
            { "id": "rg5", "title": "Announced", "first-release-date": "2027-01-01",
              "primary-type": "Album", "secondary-types": [],
              "artist-credit": [{ "name": "Froid" }] },
            { "id": "rg6", "title": "Not Theirs", "first-release-date": "2026-08-01",
              "primary-type": "Album", "secondary-types": [],
              "artist-credit": [{ "name": "Doktor Froid" }] },
        ]});
        let out = parse_release_groups(&body, "Froid", "2026-05-01", "2026-09-21");
        let titles: Vec<&str> = out.iter().map(|r| r.title.as_str()).collect();
        // live, old, spoken-word, future, and wrong-artist all dropped
        assert_eq!(titles, vec!["New One"]);
        assert_eq!(out[0].confidence, 100);
        assert!(out[0]
            .cover
            .contains("coverartarchive.org/release-group/rg1"));
    }

    #[test]
    fn owned_track_folds_accents_and_credit_lists() {
        let owned = vec![("Kendrick Lamar".to_owned(), "DAMN.".to_owned())];
        assert!(owned_track(&owned, "Kendrick Lamar", "DAMN."));
        assert!(owned_track(&owned, "Kendrick Lamar & SZA", "damn."));
        assert!(!owned_track(&owned, "Kendrick Lamar", "Mr. Morale"));
        assert!(!owned_track(&owned, "J. Cole", "DAMN."));
    }

    #[test]
    fn a_partly_owned_album_is_still_offered() {
        // The listener has one single of an album that has since come out. The
        // other five tracks are still missing, so the album has to stay visible -
        // album-title matching would have hidden it on the strength of that one.
        let owned = vec![("Crônicas de um Babaca".to_owned(), "Só".to_owned())];
        let album = [
            "Super Crônico",
            "Só",
            "Liquidez",
            "Bon Vivant",
            "Pilacagem",
            "Hiato II",
            "Nada Aqui Anda Certo",
            "Sangue no Meu Jeans",
        ]
        .map(String::from);
        assert!(!release_fully_owned(
            &owned,
            "Crônicas de um Babaca",
            &album
        ));
    }

    #[test]
    fn a_fully_downloaded_album_is_recognised_by_its_album_name() {
        // The listener owns all 11 tracks of Humor Negro, but three of
        // them carry YouTube titles the release's own tracklist never
        // uses ("Froid - Algo Lúcido", "Cold - Endrick", and an
        // "(Official Music Video)" suffix), so track-title matching
        // reports the album as unowned. The album name is the stable
        // half of the pair, and the user owns as many tracks from that
        // album as the release has.
        let owned_albums = vec![("Froid".to_owned(), "Humor Negro".to_owned(), 11)];
        assert!(release_owned_by_album(
            &owned_albums,
            "Froid",
            "Humor Negro",
            11
        ));
    }

    #[test]
    fn a_partly_owned_album_is_not_recognised_by_its_album_name() {
        // One track of eight is not the record, so the album stays
        // visible: the count is compared, not just the name.
        let owned_albums = vec![(
            "Crônicas de um Babaca".to_owned(),
            "Super Crônico".to_owned(),
            1,
        )];
        assert!(!release_owned_by_album(
            &owned_albums,
            "Crônicas de um Babaca",
            "Super Crônico",
            8
        ));
    }

    #[test]
    fn a_release_whose_track_count_is_unknown_is_not_judged_by_album() {
        // MusicBrainz and ListenBrainz rows carry no track count, so there is
        // nothing to count against; the title comparison decides instead.
        let owned_albums = vec![("Froid".to_owned(), "Humor Negro".to_owned(), 11)];
        assert!(!release_owned_by_album(
            &owned_albums,
            "Froid",
            "Humor Negro",
            0
        ));
    }

    fn release(date: &str, uncertain: bool, source: &str) -> Release {
        Release {
            artist: "A".to_owned(),
            title: "T".to_owned(),
            date: date.to_owned(),
            kind: "Album".to_owned(),
            cover: String::new(),
            mbid: String::new(),
            confidence: 100,
            tracks: Vec::new(),
            source: source.to_owned(),
            track_count: 0,
            date_uncertain: uncertain,
        }
    }

    #[test]
    fn a_dated_copy_of_a_release_survives_dedupe_over_an_undated_one() {
        // The YouTube copy's date is a later playlist edit, so it is "newer"
        // and uncertain; the MusicBrainz copy has the real release date.
        let mut rs = [
            release("2026-10-01", true, "youtube"),
            release("2026-08-14", false, ""),
        ];
        rs.sort_by(release_order);
        assert_eq!(
            rs[0].source, "",
            "the undated copy sorted first and would win dedupe"
        );
    }

    #[test]
    fn dated_releases_sort_newest_first() {
        let mut rs = [
            release("2026-08-01", false, "a"),
            release("2026-09-01", false, "b"),
        ];
        rs.sort_by(release_order);
        assert_eq!(rs[0].source, "b");
    }

    #[test]
    fn a_fully_owned_album_is_suppressed() {
        let owned = vec![
            ("Crônicas de um Babaca".to_owned(), "Só".to_owned()),
            ("Crônicas de um Babaca".to_owned(), "Liquidez".to_owned()),
        ];
        assert!(release_fully_owned(
            &owned,
            "Crônicas de um Babaca",
            &["Só", "Liquidez"].map(String::from)
        ));
    }

    #[test]
    fn no_tracklist_never_counts_as_fully_owned() {
        // Empty means "this source told us no tracklist", not "an empty album".
        // Treating it as owned-by-default would blank the list.
        let owned = vec![("Ye".to_owned(), "BULLY".to_owned())];
        assert!(!release_fully_owned(&owned, "Ye", &[]));
    }

    #[test]
    fn release_key_prefers_mbid_then_normalised_pair() {
        assert_eq!(release_key("abcd-1234", "Ye", "BULLY"), "release:abcd-1234");
        assert_eq!(
            release_key("", "Motörhead", "Ace of Spades"),
            release_key("", "motorhead", "ace of spades")
        );
        assert_ne!(
            release_key("", "Ye", "BULLY"),
            release_key("", "Ye", "Donda")
        );
    }

    #[test]
    fn pick_hit_skips_clips_and_mixes() {
        let hits = vec![
            Acquire {
                source_id: "clip".to_owned(),
                duration: 10,
                ..Acquire::default()
            },
            Acquire {
                source_id: "mix".to_owned(),
                duration: 3600,
                ..Acquire::default()
            },
            Acquire {
                source_id: "song".to_owned(),
                duration: 200,
                ..Acquire::default()
            },
        ];
        assert_eq!(pick_hit(&hits).unwrap().source_id, "song");
        let zero = vec![Acquire {
            source_id: "unknown".to_owned(),
            duration: 0,
            ..Acquire::default()
        }];
        assert_eq!(pick_hit(&zero).unwrap().source_id, "unknown"); // 0 = unknown, allowed
    }

    #[test]
    fn interleave_alternates_then_tops_up() {
        let owned = vec![
            card("owned", "o1"),
            card("owned", "o2"),
            card("owned", "o3"),
        ];
        let cands = vec![card("candidate", "c1")];
        let deck = interleave(&owned, &cands, 4);
        // n/2 = 2 owned, 1 cand, then owned fills the gap -> 3 owned + 1 cand = 4.
        assert_eq!(deck.len(), 4);
        assert_eq!(deck[0].id, "o1");
        assert_eq!(deck[1].id, "c1");
    }

    #[test]
    fn is_mbid_matches_the_uuid_shape() {
        assert!(is_mbid("11111111-2222-3333-4444-555555555555"));
        assert!(!is_mbid("not-a-mbid"));
        assert!(!is_mbid("11111111222233334444555555555555"));
    }

    #[test]
    fn guard_accepts_explo_files_and_refuses_everything_else() {
        let root = std::env::temp_dir().join(format!("mh-guard-{:016x}", rand::random::<u64>()));
        let explo = root.join("explo");
        std::fs::create_dir_all(&explo).unwrap();
        std::fs::write(explo.join("ok.opus"), b"x").unwrap();
        std::fs::write(root.join("private.opus"), b"x").unwrap();
        let music_root = root.to_str().unwrap();

        // Accepted: a regular file directly under explo/.
        let ok = explo.join("ok.opus");
        assert!(guard_delete(ok.to_str().unwrap(), music_root, "explo/").is_ok());

        // Refused: outside the prefix.
        let outside = root.join("private.opus");
        assert!(guard_delete(outside.to_str().unwrap(), music_root, "explo/").is_err());

        // Refused: traversal out of explo/.
        let escape = explo.join("../private.opus");
        assert!(guard_delete(escape.to_str().unwrap(), music_root, "explo/").is_err());

        // Refused: a symlinked directory under explo/ that points outside.
        #[cfg(unix)]
        {
            let secret = root.join("secret");
            std::fs::create_dir_all(&secret).unwrap();
            std::fs::write(secret.join("passwd"), b"x").unwrap();
            let link = explo.join("link");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            let via = link.join("passwd");
            assert!(guard_delete(via.to_str().unwrap(), music_root, "explo/").is_err());
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn short_reason_prefers_a_real_failure_over_unconfigured() {
        let problems = vec![
            "LASTFM_API_KEY is not set".to_owned(),
            "listenbrainz playlists: timeout".to_owned(),
        ];
        assert_eq!(short_reason(&problems), "listenbrainz playlists: timeout");
        let both = vec![
            "LASTFM_API_KEY is not set".to_owned(),
            "LISTENBRAINZ_USER is not set".to_owned(),
        ];
        assert_eq!(short_reason(&both), "no suggestion source is configured");
    }

    fn cached(source: &str, mbid: &str, cover: &str) -> mh_db::discovery::CachedRelease {
        mh_db::discovery::CachedRelease {
            mbid: mbid.to_owned(),
            artist: "Froid".to_owned(),
            title: "Gado".to_owned(),
            kind: "Album".to_owned(),
            cover_url: cover.to_owned(),
            release_date: "2026-08-22".to_owned(),
            source_id: String::new(),
            position: 0,
            source: source.to_owned(),
            date_uncertain: false,
        }
    }

    /// The stored YouTube cover is a signed URL that expires: it must never be
    /// what the client is given, or every cover goes dead within days.
    #[test]
    fn a_youtube_cover_is_served_through_the_playlist_route_not_the_stored_url() {
        let row = cached(
            "youtube",
            "OLAK5uy_lTqNcnMJx7jZIl5Z_0X0QPyreMpEqKpUg",
            "https://i9.ytimg.com/s_p/OLAK5uy_x/sddefault.jpg?sqp=stale&rs=old",
        );
        assert_eq!(
            cover_for(&row),
            "/api/discovery/cover/OLAK5uy_lTqNcnMJx7jZIl5Z_0X0QPyreMpEqKpUg"
        );
    }

    #[test]
    fn a_non_youtube_cover_keeps_its_own_url() {
        let row = cached(
            "",
            "a1b2c3",
            "https://coverartarchive.org/release-group/a1b2c3/front-250",
        );
        assert_eq!(
            cover_for(&row),
            "https://coverartarchive.org/release-group/a1b2c3/front-250"
        );
    }
}
