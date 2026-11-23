//! Persistent configuration, state, and outbox operations.
mod delivery_store;
mod flow_store;
mod history_store;
pub mod model;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("flow configuration changed while the query was running")]
    StaleRevision,
    #[error("{0}")]
    Validation(String),
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Connect and apply idempotent V1 schema migrations.
    ///
    /// # Errors
    /// Reports SQLite connection or migration errors.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        // A plaintext webhook URL/header database must not be world-readable.
        // Preserve the special in-memory path used by automated tests.
        #[cfg(unix)]
        if path.as_ref() != Path::new(":memory:") {
            use std::os::unix::fs::OpenOptionsExt;
            let _file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .mode(0o600)
                .open(path.as_ref())?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::raw_sql(include_str!("../migrations/0001_initial.sql"))
            .execute(&pool)
            .await?;
        Ok(Self { pool })
    }

    #[must_use]
    pub const fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// The only startup recovery needed for V1. Older messages are abandoned.
    ///
    /// # Errors
    /// Reports SQLite errors.
    pub async fn recover_on_start(&self) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE webhook_messages SET status = 'interrupted', next_attempt_at = NULL WHERE status IN ('pending', 'sending')")
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE webhook_attempts SET outcome = 'interrupted', finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE outcome = 'in_progress'")
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET status = 'interrupted', finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), error = 'Application stopped before the run completed' WHERE status = 'running'")
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}
