//! Writes for playlists and per-user annotations (star, scrobble).
//!
//! Playlist ownership is checked by the caller via [`playlist_owner`]; these
//! functions assume the caller is authorised. `item_type` for a star is derived
//! from whether the id is a track, album, or artist, so the starred lists can be
//! rebuilt per kind.

use mh_core::model::Playlist;
use sqlx::PgPool;
use uuid::Uuid;

use crate::DbError;

/// The owner username of a playlist, or `None` if it does not exist.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn playlist_owner(pool: &PgPool, id: Uuid) -> Result<Option<String>, DbError> {
    let owner =
        sqlx::query_scalar::<_, String>("SELECT owner_username FROM playlists WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(owner)
}

/// Create a playlist owned by `owner`, seeded with `song_ids` (non-existent ids
/// are skipped), and return it.
///
/// # Errors
/// Returns [`DbError`] if a query fails.
pub async fn create_playlist(
    pool: &PgPool,
    owner: &str,
    name: &str,
    song_ids: &[Uuid],
) -> Result<Playlist, DbError> {
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO playlists (owner_username, name) VALUES ($1, $2) RETURNING id",
    )
    .bind(owner)
    .bind(name)
    .fetch_one(pool)
    .await?;
    let added = insert_tracks(pool, id, song_ids, 0).await?;
    Ok(Playlist {
        id: id.to_string(),
        name: name.to_owned(),
        owner: owner.to_owned(),
        song_count: i32::try_from(added).unwrap_or(i32::MAX),
        cover_url: format!("/api/cover/{id}"),
    })
}

/// Append existing `song_ids` to a playlist after its current last position,
/// returning how many were added. Non-existent ids are skipped.
///
/// # Errors
/// Returns [`DbError`] if a query fails.
pub async fn add_to_playlist(pool: &PgPool, id: Uuid, song_ids: &[Uuid]) -> Result<i64, DbError> {
    let max_pos = sqlx::query_scalar::<_, Option<i32>>(
        "SELECT max(position) FROM playlist_tracks WHERE playlist_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    let added = insert_tracks(pool, id, song_ids, i64::from(max_pos.unwrap_or(0))).await?;
    sqlx::query("UPDATE playlists SET updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(added)
}

// Bulk-insert the existing tracks among `song_ids` at `offset + ordinal`,
// returning how many rows landed.
async fn insert_tracks(
    pool: &PgPool,
    playlist_id: Uuid,
    song_ids: &[Uuid],
    offset: i64,
) -> Result<i64, DbError> {
    if song_ids.is_empty() {
        return Ok(0);
    }
    let added = sqlx::query_scalar::<_, i64>(
        "WITH ins AS (
             INSERT INTO playlist_tracks (playlist_id, track_id, position)
             SELECT $1, t.id, ($3 + ord.pos)::int
             FROM unnest($2::uuid[]) WITH ORDINALITY AS ord(tid, pos)
             JOIN tracks t ON t.id = ord.tid
             ON CONFLICT DO NOTHING
             RETURNING 1)
         SELECT count(*) FROM ins",
    )
    .bind(playlist_id)
    .bind(song_ids)
    .bind(offset)
    .fetch_one(pool)
    .await?;
    Ok(added)
}

/// Rename a playlist.
///
/// # Errors
/// Returns [`DbError`] if the update fails.
pub async fn rename_playlist(pool: &PgPool, id: Uuid, name: &str) -> Result<(), DbError> {
    sqlx::query("UPDATE playlists SET name = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete a playlist and its track rows (cascade).
///
/// # Errors
/// Returns [`DbError`] if the delete fails.
pub async fn delete_playlist(pool: &PgPool, id: Uuid) -> Result<(), DbError> {
    sqlx::query("DELETE FROM playlists WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Star or unstar an item. `item_type` is derived from the id.
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn set_star(
    pool: &PgPool,
    user_id: Uuid,
    item_id: Uuid,
    starred: bool,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO annotations (user_id, item_id, item_type, starred, starred_at)
         VALUES ($1, $2,
             (SELECT CASE
                 WHEN EXISTS (SELECT 1 FROM tracks WHERE id = $2) THEN 'track'
                 WHEN EXISTS (SELECT 1 FROM tracks WHERE album_id = $2) THEN 'album'
                 WHEN EXISTS (SELECT 1 FROM tracks WHERE artist_id = $2) THEN 'artist'
                 ELSE 'track' END),
             $3, CASE WHEN $3 THEN now() ELSE NULL END)
         ON CONFLICT (user_id, item_id)
         DO UPDATE SET starred = $3, starred_at = CASE WHEN $3 THEN now() ELSE NULL END",
    )
    .bind(user_id)
    .bind(item_id)
    .bind(starred)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record a completed play: increment the track's play count and stamp the time.
/// Books are skipped (they are not plays). Returns whether a play was recorded.
///
/// # Errors
/// Returns [`DbError`] if a query fails.
pub async fn scrobble(pool: &PgPool, user_id: Uuid, track_id: Uuid) -> Result<bool, DbError> {
    let is_book = sqlx::query_scalar::<_, bool>("SELECT is_book FROM tracks WHERE id = $1")
        .bind(track_id)
        .fetch_optional(pool)
        .await?;
    if is_book != Some(false) {
        return Ok(false); // a book chapter, or not a known track
    }
    sqlx::query(
        "INSERT INTO annotations (user_id, item_id, item_type, play_count, last_played_at)
         VALUES ($1, $2, 'track', 1, now())
         ON CONFLICT (user_id, item_id)
         DO UPDATE SET play_count = annotations.play_count + 1, last_played_at = now()",
    )
    .bind(user_id)
    .bind(track_id)
    .execute(pool)
    .await?;
    Ok(true)
}
