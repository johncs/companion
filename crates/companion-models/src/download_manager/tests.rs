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
            let part = part_file_path(dest_path);
            std::fs::write(&part, b"partial")?;
            on_progress(50, Some(100));

            if self.pause_on_first.swap(false, Ordering::SeqCst) {
                while !stop_flag.load(Ordering::Relaxed) {
                    tokio::task::yield_now().await;
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

#[tokio::test]
async fn download_manager_start_pause_resume_and_complete() {
    let dir = std::env::temp_dir().join(format!("companion_dm_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.join("test.db")).unwrap();
    let mgr = DownloadManager::with_fetcher(
        db.clone(),
        dir.join("models"),
        tokio::runtime::Handle::current(),
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
    assert_eq!(handle.initial.status, DownloadStatus::Active);

    let mut paused_once = false;
    while let Ok(snap) = handle.rx.recv().await {
        if snap.done == 50 && !snap.status.is_paused() && !paused_once {
            paused_once = true;
            mgr.pause(&handle.key);
        } else if snap.status == DownloadStatus::Paused {
            assert!(mgr.resume(&handle.key));
        } else if snap.status.is_terminal() {
            assert_eq!(snap.status, DownloadStatus::Completed);
            break;
        }
    }

    assert_eq!(db.list_models().unwrap().len(), 1);
    assert!(db.list_pending_downloads().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn download_manager_cancel_cleans_part_files_active_and_paused() {
    let dir = std::env::temp_dir().join(format!("companion_dm_cancel_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.join("test.db")).unwrap();
    let mgr = DownloadManager::with_fetcher(
        db.clone(),
        dir.join("models"),
        tokio::runtime::Handle::current(),
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

    while let Ok(snap) = handle.rx.recv().await {
        if snap.done == 50 && !snap.status.is_paused() {
            mgr.pause(&handle.key);
        } else if snap.status == DownloadStatus::Paused {
            mgr.cancel(&handle.key).unwrap();
        } else if snap.status.is_terminal() {
            assert_eq!(snap.status, DownloadStatus::Cancelled);
            break;
        }
    }

    let part = dir.join("models/org--cancel/m.gguf.part");
    for _ in 0..1000 {
        if !part.exists() && db.list_pending_downloads().unwrap().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert!(!part.exists());
    assert!(db.list_pending_downloads().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn restore_pending_restores_part_files_and_prunes_stale_entries() {
    let dir = std::env::temp_dir().join(format!("companion_dm_restore_{}", uuid::Uuid::new_v4()));
    let models_dir = dir.join("models");
    std::fs::create_dir_all(models_dir.join("org--keep")).unwrap();
    std::fs::write(models_dir.join("org--keep/keep.gguf.part"), b"1234").unwrap();

    let db = Database::open(dir.join("test.db")).unwrap();
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
        tokio::runtime::Handle::current(),
        Arc::new(MockFetcher {
            pause_on_first: AtomicBool::new(false),
        }),
    );
    let handles = mgr.restore_pending();
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0].key, ("org/keep".into(), "keep.gguf".into()));
    assert_eq!(handles[0].initial.done, 4);
    assert_eq!(handles[0].initial.total, Some(100));
    assert_eq!(handles[0].initial.status, DownloadStatus::Paused);
    assert_eq!(db.list_pending_downloads().unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mmproj_attach_updates_existing_model_without_duplicating() {
    let dir = std::env::temp_dir().join(format!("companion_dm_mmproj_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.join("test.db")).unwrap();
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
        tokio::runtime::Handle::current(),
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

    while let Ok(snap) = handle.rx.recv().await {
        if snap.status.is_terminal() {
            assert_eq!(snap.status, DownloadStatus::Completed);
            break;
        }
    }

    let models = db.list_models().unwrap();
    assert_eq!(models.len(), 1);
    assert!(
        models[0]
            .mmproj_path
            .as_deref()
            .is_some_and(|p| p.ends_with("mmproj.gguf"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mmproj_failure_cancels_sibling_model_fetch() {
    struct FailMmprojFetcher {
        saw_stop: Arc<AtomicBool>,
    }
    impl ModelFetcher for FailMmprojFetcher {
        fn fetch<'a>(
            &'a self,
            _repo_id: &'a str,
            filename: &'a str,
            _dest_path: &'a Path,
            stop_flag: Arc<AtomicBool>,
            _on_progress: ProgressCb,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            let is_mmproj = filename.contains("mmproj");
            let saw_stop = self.saw_stop.clone();
            Box::pin(async move {
                if is_mmproj {
                    anyhow::bail!("mmproj HTTP 404");
                }
                struct CancelGuard(Arc<AtomicBool>);
                impl Drop for CancelGuard {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Relaxed);
                    }
                }
                let guard = CancelGuard(saw_stop);
                for _ in 0..10_000 {
                    if stop_flag.load(Ordering::Relaxed) {
                        anyhow::bail!("cancelled by sibling");
                    }
                    tokio::task::yield_now().await;
                }
                std::mem::forget(guard);
                Ok(())
            })
        }
    }

    let dir = std::env::temp_dir().join(format!("companion_dm_sib_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.join("test.db")).unwrap();
    let saw_stop = Arc::new(AtomicBool::new(false));

    let mgr = DownloadManager::with_fetcher(
        db,
        dir.join("models"),
        tokio::runtime::Handle::current(),
        Arc::new(FailMmprojFetcher {
            saw_stop: saw_stop.clone(),
        }),
    );
    let mut handle = mgr.start_download(DownloadRequest {
        repo_id: "org/model".into(),
        filename: "model.gguf".into(),
        mmproj_filename: Some("mmproj.gguf".into()),
        display_name: "Model".into(),
        attach_to_model_id: None,
    });

    while let Ok(snap) = handle.rx.recv().await {
        if snap.status.is_terminal() {
            assert!(matches!(snap.status, DownloadStatus::Failed(ref e) if e.contains("404")));
            break;
        }
    }
    assert!(saw_stop.load(Ordering::Relaxed));
    let _ = std::fs::remove_dir_all(&dir);
}
