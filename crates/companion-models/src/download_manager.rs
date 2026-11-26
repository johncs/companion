//! Injectable, GTK-free resumable model & vision-projector download manager.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::sync::broadcast;

use companion_core::runtime;
use crate::huggingface::{download_gguf_file, remove_part_file};
use crate::registry::ModelRegistryService;
use companion_storage::{Database, PendingDownload};

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
pub struct HfModelFetcher;

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
            download_gguf_file(repo_id, filename, dest_path, stop_flag, move |done, total| {
                on_progress(done, total);
            })
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

/// Immutable point-in-time state snapshot broadcast to UI subscribers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadSnapshot {
    pub done: u64,
    pub total: Option<u64>,
    pub is_paused: bool,
    pub finished: bool,
    pub cancelled: bool,
    pub error: Option<String>,
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
    pub rx: broadcast::Receiver<DownloadSnapshot>,
}

struct ActiveDownloadEntry {
    repo_id: String,
    filename: String,
    mmproj_filename: Option<String>,
    display_name: String,
    attach_to_model_id: Option<String>,
    done: u64,
    total: Option<u64>,
    is_paused: bool,
    is_cancelled: bool,
    stop_flag: Arc<AtomicBool>,
    tx: broadcast::Sender<DownloadSnapshot>,
}

impl ActiveDownloadEntry {
    fn new(
        req: DownloadRequest,
        done: u64,
        total: Option<u64>,
        is_paused: bool,
    ) -> Self {
        let (tx, _) = broadcast::channel(32);
        Self {
            repo_id: req.repo_id,
            filename: req.filename,
            mmproj_filename: req.mmproj_filename,
            display_name: req.display_name,
            attach_to_model_id: req.attach_to_model_id,
            done,
            total,
            is_paused,
            is_cancelled: false,
            stop_flag: Arc::new(AtomicBool::new(false)),
            tx,
        }
    }

    fn snapshot(&self, finished: bool, error: Option<String>) -> DownloadSnapshot {
        DownloadSnapshot {
            done: self.done,
            total: self.total,
            is_paused: self.is_paused,
            finished,
            cancelled: self.is_cancelled,
            error,
        }
    }

    fn emit(&self, finished: bool, error: Option<String>) {
        let _ = self.tx.send(self.snapshot(finished, error));
    }

    fn update_progress(&mut self, done: u64, total: Option<u64>) {
        self.done = done;
        if total.is_some() {
            self.total = total;
        }
        self.emit(false, None);
    }
}

struct DownloadManagerInner {
    active: Mutex<HashMap<DownloadKey, Arc<Mutex<ActiveDownloadEntry>>>>,
    db: Database,
    models_dir: PathBuf,
    fetcher: Arc<dyn ModelFetcher>,
}

/// Manages concurrent, resumable model and `mmproj` downloads without global mutable state.
#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<DownloadManagerInner>,
}

impl DownloadManager {
    pub fn new(db: Database, models_dir: PathBuf) -> Self {
        Self::with_fetcher(db, models_dir, Arc::new(HfModelFetcher))
    }

    pub fn with_fetcher(
        db: Database,
        models_dir: PathBuf,
        fetcher: Arc<dyn ModelFetcher>,
    ) -> Self {
        Self {
            inner: Arc::new(DownloadManagerInner {
                active: Mutex::new(HashMap::new()),
                db,
                models_dir,
                fetcher,
            }),
        }
    }

    pub fn start_download(&self, req: DownloadRequest) -> DownloadHandle {
        let key = (req.repo_id.clone(), req.filename.clone());
        let display_name = req.display_name.clone();
        let (entry_arc, is_new, rx) = {
            let mut map = self
                .inner
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = map.get(&key) {
                let rx = existing
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .tx
                    .subscribe();
                (existing.clone(), false, rx)
            } else {
                let entry = ActiveDownloadEntry::new(req.clone(), 0, None, false);
                let rx = entry.tx.subscribe();
                let arc = Arc::new(Mutex::new(entry));
                map.insert(key.clone(), arc.clone());
                (arc, true, rx)
            }
        };

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
                runtime().spawn_blocking(move || {
                    let _ = db_pend.upsert_pending_download(pend);
                });
            }
            self.launch_entry_task(entry_arc);
        }

        DownloadHandle {
            key,
            display_name,
            rx,
        }
    }

    pub fn pause(&self, key: &DownloadKey) {
        let entry_opt = self
            .inner
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        if let Some(entry_arc) = entry_opt
            && let Ok(mut entry) = entry_arc.lock()
        {
            entry.is_paused = true;
            entry.stop_flag.store(true, Ordering::Relaxed);
            entry.emit(false, None);
        }
    }

    pub fn resume(&self, key: &DownloadKey) -> bool {
        let entry_opt = self
            .inner
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        let Some(entry_arc) = entry_opt else {
            return false;
        };
        let is_paused = entry_arc
            .lock()
            .map(|e| e.is_paused)
            .unwrap_or(false);
        if is_paused {
            self.launch_entry_task(entry_arc);
        }
        true
    }

    pub fn cancel(&self, key: &DownloadKey) -> Result<()> {
        let entry_opt = self
            .inner
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        let Some(entry_arc) = entry_opt else {
            return Ok(());
        };

        let (was_paused, repo_id, filename, mmproj_name) = {
            let mut entry = entry_arc
                .lock()
                .map_err(|_| anyhow::anyhow!("Download entry mutex poisoned"))?;
            let was_paused = entry.is_paused;
            entry.is_cancelled = true;
            entry.is_paused = false;
            entry.stop_flag.store(true, Ordering::Relaxed);
            (
                was_paused,
                entry.repo_id.clone(),
                entry.filename.clone(),
                entry.mmproj_filename.clone(),
            )
        };

        if was_paused {
            self.inner
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(key);
            if let Ok(entry) = entry_arc.lock() {
                entry.emit(true, None);
            }
            let repo_dir = self.repo_dir(&repo_id);
            let dpath = repo_dir.join(&filename);
            let mpath = mmproj_name.as_deref().map(|m| repo_dir.join(m));
            let db_c = self.inner.db.clone();
            let r = repo_id;
            let f = filename;
            runtime().spawn(async move {
                remove_part_file(&dpath).await;
                if let Some(mp) = mpath {
                    remove_part_file(&mp).await;
                }
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = db_c.remove_pending_download(&r, &f);
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
            let key = (item.repo_id.clone(), item.filename.clone());
            let repo_dir = self.repo_dir(&item.repo_id);
            let main_part = repo_dir.join(&item.filename).with_extension("gguf.part");
            let main_done = repo_dir.join(&item.filename);
            let mm_part = item
                .mmproj_filename
                .as_deref()
                .map(|m| repo_dir.join(m).with_extension("gguf.part"));
            let mm_done = item.mmproj_filename.as_deref().map(|m| repo_dir.join(m));

            let has_part = main_part.exists() || mm_part.as_ref().is_some_and(|p| p.exists());
            if !has_part {
                let _ = self
                    .inner
                    .db
                    .remove_pending_download(&item.repo_id, &item.filename);
                continue;
            }
            let part_bytes = |part: &Path, done: &Path| {
                std::fs::metadata(part)
                    .or_else(|_| std::fs::metadata(done))
                    .map(|m| m.len())
                    .unwrap_or(0)
            };
            let existing = part_bytes(&main_part, &main_done)
                + mm_part
                    .as_ref()
                    .zip(mm_done.as_ref())
                    .map(|(p, d)| part_bytes(p, d))
                    .unwrap_or(0);

            let mut map = self
                .inner
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if map.contains_key(&key) {
                continue;
            }
            let display_name = item.display_name.clone();
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
                true,
            );
            let rx = entry.tx.subscribe();
            map.insert(key.clone(), Arc::new(Mutex::new(entry)));
            handles.push(DownloadHandle {
                key,
                display_name,
                rx,
            });
        }
        handles
    }

    pub fn subscribe(&self, key: &DownloadKey) -> Option<broadcast::Receiver<DownloadSnapshot>> {
        self.inner
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .and_then(|e| e.lock().ok().map(|g| g.tx.subscribe()))
    }

    pub fn is_repo_active(&self, repo_id: &str) -> bool {
        self.inner
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .any(|(r, _)| r == repo_id)
    }

    pub fn active_downloads(&self) -> Vec<DownloadSummary> {
        let map = self
            .inner
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.iter()
            .filter_map(|(key, arc)| {
                arc.lock().ok().map(|e| DownloadSummary {
                    key: key.clone(),
                    display_name: e.display_name.clone(),
                    snapshot: e.snapshot(false, None),
                })
            })
            .collect()
    }

    fn repo_dir(&self, repo_id: &str) -> PathBuf {
        self.inner.models_dir.join(repo_id.replace('/', "--"))
    }

    fn launch_entry_task(&self, entry_arc: Arc<Mutex<ActiveDownloadEntry>>) {
        let (repo_id, filename, mmproj_name, display_name, attach_id, stop_flag) = {
            let mut entry = entry_arc.lock().unwrap_or_else(|e| e.into_inner());
            let flag = Arc::new(AtomicBool::new(false));
            entry.stop_flag = flag.clone();
            entry.is_paused = false;
            entry.emit(false, None);
            (
                entry.repo_id.clone(),
                entry.filename.clone(),
                entry.mmproj_filename.clone(),
                entry.display_name.clone(),
                entry.attach_to_model_id.clone(),
                flag,
            )
        };

        let repo_dir = self.repo_dir(&repo_id);
        let dest_path = repo_dir.join(&filename);
        let mmproj_dest = mmproj_name.as_deref().map(|m| repo_dir.join(m));
        let inner = self.inner.clone();

        runtime().spawn(async move {
            let job = FetchJob {
                repo_id: &repo_id,
                filename: &filename,
                dest_path: &dest_path,
                mmproj_name: mmproj_name.as_deref(),
                mmproj_dest: mmproj_dest.as_deref(),
                persist_total: attach_id.is_none(),
                stop_flag,
            };
            let res = run_fetch_job(&inner, &entry_arc, job).await;

            handle_fetch_completion(
                &inner,
                &entry_arc,
                CompletionInfo {
                    repo_id,
                    filename,
                    display_name,
                    dest_path,
                    mmproj_dest,
                    attach_to_model_id: attach_id,
                },
                res,
            )
            .await;
        });
    }
}

struct FetchJob<'a> {
    repo_id: &'a str,
    filename: &'a str,
    dest_path: &'a Path,
    mmproj_name: Option<&'a str>,
    mmproj_dest: Option<&'a Path>,
    persist_total: bool,
    stop_flag: Arc<AtomicBool>,
}

struct CompletionInfo {
    repo_id: String,
    filename: String,
    display_name: String,
    dest_path: PathBuf,
    mmproj_dest: Option<PathBuf>,
    attach_to_model_id: Option<String>,
}

async fn run_fetch_job(
    inner: &DownloadManagerInner,
    entry_arc: &Arc<Mutex<ActiveDownloadEntry>>,
    job: FetchJob<'_>,
) -> Result<()> {
    let saved_total = Arc::new(AtomicU64::new(0));
    let update_progress: ProgressCb = {
        let entry_arc = entry_arc.clone();
        let db_t = inner.db.clone();
        let rid_t = job.repo_id.to_string();
        let fname_t = job.filename.to_string();
        let saved_total = saved_total.clone();
        let persist_total = job.persist_total;
        Arc::new(move |done: u64, total: Option<u64>| {
            if persist_total
                && let Some(t) = total
                && t > 0
                && saved_total.swap(t, Ordering::Relaxed) != t
            {
                let db_c = db_t.clone();
                let r = rid_t.clone();
                let f = fname_t.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = db_c.update_pending_download_total(&r, &f, t);
                });
            }
            if let Ok(mut entry) = entry_arc.lock() {
                entry.update_progress(done, total);
            }
        })
    };

    if let (Some(mm_file), Some(mm_path)) = (job.mmproj_name, job.mmproj_dest) {
        let dm = Arc::new(AtomicU64::new(0));
        let tm = Arc::new(AtomicU64::new(0));
        let dmm = Arc::new(AtomicU64::new(0));
        let tmm = Arc::new(AtomicU64::new(0));

        let cb_main: ProgressCb = {
            let (dm, tm, dmm, tmm, up) = (
                dm.clone(),
                tm.clone(),
                dmm.clone(),
                tmm.clone(),
                update_progress.clone(),
            );
            Arc::new(move |done, total| {
                dm.store(done, Ordering::Relaxed);
                if let Some(t) = total {
                    tm.store(t, Ordering::Relaxed);
                }
                let tot = tm.load(Ordering::Relaxed) + tmm.load(Ordering::Relaxed);
                up(
                    dm.load(Ordering::Relaxed) + dmm.load(Ordering::Relaxed),
                    (tot > 0).then_some(tot),
                );
            })
        };
        let cb_mm: ProgressCb = Arc::new(move |done, total| {
            dmm.store(done, Ordering::Relaxed);
            if let Some(t) = total {
                tmm.store(t, Ordering::Relaxed);
            }
            let tot = tm.load(Ordering::Relaxed) + tmm.load(Ordering::Relaxed);
            update_progress(
                dm.load(Ordering::Relaxed) + dmm.load(Ordering::Relaxed),
                (tot > 0).then_some(tot),
            );
        });

        let f_main = inner.fetcher.fetch(
            job.repo_id,
            job.filename,
            job.dest_path,
            job.stop_flag.clone(),
            cb_main,
        );
        let f_mm =
            inner
                .fetcher
                .fetch(job.repo_id, mm_file, mm_path, job.stop_flag, cb_mm);
        let (r_main, r_mm) = tokio::join!(f_main, f_mm);
        r_main.and(r_mm)
    } else {
        inner
            .fetcher
            .fetch(
                job.repo_id,
                job.filename,
                job.dest_path,
                job.stop_flag,
                update_progress,
            )
            .await
    }
}

async fn handle_fetch_completion(
    inner: &DownloadManagerInner,
    entry_arc: &Arc<Mutex<ActiveDownloadEntry>>,
    info: CompletionInfo,
    res: Result<()>,
) {
    let CompletionInfo {
        repo_id,
        filename,
        display_name,
        dest_path,
        mmproj_dest,
        attach_to_model_id,
    } = info;
    match res {
        Ok(()) => {
            let db_reg = inner.db.clone();
            let rid_reg = repo_id.clone();
            let fname_reg = filename.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Some(model_id) = attach_to_model_id {
                    if let Ok(Some(mut model)) = db_reg.get_model(&model_id) {
                        model.mmproj_path = Some(dest_path.to_string_lossy().to_string());
                        let _ = db_reg.upsert_model(&model);
                    }
                } else {
                    if let Ok(record) = ModelRegistryService::register_local_gguf(
                        &db_reg,
                        Some(display_name),
                        &dest_path,
                        mmproj_dest.as_deref(),
                        Some(rid_reg.clone()),
                    ) && db_reg.get_active_model_id().ok().flatten().is_none()
                    {
                        let _ = db_reg.set_active_model_id(Some(&record.id));
                    }
                    let _ = db_reg.remove_pending_download(&rid_reg, &fname_reg);
                }
            })
            .await;

            inner
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(repo_id, filename));
            if let Ok(entry) = entry_arc.lock() {
                entry.emit(true, None);
            }
        }
        Err(err) => {
            let (is_paused, is_cancelled) = entry_arc
                .lock()
                .map(|e| (e.is_paused, e.is_cancelled))
                .unwrap_or((false, false));
            if is_paused && !is_cancelled {
                if let Ok(entry) = entry_arc.lock() {
                    entry.emit(false, None);
                }
                return;
            }

            inner
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(repo_id.clone(), filename.clone()));
            if is_cancelled {
                remove_part_file(&dest_path).await;
                if let Some(mp) = mmproj_dest {
                    remove_part_file(&mp).await;
                }
                if attach_to_model_id.is_none() {
                    let db_c = inner.db.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = db_c.remove_pending_download(&repo_id, &filename);
                    })
                    .await;
                }
                if let Ok(entry) = entry_arc.lock() {
                    entry.emit(true, None);
                }
                return;
            }

            if let Ok(entry) = entry_arc.lock() {
                entry.emit(true, Some(err.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockFetcher {
        pause_on_first: AtomicBool,
    }

    impl ModelFetcher for MockFetcher {
        fn fetch<'a>(
            &'a self,
            _repo_id: &'a str,
            _filename: &'a str,
            dest_path: &'a Path,
            stop_flag: Arc<AtomicBool>,
            on_progress: ProgressCb,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            Box::pin(async move {
                if let Some(parent) = dest_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let part = dest_path.with_extension("gguf.part");
                std::fs::write(&part, b"partial")?;
                on_progress(50, Some(100));

                if self.pause_on_first.swap(false, Ordering::SeqCst) {
                    while !stop_flag.load(Ordering::Relaxed) {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    anyhow::bail!("paused");
                }

                std::fs::write(dest_path, b"GGUF")?;
                let _ = std::fs::remove_file(&part);
                on_progress(100, Some(100));
                Ok(())
            })
        }
    }

    #[test]
    fn download_manager_start_pause_resume_and_complete() {
        let dir = std::env::temp_dir().join(format!("companion_dm_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("test.db")).unwrap();
        let mgr = DownloadManager::with_fetcher(
            db.clone(),
            dir.join("models"),
            Arc::new(MockFetcher {
                pause_on_first: AtomicBool::new(true),
            }),
        );

        let mut handle = mgr.start_download(DownloadRequest {
            repo_id: "org/test".into(),
            filename: "model.gguf".into(),
            mmproj_filename: None,
            display_name: "Test".into(),
            attach_to_model_id: None,
        });

        runtime().block_on(async {
            let mut paused_once = false;
            while let Ok(snap) = handle.rx.recv().await {
                if snap.done == 50 && !snap.is_paused && !paused_once {
                    paused_once = true;
                    mgr.pause(&handle.key);
                } else if snap.is_paused {
                    assert!(mgr.resume(&handle.key));
                } else if snap.finished {
                    assert!(!snap.cancelled);
                    assert!(snap.error.is_none());
                    break;
                }
            }
        });

        assert_eq!(db.list_models().unwrap().len(), 1);
        assert!(db.list_pending_downloads().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_manager_cancel_cleans_part_files_active_and_paused() {
        let dir = std::env::temp_dir().join(format!("companion_dm_cancel_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("test.db")).unwrap();
        let mgr = DownloadManager::with_fetcher(
            db.clone(),
            dir.join("models"),
            Arc::new(MockFetcher {
                pause_on_first: AtomicBool::new(true),
            }),
        );

        let mut handle = mgr.start_download(DownloadRequest {
            repo_id: "org/cancel".into(),
            filename: "m.gguf".into(),
            mmproj_filename: None,
            display_name: "Cancel".into(),
            attach_to_model_id: None,
        });

        runtime().block_on(async {
            while let Ok(snap) = handle.rx.recv().await {
                if snap.done == 50 && !snap.is_paused {
                    mgr.pause(&handle.key);
                } else if snap.is_paused {
                    mgr.cancel(&handle.key).unwrap();
                } else if snap.finished {
                    assert!(snap.cancelled);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        });

        let part = dir.join("models/org--cancel/m.gguf.part");
        assert!(!part.exists());
        assert!(db.list_pending_downloads().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_pending_restores_part_files_and_prunes_stale_entries() {
        let dir = std::env::temp_dir().join(format!("companion_dm_restore_{}", uuid::Uuid::new_v4()));
        let models_dir = dir.join("models");
        std::fs::create_dir_all(models_dir.join("org--keep")).unwrap();
        std::fs::write(models_dir.join("org--keep/keep.gguf.part"), b"1234").unwrap();

        let db = Database::open(&dir.join("test.db")).unwrap();
        db.upsert_pending_download(PendingDownload {
            repo_id: "org/keep".into(),
            filename: "keep.gguf".into(),
            mmproj_filename: None,
            display_name: "Keep".into(),
            total_bytes: Some(100),
        })
        .unwrap();
        db.upsert_pending_download(PendingDownload {
            repo_id: "org/stale".into(),
            filename: "gone.gguf".into(),
            mmproj_filename: None,
            display_name: "Stale".into(),
            total_bytes: Some(100),
        })
        .unwrap();

        let mgr = DownloadManager::with_fetcher(
            db.clone(),
            models_dir,
            Arc::new(MockFetcher {
                pause_on_first: AtomicBool::new(false),
            }),
        );
        let handles = mgr.restore_pending();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].key, ("org/keep".into(), "keep.gguf".into()));
        assert_eq!(db.list_pending_downloads().unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mmproj_attach_updates_existing_model_without_duplicating() {
        let dir = std::env::temp_dir().join(format!("companion_dm_mmproj_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("test.db")).unwrap();
        db.upsert_model(&companion_storage::ModelRecord {
            id: "m-vision".into(),
            display_name: "Vision Base".into(),
            provider_type: "llama_cpp".into(),
            file_path: Some("/tmp/base.gguf".into()),
            mmproj_path: None,
            hf_repo: Some("org/vision".into()),
            quantization: None,
            supports_thinking: false,
            default_params: companion_core::model_params::ModelParams::default(),
            avg_tokens_per_sec: None,
        })
        .unwrap();

        let mgr = DownloadManager::with_fetcher(
            db.clone(),
            dir.join("models"),
            Arc::new(MockFetcher {
                pause_on_first: AtomicBool::new(false),
            }),
        );
        let mut handle = mgr.start_download(DownloadRequest {
            repo_id: "org/vision".into(),
            filename: "mmproj.gguf".into(),
            mmproj_filename: None,
            display_name: "Vision Base".into(),
            attach_to_model_id: Some("m-vision".into()),
        });

        runtime().block_on(async {
            while let Ok(snap) = handle.rx.recv().await {
                if snap.finished {
                    assert!(snap.error.is_none());
                    break;
                }
            }
        });

        let models = db.list_models().unwrap();
        assert_eq!(models.len(), 1);
        assert!(models[0]
            .mmproj_path
            .as_deref()
            .is_some_and(|p| p.ends_with("mmproj.gguf")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
