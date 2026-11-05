//! Writes and reconciliation for the scanned library (`tracks` + `libraries`).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::DbError;

/// A track as derived by the scanner, ready to upsert.
#[derive(Debug, Clone)]
pub struct NewTrack {
    pub id: Uuid,
    pub library_id: i32,
    pub rel_path: String,
    pub abs_path: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Uuid,
    pub album_artist: String,
    pub album: String,
    pub album_id: Option<Uuid>,
    pub track_no: i32,
    pub disc_no: i32,
    pub year: i32,
    pub genre: String,
    pub duration_sec: i32,
    pub size_bytes: i64,
    pub has_cover: bool,
    pub is_book: bool,
    pub mtime: Option<DateTime<Utc>>,
}

/// Get (or create) the library row for a path root, returning its id.
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn get_or_create_library(
    pool: &PgPool,
    path: &str,
    name: &str,
    kind: &str,
    owner: &str,
) -> Result<i32, DbError> {
    let id = sqlx::query_scalar::<_, i32>(
        "INSERT INTO libraries (path, name, kind, owner)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (path) DO UPDATE
             SET name = EXCLUDED.name, kind = EXCLUDED.kind, owner = EXCLUDED.owner
         RETURNING id",
    )
    .bind(path)
    .bind(name)
    .bind(kind)
    .bind(owner)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Insert or update one track, keyed on its stable id.
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn upsert_track(pool: &PgPool, t: &NewTrack) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO tracks (
             id, library_id, rel_path, abs_path, title, artist, artist_id, album_artist,
             album, album_id, track_no, disc_no, year, genre, duration_sec, size_bytes,
             has_cover, is_book, mtime)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)
         ON CONFLICT (id) DO UPDATE SET
             library_id = EXCLUDED.library_id, rel_path = EXCLUDED.rel_path,
             abs_path = EXCLUDED.abs_path, title = EXCLUDED.title, artist = EXCLUDED.artist,
             artist_id = EXCLUDED.artist_id, album_artist = EXCLUDED.album_artist,
             album = EXCLUDED.album, album_id = EXCLUDED.album_id, track_no = EXCLUDED.track_no,
             disc_no = EXCLUDED.disc_no, year = EXCLUDED.year, genre = EXCLUDED.genre,
             duration_sec = EXCLUDED.duration_sec, size_bytes = EXCLUDED.size_bytes,
             has_cover = EXCLUDED.has_cover, is_book = EXCLUDED.is_book, mtime = EXCLUDED.mtime",
    )
    .bind(t.id)
    .bind(t.library_id)
    .bind(&t.rel_path)
    .bind(&t.abs_path)
    .bind(&t.title)
    .bind(&t.artist)
    .bind(t.artist_id)
    .bind(&t.album_artist)
    .bind(&t.album)
    .bind(t.album_id)
    .bind(t.track_no)
    .bind(t.disc_no)
    .bind(t.year)
    .bind(&t.genre)
    .bind(t.duration_sec)
    .bind(t.size_bytes)
    .bind(t.has_cover)
    .bind(t.is_book)
    .bind(t.mtime)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace a track's artist credits from its comma-joined credit string.
///
/// Split into individual artists, upsert each, and rewrite `track_artists` so a
/// re-scan after a tag edit drops stale collaborators. Credit order is kept in
/// `pos`; ids match [`mh_core::ids::artist_id`], so the scanner and the 0003
/// backfill agree and never duplicate.
///
/// # Errors
/// Returns [`DbError`] if any statement fails.
pub async fn set_track_artists(pool: &PgPool, track_id: Uuid, credit: &str) -> Result<(), DbError> {
    let names = mh_core::ids::split_artists(credit);
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM track_artists WHERE track_id = $1")
        .bind(track_id)
        .execute(&mut *tx)
        .await?;
    for (i, name) in names.iter().enumerate() {
        let aid = mh_core::ids::artist_id(name);
        sqlx::query(
            "INSERT INTO artists (id, name) VALUES ($1, $2) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
        )
        .bind(aid)
        .bind(name)
        .execute(&mut *tx)
        .await?;
        let pos = i32::try_from(i + 1).unwrap_or(i32::MAX);
        sqlx::query(
            "INSERT INTO track_artists (track_id, artist_id, pos) VALUES ($1, $2, $3) \
             ON CONFLICT (track_id, artist_id) DO UPDATE SET pos = EXCLUDED.pos",
        )
        .bind(track_id)
        .bind(aid)
        .bind(pos)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Every indexed track's `(mtime, size)` keyed by absolute path, for the
/// scanner's incremental skip check.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn all_stats(
    pool: &PgPool,
) -> Result<HashMap<String, (Option<DateTime<Utc>>, i64)>, DbError> {
    let rows = sqlx::query_as::<_, (String, Option<DateTime<Utc>>, i64)>(
        "SELECT abs_path, mtime, size_bytes FROM tracks",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(path, mtime, size)| (path, (mtime, size)))
        .collect())
}

/// Delete every track whose absolute path is not in `present`, returning the ids
/// removed so the caller can drop their cached cover art.
///
/// An empty `present` clears the library, which is the correct result for an
/// empty root.
///
/// # Errors
/// Returns [`DbError`] if the delete fails.
pub async fn delete_absent(pool: &PgPool, present: &[String]) -> Result<Vec<Uuid>, DbError> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        "DELETE FROM tracks WHERE NOT (abs_path = ANY($1)) RETURNING id",
    )
    .bind(present)
    .fetch_all(pool)
    .await?;
    Ok(ids)
}
