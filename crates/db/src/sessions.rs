//! The `sessions` table: server-side sessions keyed by SHA256(cookie id).
//!
//! Token generation and hashing live in the server's auth module; this layer
//! only stores the pre-computed `id_hash` so `db` carries no crypto.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::DbError;

/// A resolved session joined to its user.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
}

/// Store a session. `id_hash` is SHA256(cookie token); `expires` is the absolute
/// expiry the resolver checks against `now()`.
///
/// # Errors
/// Returns [`DbError`] if the insert fails.
pub async fn create(
    pool: &PgPool,
    id_hash: &[u8],
    username: &str,
    expires: DateTime<Utc>,
) -> Result<(), DbError> {
    sqlx::query("INSERT INTO sessions (id_hash, username, expires_at) VALUES ($1, $2, $3)")
        .bind(id_hash)
        .bind(username)
        .bind(expires)
        .execute(pool)
        .await?;
    Ok(())
}

/// Resolve a session hash to its live session + user, or `None` when the hash is
/// unknown or the session has expired.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn resolve(pool: &PgPool, id_hash: &[u8]) -> Result<Option<SessionRow>, DbError> {
    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT u.id, u.username, u.display_name
         FROM sessions s JOIN users u ON u.username = s.username
         WHERE s.id_hash = $1 AND s.expires_at > now()",
    )
    .bind(id_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(user_id, username, display_name)| SessionRow {
        user_id,
        username,
        display_name,
    }))
}

/// Drop a session by its hash.
///
/// # Errors
/// Returns [`DbError`] if the delete fails.
pub async fn delete(pool: &PgPool, id_hash: &[u8]) -> Result<(), DbError> {
    sqlx::query("DELETE FROM sessions WHERE id_hash = $1")
        .bind(id_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Remove every expired session.
///
/// # Errors
/// Returns [`DbError`] if the delete fails.
pub async fn sweep(pool: &PgPool) -> Result<(), DbError> {
    sqlx::query("DELETE FROM sessions WHERE expires_at <= now()")
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::users;
    use sqlx::postgres::PgPoolOptions;

    // Integration tests: skipped unless DATABASE_URL points at a live Postgres.
    // CI runs them against a throwaway Postgres so the DB layer is covered.
    async fn test_pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        PgPoolOptions::new().connect(&url).await.ok()
    }

    #[tokio::test]
    async fn create_resolve_delete_roundtrip() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let kc = format!("kc-{}", Uuid::new_v4());
        let user = format!("user-{}", Uuid::new_v4());
        users::upsert(&pool, &kc, &user, "Display Name")
            .await
            .unwrap();

        let id_hash = Uuid::new_v4().into_bytes().to_vec();
        create(
            &pool,
            &id_hash,
            &user,
            Utc::now() + chrono::Duration::days(1),
        )
        .await
        .unwrap();

        let row = resolve(&pool, &id_hash).await.unwrap().unwrap();
        assert_eq!(row.username, user);
        assert_eq!(row.display_name, "Display Name");

        delete(&pool, &id_hash).await.unwrap();
        assert!(resolve(&pool, &id_hash).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_session_does_not_resolve() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let kc = format!("kc-{}", Uuid::new_v4());
        let user = format!("user-{}", Uuid::new_v4());
        users::upsert(&pool, &kc, &user, "X").await.unwrap();
        let id_hash = Uuid::new_v4().into_bytes().to_vec();
        create(
            &pool,
            &id_hash,
            &user,
            Utc::now() - chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
        assert!(resolve(&pool, &id_hash).await.unwrap().is_none());
    }
}
