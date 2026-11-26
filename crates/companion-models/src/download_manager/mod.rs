//! Injectable, GTK-free resumable model & vision-projector download manager.

mod state;
#[cfg(test)]
mod tests;
mod worker;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tokio::sync::broadcast;

use crate::huggingface::{HfClient, part_file_path, remove_part_file};
use companion_storage::{Database, PendingDownload};

use state::{ActiveDownloadEntry, ActiveDownloadsMap};
use worker::{CompletionInfo, FetchJob, handle_fetch_completion, run_fetch_job};

pub use state::{DownloadSnapshot, DownloadStatus};

/// Unique identifier for a model download: `(repo_id, filename)`.
pub type DownloadKey = (String, String);

/// Progress callback invoked by [`ModelFetcher::fetch`].
pub type ProgressCb = Arc<dyn Fn(u64, Option<u64>) + Send + Sync>;

/// Pluggable download transport (defaults to Hugging Face [`HfModelFetcher`], stubbable in tests).
pub trait ModelFetcher: Send + Sync {
    fn fetch<'a>(
        &'a self,
        repo_id: &'a str,
        filename: &'a str,
        dest_path: &'a Path,
        stop_flag: Arc<AtomicBool>,
        on_progress: ProgressCb,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

/// Default Hugging Face GGUF fetcher.
#[derive(Default)]
pub struct HfModelFetcher {
    client: HfClient,
}

impl ModelFetcher for HfModelFetcher {
    fn fetch<'a>(
        &'a self,
        repo_id: &'a str,
        filename: &'a str,
        dest_path: &'a Path,
        stop_flag: Arc<AtomicBool>,
        on_progress: ProgressCb,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.client
                .download_gguf_file(
                    repo_id,
                    filename,
                    dest_path,
                    stop_flag,
                    move |done, total| {
                        on_progress(done, total);
                    },
                )
                .await
        })
    }
}

/// Parameters for starting a new managed download.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub repo_id: String,
    pub filename: String,
    pub mmproj_filename: Option<String>,
    pub display_name: String,
    /// When `Some(model_id)`, completion attaches `dest_path` as `mmproj_path` on that model
    /// instead of registering a new standalone model.
    pub attach_to_model_id: Option<String>,
}

/// Summary of an active or paused download in [`DownloadManager`].
#[derive(Clone, Debug)]
pub struct DownloadSummary {
    pub key: DownloadKey,
    pub display_name: String,
    pub snapshot: DownloadSnapshot,
}

/// Handle returned when starting or restoring a download.
pub struct DownloadHandle {
    pub key: DownloadKey,
    pub display_name: String,
    pub initial: DownloadSnapshot,
    pub rx: broadcast::Receiver<DownloadSnapshot>,
}

pub(super) struct DownloadManagerInner {
    active: ActiveDownloadsMap,
    db: Database,
    models_dir: PathBuf,
    handle: tokio::runtime::Handle,
    fetcher: Arc<dyn ModelFetcher>,
}

/// Manages concurrent, resumable model and `mmproj` downloads without global mutable state.
#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<DownloadManagerInner>,
}

impl DownloadManager {
    pub fn new(db: Database, models_dir: PathBuf, handle: tokio::runtime::Handle) -> Self {
        Self::with_fetcher(db, models_dir, handle, Arc::new(HfModelFetcher::default()))
    }

    pub fn with_fetcher(
        db: Database,
        models_dir: PathBuf,
        handle: tokio::runtime::Handle,
        fetcher: Arc<dyn ModelFetcher>,
    ) -> Self {
        Self {
            inner: Arc::new(DownloadManagerInner {
                active: ActiveDownloadsMap::default(),
                db,
                models_dir,
                handle,
                fetcher,
            }),
        }
    }

    pub fn start_download(&self, req: DownloadRequest) -> DownloadHandle {
        let key = (req.repo_id.clone(), req.filename.clone());
        let display_name = req.display_name.clone();
        let (is_new, initial, rx) = self.inner.active.with_map(|map| {
            if let Some(existing) = map.get(&key) {
                (false, existing.snapshot(), existing.tx.subscribe())
            } else {
                let entry = ActiveDownloadEntry::new(req.clone(), 0, None, DownloadStatus::Active);
                let init = entry.snapshot();
                let rx = entry.tx.subscribe();
                map.insert(key.clone(), entry);
                (true, init, rx)
            }
        });

        if is_new {
            if req.attach_to_model_id.is_none() {
                let db_pend = self.inner.db.clone();
                let pend = PendingDownload {
                    repo_id: req.repo_id,
                    filename: req.filename,
                    mmproj_filename: req.mmproj_filename,
                    display_name: req.display_name,
                    total_bytes: None,
                };
                self.inner.handle.spawn_blocking(move || {
                    let _ = db_pend.upsert_pending_download(pend);
                });
            }
            self.launch_entry_task(&key);
        }

        DownloadHandle {
            key,
            display_name,
            initial,
            rx,
        }
    }

    pub fn pause(&self, key: &DownloadKey) {
        self.inner.active.with_entry(key, |entry| {
            entry.status = DownloadStatus::Paused;
            entry.stop_flag.store(true, Ordering::Relaxed);
            entry.emit();
        });
    }

    pub fn resume(&self, key: &DownloadKey) -> bool {
        let Some(was_paused) = self
            .inner
            .active
            .with_entry(key, |e| e.status == DownloadStatus::Paused)
        else {
            return false;
        };
        if was_paused {
            self.launch_entry_task(key);
        }
        true
    }

    pub fn cancel(&self, key: &DownloadKey) -> Result<()> {
        let Some((was_paused, repo_id, filename, mmproj_name)) =
            self.inner.active.with_entry(key, |entry| {
                let was_paused = entry.status == DownloadStatus::Paused;
                entry.status = DownloadStatus::Cancelled;
                entry.stop_flag.store(true, Ordering::Relaxed);
                (
                    was_paused,
                    entry.repo_id.clone(),
                    entry.filename.clone(),
                    entry.mmproj_filename.clone(),
                )
            })
        else {
            return Ok(());
        };

        if was_paused && let Some(mut entry) = self.inner.active.remove(key) {
            entry.status = DownloadStatus::Cancelled;
            entry.emit();
            let repo_dir = self.repo_dir(&repo_id);
            let dpath = repo_dir.join(&filename);
            let mpath = mmproj_name.as_deref().map(|m| repo_dir.join(m));
            let db_c = self.inner.db.clone();
            self.inner.handle.spawn(async move {
                remove_part_file(&dpath).await;
                if let Some(mp) = mpath {
                    remove_part_file(&mp).await;
                }
                let _ = tokio::task::spawn_blocking(move || {
                    db_c.remove_pending_download(&repo_id, &filename)
                })
                .await;
            });
        }
        Ok(())
    }

    pub fn restore_pending(&self) -> Vec<DownloadHandle> {
        let pending = self.inner.db.list_pending_downloads().unwrap_or_default();
        let mut handles = Vec::new();
        for item in pending {
            if let Some(handle) = self.try_restore_item(item) {
                handles.push(handle);
            }
        }
        handles
    }

    fn try_restore_item(&self, item: PendingDownload) -> Option<DownloadHandle> {
        let key = (item.repo_id.clone(), item.filename.clone());
        let repo_dir = self.repo_dir(&item.repo_id);
        let main_done = repo_dir.join(&item.filename);
        let main_part = part_file_path(&main_done);
        let mm_done = item.mmproj_filename.as_deref().map(|m| repo_dir.join(m));
        let mm_part = mm_done.as_deref().map(part_file_path);

        if !main_part.exists() && !mm_part.as_ref().is_some_and(|p| p.exists()) {
            let _ = self
                .inner
                .db
                .remove_pending_download(&item.repo_id, &item.filename);
            return None;
        }
        let existing = existing_bytes(&main_part, &main_done)
            + mm_part
                .as_deref()
                .zip(mm_done.as_deref())
                .map(|(p, d)| existing_bytes(p, d))
                .unwrap_or(0);

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

    pub fn subscribe(&self, key: &DownloadKey) -> Option<broadcast::Receiver<DownloadSnapshot>> {
        self.inner.active.subscribe(key)
    }

    pub fn is_repo_active(&self, repo_id: &str) -> bool {
        self.inner.active.is_repo_active(repo_id)
    }

    pub fn active_downloads(&self) -> Vec<DownloadSummary> {
        self.inner.active.summaries()
    }

    fn repo_dir(&self, repo_id: &str) -> PathBuf {
        self.inner.models_dir.join(repo_id.replace('/', "--"))
    }

    fn launch_entry_task(&self, key: &DownloadKey) {
        let Some((repo_id, filename, mmproj_name, display_name, attach_id, stop_flag, attempt_id)) =
            self.inner.active.with_entry(key, |entry| {
                let flag = Arc::new(AtomicBool::new(false));
                entry.stop_flag = flag.clone();
                entry.attempt_id = entry.attempt_id.wrapping_add(1);
                entry.status = DownloadStatus::Active;
                entry.emit();
                (
                    entry.repo_id.clone(),
                    entry.filename.clone(),
                    entry.mmproj_filename.clone(),
                    entry.display_name.clone(),
                    entry.attach_to_model_id.clone(),
                    flag,
                    entry.attempt_id,
                )
            })
        else {
            return;
        };

        let repo_dir = self.repo_dir(&repo_id);
        let dest_path = repo_dir.join(&filename);
        let mmproj_dest = mmproj_name.as_deref().map(|m| repo_dir.join(m));
        let inner = self.inner.clone();
        let key_owned = (repo_id, filename);

        self.inner.handle.spawn(async move {
            let res = run_fetch_job(
                &inner,
                FetchJob {
                    key: &key_owned,
                    dest_path: &dest_path,
                    mmproj_name: mmproj_name.as_deref(),
                    mmproj_dest: mmproj_dest.as_deref(),
                    persist_total: attach_id.is_none(),
                    stop_flag,
                },
            )
            .await;

            handle_fetch_completion(
                &inner,
                CompletionInfo {
                    key: key_owned,
                    display_name,
                    dest_path,
                    mmproj_dest,
                    attach_to_model_id: attach_id,
                    attempt_id,
                },
                res,
            )
            .await;
        });
    }
}

fn existing_bytes(part: &Path, done: &Path) -> u64 {
    std::fs::metadata(part)
        .or_else(|_| std::fs::metadata(done))
        .map(|m| m.len())
        .unwrap_or(0)
}
