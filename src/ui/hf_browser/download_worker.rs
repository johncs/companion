//! Background Tokio download worker for Hugging Face GGUF + optional `mmproj` files.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gtk::glib;

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::huggingface::{download_gguf_file, remove_part_file};
use crate::models::managed_models_dir;
use crate::storage::Database;
use crate::ui::widgets::send_desktop_notification;

use super::download_state::{ACTIVE_DOWNLOADS, ActiveDownloadEntry};

pub(super) fn launch_entry_task(entry_arc: Arc<Mutex<ActiveDownloadEntry>>, db: Database) {
    let (repo_id, filename, mmproj_name, display_name, stop_flag) = {
        let mut entry = entry_arc.lock().expect("active download mutex poisoned");
        let flag = Arc::new(AtomicBool::new(false));
        entry.stop_flag = flag.clone();
        entry.is_paused = false;
        entry.emit(false, None);
        (
            entry.repo_id.clone(),
            entry.filename.clone(),
            entry.mmproj_filename.clone(),
            entry.display_name.clone(),
            flag,
        )
    };

    let safe_repo = repo_id.replace('/', "--");
    let repo_dir = managed_models_dir().join(&safe_repo);
    let dest_path = repo_dir.join(&filename);
    let mmproj_dest = mmproj_name.as_deref().map(|m| repo_dir.join(m));

    runtime().spawn(async move {
        let done_main = Arc::new(AtomicU64::new(0));
        let total_main = Arc::new(AtomicU64::new(0));
        let done_mm = Arc::new(AtomicU64::new(0));
        let total_mm = Arc::new(AtomicU64::new(0));
        let saved_total = Arc::new(AtomicU64::new(0));

        let update_progress = {
            let entry_arc = entry_arc.clone();
            let db_t = db.clone();
            let rid_t = repo_id.clone();
            let fname_t = filename.clone();
            let saved_total = saved_total.clone();
            move |done: u64, total: Option<u64>| {
                if let Some(t) = total
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
            }
        };

        let send_combined = {
            let dm = done_main.clone();
            let tm = total_main.clone();
            let dmm = done_mm.clone();
            let tmm = total_mm.clone();
            let update = update_progress.clone();
            move || {
                let done = dm
                    .load(Ordering::Relaxed)
                    .saturating_add(dmm.load(Ordering::Relaxed));
                let total = tm
                    .load(Ordering::Relaxed)
                    .saturating_add(tmm.load(Ordering::Relaxed));
                update(done, (total > 0).then_some(total));
            }
        };

        let res = if let (Some(mm_file), Some(mm_path)) = (&mmproj_name, mmproj_dest.clone()) {
            let main_fut = download_gguf_file(&repo_id, &filename, &dest_path, stop_flag.clone(), {
                let dm = done_main.clone();
                let tm = total_main.clone();
                let send = send_combined.clone();
                move |done, total| {
                    dm.store(done, Ordering::Relaxed);
                    if let Some(t) = total {
                        tm.store(t, Ordering::Relaxed);
                    }
                    send();
                }
            });
            let mm_fut = download_gguf_file(&repo_id, mm_file, &mm_path, stop_flag.clone(), {
                let dmm = done_mm.clone();
                let tmm = total_mm.clone();
                let send = send_combined;
                move |done, total| {
                    dmm.store(done, Ordering::Relaxed);
                    if let Some(t) = total {
                        tmm.store(t, Ordering::Relaxed);
                    }
                    send();
                }
            });
            let (r_main, r_mm) = tokio::join!(main_fut, mm_fut);
            r_main.and(r_mm)
        } else {
            download_gguf_file(&repo_id, &filename, &dest_path, stop_flag, update_progress).await
        };

        match res {
            Ok(()) => {
                let db_reg = db.clone();
                let dname_reg = display_name.clone();
                let dpath_reg = dest_path.clone();
                let mpath_reg = mmproj_dest.clone();
                let rid_reg = repo_id.clone();
                let fname_reg = filename.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(record) = db_reg.register_local_gguf(
                        Some(dname_reg),
                        &dpath_reg,
                        mpath_reg.as_deref(),
                        Some(rid_reg.clone()),
                    ) && db_reg.get_active_model_id().ok().flatten().is_none()
                    {
                        let _ = db_reg.set_active_model_id(Some(&record.id));
                    }
                    let _ = db_reg.remove_pending_download(&rid_reg, &fname_reg);
                })
                .await;

                if let Ok(mut map) = ACTIVE_DOWNLOADS.lock() {
                    map.remove(&(repo_id.clone(), filename.clone()));
                }
                if let Ok(entry) = entry_arc.lock() {
                    entry.emit(true, None);
                }
                let notif_id = format!("download-{repo_id}");
                let dname_notif = display_name.clone();
                glib::idle_add_once(move || {
                    send_desktop_notification(
                        &notif_id,
                        &gettext("Model Downloaded"),
                        &tr_f("{name} is ready to chat.", &[("name", &dname_notif)]),
                    );
                });
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

                if let Ok(mut map) = ACTIVE_DOWNLOADS.lock() {
                    map.remove(&(repo_id.clone(), filename.clone()));
                }

                if is_cancelled {
                    remove_part_file(&dest_path).await;
                    if let Some(mp) = mmproj_dest {
                        remove_part_file(&mp).await;
                    }
                    let db_c = db.clone();
                    let rid_c = repo_id.clone();
                    let fname_c = filename.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = db_c.remove_pending_download(&rid_c, &fname_c);
                    })
                    .await;
                    if let Ok(entry) = entry_arc.lock() {
                        entry.emit(true, None);
                    }
                    return;
                }

                let err_str = err.to_string();
                if let Ok(entry) = entry_arc.lock() {
                    entry.emit(true, Some(err_str.clone()));
                }
                let notif_id = format!("download-{repo_id}");
                glib::idle_add_once(move || {
                    send_desktop_notification(
                        &notif_id,
                        &gettext("Download Failed"),
                        &tr_f("{name}: {err}", &[("name", &display_name), ("err", &err_str)]),
                    );
                });
            }
        }
    });
}
