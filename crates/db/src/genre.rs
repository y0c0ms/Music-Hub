//! Genre classification storage and the Postgres-as-cache helpers.
//!
//! Track genres live in `genres` + `track_genres`. Artist-level genres and raw
//! Last.fm/ListenBrainz responses live in `api_cache` with a TTL, so enrichment
//! and the discovery deck do not re-hit rate-limited services on every request.

use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::DbError;

/// The genres attached to one track, alphabetical. Empty when unclassified.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn genres_for(pool: &PgPool, track_id: Uuid) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT g.name FROM track_genres tg
         JOIN genres g ON g.id = tg.genre_id
         WHERE tg.track_id = $1
         ORDER BY g.name",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Replace a track's genres in one transaction, interning each name in `genres`.
///
/// A half-write never leaves stale genres: the old links go and the new ones
/// land together, or neither does.
///
/// # Errors
/// Returns [`DbError`] if any statement fails.
pub async fn set_track_genres(
    pool: &PgPool,
    track_id: Uuid,
    genres: &[String],
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM track_genres WHERE track_id = $1")
        .bind(track_id)
        .execute(&mut *tx)
        .await?;
    for name in genres {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let genre_id = sqlx::query_scalar::<_, i32>(
            "INSERT INTO genres (name) VALUES ($1)
             ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name
             RETURNING id",
        )
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO track_genres (track_id, genre_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(track_id)
        .bind(genre_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Cached genres for an artist name (case-insensitive).
///
/// `Some(vec)` — including an empty vec — is a cached answer: the artist has been
/// classified and this is the result, even when the result is "no genres". `None`
/// means the artist has never been looked up, which is what lets the enricher
/// skip a name it already knows to be empty.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn artist_genres(pool: &PgPool, artist: &str) -> Result<Option<Vec<String>>, DbError> {
    Ok(cache_get(pool, &artist_key(artist))
        .await?
        .map(|v| as_string_list(&v)))
}

/// Cache an artist's genres for a long TTL: library classification is stable.
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn set_artist_genres(
    pool: &PgPool,
    artist: &str,
    genres: &[String],
) -> Result<(), DbError> {
    let value = Value::Array(genres.iter().map(|g| Value::String(g.clone())).collect());
    // Artist classifications are stable, so cache them for a long while.
    cache_put(pool, &artist_key(artist), &value, Duration::days(90)).await
}

/// Read a still-fresh cached value, or `None` if it is missing or expired.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn cache_get(pool: &PgPool, key: &str) -> Result<Option<Value>, DbError> {
    let row = sqlx::query_scalar::<_, sqlx::types::Json<Value>>(
        "SELECT value FROM api_cache WHERE key = $1 AND expires_at > now()",
    )
    .bind(key)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|j| j.0))
}

/// Upsert a cached value with a TTL measured from now.
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn cache_put(
    pool: &PgPool,
    key: &str,
    value: &Value,
    ttl: Duration,
) -> Result<(), DbError> {
    let expires = Utc::now() + ttl;
    sqlx::query(
        "INSERT INTO api_cache (key, value, expires_at) VALUES ($1, $2, $3)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, expires_at = EXCLUDED.expires_at",
    )
    .bind(key)
    .bind(sqlx::types::Json(value))
    .bind(expires)
    .execute(pool)
    .await?;
    Ok(())
}

/// Non-book tracks with no genres yet, oldest first, for the enricher's next pass.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn unclassified(
    pool: &PgPool,
    limit: i64,
) -> Result<Vec<(Uuid, String, String)>, DbError> {
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT t.id, t.artist, t.title FROM tracks t
         WHERE NOT t.is_book
           AND NOT EXISTS (SELECT 1 FROM track_genres tg WHERE tg.track_id = t.id)
         ORDER BY t.created_at
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

fn artist_key(artist: &str) -> String {
    format!("genre:artist:{}", artist.trim().to_lowercase())
}

fn as_string_list(v: &Value) -> Vec<String> {
    v.as_array().map_or_else(Vec::new, |a| {
        a.iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::library::{self, NewTrack};
    use sqlx::postgres::PgPoolOptions;

    async fn test_pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        PgPoolOptions::new().connect(&url).await.ok()
    }

    #[tokio::test]
    async fn artist_genres_round_trip_through_the_cache() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let artist = format!("Test Artist {}", Uuid::new_v4());
        assert!(artist_genres(&pool, &artist).await.unwrap().is_none());

        set_artist_genres(&pool, &artist, &["rock".to_owned(), "indie".to_owned()])
            .await
            .unwrap();
        let got = artist_genres(&pool, &artist).await.unwrap().unwrap();
        assert_eq!(got, vec!["rock".to_owned(), "indie".to_owned()]);

        // A known-empty answer is still Some, so the enricher will not re-fetch.
        set_artist_genres(&pool, &artist, &[]).await.unwrap();
        assert_eq!(artist_genres(&pool, &artist).await.unwrap(), Some(vec![]));

        sqlx::query("DELETE FROM api_cache WHERE key = $1")
            .bind(artist_key(&artist))
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn track_genres_round_trip() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let root = format!("/tmp/mh-genre-{}", Uuid::new_v4());
        let abs = format!("{root}/explo/song.opus");
        let library_id = library::get_or_create_library(&pool, &root, "test", "shared", "")
            .await
            .unwrap();
        let id = Uuid::new_v4();
        library::upsert_track(
            &pool,
            &NewTrack {
                id,
                library_id,
                rel_path: "explo/song.opus".to_owned(),
                abs_path: abs.clone(),
                title: "Song".to_owned(),
                artist: "Artist".to_owned(),
                artist_id: Uuid::new_v4(),
                album_artist: String::new(),
                album: String::new(),
                album_id: None,
                track_no: 0,
                disc_no: 0,
                year: 0,
                genre: String::new(),
                duration_sec: 1,
                size_bytes: 1,
                has_cover: false,
                is_book: false,
                mtime: None,
            },
        )
        .await
        .unwrap();

        assert!(genres_for(&pool, id).await.unwrap().is_empty());
        set_track_genres(&pool, id, &["hip-hop".to_owned(), "rock".to_owned()])
            .await
            .unwrap();
        // ORDER BY name: "hip-hop" < "rock".
        assert_eq!(
            genres_for(&pool, id).await.unwrap(),
            vec!["hip-hop".to_owned(), "rock".to_owned()]
        );

        // Rewriting replaces, never appends.
        set_track_genres(&pool, id, &["jazz".to_owned()])
            .await
            .unwrap();
        assert_eq!(
            genres_for(&pool, id).await.unwrap(),
            vec!["jazz".to_owned()]
        );

        sqlx::query("DELETE FROM tracks WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM libraries WHERE id = $1")
            .bind(library_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
