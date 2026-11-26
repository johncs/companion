//! UI attachment, pause/cancel wiring, and pending download restoration for `DownloadTracker`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use gtk::glib;

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::huggingface::remove_part_file;
use crate::models::{friendly_model_name, managed_models_dir};
use crate::storage::{Database, PendingDownload};
use crate::ui::widgets::{present_destructive_confirm, spawn_blocking_ui};

use super::download_controls::{DownloadControls, DownloadTracker};
use super::download_state::{ACTIVE_DOWNLOADS, ActiveDownloadEntry};
use super::download_worker::launch_entry_task;

#[allow(clippy::too_many_arguments)]
pub(super) fn attach_entry_to_tracker(
    entry_arc: Arc<Mutex<ActiveDownloadEntry>>,
    db: &Database,
    tracker: &DownloadTracker,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    row_btn: Option<&gtk::Button>,
    row_controls: Option<&DownloadControls>,
    on_pause_handler: Option<&Rc<RefCell<Option<Rc<dyn Fn()>>>>>,
    on_cancel_handler: Option<&Rc<RefCell<Option<Rc<dyn Fn()>>>>>,
) {
    let (repo_id, filename, mmproj_name, display_name, init_done, init_total, init_paused, mut rx) = {
        let entry = entry_arc.lock().expect("active download mutex poisoned");
        (
            entry.repo_id.clone(),
            entry.filename.clone(),
            entry.mmproj_filename.clone(),
            entry.display_name.clone(),
            entry.done,
            entry.total,
            entry.is_paused,
            entry.tx.subscribe(),
        )
    };

    let on_pause: Rc<dyn Fn()> = {
        let entry_arc = entry_arc.clone();
        let db = db.clone();
        Rc::new(move || {
            let should_resume = {
                let Ok(mut entry) = entry_arc.lock() else {
                    return;
                };
                if !entry.is_paused {
                    entry.pause();
                    false
                } else {
                    true
                }
            };
            if should_resume {
                launch_entry_task(entry_arc.clone(), db.clone());
            }
        })
    };

    let on_cancel: Rc<dyn Fn()> = {
        let entry_arc = entry_arc.clone();
        let db = db.clone();
        let dialog_confirm = dialog.clone();
        let display_name_confirm = display_name.clone();
        let repo_id_c = repo_id.clone();
        let filename_c = filename.clone();
        let mmproj_c = mmproj_name.clone();
        Rc::new(move || {
            let entry_arc = entry_arc.clone();
            let db = db.clone();
            let repo_id_c = repo_id_c.clone();
            let filename_c = filename_c.clone();
            let mmproj_c = mmproj_c.clone();
            let body = tr_f(
                "Cancel downloading \"{name}\" and delete the partial download from disk?",
                &[("name", &display_name_confirm)],
            );

            present_destructive_confirm(
                &dialog_confirm,
                &gettext("Cancel Download?"),
                &body,
                &gettext("Cancel Download"),
                move || {
                    let is_still_active = ACTIVE_DOWNLOADS
                        .lock()
                        .map(|m| m.contains_key(&(repo_id_c.clone(), filename_c.clone())))
                        .unwrap_or(false);
                    if !is_still_active {
                        return;
                    }

                    let was_paused = {
                        let Ok(mut entry) = entry_arc.lock() else {
                            return;
                        };
                        entry.cancel()
                    };

                    if was_paused {
                        if let Ok(mut map) = ACTIVE_DOWNLOADS.lock() {
                            map.remove(&(repo_id_c.clone(), filename_c.clone()));
                        }
                        if let Ok(entry) = entry_arc.lock() {
                            entry.emit(true, None);
                        }
                        let safe_repo = repo_id_c.replace('/', "--");
                        let repo_dir = managed_models_dir().join(&safe_repo);
                        let dpath = repo_dir.join(&filename_c);
                        let mpath = mmproj_c.as_deref().map(|m| repo_dir.join(m));
                        let db_c = db.clone();
                        let r = repo_id_c.clone();
                        let f = filename_c.clone();
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
                },
            );
        })
    };

    if let Some(p) = on_pause_handler {
        *p.borrow_mut() = Some(on_pause.clone());
    }
    if let Some(c) = on_cancel_handler {
        *c.borrow_mut() = Some(on_cancel.clone());
    }

    let (tracker_controls, finish_tracking) =
        tracker.track(&repo_id, &display_name, on_pause, on_cancel);
    tracker_controls.set_progress(init_done, init_total);
    tracker_controls.set_paused(init_paused);

    if let Some(c) = row_controls {
        c.reset_active();
        c.set_progress(init_done, init_total);
        c.set_paused(init_paused);
    }

    let row_btn_opt = row_btn.cloned();
    let row_controls_opt = row_controls.cloned();
    let on_pause_opt = on_pause_handler.cloned();
    let on_cancel_opt = on_cancel_handler.cloned();
    let dialog_toast = dialog.clone();
    let refresh_installed = refresh_installed.clone();

    glib::MainContext::default().spawn_local(async move {
        while let Ok(snap) = rx.recv().await {
            tracker_controls.set_progress(snap.done, snap.total);
            tracker_controls.set_paused(snap.is_paused);
            if let Some(c) = &row_controls_opt {
                c.set_progress(snap.done, snap.total);
                c.set_paused(snap.is_paused);
            }
            if snap.finished {
                finish_tracking();
                if let Some(p) = &on_pause_opt {
                    *p.borrow_mut() = None;
                }
                if let Some(c) = &on_cancel_opt {
                    *c.borrow_mut() = None;
                }
                if let Some(c) = &row_controls_opt {
                    c.container.set_visible(false);
                }
                if let Some(b) = &row_btn_opt {
                    b.set_visible(true);
                }
                if !snap.cancelled {
                    if let Some(err) = snap.error {
                        dialog_toast.add_toast(adw::Toast::new(&tr_f(
                            "Download stopped: {err}",
                            &[("err", &err)],
                        )));
                    } else {
                        refresh_installed();
                        dialog_toast.add_toast(adw::Toast::new(&tr_f(
                            "Downloaded \"{name}\"",
                            &[("name", &display_name)],
                        )));
                    }
                }
                break;
            }
        }
    });
}

/// Restore any active in-process downloads (after reopening Settings) and any paused downloads
/// from a previous app session whose `.gguf.part` files still exist on disk.
pub fn restore_pending_downloads(
    tracker: &DownloadTracker,
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
) {
    let active_entries: Vec<Arc<Mutex<ActiveDownloadEntry>>> = ACTIVE_DOWNLOADS
        .lock()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default();

    for entry_arc in active_entries {
        attach_entry_to_tracker(
            entry_arc,
            db,
            tracker,
            dialog,
            refresh_installed,
            None,
            None,
            None,
            None,
        );
    }

    let db_bg = db.clone();
    let tracker = tracker.clone();
    let db = db.clone();
    let dialog = dialog.clone();
    let refresh_installed = refresh_installed.clone();

    spawn_blocking_ui(
        move || {
            let pending = db_bg.list_pending_downloads().unwrap_or_default();
            let mut restorable = Vec::new();
            for item in pending {
                let already_active = ACTIVE_DOWNLOADS
                    .lock()
                    .map(|m| m.contains_key(&(item.repo_id.clone(), item.filename.clone())))
                    .unwrap_or(false);
                if already_active {
                    continue;
                }
                let safe_repo = item.repo_id.replace('/', "--");
                let repo_dir = managed_models_dir().join(&safe_repo);
                let main_part = repo_dir.join(&item.filename).with_extension("gguf.part");
                let main_done = repo_dir.join(&item.filename);
                let mm_part = item
                    .mmproj_filename
                    .as_deref()
                    .map(|m| repo_dir.join(m).with_extension("gguf.part"));
                let mm_done = item.mmproj_filename.as_deref().map(|m| repo_dir.join(m));

                let has_part = main_part.exists() || mm_part.as_ref().is_some_and(|p| p.exists());
                if !has_part {
                    let _ = db_bg.remove_pending_download(&item.repo_id, &item.filename);
                    continue;
                }
                let part_bytes = |part: &std::path::Path, done: &std::path::Path| {
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
                restorable.push((item, existing));
            }
            restorable
        },
        move |items| {
            for (item, existing_bytes) in items {
                let key = (item.repo_id.clone(), item.filename.clone());
                let entry_arc = {
                    let Ok(mut map) = ACTIVE_DOWNLOADS.lock() else {
                        continue;
                    };
                    if map.contains_key(&key) {
                        continue;
                    }
                    let arc = Arc::new(Mutex::new(ActiveDownloadEntry::new(
                        item.repo_id.clone(),
                        item.filename.clone(),
                        item.mmproj_filename.clone(),
                        item.display_name.clone(),
                        existing_bytes,
                        item.total_bytes,
                        true,
                    )));
                    map.insert(key, arc.clone());
                    arc
                };
                attach_entry_to_tracker(
                    entry_arc,
                    &db,
                    &tracker,
                    &dialog,
                    &refresh_installed,
                    None,
                    None,
                    None,
                    None,
                );
            }
        },
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn start_hf_download(
    btn: &gtk::Button,
    controls: &DownloadControls,
    on_pause_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_cancel_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    repo_id: &str,
    filename: &str,
    mmproj_name: Option<&str>,
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
) {
    btn.set_visible(false);
    controls.reset_active();

    let display_name = friendly_model_name(filename);
    let key = (repo_id.to_string(), filename.to_string());
    let (entry_arc, is_new) = {
        let Ok(mut map) = ACTIVE_DOWNLOADS.lock() else {
            return;
        };
        if let Some(existing) = map.get(&key) {
            (existing.clone(), false)
        } else {
            let arc = Arc::new(Mutex::new(ActiveDownloadEntry::new(
                repo_id.to_string(),
                filename.to_string(),
                mmproj_name.map(str::to_string),
                display_name.clone(),
                0,
                None,
                false,
            )));
            map.insert(key, arc.clone());
            (arc, true)
        }
    };

    if is_new {
        let db_pend = db.clone();
        let pend = PendingDownload {
            repo_id: repo_id.to_string(),
            filename: filename.to_string(),
            mmproj_filename: mmproj_name.map(str::to_string),
            display_name,
            total_bytes: None,
        };
        runtime().spawn_blocking(move || {
            let _ = db_pend.upsert_pending_download(pend);
        });
    }

    attach_entry_to_tracker(
        entry_arc.clone(),
        db,
        tracker,
        dialog,
        refresh_installed,
        Some(btn),
        Some(controls),
        Some(on_pause_handler),
        Some(on_cancel_handler),
    );

    if is_new {
        launch_entry_task(entry_arc, db.clone());
    }
}
