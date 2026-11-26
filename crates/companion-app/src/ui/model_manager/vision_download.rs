//! Resumable `mmproj` vision projector download powered by [`DownloadManager`].

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use adw::prelude::*;
use gtk::glib;

use crate::i18n::{gettext, tr_f};
use crate::models::download_manager::{DownloadManager, DownloadRequest, DownloadStatus};
use crate::models::friendly_model_name;
use crate::models::huggingface::{HfGgufFile, select_best_mmproj};
use crate::storage::ModelRecord;
use crate::ui::hf_browser::{
    DownloadControls, DownloadTracker, HfBrowserContext, subscribe_download_lifecycle,
};
use crate::ui::widgets::send_desktop_notification;

static VISION_DOWNLOAD_GEN: AtomicU64 = AtomicU64::new(0);

/// Inline UI state for a model's Vision projector download row.
#[derive(Clone)]
pub(super) struct VisionRowUi {
    pub(super) btn: gtk::Button,
    pub(super) controls: DownloadControls,
}

pub(super) fn start_mmproj_download(
    ui: VisionRowUi,
    hf_ctx: &HfBrowserContext,
    model: ModelRecord,
    repo_id: String,
    preselected_file: Option<HfGgufFile>,
) {
    ui.btn.set_visible(false);
    ui.controls.reset_active();

    let generation = VISION_DOWNLOAD_GEN.fetch_add(1, Ordering::Relaxed) + 1;
    let model_name = friendly_model_name(&model.display_name);
    if let Some(file) = preselected_file {
        begin_managed_mmproj(MmprojInstallReq {
            ui,
            dialog: hf_ctx.dialog.clone(),
            download_manager: hf_ctx.download_manager.clone(),
            tracker: hf_ctx.tracker.clone(),
            model,
            model_name,
            repo_id,
            mmproj_file: file,
            refresh_installed: hf_ctx.refresh_installed.clone(),
        });
        return;
    }

    let (tx, rx) = tokio::sync::oneshot::channel::<Result<HfGgufFile, String>>();
    let repo_lookup = repo_id.clone();
    let hf_client = hf_ctx.services.hf_client.clone();
    hf_ctx.services.rt_handle.spawn(async move {
        let res = hf_client
            .list_repo_gguf_files(&repo_lookup)
            .await
            .map_err(|e| e.to_string())
            .and_then(|files| {
                select_best_mmproj(&files).ok_or_else(|| {
                    gettext("No mmproj vision projector file was found in this repository")
                })
            });
        let _ = tx.send(res);
    });

    await_mmproj_lookup(rx, generation, ui, hf_ctx, model, model_name, repo_id);
}

fn await_mmproj_lookup(
    rx: tokio::sync::oneshot::Receiver<Result<HfGgufFile, String>>,
    generation: u64,
    ui: VisionRowUi,
    hf_ctx: &HfBrowserContext,
    model: ModelRecord,
    model_name: String,
    repo_id: String,
) {
    let (dialog, mgr, tracker, refresh) = (
        hf_ctx.dialog.downgrade(),
        hf_ctx.download_manager.clone(),
        hf_ctx.tracker.clone(),
        hf_ctx.refresh_installed.clone(),
    );
    glib::MainContext::default().spawn_local(async move {
        let res = rx.await;
        if VISION_DOWNLOAD_GEN.load(Ordering::Relaxed) != generation {
            return;
        }
        let Some(dialog) = dialog.upgrade() else {
            return;
        };
        match res {
            Ok(Ok(file)) => {
                begin_managed_mmproj(MmprojInstallReq {
                    ui,
                    dialog,
                    download_manager: mgr,
                    tracker,
                    model,
                    model_name,
                    repo_id,
                    mmproj_file: file,
                    refresh_installed: refresh,
                });
            }
            Ok(Err(err)) => {
                restore_vision_row_idle(&ui);
                dialog.add_toast(adw::Toast::new(&tr_f(
                    "Failed to install Vision: {err}",
                    &[("err", &err)],
                )));
            }
            Err(_) => restore_vision_row_idle(&ui),
        }
    });
}

fn restore_vision_row_idle(ui: &VisionRowUi) {
    ui.controls.container.set_visible(false);
    ui.btn.set_visible(true);
    ui.btn.set_sensitive(true);
}

struct MmprojInstallReq {
    ui: VisionRowUi,
    dialog: adw::PreferencesDialog,
    download_manager: DownloadManager,
    tracker: DownloadTracker,
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
        tracker,
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
    ui.controls.bind_download(
        &download_manager,
        handle.key.clone(),
        model_name.clone(),
        &dialog,
    );
    let (tracker_controls, finish_tracker) =
        tracker.track(&download_manager, &handle.key, &model_name, &dialog);
    let (dialog_cb, controls_done, btn_done) =
        (dialog.clone(), ui.controls.clone(), ui.btn.clone());
    subscribe_download_lifecycle(
        None,
        handle.rx,
        tracker_controls,
        Some(ui.controls),
        Some(&dialog),
        move |status| {
            finish_mmproj_download(
                &finish_tracker,
                &controls_done,
                &btn_done,
                &dialog_cb,
                &refresh_installed,
                (&model.id, &model_name),
                status,
            );
        },
    );
}

fn finish_mmproj_download(
    finish_tracker: &Rc<dyn Fn()>,
    controls_done: &DownloadControls,
    btn_done: &gtk::Button,
    dialog_cb: &adw::PreferencesDialog,
    refresh: &Rc<dyn Fn()>,
    (model_id, model_name): (&str, &str),
    status: DownloadStatus,
) {
    finish_tracker();
    controls_done.unbind();
    restore_vision_row_idle(&VisionRowUi {
        btn: btn_done.clone(),
        controls: controls_done.clone(),
    });
    match status {
        DownloadStatus::Completed => {
            notify_vision_result(dialog_cb, refresh, model_id, model_name, None);
        }
        DownloadStatus::Failed(err) => {
            notify_vision_result(dialog_cb, refresh, model_id, model_name, Some(err));
        }
        _ => {}
    }
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
