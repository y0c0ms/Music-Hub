//! Audiobook shelf source rows and per-user book progress/settings.
//!
//! The shelf itself is assembled in the server (grouping tracks by folder and
//! reading `book.json` sidecars from disk); this module answers only the two
//! database questions: which library tracks are book chapters, and where each
//! person is in each book. Progress is one row per `(user, book)`: the current
//! chapter (`track_id`) and offset, plus the per-book speed and sleep timer. An
//! empty `track_id` means settings exist but listening has not started.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::DbError;

/// A library track under the books prefix — the raw material for the shelf.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BookTrack {
    pub id: Uuid,
    pub abs_path: String,
    pub title: String,
    pub artist: String,
    pub duration_sec: i32,
    pub has_cover: bool,
}

/// One person's place and settings in one book.
#[derive(Debug, Clone)]
pub struct Progress {
    pub book_id: String,
    /// Empty when only settings were saved and listening has not started.
    pub track_id: String,
    pub position_sec: i32,
    pub speed: f64,
    pub sleep_minutes: i32,
    /// Milliseconds since the Unix epoch.
    pub updated_ms: i64,
}

type ProgressRow = (String, String, i32, f64, i32, i64);

fn into_progress(row: ProgressRow) -> Progress {
    let (book_id, track_id, position_sec, speed, sleep_minutes, updated_ms) = row;
    Progress {
        book_id,
        track_id,
        position_sec,
        speed,
        sleep_minutes,
        updated_ms,
    }
}

const PROGRESS_COLUMNS: &str = "book_id, track_id, position_sec, speed, sleep_minutes, \
     (extract(epoch from updated_at) * 1000)::bigint";

/// Every library track flagged as a book chapter.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn book_tracks(pool: &PgPool) -> Result<Vec<BookTrack>, DbError> {
    let rows = sqlx::query_as::<_, BookTrack>(
        "SELECT id, abs_path, title, artist, duration_sec, has_cover FROM tracks WHERE is_book",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// One user's progress across all books, keyed by book id.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn all_progress(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<HashMap<String, Progress>, DbError> {
    let rows: Vec<ProgressRow> = sqlx::query_as(&format!(
        "SELECT {PROGRESS_COLUMNS} FROM book_progress WHERE user_id = $1"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(into_progress)
        .map(|p| (p.book_id.clone(), p))
        .collect())
}

/// One user's progress in one book.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn get_progress(
    pool: &PgPool,
    user_id: Uuid,
    book_id: &str,
) -> Result<Option<Progress>, DbError> {
    let row: Option<ProgressRow> = sqlx::query_as(&format!(
        "SELECT {PROGRESS_COLUMNS} FROM book_progress WHERE user_id = $1 AND book_id = $2"
    ))
    .bind(user_id)
    .bind(book_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(into_progress))
}

/// Record where a user is in a book. Leaves speed and sleep timer untouched.
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn save_progress(
    pool: &PgPool,
    user_id: Uuid,
    book_id: &str,
    track_id: &str,
    position_sec: i32,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO book_progress (user_id, book_id, track_id, position_sec, updated_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (user_id, book_id) DO UPDATE
         SET track_id = EXCLUDED.track_id,
             position_sec = EXCLUDED.position_sec,
             updated_at = now()",
    )
    .bind(user_id)
    .bind(book_id)
    .bind(track_id)
    .bind(position_sec)
    .execute(pool)
    .await?;
    Ok(())
}

/// Set the per-book speed and/or sleep timer, leaving the listening position
/// untouched. `None` fields keep their stored value (or the default on a first
/// write, which does NOT mark the book started).
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn save_settings(
    pool: &PgPool,
    user_id: Uuid,
    book_id: &str,
    speed: Option<f64>,
    sleep_minutes: Option<i32>,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO book_progress (user_id, book_id, track_id, speed, sleep_minutes, updated_at)
         VALUES ($1, $2, '', COALESCE($3, 1.0), COALESCE($4, 0), now())
         ON CONFLICT (user_id, book_id) DO UPDATE
         SET speed = COALESCE($3, book_progress.speed),
             sleep_minutes = COALESCE($4, book_progress.sleep_minutes),
             updated_at = now()",
    )
    .bind(user_id)
    .bind(book_id)
    .bind(speed)
    .bind(sleep_minutes)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    async fn pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        PgPoolOptions::new().connect(&url).await.ok()
    }

    #[tokio::test]
    async fn progress_and_settings_round_trip_independently() {
        let Some(pool) = pool().await else {
            return;
        };
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, kc_sub, username) VALUES ($1, $2, $2)")
            .bind(user)
            .bind(format!("bk-{user}"))
            .execute(&pool)
            .await
            .unwrap();

        // Settings before any listening: a row exists but is NOT started.
        save_settings(&pool, user, "bk_x", Some(1.5), None)
            .await
            .unwrap();
        let p = get_progress(&pool, user, "bk_x").await.unwrap().unwrap();
        assert!((p.speed - 1.5).abs() < 1e-9);
        assert_eq!(p.sleep_minutes, 0);
        assert_eq!(p.track_id, ""); // not started

        // Listening sets the position without disturbing speed.
        save_progress(&pool, user, "bk_x", "tr-3", 42)
            .await
            .unwrap();
        let p = get_progress(&pool, user, "bk_x").await.unwrap().unwrap();
        assert_eq!(p.track_id, "tr-3");
        assert_eq!(p.position_sec, 42);
        assert!((p.speed - 1.5).abs() < 1e-9); // speed preserved

        // A later settings change keeps the position.
        save_settings(&pool, user, "bk_x", None, Some(20))
            .await
            .unwrap();
        let p = get_progress(&pool, user, "bk_x").await.unwrap().unwrap();
        assert_eq!(p.position_sec, 42);
        assert_eq!(p.sleep_minutes, 20);
        assert!((p.speed - 1.5).abs() < 1e-9);

        let all = all_progress(&pool, user).await.unwrap();
        assert!(all.contains_key("bk_x"));

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
    }
}
