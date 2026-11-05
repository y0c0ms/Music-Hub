//! The `users` table: identity mirrored from Keycloak.

use sqlx::PgPool;
use uuid::Uuid;

use crate::DbError;

/// Insert or update the user for this Keycloak subject and return its id.
///
/// Called at login so `username`/`display_name` track Keycloak. The id is a
/// pg18 `uuidv7()` (time-ordered) assigned by the database default.
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn upsert(
    pool: &PgPool,
    kc_sub: &str,
    username: &str,
    display_name: &str,
) -> Result<Uuid, DbError> {
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO users (kc_sub, username, display_name)
         VALUES ($1, $2, $3)
         ON CONFLICT (kc_sub) DO UPDATE
             SET username = EXCLUDED.username, display_name = EXCLUDED.display_name
         RETURNING id",
    )
    .bind(kc_sub)
    .bind(username)
    .bind(display_name)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// The user id for `username`, or `None` if unknown.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn id_by_username(pool: &PgPool, username: &str) -> Result<Option<Uuid>, DbError> {
    let id = sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE username = $1")
        .bind(username)
        .fetch_optional(pool)
        .await?;
    Ok(id)
}
