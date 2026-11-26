//! Resumable `mmproj` vision projector download powered by [`DownloadManager`].

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::download_manager::{DownloadKey, DownloadManager, DownloadRequest};
use crate::models::friendly_model_name;
use crate::models::huggingface::{HfGgufFile, list_repo_gguf_files, select_best_mmproj};
use crate::storage::ModelRecord;
use crate::ui::hf_browser::DownloadControls;
use crate::ui::widgets::{present_destructive_confirm, send_desktop_notification};

/// Inline UI state for a model's Vision projector download row.
#[derive(Clone)]
pub(super) struct VisionRowUi {
    pub(super) btn: gtk::Button,
    pub(super) controls: DownloadControls,
    pub(super) on_pause: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    pub(super) on_cancel: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
}

pub(super) fn start_mmproj_download(
    ui: VisionRowUi,
    dialog: &adw::PreferencesDialog,
    download_manager: &DownloadManager,
    model: ModelRecord,
    repo_id: String,
    preselected_file: Option<HfGgufFile>,
    refresh_installed: &Rc<dyn Fn()>,
) {
    ui.btn.set_visible(false);
    ui.controls.reset_active();

    let model_name = friendly_model_name(&model.display_name);
    if let Some(file) = preselected_file {
        begin_managed_mmproj(MmprojInstallReq {
            ui,
            dialog: dialog.clone(),
            download_manager: download_manager.clone(),
            model,
            model_name,
            repo_id,
            mmproj_file: file,
            refresh_installed: refresh_installed.clone(),
        });
        return;
    }

    let (tx, rx) = tokio::sync::oneshot::channel::<Result<HfGgufFile, String>>();
    let repo_lookup = repo_id.clone();
    runtime().spawn(async move {
        let res = list_repo_gguf_files(&repo_lookup)
            .await
            .map_err(|e| e.to_string())
            .and_then(|files| {
                select_best_mmproj(&files).ok_or_else(|| {
                    gettext("No mmproj vision projector file was found in this repository")
                })
            });
        let _ = tx.send(res);
    });

    let dialog = dialog.clone();
    let mgr = download_manager.clone();
    let refresh = refresh_installed.clone();
    glib::MainContext::default().spawn_local(async move {
        match rx.await {
            Ok(Ok(file)) => {
                begin_managed_mmproj(MmprojInstallReq {
                    ui,
                    dialog,
                    download_manager: mgr,
                    model,
                    model_name,
                    repo_id,
                    mmproj_file: file,
                    refresh_installed: refresh,
                });
            }
            Ok(Err(err)) => {
                ui.controls.container.set_visible(false);
                ui.btn.set_visible(true);
                ui.btn.set_sensitive(true);
                dialog.add_toast(adw::Toast::new(&tr_f(
                    "Failed to install Vision: {err}",
                    &[("err", &err)],
                )));
            }
            Err(_) => {
                ui.controls.container.set_visible(false);
                ui.btn.set_visible(true);
                ui.btn.set_sensitive(true);
            }
        }
    });
}

struct MmprojInstallReq {
    ui: VisionRowUi,
    dialog: adw::PreferencesDialog,
    download_manager: DownloadManager,
    model: ModelRecord,
    model_name: String,
    repo_id: String,
    mmproj_file: HfGgufFile,
    refresh_installed: Rc<dyn Fn()>,
}

fn begin_managed_mmproj(req: MmprojInstallReq) {
    let MmprojInstallReq {
        ui,
        dialog,
        download_manager,
        model,
        model_name,
        repo_id,
        mmproj_file,
        refresh_installed,
    } = req;
    let mut handle = download_manager.start_download(DownloadRequest {
        repo_id,
        filename: mmproj_file.filename,
        mmproj_filename: None,
        display_name: model_name.clone(),
        attach_to_model_id: Some(model.id.clone()),
    });

    wire_vision_pause_cancel(&ui, &dialog, &download_manager, &handle.key, &model_name);

    let dialog = dialog.clone();
    let refresh = refresh_installed.clone();
    let model_id = model.id;
    glib::MainContext::default().spawn_local(async move {
        while let Ok(snap) = handle.rx.recv().await {
            ui.controls.set_progress(snap.done, snap.total);
            ui.controls.set_paused(snap.is_paused);
            if snap.finished {
                *ui.on_pause.borrow_mut() = None;
                *ui.on_cancel.borrow_mut() = None;
                ui.controls.container.set_visible(false);
                ui.btn.set_visible(true);
                ui.btn.set_sensitive(true);
                if !snap.cancelled {
                    notify_vision_result(&dialog, &refresh, &model_id, &model_name, snap.error);
                }
                break;
            }
        }
    });
}

fn wire_vision_pause_cancel(
    ui: &VisionRowUi,
    dialog: &adw::PreferencesDialog,
    download_manager: &DownloadManager,
    key: &DownloadKey,
    model_name: &str,
) {
    let mgr_p = download_manager.clone();
    let key_p = key.clone();
    *ui.on_pause.borrow_mut() = Some(Rc::new(move || {
        let is_paused = mgr_p
            .active_downloads()
            .into_iter()
            .find(|s| s.key == key_p)
            .is_some_and(|s| s.snapshot.is_paused);
        if is_paused {
            mgr_p.resume(&key_p);
        } else {
            mgr_p.pause(&key_p);
        }
    }));

    let mgr_c = download_manager.clone();
    let key_c = key.clone();
    let dialog_c = dialog.clone();
    let name_c = model_name.to_string();
    *ui.on_cancel.borrow_mut() = Some(Rc::new(move || {
        let mgr_inner = mgr_c.clone();
        let key_inner = key_c.clone();
        let body = tr_f(
            "Cancel downloading the Vision projector for \"{name}\"?",
            &[("name", &name_c)],
        );
        present_destructive_confirm(
            &dialog_c,
            &gettext("Cancel Vision Download?"),
            &body,
            &gettext("Cancel Download"),
            move || {
                let _ = mgr_inner.cancel(&key_inner);
            },
        );
    }));
}

fn notify_vision_result(
    dialog: &adw::PreferencesDialog,
    refresh: &Rc<dyn Fn()>,
    model_id: &str,
    model_name: &str,
    error: Option<String>,
) {
    match error {
        None => {
            refresh();
            dialog.add_toast(adw::Toast::new(&tr_f(
                "Enabled Vision for \"{name}\"",
                &[("name", model_name)],
            )));
            send_desktop_notification(
                &format!("vision-{model_id}"),
                &gettext("Vision Enabled"),
                &tr_f("Vision projector for {name} is ready.", &[("name", model_name)]),
            );
        }
        Some(err) => {
            dialog.add_toast(adw::Toast::new(&tr_f(
                "Failed to install Vision: {err}",
                &[("err", &err)],
            )));
            send_desktop_notification(
                &format!("vision-{model_id}"),
                &gettext("Vision Download Failed"),
                &tr_f("{name}: {err}", &[("name", model_name), ("err", &err)]),
            );
        }
    }
}
