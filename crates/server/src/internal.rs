//! Service-to-service handlers, behind [`crate::auth::require_service`].
//!
//! Everything here is reachable with `SERVICE_TOKEN` and nothing else. Kept in
//! one module under one route prefix on purpose: a shared secret's blast radius
//! should be something you can read in a single file rather than reconstruct by
//! grepping middleware across the router.
//!
//! No handler here takes `Extension<Session>`, and none may. A service is not a
//! user - it has no library of its own and must never act as somebody. That is
//! enforced structurally: `require_service` injects no session, so a handler
//! that asked for one would fail at request time.
//!
//! The one caller today is `music-request`, which downloads a single song into
//! the library on demand. It used Navidrome for both of these - Subsonic
//! `startScan` and `search3` - which was the smaller of the two reasons
//! Navidrome could not be retired after the rewrite.

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::AppState;

#[derive(Deserialize)]
pub struct OwnedQuery {
    q: Option<String>,
    n: Option<String>,
}

#[derive(Serialize)]
pub struct OwnedTrack {
    pub artist: String,
    pub title: String,
}

#[derive(Serialize)]
pub struct OwnedResults {
    pub tracks: Vec<OwnedTrack>,
}

/// `GET /api/internal/owned?q=…` — tracks already in the library matching `q`.
///
/// Answers "do I already have this?" before `music-request` downloads anything.
/// The library is shared with Syncify, Explo and LidaTube, and a silent
/// duplicate is how a music tree rots - the first real test download in August
/// duplicated a track already sitting in LikedSongs.
///
/// Returns artist and title only. The caller compares them as normalised
/// strings; ids, paths and durations would just be library structure leaking to
/// a service that has no use for it.
///
/// # Errors
/// Returns [`crate::error::AppError`] on a database failure.
pub async fn owned(
    State(state): State<AppState>,
    Query(q): Query<OwnedQuery>,
) -> AppResult<Json<OwnedResults>> {
    let query = q.q.as_deref().unwrap_or("").trim().to_owned();
    // Same clamp as the session-facing search: a service asking for 10_000 rows
    // is a bug or a probe, and neither deserves the query.
    let n = crate::library::clamp(q.n.as_deref(), 20, 1, 50);
    let rows = mh_db::browse::owned_matching(&state.pool, &query, n).await?;
    Ok(Json(OwnedResults {
        tracks: rows
            .into_iter()
            .map(|(artist, title)| OwnedTrack { artist, title })
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct RetagQuery {
    /// A YouTube album playlist id (`OLAK5uy_…`) - how most albums are published,
    /// and the reliable path. Takes precedence over `q`.
    list: Option<String>,
    /// A search query, for a major-label album YouTube Music indexes as an
    /// `MPREb_` album entity. Less reliable: it may resolve a different record.
    q: Option<String>,
    user: Option<String>,
    /// Off by default: a dry run reports what WOULD be tagged so the resolved
    /// album is confirmed before any file is touched.
    #[serde(default)]
    apply: bool,
}

#[derive(Serialize)]
pub struct RetagResult {
    pub album: String,
    pub artist: String,
    pub on_disk: Vec<String>,
    pub missing: Vec<String>,
    pub applied: usize,
}

/// `POST /api/internal/retag-album` — tag a `user`'s on-disk tracks with an album.
///
/// Resolve an album (a playlist id via `list`, or a search via `q`) and stamp
/// its album tag onto the on-disk tracks whose titles match, so a record
/// downloaded before album tagging groups.
///
/// RE-TAGS ONLY: a track not already on disk is reported as missing, never
/// downloaded - a wrong album resolved here must not pull in a stranger's record
/// (it did once, and cost a cleanup). `apply=false` (default) is a dry run
/// reporting the resolved album and its on-disk matches, changing nothing;
/// `apply=true` re-tags the matches and rescans.
///
/// # Errors
/// Returns [`crate::error::AppError`] if neither `list` nor `q` is given, `user`
/// is empty, or the resolve fails.
pub async fn retag_album(
    State(state): State<AppState>,
    Query(q): Query<RetagQuery>,
) -> AppResult<Json<RetagResult>> {
    let user = q.user.as_deref().unwrap_or("").trim().to_owned();
    if user.is_empty() {
        return Err(AppError::BadRequest("user is required".to_owned()));
    }
    let list = q.list.as_deref().unwrap_or("").trim();
    let query = q.q.as_deref().unwrap_or("").trim();
    let album = if list.is_empty() {
        if query.is_empty() {
            return Err(AppError::BadRequest("list or q is required".to_owned()));
        }
        mh_media::acquire::search_albums(query, 1)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::BadRequest("no album found".to_owned()))?
    } else {
        mh_media::acquire::resolve_playlist(list).await?
    };
    let dest = format!("users/{user}/downloads");
    let root = &state.config.music_root;
    let mut on_disk = Vec::new();
    let mut missing = Vec::new();
    for t in &album.tracks {
        if mh_media::acquire::is_present(root, &dest, &t.title, &t.artist) {
            on_disk.push(t.title.clone());
        } else {
            missing.push(t.title.clone());
        }
    }
    let mut applied = 0;
    if q.apply {
        for t in &album.tracks {
            match mh_media::acquire::retag_if_present(
                root,
                &dest,
                &t.title,
                &t.artist,
                &album.title,
                &album.artist,
            )
            .await
            {
                Ok(true) => applied += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(track = %t.title, error = %e, "retag failed"),
            }
        }
        crate::scan::scan(&state).await?;
    }
    Ok(Json(RetagResult {
        album: album.title,
        artist: album.artist,
        on_disk,
        missing,
        applied,
    }))
}
