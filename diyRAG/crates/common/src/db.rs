#![forbid(unsafe_code)]
//! PostgreSQL access (spec §5.1, §3.2).
//!
//! Thin helpers around `sqlx` to initialize a connection [`PgPool`] and run the
//! in-repo migrations (`/migrations`, applied via `sqlx::migrate!`). All query
//! code elsewhere MUST use parameter binding — never string concatenation
//! (spec §12.4).

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::config::DatabaseConfig;
use crate::errors::AppError;

/// Initialize the Postgres connection pool from [`DatabaseConfig`].
///
/// Honors `max_connections`; the DSN is treated as a secret and never logged.
pub async fn init_pool(cfg: &DatabaseConfig) -> Result<PgPool, AppError> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .connect(&cfg.url)
        .await
        .map_err(|e| AppError::Dependency {
            dependency: "postgres".to_owned(),
            message: e.to_string(),
        })
}

/// Run pending migrations against the pool (spec §17 first-run bootstrap).
///
/// Uses the SQL migrations embedded from the workspace `/migrations` directory.
pub async fn run_migrations(_pool: &PgPool) -> Result<(), AppError> {
    // TODO: invoke `sqlx::migrate!("../../migrations").run(pool).await` once the
    // migration set exists. Path is relative to this crate's manifest dir.
    // Map MigrateError into AppError::Database / AppError::Internal.
    Err(AppError::Internal {
        message: "run_migrations not yet implemented".to_owned(),
    })
}

/// Lightweight readiness probe: `SELECT 1` round-trip used by `/readyz`.
pub async fn ping(pool: &PgPool) -> Result<(), AppError> {
    sqlx::query("SELECT 1")
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|e| AppError::Dependency {
            dependency: "postgres".to_owned(),
            message: e.to_string(),
        })
}
