//! Pure download state machine, snapshot struct, and active-download registry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DownloadSnapshot {
    pub(super) done: u64,
    pub(super) total: Option<u64>,
    pub(super) is_paused: bool,
    pub(super) finished: bool,
    pub(super) cancelled: bool,
    pub(super) error: Option<String>,
}

pub(super) struct ActiveDownloadEntry {
    pub(super) repo_id: String,
    pub(super) filename: String,
    pub(super) mmproj_filename: Option<String>,
    pub(super) display_name: String,
    pub(super) done: u64,
    pub(super) total: Option<u64>,
    pub(super) is_paused: bool,
    pub(super) is_cancelled: bool,
    pub(super) stop_flag: Arc<AtomicBool>,
    pub(super) tx: tokio::sync::broadcast::Sender<DownloadSnapshot>,
}

impl ActiveDownloadEntry {
    pub(super) fn new(
        repo_id: String,
        filename: String,
        mmproj_filename: Option<String>,
        display_name: String,
        done: u64,
        total: Option<u64>,
        is_paused: bool,
    ) -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(32);
        Self {
            repo_id,
            filename,
            mmproj_filename,
            display_name,
            done,
            total,
            is_paused,
            is_cancelled: false,
            stop_flag: Arc::new(AtomicBool::new(false)),
            tx,
        }
    }

    pub(super) fn snapshot(&self, finished: bool, error: Option<String>) -> DownloadSnapshot {
        DownloadSnapshot {
            done: self.done,
            total: self.total,
            is_paused: self.is_paused,
            finished,
            cancelled: self.is_cancelled,
            error,
        }
    }

    pub(super) fn emit(&self, finished: bool, error: Option<String>) {
        let _ = self.tx.send(self.snapshot(finished, error));
    }

    pub(super) fn pause(&mut self) {
        self.is_paused = true;
        self.stop_flag.store(true, Ordering::Relaxed);
        self.emit(false, None);
    }

    /// Mark entry as cancelled and return whether it was previously paused.
    pub(super) fn cancel(&mut self) -> bool {
        let was_paused = self.is_paused;
        self.is_cancelled = true;
        self.is_paused = false;
        self.stop_flag.store(true, Ordering::Relaxed);
        was_paused
    }

    pub(super) fn update_progress(&mut self, done: u64, total: Option<u64>) {
        self.done = done;
        if total.is_some() {
            self.total = total;
        }
        self.emit(false, None);
    }
}

pub(super) type ActiveDownloadMap = HashMap<(String, String), Arc<Mutex<ActiveDownloadEntry>>>;

pub(super) static ACTIVE_DOWNLOADS: LazyLock<Mutex<ActiveDownloadMap>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_entry_transitions_progress_pause_and_cancel() {
        let mut entry = ActiveDownloadEntry::new(
            "org/repo".into(),
            "model.gguf".into(),
            None,
            "Model".into(),
            0,
            Some(100),
            false,
        );
        let mut rx = entry.tx.subscribe();

        entry.update_progress(40, Some(100));
        let snap1 = rx.try_recv().unwrap();
        assert_eq!(snap1.done, 40);
        assert_eq!(snap1.total, Some(100));
        assert!(!snap1.is_paused);
        assert!(!snap1.finished);

        entry.pause();
        assert!(entry.stop_flag.load(Ordering::Relaxed));
        let snap2 = rx.try_recv().unwrap();
        assert!(snap2.is_paused);
        assert!(!snap2.cancelled);

        let was_paused = entry.cancel();
        assert!(was_paused);
        entry.emit(true, None);
        let snap3 = rx.try_recv().unwrap();
        assert!(snap3.cancelled);
        assert!(snap3.finished);
        assert!(!snap3.is_paused);
    }

    #[test]
    fn active_cancel_and_unknown_total_preserve_invariants() {
        let mut entry = ActiveDownloadEntry::new(
            "org/repo".into(),
            "model.gguf".into(),
            None,
            "Model".into(),
            0,
            Some(500),
            false,
        );
        let mut rx = entry.tx.subscribe();

        entry.update_progress(250, None);
        let snap = rx.try_recv().unwrap();
        assert_eq!(snap.done, 250);
        assert_eq!(snap.total, Some(500));

        let was_paused = entry.cancel();
        assert!(!was_paused);
        assert!(entry.is_cancelled);
        assert!(entry.stop_flag.load(Ordering::Relaxed));
    }
}
