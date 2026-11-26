//! Injectable, GTK-free resumable model & vision-projector download manager.

mod restore;
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

use crate::huggingface::{HfClient, remove_part_file};
use companion_storage::{Database, PendingDownload};

pub use state::DownloadError;

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

impl HfModelFetcher {
    pub fn with_client(client: HfClient) -> Self {
        Self { client }
    }
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
            let pending = req.attach_to_model_id.is_none().then_some(PendingDownload {
                repo_id: req.repo_id,
                filename: req.filename,
                mmproj_filename: req.mmproj_filename,
                display_name: req.display_name,
                total_bytes: None,
            });
            self.launch_entry_task(&key, pending);
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
            if entry.status == DownloadStatus::Active {
                entry.status = DownloadStatus::Paused;
                entry.stop_flag.store(true, Ordering::Relaxed);
                entry.emit();
            }
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
            self.launch_entry_task(key, None);
        }
        true
    }

    pub fn cancel(&self, key: &DownloadKey) {
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
            return;
        };

        if was_paused && let Some(mut entry) = self.inner.active.remove(key) {
            entry.status = DownloadStatus::Cancelled;
            entry.emit();
            let repo_dir = self.repo_dir(&repo_id);
            let dpath = join_safe_filename(&repo_dir, &filename);
            let mpath = mmproj_name
                .as_deref()
                .map(|m| join_safe_filename(&repo_dir, m));
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

    pub(super) fn repo_dir(&self, repo_id: &str) -> PathBuf {
        let safe_repo: String = repo_id
            .split('/')
            .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
            .collect::<Vec<_>>()
            .join("--");
        let leaf = if safe_repo.is_empty() {
            "unknown-repo"
        } else {
            safe_repo.as_str()
        };
        self.inner.models_dir.join(leaf)
    }

    fn launch_entry_task(&self, key: &DownloadKey, initial_pending: Option<PendingDownload>) {
        let (done_tx, done_rx) = tokio::sync::watch::channel(false);
        let Some((
            repo_id,
            filename,
            mmproj_name,
            display_name,
            attach_id,
            stop_flag,
            attempt_id,
            prev_worker_done,
        )) = self.inner.active.with_entry(key, |entry| {
            let flag = Arc::new(AtomicBool::new(false));
            let prev_done = entry.worker_done.replace(done_rx);
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
                prev_done,
            )
        })
        else {
            return;
        };

        let repo_dir = self.repo_dir(&repo_id);
        let dest_path = join_safe_filename(&repo_dir, &filename);
        let mmproj_dest = mmproj_name
            .as_deref()
            .map(|m| join_safe_filename(&repo_dir, m));
        let inner = self.inner.clone();
        let key_owned = (repo_id, filename);

        self.inner.handle.spawn(async move {
            if let Some(mut prev_done) = prev_worker_done {
                let _ = prev_done.wait_for(|&done| done).await;
            }
            if let Some(pend) = initial_pending {
                let db_pend = inner.db.clone();
                match tokio::task::spawn_blocking(move || db_pend.upsert_pending_download(pend))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => tracing::warn!("Failed to persist pending download: {err}"),
                    Err(err) => tracing::warn!("Pending download DB task failed: {err}"),
                }
            }
            let cancelled_before_start = inner
                .active
                .with_entry(&key_owned, |e| {
                    e.attempt_id != attempt_id || e.status != DownloadStatus::Active
                })
                .unwrap_or(true);
            let res = if cancelled_before_start {
                Err(anyhow::anyhow!("Download paused or cancelled"))
            } else {
                run_fetch_job(
                    &inner,
                    FetchJob {
                        key: &key_owned,
                        attempt_id,
                        dest_path: &dest_path,
                        mmproj_name: mmproj_name.as_deref(),
                        mmproj_dest: mmproj_dest.as_deref(),
                        persist_total: attach_id.is_none(),
                        stop_flag,
                    },
                )
                .await
            };

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
            let _ = done_tx.send(true);
        });
    }
}

pub(super) fn join_safe_filename(dir: &Path, raw: &str) -> PathBuf {
    let mut out = dir.to_path_buf();
    for comp in Path::new(raw).components() {
        if let std::path::Component::Normal(seg) = comp {
            out.push(seg);
        }
    }
    if out == dir {
        out.push("model.gguf");
    }
    out
}
