//! Algebraic download status machine and single-mutex active download registry.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use super::{DownloadKey, DownloadRequest, DownloadSummary};

/// Mutually exclusive lifecycle state of a model download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadStatus {
    Active,
    Paused,
    Completed,
    Cancelled,
    Failed(String),
}

impl DownloadStatus {
    pub fn is_paused(&self) -> bool {
        matches!(self, Self::Paused)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed(_))
    }
}

/// Immutable point-in-time state snapshot broadcast to UI subscribers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadSnapshot {
    pub done: u64,
    pub total: Option<u64>,
    pub status: DownloadStatus,
}

pub(super) struct ActiveDownloadEntry {
    pub(super) repo_id: String,
    pub(super) filename: String,
    pub(super) mmproj_filename: Option<String>,
    pub(super) display_name: String,
    pub(super) attach_to_model_id: Option<String>,
    pub(super) done: u64,
    pub(super) total: Option<u64>,
    pub(super) status: DownloadStatus,
    pub(super) stop_flag: Arc<AtomicBool>,
    pub(super) attempt_id: u64,
    pub(super) tx: broadcast::Sender<DownloadSnapshot>,
}

impl ActiveDownloadEntry {
    pub(super) fn new(
        req: DownloadRequest,
        done: u64,
        total: Option<u64>,
        status: DownloadStatus,
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
            status,
            stop_flag: Arc::new(AtomicBool::new(false)),
            attempt_id: 0,
            tx,
        }
    }

    pub(super) fn snapshot(&self) -> DownloadSnapshot {
        DownloadSnapshot {
            done: self.done,
            total: self.total,
            status: self.status.clone(),
        }
    }

    pub(super) fn emit(&self) {
        let _ = self.tx.send(self.snapshot());
    }

    pub(super) fn update_progress(&mut self, done: u64, total: Option<u64>) {
        self.done = done;
        if total.is_some() {
            self.total = total;
        }
        self.emit();
    }
}

/// Thread-safe registry of active/paused downloads behind a single lock.
#[derive(Default)]
pub(super) struct ActiveDownloadsMap {
    inner: Mutex<HashMap<DownloadKey, ActiveDownloadEntry>>,
}

impl ActiveDownloadsMap {
    pub(super) fn with_map<R>(
        &self,
        f: impl FnOnce(&mut HashMap<DownloadKey, ActiveDownloadEntry>) -> R,
    ) -> R {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    pub(super) fn with_entry<R>(
        &self,
        key: &DownloadKey,
        f: impl FnOnce(&mut ActiveDownloadEntry) -> R,
    ) -> Option<R> {
        self.with_map(|map| map.get_mut(key).map(f))
    }

    pub(super) fn subscribe(
        &self,
        key: &DownloadKey,
    ) -> Option<broadcast::Receiver<DownloadSnapshot>> {
        self.with_map(|map| map.get(key).map(|e| e.tx.subscribe()))
    }

    pub(super) fn remove(&self, key: &DownloadKey) -> Option<ActiveDownloadEntry> {
        self.with_map(|map| map.remove(key))
    }

    pub(super) fn is_repo_active(&self, repo_id: &str) -> bool {
        self.with_map(|map| map.keys().any(|(r, _)| r == repo_id))
    }

    pub(super) fn summaries(&self) -> Vec<DownloadSummary> {
        self.with_map(|map| {
            map.iter()
                .map(|(key, entry)| DownloadSummary {
                    key: key.clone(),
                    display_name: entry.display_name.clone(),
                    snapshot: entry.snapshot(),
                })
                .collect()
        })
    }
}
