//! The small amount of configuration a person can change from inside the app.
//!
//! Today that is one thing: which ListenBrainz account their discovery
//! recommendations come from. It matters because recommendations are personal -
//! two people sharing one ListenBrainz account get one person's taste, which is
//! exactly the complaint that surfaced these handlers being missing after the
//! Rust cutover.

use sqlx::PgPool;
use uuid::Uuid;

use crate::DbError;

/// A user's stored ListenBrainz link: `(username, token)`.
///
/// Absent rows read as `("", "")` rather than `None`, because "no settings row"
/// and "a settings row with nothing in it" mean the same thing to every caller
/// and distinguishing them would only invite a `match` that both arms handle
/// identically.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn get(pool: &PgPool, user_id: Uuid) -> Result<(String, String), DbError> {
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT listenbrainz_user, listenbrainz_token FROM settings WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or_default())
}

/// Store a validated ListenBrainz link.
///
/// The username is written alongside the token because it was PROVEN by
/// validating the token, not typed by the user - anyone can claim to be
/// "alice", only the account holder holds the token.
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn set_listenbrainz(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    token: &str,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO settings (user_id, listenbrainz_user, listenbrainz_token) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (user_id) DO UPDATE SET \
             listenbrainz_user = EXCLUDED.listenbrainz_user, \
             listenbrainz_token = EXCLUDED.listenbrainz_token",
    )
    .bind(user_id)
    .bind(username)
    .bind(token)
    .execute(pool)
    .await?;
    Ok(())
}

/// Clear the link, revealing the server-wide fallback again.
///
/// Blanks the columns rather than deleting the row: `settings` is where any
/// future per-user preference lands, so removing the row would throw away
/// settings that have nothing to do with ListenBrainz.
///
/// # Errors
/// Returns [`DbError`] if the update fails.
pub async fn clear_listenbrainz(pool: &PgPool, user_id: Uuid) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE settings SET listenbrainz_user = '', listenbrainz_token = '' WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}
