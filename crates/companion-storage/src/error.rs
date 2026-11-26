//! Typed error definitions for SQLite storage operations.

use thiserror::Error;

/// Domain errors produced by [`crate::Database`] operations.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("Record not found: {0}")]
    NotFound(String),
    #[error("Database constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("Database schema version {found} is newer than supported version {supported}")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("SQLite error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Database mutex poisoned")]
    LockPoisoned,
}
