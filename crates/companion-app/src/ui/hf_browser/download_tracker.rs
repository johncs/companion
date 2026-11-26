//! UI attachment, pause/cancel wiring, and pending download restoration via [`DownloadManager`].

use std::rc::Rc;

use adw::prelude::*;
use tokio::sync::broadcast;

use crate::i18n::{gettext, tr_f};
use crate::models::download_manager::{
    DownloadKey, DownloadRequest, DownloadSnapshot, DownloadStatus,
};
use crate::models::friendly_model_name;
use crate::ui::widgets::{send_desktop_notification, spawn_blocking_ui};

use super::HfBrowserContext;
use super::download_controls::{DownloadControls, subscribe_download_lifecycle};

/// Optional inline row controls bound alongside the global [`DownloadTracker`] row.
pub(super) struct RowDownloadUi {
    pub(super) btn: gtk::Button,
    pub(super) controls: DownloadControls,
}

fn attach_subscription_to_tracker(
    key: DownloadKey,
    display_name: String,
    initial: Option<DownloadSnapshot>,
    rx: broadcast::Receiver<DownloadSnapshot>,
    ctx: &HfBrowserContext,
    row_ui: Option<RowDownloadUi>,
) {
    let (row_btn, row_controls) = bind_row_download_ui(row_ui, &key, &display_name, ctx);
    let (tracker_controls, finish_tracker) =
        ctx.tracker
            .track(&ctx.download_manager, &key, &display_name, &ctx.dialog);
    let (dialog, refresh_installed, repo_id, row_controls_done) = (
        ctx.dialog.clone(),
        ctx.refresh_installed.clone(),
        key.0,
        row_controls.clone(),
    );
    subscribe_download_lifecycle(
        initial,
        rx,
        tracker_controls,
        row_controls,
        Some(&ctx.dialog),
        move |status| {
            finish_tracker();
            if let Some(rc) = &row_controls_done {
                rc.container.set_visible(false);
            }
            if let Some(btn) = &row_btn {
                btn.set_visible(true);
                btn.set_sensitive(true);
            }
            notify_download_outcome(
                &dialog,
                &refresh_installed,
                &repo_id,
                &display_name,
                &status,
            );
        },
    );
}

fn bind_row_download_ui(
    row_ui: Option<RowDownloadUi>,
    key: &DownloadKey,
    display_name: &str,
    ctx: &HfBrowserContext,
) -> (Option<gtk::Button>, Option<DownloadControls>) {
    match row_ui {
        Some(ui) => {
            ui.controls.bind_download(
                &ctx.download_manager,
                key.clone(),
                display_name.to_string(),
                &ctx.dialog,
            );
            (Some(ui.btn), Some(ui.controls))
        }
        None => (None, None),
    }
}

fn notify_download_outcome(
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    repo_id: &str,
    display_name: &str,
    status: &DownloadStatus,
) {
    match status {
        DownloadStatus::Failed(err) => {
            dialog.add_toast(adw::Toast::new(&tr_f(
                "Download failed: {err}",
                &[("err", err)],
            )));
            send_desktop_notification(
                &format!("download-{repo_id}"),
                &gettext("Download Failed"),
                &tr_f("{name}: {err}", &[("name", display_name), ("err", err)]),
            );
        }
        DownloadStatus::Completed => {
            refresh_installed();
            dialog.add_toast(adw::Toast::new(&tr_f(
                "Installed {name}",
                &[("name", display_name)],
            )));
            send_desktop_notification(
                &format!("download-{repo_id}"),
                &gettext("Model Downloaded"),
                &tr_f("{name} is ready to chat.", &[("name", display_name)]),
            );
        }
        _ => {}
    }
}

/// Restore any active in-process downloads and any paused downloads from a previous session.
pub fn restore_pending_downloads(ctx: &HfBrowserContext) {
    for summary in ctx.download_manager.active_downloads() {
        if let Some(rx) = ctx.download_manager.subscribe(&summary.key) {
            attach_subscription_to_tracker(
                summary.key,
                summary.display_name,
                Some(summary.snapshot),
                rx,
                ctx,
                None,
            );
        }
    }

    let mgr_bg = ctx.download_manager.clone();
    let rt_bg = ctx.services.rt_handle.clone();
    let ctx_ui = ctx.clone();
    spawn_blocking_ui(
        &ctx.services.rt_handle,
        move || rt_bg.block_on(mgr_bg.restore_pending()),
        move |handles| {
            for handle in handles {
                attach_subscription_to_tracker(
                    handle.key,
                    handle.display_name,
                    Some(handle.initial),
                    handle.rx,
                    &ctx_ui,
                    None,
                );
            }
        },
    );
}

/// Start (or attach to) a Hugging Face model download via [`DownloadManager`].
pub(super) fn start_hf_download(
    ctx: &HfBrowserContext,
    repo_id: &str,
    filename: &str,
    mmproj_name: Option<&str>,
    row_ui: RowDownloadUi,
) {
    row_ui.btn.set_visible(false);
    row_ui.controls.reset_active();

    let display_name = friendly_model_name(filename);
    let handle = ctx.download_manager.start_download(DownloadRequest {
        repo_id: repo_id.to_string(),
        filename: filename.to_string(),
        mmproj_filename: mmproj_name.map(str::to_string),
        display_name: display_name.clone(),
        attach_to_model_id: None,
    });

    attach_subscription_to_tracker(handle.key, display_name, None, handle.rx, ctx, Some(row_ui));
}
