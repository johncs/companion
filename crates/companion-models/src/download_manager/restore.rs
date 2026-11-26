//! Async startup restoration of interrupted `.part` downloads from SQLite.

use std::path::{Path, PathBuf};

use companion_storage::PendingDownload;

use super::state::{ActiveDownloadEntry, DownloadStatus};
use super::{DownloadHandle, DownloadKey, DownloadManager, DownloadRequest, join_safe_filename};
use crate::huggingface::part_file_path;

impl DownloadManager {
    pub async fn restore_pending(&self) -> Vec<DownloadHandle> {
        let db = self.inner.db.clone();
        let pending = tokio::task::spawn_blocking(move || db.list_pending_downloads())
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();

        let mut handles = Vec::new();
        for item in pending {
            if let Some(handle) = self.try_restore_item(item).await {
                handles.push(handle);
            }
        }
        handles
    }

    async fn try_restore_item(&self, item: PendingDownload) -> Option<DownloadHandle> {
        let key = (item.repo_id.clone(), item.filename.clone());
        let repo_dir = self.repo_dir(&item.repo_id);
        let main_done = join_safe_filename(&repo_dir, &item.filename);
        let main_part = part_file_path(&main_done);
        let mm_done = item
            .mmproj_filename
            .as_deref()
            .map(|m| join_safe_filename(&repo_dir, m));
        let mm_part = mm_done.as_deref().map(part_file_path);

        let (has_part, existing) =
            probe_restorable_bytes(main_part, main_done, mm_part, mm_done).await?;

        if !has_part {
            let db = self.inner.db.clone();
            let (r, f) = (item.repo_id, item.filename);
            if let Ok(Err(err)) =
                tokio::task::spawn_blocking(move || db.remove_pending_download(&r, &f)).await
            {
                tracing::warn!("Failed to remove stale pending download: {err}");
            }
            return None;
        }

        self.insert_paused_download(key, item, existing)
    }

    fn insert_paused_download(
        &self,
        key: DownloadKey,
        item: PendingDownload,
        existing: u64,
    ) -> Option<DownloadHandle> {
        let display_name = item.display_name.clone();
        self.inner.active.with_map(|map| {
            if map.contains_key(&key) {
                return None;
            }
            let entry = ActiveDownloadEntry::new(
                DownloadRequest {
                    repo_id: item.repo_id,
                    filename: item.filename,
                    mmproj_filename: item.mmproj_filename,
                    display_name: display_name.clone(),
                    attach_to_model_id: None,
                },
                existing,
                item.total_bytes,
                DownloadStatus::Paused,
            );
            let initial = entry.snapshot();
            let rx = entry.tx.subscribe();
            map.insert(key.clone(), entry);
            Some(DownloadHandle {
                key,
                display_name,
                initial,
                rx,
            })
        })
    }
}

async fn probe_restorable_bytes(
    main_part: PathBuf,
    main_done: PathBuf,
    mm_part: Option<PathBuf>,
    mm_done: Option<PathBuf>,
) -> Option<(bool, u64)> {
    tokio::task::spawn_blocking(move || {
        let has = main_part.exists() || mm_part.as_ref().is_some_and(|p| p.exists());
        let bytes = if has {
            existing_bytes(&main_part, &main_done)
                + mm_part
                    .as_deref()
                    .zip(mm_done.as_deref())
                    .map(|(p, d)| existing_bytes(p, d))
                    .unwrap_or(0)
        } else {
            0
        };
        (has, bytes)
    })
    .await
    .ok()
}

fn existing_bytes(part: &Path, done: &Path) -> u64 {
    std::fs::metadata(part)
        .or_else(|_| std::fs::metadata(done))
        .map(|m| m.len())
        .unwrap_or(0)
}
