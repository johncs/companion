//! Local SQLite persistence layer (WAL mode + FTS5 full-text search).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod agent;
pub mod attachments;
pub mod conversations;
pub mod downloads;
pub mod entities;
pub mod folders;
pub mod messages;
pub mod models;
pub mod schema;
pub mod settings;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::Connection;

pub use entities::{
    AttachmentRecord, Conversation, ConversationFolder, ModelRecord, NewMessage, PendingDownload,
    ToolCallRecord,
};

/// Thread-safe handle to Companion's local SQLite database.
#[derive(Clone)]
pub struct Database {
    pub(crate) conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

impl Database {
    /// Open (or create) the SQLite database at `db_path`, configure WAL mode, and run migrations.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create database directory {:?}", parent))?;
        }

        let mut conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open SQLite database {:?}", db_path))?;

        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            ",
        )?;

        schema::run_migrations(&mut conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    /// Open an isolated in-memory database (primarily for tests).
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        schema::run_migrations(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Return the filesystem path of the active database.
    pub fn path(&self) -> &Path {
        &self.db_path
    }

    /// Execute a closure while holding the database mutex.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("Database mutex poisoned"))?;
        f(&guard)
    }

    /// Execute a closure inside an atomic SQLite transaction while holding the database mutex.
    pub fn with_tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("Database mutex poisoned"))?;
        let tx = guard.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }
}
