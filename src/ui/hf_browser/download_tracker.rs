//! UI attachment, pause/cancel wiring, and pending download restoration via [`DownloadManager`].

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tokio::sync::broadcast;

use crate::i18n::{gettext, tr_f};
use crate::models::download_manager::{DownloadKey, DownloadRequest, DownloadSnapshot};
use crate::models::friendly_model_name;
use crate::ui::widgets::{
    present_destructive_confirm, send_desktop_notification, spawn_blocking_ui,
};

use super::HfBrowserContext;
use super::download_controls::DownloadControls;

/// Optional inline row controls bound alongside the global [`DownloadTracker`] row.
pub(super) struct RowDownloadUi {
    pub(super) btn: gtk::Button,
    pub(super) controls: DownloadControls,
    pub(super) on_pause: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    pub(super) on_cancel: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
}

fn attach_subscription_to_tracker(
    key: DownloadKey,
    display_name: String,
    initial: Option<DownloadSnapshot>,
    mut rx: broadcast::Receiver<DownloadSnapshot>,
    ctx: &HfBrowserContext,
    row_ui: Option<RowDownloadUi>,
) {
    let mgr_pause = ctx.download_manager.clone();
    let key_pause = key.clone();
    let on_pause: Rc<dyn Fn()> = Rc::new(move || {
        let is_paused = mgr_pause
            .active_downloads()
            .into_iter()
            .find(|s| s.key == key_pause)
            .is_some_and(|s| s.snapshot.is_paused);
        if is_paused {
            mgr_pause.resume(&key_pause);
        } else {
            mgr_pause.pause(&key_pause);
        }
    });

    let mgr_cancel = ctx.download_manager.clone();
    let dialog_confirm = ctx.dialog.clone();
    let display_name_confirm = display_name.clone();
    let key_cancel = key.clone();
    let on_cancel: Rc<dyn Fn()> = Rc::new(move || {
        let mgr_c = mgr_cancel.clone();
        let kc = key_cancel.clone();
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
                let _ = mgr_c.cancel(&kc);
            },
        );
    });

    let (row_btn, row_controls) = match row_ui {
        Some(ui) => {
            *ui.on_pause.borrow_mut() = Some(on_pause.clone());
            *ui.on_cancel.borrow_mut() = Some(on_cancel.clone());
            (Some(ui.btn), Some(ui.controls))
        }
        None => (None, None),
    };

    let (tracker_controls, finish_tracker) =
        ctx.tracker
            .track(&key.0, &display_name, on_pause, on_cancel);

    if let Some(init) = initial {
        apply_snapshot(&tracker_controls, row_controls.as_ref(), &init);
    }

    let dialog = ctx.dialog.clone();
    let refresh_installed = ctx.refresh_installed.clone();
    let repo_id = key.0;

    glib::MainContext::default().spawn_local(async move {
        while let Ok(snap) = rx.recv().await {
            apply_snapshot(&tracker_controls, row_controls.as_ref(), &snap);

            if snap.finished {
                finish_tracker();
                if let Some(rc) = &row_controls {
                    rc.container.set_visible(false);
                }
                if let Some(btn) = &row_btn {
                    btn.set_visible(true);
                    btn.set_sensitive(true);
                }
                if !snap.cancelled {
                    if let Some(err) = snap.error {
                        dialog.add_toast(adw::Toast::new(&tr_f(
                            "Download failed: {err}",
                            &[("err", &err)],
                        )));
                        send_desktop_notification(
                            &format!("download-{repo_id}"),
                            &gettext("Download Failed"),
                            &tr_f("{name}: {err}", &[("name", &display_name), ("err", &err)]),
                        );
                    } else {
                        refresh_installed();
                        dialog.add_toast(adw::Toast::new(&tr_f(
                            "Installed {name}",
                            &[("name", &display_name)],
                        )));
                        send_desktop_notification(
                            &format!("download-{repo_id}"),
                            &gettext("Model Downloaded"),
                            &tr_f("{name} is ready to chat.", &[("name", &display_name)]),
                        );
                    }
                }
                break;
            }
        }
    });
}

fn apply_snapshot(
    tracker_controls: &DownloadControls,
    row_controls: Option<&DownloadControls>,
    snap: &DownloadSnapshot,
) {
    tracker_controls.set_progress(snap.done, snap.total);
    tracker_controls.set_paused(snap.is_paused);
    if let Some(rc) = row_controls {
        rc.set_progress(snap.done, snap.total);
        rc.set_paused(snap.is_paused);
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
    let ctx_ui = ctx.clone();
    spawn_blocking_ui(
        move || mgr_bg.restore_pending(),
        move |handles| {
            for handle in handles {
                let init = DownloadSnapshot {
                    done: 0,
                    total: None,
                    is_paused: true,
                    finished: false,
                    cancelled: false,
                    error: None,
                };
                attach_subscription_to_tracker(
                    handle.key,
                    handle.display_name,
                    Some(init),
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
