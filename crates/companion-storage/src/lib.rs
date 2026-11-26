//! Local SQLite persistence layer (WAL mode + FTS5 full-text search).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod agent;
pub mod attachments;
pub mod conversations;
pub mod downloads;
pub mod entities;
pub mod error;
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
    AttachmentRecord, Conversation, ConversationFolder, ModelRecord, NewConversation, NewMessage,
    PendingDownload, ToolCallRecord,
};
pub use error::StorageError;

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

    /// Return total on-disk byte size of the SQLite database and its `-wal`/`-shm` sidecars.
    pub fn storage_bytes(&self) -> u64 {
        sqlite_storage_bytes(&self.db_path)
    }

    /// Execute a closure while holding the database mutex.
    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("Database mutex poisoned"))?;
        f(&guard)
    }

    /// Execute a closure inside an atomic SQLite transaction while holding the database mutex.
    pub(crate) fn with_tx<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
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

/// Sum the on-disk byte size of a SQLite database file and its `-wal`/`-shm` sidecars.
pub fn sqlite_storage_bytes(db_path: &Path) -> u64 {
    let base = db_path.as_os_str();
    let mut wal_os = base.to_os_string();
    wal_os.push("-wal");
    let mut shm_os = base.to_os_string();
    shm_os.push("-shm");

    [db_path, Path::new(&wal_os), Path::new(&shm_os)]
        .into_iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_storage_sums_db_and_wal_shm_sidecars() {
        let dir = std::env::temp_dir().join(format!("companion_pref_db_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("companion.db");
        std::fs::write(&db_path, vec![1u8; 100]).unwrap();
        std::fs::write(dir.join("companion.db-wal"), vec![2u8; 250]).unwrap();
        std::fs::write(dir.join("companion.db-shm"), vec![3u8; 50]).unwrap();

        assert_eq!(sqlite_storage_bytes(&db_path), 400);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_tx_rolls_back_on_error() {
        let db = Database::open_in_memory().expect("open in-memory db");
        let err = db
            .with_tx(|tx| -> Result<()> {
                tx.execute(
                    "INSERT INTO app_settings (key, value) VALUES ('tx_test', 'val')",
                    [],
                )?;
                anyhow::bail!("abort tx")
            })
            .unwrap_err();
        assert!(err.to_string().contains("abort tx"));
        assert_eq!(db.get_setting("tx_test").unwrap(), None);
    }
}
