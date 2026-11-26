//! SQLite persistence for resumable Hugging Face model downloads.

use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use super::Database;
use super::entities::PendingDownload;

impl Database {
    /// List all pending/paused model downloads in insertion order.
    pub fn list_pending_downloads(&self) -> Result<Vec<PendingDownload>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT repo_id, filename, mmproj_filename, display_name, total_bytes
                 FROM pending_downloads
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                let total: Option<i64> = row.get(4)?;
                Ok(PendingDownload {
                    repo_id: row.get(0)?,
                    filename: row.get(1)?,
                    mmproj_filename: row.get(2)?,
                    display_name: row.get(3)?,
                    total_bytes: total.and_then(|v| u64::try_from(v).ok()),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
    }

    /// Insert or update a pending download entry atomically by `(repo_id, filename)`.
    pub fn upsert_pending_download(&self, item: PendingDownload) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO pending_downloads (repo_id, filename, mmproj_filename, display_name, total_bytes, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(repo_id, filename) DO UPDATE SET
                    mmproj_filename = excluded.mmproj_filename,
                    display_name = excluded.display_name,
                    total_bytes = COALESCE(excluded.total_bytes, pending_downloads.total_bytes)",
                params![
                    item.repo_id,
                    item.filename,
                    item.mmproj_filename,
                    item.display_name,
                    item.total_bytes.map(|b| b as i64),
                    now,
                ],
            )?;
            Ok(())
        })
    }

    /// Update the known total byte size for a pending download.
    pub fn update_pending_download_total(
        &self,
        repo_id: &str,
        filename: &str,
        total_bytes: u64,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE pending_downloads
                 SET total_bytes = ?1
                 WHERE repo_id = ?2 AND filename = ?3",
                params![total_bytes as i64, repo_id, filename],
            )?;
            Ok(())
        })
    }

    /// Remove a pending download entry when completed or cancelled.
    pub fn remove_pending_download(&self, repo_id: &str, filename: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM pending_downloads WHERE repo_id = ?1 AND filename = ?2",
                params![repo_id, filename],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_updates_and_removes_pending_downloads() {
        let db = Database::open_in_memory().expect("open in-memory db");

        db.upsert_pending_download(PendingDownload {
            repo_id: "google/gemma-4-E2B-it-qat-q4_0-gguf".to_string(),
            filename: "gemma-4-E2B-it-Q4_0.gguf".to_string(),
            mmproj_filename: Some("gemma-4-E2B-it-mmproj.gguf".to_string()),
            display_name: "Gemma 4 E2B".to_string(),
            total_bytes: None,
        })
        .unwrap();
        db.update_pending_download_total(
            "google/gemma-4-E2B-it-qat-q4_0-gguf",
            "gemma-4-E2B-it-Q4_0.gguf",
            3_000_000_000,
        )
        .unwrap();

        let list = db.list_pending_downloads().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].total_bytes, Some(3_000_000_000));

        db.remove_pending_download(
            "google/gemma-4-E2B-it-qat-q4_0-gguf",
            "gemma-4-E2B-it-Q4_0.gguf",
        )
        .unwrap();
        assert!(db.list_pending_downloads().unwrap().is_empty());
    }

    #[test]
    fn concurrent_pending_download_operations_do_not_lose_updates() {
        let db = Database::open_in_memory().expect("open in-memory db");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let db = db.clone();
                std::thread::spawn(move || {
                    let repo = format!("org/model-{i}");
                    let file = format!("model-{i}.gguf");
                    db.upsert_pending_download(PendingDownload {
                        repo_id: repo.clone(),
                        filename: file.clone(),
                        mmproj_filename: None,
                        display_name: format!("Model {i}"),
                        total_bytes: None,
                    })
                    .unwrap();
                    db.update_pending_download_total(&repo, &file, (i as u64 + 1) * 1024)
                        .unwrap();
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let items = db.list_pending_downloads().unwrap();
        assert_eq!(items.len(), 8);
        assert!(items.iter().all(|item| item.total_bytes.is_some()));

        for i in 0..4 {
            db.remove_pending_download(&format!("org/model-{i}"), &format!("model-{i}.gguf"))
                .unwrap();
        }
        assert_eq!(db.list_pending_downloads().unwrap().len(), 4);
    }
}
