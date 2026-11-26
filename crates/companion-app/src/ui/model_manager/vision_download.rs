//! Resumable `mmproj` vision projector download powered by [`DownloadManager`].

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::download_manager::{DownloadManager, DownloadRequest, DownloadStatus};
use crate::models::friendly_model_name;
use crate::models::huggingface::{HfGgufFile, list_repo_gguf_files, select_best_mmproj};
use crate::storage::ModelRecord;
use crate::ui::hf_browser::{DownloadControls, subscribe_download_lifecycle};
use crate::ui::widgets::send_desktop_notification;

/// Inline UI state for a model's Vision projector download row.
#[derive(Clone)]
pub(super) struct VisionRowUi {
    pub(super) btn: gtk::Button,
    pub(super) controls: DownloadControls,
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
    let handle = download_manager.start_download(DownloadRequest {
        repo_id,
        filename: mmproj_file.filename,
        mmproj_filename: None,
        display_name: model_name.clone(),
        attach_to_model_id: Some(model.id.clone()),
    });

    ui.controls
        .bind_download(&download_manager, handle.key, model_name.clone(), &dialog);

    let dialog = dialog.clone();
    let refresh = refresh_installed.clone();
    let model_id = model.id;
    let controls_done = ui.controls.clone();
    let btn_done = ui.btn.clone();
    subscribe_download_lifecycle(None, handle.rx, ui.controls, None, move |status| {
        controls_done.unbind();
        controls_done.container.set_visible(false);
        btn_done.set_visible(true);
        btn_done.set_sensitive(true);
        match status {
            DownloadStatus::Completed => {
                notify_vision_result(&dialog, &refresh, &model_id, &model_name, None);
            }
            DownloadStatus::Failed(err) => {
                notify_vision_result(&dialog, &refresh, &model_id, &model_name, Some(err));
            }
            _ => {}
        }
    });
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
                &tr_f(
                    "Vision projector for {name} is ready.",
                    &[("name", model_name)],
                ),
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
