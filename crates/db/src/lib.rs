//! Postgres models, queries, and the migrations runner.
//!
//! Queries use runtime `sqlx::query`/`query_as` (not the compile-time-checked
//! macros) so `cargo build`/`cargo test` never require a live database or
//! `DATABASE_URL`; SQL correctness is covered by integration tests gated on
//! `DATABASE_URL`. Fleshed out per phase.
#![forbid(unsafe_code)]

use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

/// The connection pool shared by every query.
pub type Pool = PgPool;

pub mod books;
pub mod browse;
pub mod discovery;
pub mod genre;
pub mod library;
pub mod mutate;
pub mod sessions;
pub mod settings;
pub mod users;

/// Anything that can go wrong talking to Postgres.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Open the pool, retrying the first connection to ride out the kube-router
/// ipset race where Postgres' Service is not yet routable when the pod starts.
///
/// # Errors
/// Returns [`DbError::Sqlx`] if the pool cannot be established within the retry
/// budget (~60s).
pub async fn connect(database_url: &str) -> Result<PgPool, DbError> {
    const MAX_ATTEMPTS: u32 = 30;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect(database_url)
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(e) if attempt < MAX_ATTEMPTS => {
                tracing::warn!(attempt, error = %e, "postgres not ready, retrying in 2s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => return Err(DbError::Sqlx(e)),
        }
    }
}

/// Run all pending migrations embedded from `migrations/` at compile time.
///
/// # Errors
/// Returns [`DbError::Migrate`] if a migration fails to apply.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}
