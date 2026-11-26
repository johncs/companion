//! Background fetch worker, dual-file progress aggregation, and completion registration.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::Result;

use super::state::DownloadStatus;
use super::{DownloadKey, DownloadManagerInner, ProgressCb};
use crate::huggingface::remove_part_file;
use crate::registry::ModelRegistryService;

pub(super) struct FetchJob<'a> {
    pub(super) key: &'a DownloadKey,
    pub(super) attempt_id: u64,
    pub(super) dest_path: &'a Path,
    pub(super) mmproj_name: Option<&'a str>,
    pub(super) mmproj_dest: Option<&'a Path>,
    pub(super) persist_total: bool,
    pub(super) stop_flag: Arc<AtomicBool>,
}

pub(super) struct CompletionInfo {
    pub(super) key: DownloadKey,
    pub(super) display_name: String,
    pub(super) dest_path: PathBuf,
    pub(super) mmproj_dest: Option<PathBuf>,
    pub(super) attach_to_model_id: Option<String>,
    pub(super) attempt_id: u64,
}

struct DualProgressAggregator {
    done_main: AtomicU64,
    total_main: AtomicU64,
    has_total_main: AtomicBool,
    done_mmproj: AtomicU64,
    total_mmproj: AtomicU64,
    has_total_mmproj: AtomicBool,
    emit: ProgressCb,
}

impl DualProgressAggregator {
    fn new(emit: ProgressCb) -> Arc<Self> {
        Arc::new(Self {
            done_main: AtomicU64::new(0),
            total_main: AtomicU64::new(0),
            has_total_main: AtomicBool::new(false),
            done_mmproj: AtomicU64::new(0),
            total_mmproj: AtomicU64::new(0),
            has_total_mmproj: AtomicBool::new(false),
            emit,
        })
    }

    fn record(&self, is_mmproj: bool, done: u64, total: Option<u64>) {
        if is_mmproj {
            self.done_mmproj.store(done, Ordering::Relaxed);
            if let Some(t) = total {
                self.total_mmproj.store(t, Ordering::Relaxed);
                self.has_total_mmproj.store(true, Ordering::Relaxed);
            }
        } else {
            self.done_main.store(done, Ordering::Relaxed);
            if let Some(t) = total {
                self.total_main.store(t, Ordering::Relaxed);
                self.has_total_main.store(true, Ordering::Relaxed);
            }
        }
        let both_totals = self.has_total_main.load(Ordering::Relaxed)
            && self.has_total_mmproj.load(Ordering::Relaxed);
        let tot = if both_totals {
            Some(
                self.total_main
                    .load(Ordering::Relaxed)
                    .saturating_add(self.total_mmproj.load(Ordering::Relaxed)),
            )
        } else {
            None
        };
        let done_sum = self
            .done_main
            .load(Ordering::Relaxed)
            .saturating_add(self.done_mmproj.load(Ordering::Relaxed));
        (self.emit)(done_sum, tot);
    }

    fn callback(self: &Arc<Self>, is_mmproj: bool) -> ProgressCb {
        let agg = self.clone();
        Arc::new(move |done, total| agg.record(is_mmproj, done, total))
    }
}

fn build_progress_cb(inner: &Arc<DownloadManagerInner>, job: &FetchJob<'_>) -> ProgressCb {
    let saved_total = Arc::new(AtomicU64::new(0));
    let inner_cb = inner.clone();
    let key_cb = job.key.clone();
    let persist_total = job.persist_total;
    let attempt_id = job.attempt_id;

    Arc::new(move |done: u64, total: Option<u64>| {
        if persist_total
            && let Some(t) = total
            && t > 0
            && saved_total.swap(t, Ordering::Relaxed) != t
        {
            let db_c = inner_cb.db.clone();
            let (r, f) = key_cb.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(err) = db_c.update_pending_download_total(&r, &f, t) {
                    tracing::warn!("Failed to update pending download total: {err}");
                }
            });
        }
        inner_cb.active.with_entry(&key_cb, |entry| {
            if entry.attempt_id == attempt_id && entry.status == DownloadStatus::Active {
                entry.update_progress(done, total);
            }
        });
    })
}

async fn fetch_or_reuse_existing(
    inner: &Arc<DownloadManagerInner>,
    repo_id: &str,
    filename: &str,
    dest_path: &Path,
    stop_flag: Arc<AtomicBool>,
    progress: ProgressCb,
) -> Result<()> {
    if let Ok(meta) = tokio::fs::metadata(dest_path).await
        && meta.is_file()
        && meta.len() > 0
    {
        progress(meta.len(), Some(meta.len()));
        return Ok(());
    }
    let res = inner
        .fetcher
        .fetch(repo_id, filename, dest_path, stop_flag.clone(), progress)
        .await;
    if res.is_err() {
        stop_flag.store(true, Ordering::Relaxed);
    }
    res
}

pub(super) async fn run_fetch_job(
    inner: &Arc<DownloadManagerInner>,
    job: FetchJob<'_>,
) -> Result<()> {
    let update_progress = build_progress_cb(inner, &job);
    let (repo_id, filename) = (&job.key.0, &job.key.1);

    if let (Some(mm_file), Some(mm_path)) = (job.mmproj_name, job.mmproj_dest) {
        let agg = DualProgressAggregator::new(update_progress);
        let f_main = fetch_or_reuse_existing(
            inner,
            repo_id,
            filename,
            job.dest_path,
            job.stop_flag.clone(),
            agg.callback(false),
        );
        let f_mm = fetch_or_reuse_existing(
            inner,
            repo_id,
            mm_file,
            mm_path,
            job.stop_flag.clone(),
            agg.callback(true),
        );
        let ((), ()) = tokio::try_join!(f_main, f_mm)?;
        Ok(())
    } else {
        inner
            .fetcher
            .fetch(
                repo_id,
                filename,
                job.dest_path,
                job.stop_flag,
                update_progress,
            )
            .await
    }
}

pub(super) async fn handle_fetch_completion(
    inner: &DownloadManagerInner,
    info: CompletionInfo,
    res: Result<()>,
) {
    match res {
        Ok(()) => {
            let stale_or_cancelled = inner
                .active
                .with_entry(&info.key, |e| {
                    e.attempt_id != info.attempt_id || e.status == DownloadStatus::Cancelled
                })
                .unwrap_or(true);
            if stale_or_cancelled {
                return;
            }
            let reg_res = register_completed_model(inner, &info).await;
            let removed = inner.active.with_map(|map| {
                let matches = map.get(&info.key).is_some_and(|e| {
                    e.attempt_id == info.attempt_id && e.status != DownloadStatus::Cancelled
                });
                if matches { map.remove(&info.key) } else { None }
            });
            if let Some(mut entry) = removed {
                entry.status = match reg_res {
                    Ok(()) => DownloadStatus::Completed,
                    Err(err) => DownloadStatus::Failed(err),
                };
                entry.emit();
            }
        }
        Err(err) => handle_fetch_error(inner, info, err).await,
    }
}

async fn register_completed_model(
    inner: &DownloadManagerInner,
    info: &CompletionInfo,
) -> Result<(), String> {
    let db_reg = inner.db.clone();
    let (rid_reg, fname_reg) = info.key.clone();
    let display_name = info.display_name.clone();
    let dest_path = info.dest_path.clone();
    let mmproj_dest = info.mmproj_dest.clone();
    let attach_to_model_id = info.attach_to_model_id.clone();

    tokio::task::spawn_blocking(move || {
        if let Some(model_id) = attach_to_model_id {
            let mut model = db_reg
                .get_model(&model_id)
                .map_err(|e| format!("Failed to load model record: {e}"))?
                .ok_or_else(|| format!("Model {model_id} not found"))?;
            model.mmproj_path = Some(dest_path.to_string_lossy().to_string());
            db_reg
                .upsert_model(&model)
                .map_err(|e| format!("Failed to attach vision projector: {e}"))?;
        } else {
            let record = ModelRegistryService::register_local_gguf(
                &db_reg,
                Some(display_name),
                &dest_path,
                mmproj_dest.as_deref(),
                Some(rid_reg.clone()),
            )
            .map_err(|e| format!("Failed to register downloaded model: {e}"))?;
            if db_reg.get_active_model_id().ok().flatten().is_none()
                && let Err(err) = db_reg.set_active_model_id(Some(&record.id))
            {
                tracing::warn!("Failed to set active model {}: {err}", record.id);
            }
            if let Err(err) = db_reg.remove_pending_download(&rid_reg, &fname_reg) {
                tracing::warn!("Failed to clear pending download: {err}");
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("Model registration task failed: {e}"))
    .and_then(|r| r)
}

async fn handle_fetch_error(
    inner: &DownloadManagerInner,
    info: CompletionInfo,
    err: anyhow::Error,
) {
    enum ErrorAction {
        Ignore,
        EmitPaused,
        Finish(Box<super::state::ActiveDownloadEntry>, bool),
    }

    let action = inner.active.with_map(|map| {
        let Some(entry) = map.get_mut(&info.key) else {
            return ErrorAction::Ignore;
        };
        if entry.attempt_id != info.attempt_id {
            return ErrorAction::Ignore;
        }
        if entry.status == DownloadStatus::Paused {
            entry.emit();
            return ErrorAction::EmitPaused;
        }
        let was_cancelled = entry.status == DownloadStatus::Cancelled;
        match map.remove(&info.key) {
            Some(removed) => ErrorAction::Finish(Box::new(removed), was_cancelled),
            None => ErrorAction::Ignore,
        }
    });

    let ErrorAction::Finish(mut entry, was_cancelled) = action else {
        return;
    };
    if was_cancelled {
        cleanup_cancelled_download(inner, info).await;
        entry.status = DownloadStatus::Cancelled;
    } else {
        entry.status = DownloadStatus::Failed(err.to_string());
    }
    entry.emit();
}

async fn cleanup_cancelled_download(inner: &DownloadManagerInner, info: CompletionInfo) {
    remove_part_file(&info.dest_path).await;
    if let Some(mp) = info.mmproj_dest {
        remove_part_file(&mp).await;
    }
    if info.attach_to_model_id.is_none() {
        let db_c = inner.db.clone();
        let (r, f) = info.key;
        if let Ok(Err(err)) =
            tokio::task::spawn_blocking(move || db_c.remove_pending_download(&r, &f)).await
        {
            tracing::warn!("Failed to remove cancelled pending download: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn dual_progress_aggregator_sums_main_and_mmproj_bytes() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        let agg = DualProgressAggregator::new(Arc::new(move |done, total| {
            if let Ok(mut g) = seen_cb.lock() {
                g.push((done, total));
            }
        }));

        agg.record(false, 50, Some(100));
        agg.record(true, 20, Some(40));
        agg.record(false, 100, None);

        let events = seen.lock().unwrap().clone();
        assert_eq!(events, vec![(50, None), (70, Some(140)), (120, Some(140))]);
    }
}
