//! Playlist CRUD, star/unstar, and scrobble — the library write endpoints.
//!
//! Playlists are per-user: every mutating route resolves the owner and 404s if
//! it is not the caller's, so two people in one household never see or change
//! each other's lists. Stars and play counts are per-user annotations.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use mh_core::model::Playlist;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Session;
use crate::{AppError, AppResult, AppState};

fn parse_id(raw: &str) -> AppResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| AppError::NotFound)
}

/// Parse the caller's track-id strings, skipping anything that is not a uuid.
fn parse_ids(raw: &[String]) -> Vec<Uuid> {
    raw.iter()
        .filter_map(|s| Uuid::parse_str(s.trim()).ok())
        .collect()
}

/// 404 unless the playlist exists and belongs to the caller (case-insensitive).
async fn owned(state: &AppState, id: Uuid, username: &str) -> AppResult<()> {
    let owner = mh_db::mutate::playlist_owner(&state.pool, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if owner.eq_ignore_ascii_case(username) {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

/// Body of `POST /api/library/playlists`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePlaylistBody {
    name: String,
    #[serde(default)]
    song_ids: Vec<String>,
}

/// `POST /api/library/playlists` — create a playlist, optionally pre-filled.
///
/// # Errors
/// Returns [`AppError::BadRequest`] for an empty name, or [`AppError`] on a
/// database failure.
pub async fn create_playlist(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Json(body): Json<CreatePlaylistBody>,
) -> AppResult<Json<Playlist>> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("a playlist needs a name"));
    }
    let ids = parse_ids(&body.song_ids);
    let playlist =
        // Lowercased for the same reason library owners are: `playlists` is
        // matched against the session username, and a mixed-case owner is a
        // playlist its own creator cannot see. The migration from Navidrome
        // produced exactly that - owner "Alice" against an OIDC user "alice".
        mh_db::mutate::create_playlist(&state.pool, &session.username.to_lowercase(), name, &ids)
            .await?;
    Ok(Json(playlist))
}

/// Body of `POST /api/library/playlist/{id}/tracks`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongIdsBody {
    #[serde(default)]
    song_ids: Vec<String>,
}

/// `POST /api/library/playlist/{id}/tracks` — append tracks to a playlist.
///
/// # Errors
/// Returns [`AppError::NotFound`] if the playlist is not the caller's,
/// [`AppError::BadRequest`] if no valid track is given, or [`AppError`] on a
/// database failure.
pub async fn add_to_playlist(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
    Json(body): Json<SongIdsBody>,
) -> AppResult<Json<Value>> {
    let id = parse_id(&id)?;
    owned(&state, id, &session.username).await?;
    let ids = parse_ids(&body.song_ids);
    if ids.is_empty() {
        return Err(AppError::bad_request("no track to add"));
    }
    let added = mh_db::mutate::add_to_playlist(&state.pool, id, &ids).await?;
    Ok(Json(json!({ "added": added })))
}

/// Body of `PATCH /api/library/playlist/{id}`.
#[derive(Debug, Deserialize)]
pub struct RenameBody {
    name: String,
}

/// `PATCH /api/library/playlist/{id}` — rename a playlist.
///
/// # Errors
/// Returns [`AppError::NotFound`] if not the caller's, [`AppError::BadRequest`]
/// for an empty name, or [`AppError`] on a database failure.
pub async fn rename_playlist(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> AppResult<Json<Playlist>> {
    let id = parse_id(&id)?;
    owned(&state, id, &session.username).await?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("a playlist needs a name"));
    }
    mh_db::mutate::rename_playlist(&state.pool, id, name).await?;
    Ok(Json(Playlist {
        id: id.to_string(),
        name: name.to_owned(),
        owner: String::new(),
        song_count: 0,
        cover_url: String::new(),
    }))
}

/// `DELETE /api/library/playlist/{id}` — delete a playlist (never its files).
///
/// # Errors
/// Returns [`AppError::NotFound`] if not the caller's, or [`AppError`] on a
/// database failure.
pub async fn delete_playlist(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let id = parse_id(&id)?;
    owned(&state, id, &session.username).await?;
    mh_db::mutate::delete_playlist(&state.pool, id).await?;
    Ok(Json(json!({ "deleted": id.to_string() })))
}

/// `POST /api/library/star/{id}` — star a track, album, or artist.
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn star(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let id = parse_id(&id)?;
    mh_db::mutate::set_star(&state.pool, session.user_id, id, true).await?;
    Ok(Json(json!({ "ok": true })))
}

/// `DELETE /api/library/star/{id}` — unstar an item.
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn unstar(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let id = parse_id(&id)?;
    mh_db::mutate::set_star(&state.pool, session.user_id, id, false).await?;
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/library/scrobble/{id}` — record a completed play (books skipped).
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn scrobble(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let id = parse_id(&id)?;
    let _ = mh_db::mutate::scrobble(&state.pool, session.user_id, id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use mh_db::browse::SongSort;
    use mh_db::library::NewTrack;
    use sqlx::postgres::PgPoolOptions;

    fn state_for(pool: mh_db::Pool) -> AppState {
        use std::collections::HashMap;
        let env: HashMap<String, String> = [
            ("OIDC_ISSUER", "i"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("DATABASE_URL", "postgres://x/y"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let cfg = mh_core::Config::load(|k| env.get(k).cloned()).unwrap();
        AppState::for_test(cfg, pool)
    }

    fn track(lib: i32, name: &str) -> NewTrack {
        let abs = format!("/music/{name}.opus");
        let album = format!("Alb {name}");
        NewTrack {
            id: mh_core::ids::track_id(&abs),
            library_id: lib,
            rel_path: format!("{name}.opus"),
            abs_path: abs,
            title: name.to_owned(),
            artist: name.to_owned(),
            artist_id: mh_core::ids::artist_id(name),
            album_artist: name.to_owned(),
            album_id: Some(mh_core::ids::album_id(name, &album)),
            album,
            track_no: 1,
            disc_no: 0,
            year: 2020,
            genre: String::new(),
            duration_sec: 100,
            size_bytes: 1000,
            has_cover: false,
            is_book: false,
            mtime: None,
        }
    }

    async fn drop_db(name: &str) {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if let Some((base, _)) = url.rsplit_once('/') {
                if let Ok(admin) = PgPoolOptions::new()
                    .connect(&format!("{base}/postgres"))
                    .await
                {
                    let _ = sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
                        .execute(&admin)
                        .await;
                }
            }
        }
    }

    async fn seed() -> Option<(AppState, Session, String, Uuid, Uuid)> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let (base, _) = url.rsplit_once('/')?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = format!("mh_mut_{nanos}");
        let admin = PgPoolOptions::new()
            .connect(&format!("{base}/postgres"))
            .await
            .unwrap();
        sqlx::query(&format!("CREATE DATABASE {db}"))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .connect(&format!("{base}/{db}"))
            .await
            .unwrap();
        mh_db::migrate(&pool).await.unwrap();
        let user_id = mh_db::users::upsert(&pool, "kc-sam", "sam", "Sam")
            .await
            .unwrap();
        let lib = mh_db::library::get_or_create_library(&pool, "/music", "music", "shared", "")
            .await
            .unwrap();
        let (t1, t2) = (track(lib, "one"), track(lib, "two"));
        let (id1, id2) = (t1.id, t2.id);
        mh_db::library::upsert_track(&pool, &t1).await.unwrap();
        mh_db::library::upsert_track(&pool, &t2).await.unwrap();
        let session = Session {
            user_id,
            username: "sam".to_owned(),
            display_name: "Sam".to_owned(),
        };
        Some((state_for(pool), session, db, id1, id2))
    }

    #[tokio::test]
    async fn playlist_star_scrobble_via_handlers() {
        let Some((state, session, db, t1, t2)) = seed().await else {
            return;
        };
        let (st, se) = (State(state.clone()), Extension(session.clone()));

        let created = create_playlist(
            st.clone(),
            se.clone(),
            Json(CreatePlaylistBody {
                name: " Mix ".to_owned(),
                song_ids: vec![t1.to_string()],
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(created.name, "Mix");
        assert_eq!(created.song_count, 1);
        let pid = created.id.clone();

        let added = add_to_playlist(
            st.clone(),
            se.clone(),
            Path(pid.clone()),
            Json(SongIdsBody {
                song_ids: vec![t2.to_string()],
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(added["added"], json!(1));
        assert_eq!(
            mh_db::browse::playlists(&state.pool, "sam").await.unwrap()[0].song_count,
            2
        );

        let renamed = rename_playlist(
            st.clone(),
            se.clone(),
            Path(pid.clone()),
            Json(RenameBody {
                name: "Renamed".to_owned(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(renamed.name, "Renamed");

        assert_eq!(
            star(st.clone(), se.clone(), Path(t1.to_string()))
                .await
                .unwrap()
                .0["ok"],
            json!(true)
        );
        assert_eq!(
            mh_db::browse::starred(&state.pool, session.user_id, "sam")
                .await
                .unwrap()
                .tracks
                .len(),
            1
        );
        assert_eq!(
            unstar(st.clone(), se.clone(), Path(t1.to_string()))
                .await
                .unwrap()
                .0["ok"],
            json!(true)
        );
        assert!(mh_db::browse::starred(&state.pool, session.user_id, "sam")
            .await
            .unwrap()
            .tracks
            .is_empty());

        assert_eq!(
            scrobble(st.clone(), se.clone(), Path(t1.to_string()))
                .await
                .unwrap()
                .0["ok"],
            json!(true)
        );
        let freq = mh_db::browse::songs(
            &state.pool,
            session.user_id,
            "sam",
            SongSort::Frequent,
            None,
            10,
        )
        .await
        .unwrap();
        assert_eq!(freq.len(), 1);
        assert_eq!(freq[0].id, t1.to_string());

        // A playlist that is not the caller's is a 404, never a mutation.
        let foreign = add_to_playlist(
            st.clone(),
            se.clone(),
            Path(Uuid::new_v4().to_string()),
            Json(SongIdsBody {
                song_ids: vec![t2.to_string()],
            }),
        )
        .await;
        assert!(matches!(foreign, Err(AppError::NotFound)));

        let deleted = delete_playlist(st, se, Path(pid.clone())).await.unwrap().0;
        assert_eq!(deleted["deleted"], json!(pid));
        assert!(mh_db::browse::playlists(&state.pool, "sam")
            .await
            .unwrap()
            .is_empty());

        drop(state);
        drop_db(&db).await;
    }
}
