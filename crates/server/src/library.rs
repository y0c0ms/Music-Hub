//! The library browse/search read endpoints.
//!
//! Thin handlers over [`mh_db::browse`]: they parse and clamp query parameters,
//! reject bad sorts with a 400, translate a bad or unknown id path segment into
//! a 404, and assemble the JSON envelopes the PWA expects. Visibility and the
//! per-user starred flag live in the query layer; every route here sits behind
//! the `require` session middleware, so a resolved [`Session`] is always present.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use mh_core::model::{Album, Artist, Playlist, SearchResults, Track};
use mh_db::browse::{self, AlbumSort, SongSort};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Session;
use crate::{AppError, AppResult, AppState};

/// Mirror the Go `clampCount`: junk, zero, and negatives fall back to `def`, a
/// positive value is bounded to `[min, max]`.
pub(crate) fn clamp(raw: Option<&str>, def: i64, min: i64, max: i64) -> i64 {
    match raw.and_then(|s| s.trim().parse::<i64>().ok()) {
        Some(v) if v > 0 => v.clamp(min, max),
        _ => def,
    }
}

/// Mirror the Go `offsetOf`: junk and negatives page from the start.
fn offset_of(raw: Option<&str>) -> i64 {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&n| n >= 0)
        .unwrap_or(0)
}

/// A path id segment; anything that is not a real uuid is a 404, never a 400.
fn parse_id(raw: &str) -> AppResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| AppError::NotFound)
}

/// The songs list query parameters; every one is optional with a clamped default.
#[derive(Debug, Deserialize)]
pub struct SongsQuery {
    sort: Option<String>,
    size: Option<String>,
    genre: Option<String>,
}

/// The albums grid query parameters.
#[derive(Debug, Deserialize)]
pub struct AlbumsQuery {
    #[serde(rename = "type")]
    kind: Option<String>,
    size: Option<String>,
    offset: Option<String>,
}

/// The search query parameters.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    n: Option<String>,
}

/// `GET /api/library/home` — the three song lists that fill the first screen.
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn home(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> AppResult<Json<Value>> {
    let h = browse::home(&state.pool, session.user_id, &session.username).await?;
    Ok(Json(json!({
        "recent": h.recent,
        "frequent": h.frequent,
        "random": h.random,
    })))
}

/// `GET /api/library/songs` — a flat song list ordered by `sort`.
///
/// # Errors
/// Returns [`AppError::BadRequest`] for an unknown sort or a `genre` sort with
/// no genre, or [`AppError`] on a database failure.
pub async fn songs(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Query(q): Query<SongsQuery>,
) -> AppResult<Json<Vec<Track>>> {
    let sort_raw = q.sort.as_deref().unwrap_or("newest");
    let sort = SongSort::parse(sort_raw)
        .ok_or_else(|| AppError::bad_request(format!("unknown song sort: {sort_raw}")))?;
    let size = clamp(q.size.as_deref(), 30, 30, 200);
    let genre = q.genre.as_deref();
    if sort == SongSort::Genre && genre.is_none_or(str::is_empty) {
        return Err(AppError::bad_request("genre is required when sort=genre"));
    }
    let tracks = browse::songs(
        &state.pool,
        session.user_id,
        &session.username,
        sort,
        genre,
        size,
    )
    .await?;
    Ok(Json(tracks))
}

/// `GET /api/library/song/{id}` — one track by id.
///
/// # Errors
/// Returns [`AppError::NotFound`] if the id is invalid, unknown, or not visible,
/// or [`AppError`] on a database failure.
pub async fn song(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> AppResult<Json<Track>> {
    let id = parse_id(&id)?;
    let track = browse::song(&state.pool, session.user_id, &session.username, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(track))
}

/// `GET /api/library/similar/{id}` — the seed track plus tracks like it.
///
/// All on disk: feature-vector nearest-neighbours with a genre floor. Shaped
/// `{ seed, songs }` because the PWA plays the seed first, then queues the rest.
/// The earlier port returned a bare array, so the client's `{ seed, songs }`
/// destructure was `undefined` and every song reported "nothing similar".
///
/// # Errors
/// Returns [`AppError::NotFound`] for a bad id, or [`AppError`] on a database
/// failure.
pub async fn similar(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let id = parse_id(&id)?;
    let seed = browse::song(&state.pool, session.user_id, &session.username, id).await?;
    let songs = browse::similar(&state.pool, session.user_id, &session.username, id, 40).await?;
    Ok(Json(json!({ "seed": seed, "songs": songs })))
}

/// `GET /api/library/albums` — the paged album grid.
///
/// # Errors
/// Returns [`AppError::BadRequest`] for an unknown type, or [`AppError`] on a
/// database failure.
pub async fn albums(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Query(q): Query<AlbumsQuery>,
) -> AppResult<Json<Vec<Album>>> {
    let type_raw = q.kind.as_deref().unwrap_or("alphabeticalByName");
    let kind = AlbumSort::parse(type_raw)
        .ok_or_else(|| AppError::bad_request(format!("unknown album type: {type_raw}")))?;
    let size = clamp(q.size.as_deref(), 100, 100, 500);
    let offset = offset_of(q.offset.as_deref());
    let albums = browse::albums(
        &state.pool,
        session.user_id,
        &session.username,
        kind,
        size,
        offset,
    )
    .await?;
    Ok(Json(albums))
}

/// `GET /api/library/album/{id}` — one album and its tracks.
///
/// # Errors
/// Returns [`AppError::NotFound`] if the album has no visible tracks, or
/// [`AppError`] on a database failure.
pub async fn album(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let id = parse_id(&id)?;
    let tracks = browse::album_tracks(&state.pool, session.user_id, &session.username, id).await?;
    if tracks.is_empty() {
        return Err(AppError::NotFound);
    }
    let album = browse::album_single(&state.pool, &session.username, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(json!({ "album": album, "tracks": tracks })))
}

/// `GET /api/library/artists` — every artist in the library.
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn artists(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> AppResult<Json<Vec<Artist>>> {
    let artists = browse::artists(&state.pool, &session.username).await?;
    Ok(Json(artists))
}

/// `GET /api/library/artist/{id}` — one artist, their albums, and every track
/// they are credited on (the page's play-all queue).
///
/// # Errors
/// Returns [`AppError::NotFound`] if the artist has no tracks, or [`AppError`]
/// on a database failure.
pub async fn artist(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let id = parse_id(&id)?;
    let artist = browse::artist_single(&state.pool, &session.username, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let albums = browse::albums_for_artist(&state.pool, &session.username, id).await?;
    let tracks =
        browse::tracks_for_artist(&state.pool, session.user_id, &session.username, id).await?;
    Ok(Json(
        json!({ "artist": artist, "albums": albums, "tracks": tracks }),
    ))
}

/// `GET /api/library/genres` — genres worth a mix button, most common first.
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn genres(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let rows = browse::genres(&state.pool).await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|g| json!({ "name": g.name, "tracks": g.tracks }))
        .collect();
    Ok(Json(json!(out)))
}

/// `GET /api/library/storage` — this user's usage, the whole library's usage,
/// and the music volume's free space.
///
/// # Errors
/// Returns [`AppError`] on a database failure. A failed `statvfs` is reported as
/// zeroed disk figures, not an error.
pub async fn storage(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> AppResult<Json<Value>> {
    let (you, everyone) = browse::storage_usage(&state.pool, &session.username).await?;
    let (total, free, used) = disk_usage(&state.config.music_root);
    Ok(Json(json!({
        "you": { "bytes": you.bytes, "tracks": you.tracks },
        "everyone": { "bytes": everyone.bytes, "tracks": everyone.tracks },
        "disk": { "total": total, "free": free, "used": used },
    })))
}

/// `(total, free, used)` bytes for the filesystem holding `root`. A `statvfs`
/// failure logs a warning and reports zeros rather than failing the request.
fn disk_usage(root: &str) -> (i64, i64, i64) {
    match nix::sys::statvfs::statvfs(root) {
        Ok(s) => {
            let frag = i64::try_from(s.fragment_size()).unwrap_or(0);
            let total = i64::try_from(s.blocks()).unwrap_or(0).saturating_mul(frag);
            let free = i64::try_from(s.blocks_available())
                .unwrap_or(0)
                .saturating_mul(frag);
            let unused = i64::try_from(s.blocks_free())
                .unwrap_or(0)
                .saturating_mul(frag);
            (total, free, total.saturating_sub(unused))
        }
        Err(e) => {
            tracing::warn!(error = %e, root, "statvfs failed; reporting zero disk usage");
            (0, 0, 0)
        }
    }
}

/// `GET /api/library/starred` — the user's starred tracks, albums, and artists.
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn starred(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> AppResult<Json<Value>> {
    let s = browse::starred(&state.pool, session.user_id, &session.username).await?;
    Ok(Json(json!({
        "tracks": s.tracks,
        "albums": s.albums,
        "artists": s.artists,
    })))
}

/// `GET /api/search` — library-only search. `acquirable` is always empty.
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn search(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Query(q): Query<SearchQuery>,
) -> AppResult<Json<SearchResults>> {
    let query = q.q.as_deref().unwrap_or("").trim().to_owned();
    let n = clamp(q.n.as_deref(), 20, 1, 50);
    let results =
        browse::search(&state.pool, session.user_id, &session.username, &query, n).await?;
    Ok(Json(results))
}

/// `GET /api/library/playlists` — this user's playlists.
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn playlists(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> AppResult<Json<Vec<Playlist>>> {
    let playlists = browse::playlists(&state.pool, &session.username).await?;
    Ok(Json(playlists))
}

/// `GET /api/library/playlist/{id}` — one of the user's playlists and its tracks.
///
/// # Errors
/// Returns [`AppError::NotFound`] if the playlist is unknown or owned by someone
/// else, or [`AppError`] on a database failure.
pub async fn playlist(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let id = parse_id(&id)?;
    let playlist = browse::playlist_single(&state.pool, &session.username, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let tracks = browse::playlist_tracks(&state.pool, session.user_id, id).await?;
    Ok(Json(json!({ "playlist": playlist, "tracks": tracks })))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn clamp_defaults_and_bounds() {
        assert_eq!(clamp(None, 30, 30, 200), 30);
        assert_eq!(clamp(Some("junk"), 30, 30, 200), 30);
        assert_eq!(clamp(Some("-5"), 30, 30, 200), 30);
        assert_eq!(clamp(Some("5"), 30, 30, 200), 30);
        assert_eq!(clamp(Some("50"), 30, 30, 200), 50);
        assert_eq!(clamp(Some("9999"), 30, 30, 200), 200);
    }

    #[test]
    fn offset_tolerates_junk() {
        assert_eq!(offset_of(None), 0);
        assert_eq!(offset_of(Some("junk")), 0);
        assert_eq!(offset_of(Some("-3")), 0);
        assert_eq!(offset_of(Some("40")), 40);
    }

    #[test]
    fn bad_id_is_not_found() {
        assert!(matches!(parse_id("not-a-uuid"), Err(AppError::NotFound)));
    }

    // Exercises the real queries against a throwaway database, mirroring the
    // scanner's integration test. Skipped when DATABASE_URL is unset.
    #[tokio::test]
    async fn browse_queries_against_a_seeded_db() {
        use mh_core::ids;
        use mh_db::library::{get_or_create_library, set_track_artists, upsert_track, NewTrack};
        use sqlx::postgres::PgPoolOptions;

        let Some(admin_url) = std::env::var("DATABASE_URL").ok() else {
            return;
        };

        let (base, _) = admin_url.rsplit_once('/').unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let test_db = format!("mh_browse_{nanos}");
        let admin = PgPoolOptions::new()
            .connect(&format!("{base}/postgres"))
            .await
            .unwrap();
        sqlx::query(&format!("CREATE DATABASE {test_db}"))
            .execute(&admin)
            .await
            .unwrap();
        let url = format!("{base}/{test_db}");
        let pool = PgPoolOptions::new().connect(&url).await.unwrap();
        mh_db::migrate(&pool).await.unwrap();

        // One user, one shared library, two tracks on distinct albums/artists.
        let user_id = mh_db::users::upsert(&pool, "kc-sam", "sam", "Sam")
            .await
            .unwrap();
        let lib = get_or_create_library(&pool, "/music", "music", "shared", "")
            .await
            .unwrap();

        let track = |abs: &str, title: &str, artist: &str, album: &str, size, cover| NewTrack {
            id: ids::track_id(abs),
            library_id: lib,
            rel_path: abs.trim_start_matches("/music/").to_owned(),
            abs_path: abs.to_owned(),
            title: title.to_owned(),
            artist: artist.to_owned(),
            artist_id: ids::artist_id(artist),
            album_artist: artist.to_owned(),
            album: album.to_owned(),
            album_id: Some(ids::album_id(artist, album)),
            track_no: 1,
            disc_no: 1,
            year: 2001,
            genre: String::new(),
            duration_sec: 123,
            size_bytes: size,
            has_cover: cover,
            is_book: false,
            mtime: None,
        };
        let alpha = track(
            "/music/a.opus",
            "Alpha Song",
            "Alpha Artist",
            "Alpha Album",
            1000,
            true,
        );
        let beta = track(
            "/music/b.opus",
            "Beta Song",
            "Beta Artist",
            "Beta Album",
            2000,
            false,
        );
        upsert_track(&pool, &alpha).await.unwrap();
        upsert_track(&pool, &beta).await.unwrap();
        set_track_artists(&pool, alpha.id, &alpha.artist)
            .await
            .unwrap();
        set_track_artists(&pool, beta.id, &beta.artist)
            .await
            .unwrap();

        // Star the first track for this user.
        sqlx::query(
            "INSERT INTO annotations (user_id, item_id, item_type, starred, starred_at) \
             VALUES ($1, $2, 'track', true, now())",
        )
        .bind(user_id)
        .bind(alpha.id)
        .execute(&pool)
        .await
        .unwrap();

        // songs(newest): both tracks, with the right stream/cover URLs.
        let newest = browse::songs(&pool, user_id, "sam", SongSort::Newest, None, 30)
            .await
            .unwrap();
        assert_eq!(newest.len(), 2);
        let found = newest
            .iter()
            .find(|t| t.id == alpha.id.to_string())
            .expect("alpha present");
        assert_eq!(found.stream_url, format!("/api/stream/{}", alpha.id));
        assert_eq!(found.cover_url, format!("/api/cover/{}", alpha.id));

        // songs(starred): only the starred track.
        let starred = browse::songs(&pool, user_id, "sam", SongSort::Starred, None, 30)
            .await
            .unwrap();
        assert_eq!(starred.len(), 1);
        assert_eq!(starred[0].id, alpha.id.to_string());
        assert!(starred[0].starred);

        // albums: two derived albums.
        let albums = browse::albums(&pool, user_id, "sam", AlbumSort::AlphabeticalByName, 100, 0)
            .await
            .unwrap();
        assert_eq!(albums.len(), 2);
        assert_eq!(albums[0].name, "Alpha Album");
        assert_eq!(albums[0].song_count, 1);
        assert_eq!(albums[0].cover_url, format!("/api/cover/{}", albums[0].id));

        // artists: two derived artists.
        let artists = browse::artists(&pool, "sam").await.unwrap();
        assert_eq!(artists.len(), 2);
        assert!(artists.iter().any(|a| a.name == "Alpha Artist"));

        // search: a substring of a title finds the track.
        let results = browse::search(&pool, user_id, "sam", "Alph", 20)
            .await
            .unwrap();
        assert!(results.tracks.iter().any(|t| t.title == "Alpha Song"));
        assert!(results.acquirable.is_empty());

        // storage: two tracks across the whole library.
        let (_you, everyone) = browse::storage_usage(&pool, "sam").await.unwrap();
        assert_eq!(everyone.tracks, 2);
        assert_eq!(everyone.bytes, 3000);

        // genres: none seeded.
        let genres = browse::genres(&pool).await.unwrap();
        assert!(genres.is_empty());

        // similar: the SHAPE is `{seed, songs}`, never a bare array - the port
        // regression that made every "play similar" report "nothing similar".
        // songs is empty here (no vectors or genres seeded); the contract is the
        // point, and a bare top-level array would fail the `songs` lookup below.
        let cfg = mh_core::Config::load(|k| match k {
            "OIDC_ISSUER" => Some("https://kc/realms/x".to_owned()),
            "OIDC_CLIENT_SECRET" => Some("s".to_owned()),
            "DATABASE_URL" => Some(url.clone()),
            _ => None,
        })
        .unwrap();
        let state = crate::AppState::for_test(cfg, pool.clone());
        let session = crate::auth::Session {
            user_id,
            username: "sam".to_owned(),
            display_name: "Sam".to_owned(),
        };
        let axum::Json(v) = super::similar(
            axum::extract::State(state),
            axum::Extension(session),
            axum::extract::Path(alpha.id.to_string()),
        )
        .await
        .unwrap();
        assert!(v.get("seed").is_some(), "similar carries a seed field");
        assert!(
            v.get("songs").and_then(|s| s.as_array()).is_some(),
            "similar songs is an array, not a bare top-level array"
        );
        drop(pool);
        sqlx::query(&format!("DROP DATABASE {test_db} WITH (FORCE)"))
            .execute(&admin)
            .await
            .unwrap();
    }
}
