//! Vision projector (`mmproj`) discovery, confirmation, and manual file selection dialogs.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::gguf::inspect_gguf_metadata;
use crate::models::hardware::HardwareInfo;
use crate::models::huggingface::{MmprojMatch, find_matching_mmproj};
use crate::models::{format_bytes, friendly_model_name};
use crate::storage::{Database, ModelRecord};
use crate::ui::hf_browser::{DownloadControls, DownloadTracker, build_explore_subpage_with_query};
use crate::ui::widgets::spawn_blocking_ui;

use super::vision_download::start_mmproj_download;

fn open_gguf_file_dialog<F>(dialog: &adw::PreferencesDialog, title: &str, on_selected: F)
where
    F: FnOnce(std::path::PathBuf) + 'static,
{
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&gettext("GGUF Models (*.gguf)")));
    filter.add_suffix("gguf");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);

    let file_dialog = gtk::FileDialog::builder()
        .title(title)
        .filters(&filters)
        .modal(true)
        .build();

    let window = dialog.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    file_dialog.open(window.as_ref(), None::<&gio::Cancellable>, move |res| {
        if let Ok(file) = res
            && let Some(path) = file.path()
        {
            on_selected(path);
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lookup_and_enable_vision(
    btn: &gtk::Button,
    controls: &DownloadControls,
    on_pause_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_cancel_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    dialog: &adw::PreferencesDialog,
    db: &Database,
    hw: &HardwareInfo,
    model: ModelRecord,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
    models_dir: PathBuf,
) {
    btn.set_sensitive(false);
    controls.pct_label.set_text("");
    controls.pause_btn.set_visible(false);
    controls.cancel_btn.set_visible(false);
    controls.container.set_visible(true);
    controls.progress.pulse();

    let still_searching = Rc::new(Cell::new(true));
    let searching_timer = still_searching.clone();
    let progress_timer = controls.progress.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if !searching_timer.get() {
            return glib::ControlFlow::Break;
        }
        progress_timer.pulse();
        glib::ControlFlow::Continue
    });

    let lookup_name = model
        .file_path
        .as_deref()
        .and_then(|p| Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&model.display_name)
        .to_string();

    let (tx, rx) = tokio::sync::oneshot::channel::<Result<Option<MmprojMatch>, String>>();
    runtime().spawn(async move {
        let res = find_matching_mmproj(&lookup_name)
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(res);
    });

    let btn_done = btn.clone();
    let controls_done = controls.clone();
    let on_pause_handler = on_pause_handler.clone();
    let on_cancel_handler = on_cancel_handler.clone();
    let dialog_ref = dialog.clone();
    let db = db.clone();
    let hw = hw.clone();
    let refresh = refresh_installed.clone();
    let tracker = tracker.clone();

    glib::MainContext::default().spawn_local(async move {
        let res = rx.await;
        still_searching.set(false);
        btn_done.set_sensitive(true);
        controls_done.container.set_visible(false);

        match res {
            Ok(Ok(Some(matched))) => {
                present_matched_vision_confirm(
                    &btn_done,
                    &controls_done,
                    &on_pause_handler,
                    &on_cancel_handler,
                    &dialog_ref,
                    &db,
                    &hw,
                    model,
                    matched,
                    &refresh,
                    &tracker,
                    models_dir.clone(),
                );
            }
            Ok(Ok(None)) => {
                present_manual_vision_guide(
                    &dialog_ref,
                    &db,
                    &hw,
                    model,
                    &refresh,
                    &tracker,
                    models_dir.clone(),
                    None,
                );
            }
            Ok(Err(err)) => {
                present_manual_vision_guide(
                    &dialog_ref,
                    &db,
                    &hw,
                    model,
                    &refresh,
                    &tracker,
                    models_dir.clone(),
                    Some(err),
                );
            }
            Err(_) => {}
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn present_matched_vision_confirm(
    btn: &gtk::Button,
    controls: &DownloadControls,
    on_pause_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_cancel_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    dialog: &adw::PreferencesDialog,
    db: &Database,
    hw: &HardwareInfo,
    model: ModelRecord,
    matched: MmprojMatch,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
    models_dir: PathBuf,
) {
    let size_label = format_bytes(matched.file.size_bytes);
    let model_name = friendly_model_name(&model.display_name);
    let body = tr_f(
        "Found a matching vision projector on Hugging Face for \"{model}\":\n\nRepository: {repo}\nFile: {file} ({size})",
        &[
            ("model", &model_name),
            ("repo", &matched.repo_id),
            ("file", &matched.file.filename),
            ("size", &size_label),
        ],
    );

    let alert = adw::AlertDialog::builder()
        .heading(gettext("Enable Vision"))
        .body(&body)
        .build();
    alert.add_response("cancel", &gettext("Cancel"));
    alert.add_response("other", &gettext("Other Options"));
    alert.add_response("install", &gettext("Install Vision"));
    alert.set_response_appearance("install", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("install"));
    alert.set_close_response("cancel");

    let btn = btn.clone();
    let controls = controls.clone();
    let on_pause_handler = on_pause_handler.clone();
    let on_cancel_handler = on_cancel_handler.clone();
    let dialog_ref = dialog.clone();
    let db = db.clone();
    let hw = hw.clone();
    let refresh = refresh_installed.clone();
    let tracker = tracker.clone();

    alert.connect_response(None, move |_, resp| match resp {
        "install" => {
            start_mmproj_download(
                &btn,
                &controls,
                &on_pause_handler,
                &on_cancel_handler,
                &dialog_ref,
                &db,
                model.clone(),
                matched.repo_id.clone(),
                Some(matched.file.clone()),
                &refresh,
                models_dir.clone(),
            );
        }
        "other" => {
            present_manual_vision_guide(
                &dialog_ref,
                &db,
                &hw,
                model.clone(),
                &refresh,
                &tracker,
                models_dir.clone(),
                None,
            );
        }
        _ => {}
    });

    alert.present(Some(dialog));
}

fn present_manual_vision_guide(
    dialog: &adw::PreferencesDialog,
    db: &Database,
    hw: &HardwareInfo,
    model: ModelRecord,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
    models_dir: PathBuf,
    lookup_error: Option<String>,
) {
    if let Some(err) = lookup_error {
        dialog.add_toast(adw::Toast::new(&tr_f(
            "Could not search Hugging Face: {err}",
            &[("err", &err)],
        )));
    }

    let alert = adw::AlertDialog::builder()
        .heading(gettext("Enable Vision"))
        .body(gettext(
            "No matching vision projector was found automatically on Hugging Face.\n\nSelect a matching mmproj .gguf file from your computer, or search Hugging Face manually.",
        ))
        .build();
    alert.add_response("cancel", &gettext("Cancel"));
    alert.add_response("explore", &gettext("Search Hugging Face"));
    alert.add_response("select", &gettext("Select File"));
    alert.set_response_appearance("select", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("select"));
    alert.set_close_response("cancel");

    let dialog_ref = dialog.clone();
    let db = db.clone();
    let hw = hw.clone();
    let refresh = refresh_installed.clone();
    let tracker = tracker.clone();

    alert.connect_response(None, move |_, resp| match resp {
        "select" => {
            let db = db.clone();
            let dialog_toast = dialog_ref.clone();
            let refresh = refresh.clone();
            let mut updated = model.clone();
            open_gguf_file_dialog(
                &dialog_ref,
                &gettext("Select Vision Projector (mmproj.gguf)"),
                move |path| {
                    spawn_blocking_ui(
                        move || {
                            if !inspect_gguf_metadata(&path).is_clip {
                                return Err(anyhow::anyhow!(gettext(
                                    "Selected file is not a valid CLIP vision projector (mmproj)"
                                )));
                            }
                            updated.mmproj_path = Some(path.to_string_lossy().to_string());
                            db.upsert_model(&updated)
                        },
                        move |res| match res {
                            Ok(()) => {
                                refresh();
                                dialog_toast.add_toast(adw::Toast::new(&gettext(
                                    "Enabled Vision projector",
                                )));
                            }
                            Err(err) => {
                                dialog_toast.add_toast(adw::Toast::new(&tr_f(
                                    "Failed to save: {err}",
                                    &[("err", &err.to_string())],
                                )));
                            }
                        },
                    );
                },
            );
        }
        "explore" => {
            let query = friendly_model_name(&model.display_name);
            let subpage = build_explore_subpage_with_query(
                &dialog_ref,
                db.clone(),
                hw.clone(),
                refresh.clone(),
                tracker.clone(),
                models_dir.clone(),
                Some(&query),
            );
            dialog_ref.push_subpage(&subpage);
        }
        _ => {}
    });

    alert.present(Some(dialog));
}
