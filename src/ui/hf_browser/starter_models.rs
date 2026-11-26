//! Recommended starter model group and 1-click download row.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::format_bytes;
use crate::models::huggingface::{
    HfGgufFile, RECOMMENDED_STARTER_NAME, RECOMMENDED_STARTER_REPO, list_repo_gguf_files,
    select_starter_gguf,
};
use crate::storage::Database;

use super::download_controls::{DownloadControls, DownloadTracker};
use super::download_tracker::start_hf_download;

/// Build a reusable "Recommended Starter Model" [`adw::PreferencesGroup`].
pub fn build_starter_model_group(
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
    models_dir: PathBuf,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Recommended Starter AI Model"))
        .build();
    group.add(&build_starter_model_row(
        db,
        dialog,
        refresh_installed,
        tracker,
        models_dir,
    ));
    group
}

/// Build a 1-click starter model row (`Gemma 4 E4B`) with percentage, pause/resume, and cancel.
pub fn build_starter_model_row(
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
    models_dir: PathBuf,
) -> adw::ActionRow {
    let default_sub = gettext("Fast, lightweight starter model with Text, Thinking, and Vision");
    let row = adw::ActionRow::builder()
        .title(RECOMMENDED_STARTER_NAME)
        .subtitle(&default_sub)
        .build();

    let resolved_starter: Rc<RefCell<Option<(HfGgufFile, Option<HfGgufFile>)>>> =
        Rc::new(RefCell::new(None));
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<Option<(HfGgufFile, Option<HfGgufFile>)>>();
        runtime().spawn(async move {
            let found = list_repo_gguf_files(RECOMMENDED_STARTER_REPO)
                .await
                .ok()
                .and_then(|files| select_starter_gguf(&files));
            let _ = tx.send(found);
        });
        let row_ref = row.clone();
        let resolved_ref = resolved_starter.clone();
        let friendly_line = default_sub.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Ok(Some((model_file, mmproj_file))) = rx.await {
                let mut lines = vec![
                    friendly_line,
                    tr_f(
                        "Model: {file} ({size})",
                        &[
                            ("file", &model_file.filename),
                            ("size", &format_bytes(model_file.size_bytes)),
                        ],
                    ),
                ];
                if let Some(mm) = &mmproj_file {
                    lines.push(tr_f(
                        "Vision: {file} ({size})",
                        &[
                            ("file", &mm.filename),
                            ("size", &format_bytes(mm.size_bytes)),
                        ],
                    ));
                }
                row_ref.set_subtitle(&gtk::glib::markup_escape_text(&lines.join("\n")));
                *resolved_ref.borrow_mut() = Some((model_file, mmproj_file));
            }
        });
    }

    let is_installed = db
        .list_models()
        .unwrap_or_default()
        .iter()
        .any(|m| {
            m.hf_repo.as_deref() == Some(RECOMMENDED_STARTER_REPO)
                && m.file_path
                    .as_deref()
                    .is_some_and(|p| std::path::Path::new(p).exists())
        });

    let is_downloading = tracker.is_repo_active(RECOMMENDED_STARTER_REPO);

    let controls = DownloadControls::new();
    let installed_label = gtk::Label::builder()
        .label(&gettext("Installed"))
        .valign(gtk::Align::Center)
        .css_classes(["dim-label", "caption-heading"])
        .visible(is_installed)
        .build();
    let downloading_label = gtk::Label::builder()
        .label(&gettext("Downloading"))
        .valign(gtk::Align::Center)
        .css_classes(["dim-label", "caption-heading"])
        .visible(!is_installed && is_downloading)
        .build();
    let download_btn = gtk::Button::builder()
        .label(&gettext("Download"))
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .visible(!is_installed && !is_downloading)
        .build();

    let on_pause_handler: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_cancel_handler: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    {
        let on_pause_handler = on_pause_handler.clone();
        controls.pause_btn.connect_clicked(move |_| {
            if let Some(cb) = on_pause_handler.borrow().clone() {
                cb();
            }
        });
    }
    {
        let on_cancel_handler = on_cancel_handler.clone();
        controls.cancel_btn.connect_clicked(move |_| {
            if let Some(cb) = on_cancel_handler.borrow().clone() {
                cb();
            }
        });
    }

    {
        let db = db.clone();
        let dialog = dialog.clone();
        let refresh_installed = refresh_installed.clone();
        let tracker = tracker.clone();
        let controls = controls.clone();
        let installed_label = installed_label.clone();
        let resolved_starter = resolved_starter.clone();

        download_btn.connect_clicked(move |btn| {
            if let Some((model_file, mmproj_file)) = resolved_starter.borrow().clone() {
                let refresh_with_badge: Rc<dyn Fn()> = {
                    let refresh_installed = refresh_installed.clone();
                    let btn = btn.clone();
                    let installed_label = installed_label.clone();
                    Rc::new(move || {
                        btn.set_visible(false);
                        installed_label.set_visible(true);
                        refresh_installed();
                    })
                };
                start_hf_download(
                    btn,
                    &controls,
                    &on_pause_handler,
                    &on_cancel_handler,
                    RECOMMENDED_STARTER_REPO,
                    &model_file.filename,
                    mmproj_file.as_ref().map(|m| m.filename.as_str()),
                    &db,
                    &dialog,
                    &refresh_with_badge,
                    &tracker,
                    models_dir.clone(),
                );
                return;
            }

            btn.set_sensitive(false);
            controls.pct_label.set_text("");
            controls.pause_btn.set_visible(false);
            controls.cancel_btn.set_visible(false);
            controls.container.set_visible(true);
            controls.progress.pulse();

            let (tx, rx) = tokio::sync::oneshot::channel::<
                Result<(HfGgufFile, Option<HfGgufFile>), String>,
            >();
            runtime().spawn(async move {
                let res = list_repo_gguf_files(RECOMMENDED_STARTER_REPO)
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|files| {
                        select_starter_gguf(&files).ok_or_else(|| {
                            gettext("No starter AI model found in repository")
                        })
                    });
                let _ = tx.send(res);
            });

            let btn = btn.clone();
            let controls = controls.clone();
            let on_pause_handler = on_pause_handler.clone();
            let on_cancel_handler = on_cancel_handler.clone();
            let db = db.clone();
            let dialog = dialog.clone();
            let refresh_installed = refresh_installed.clone();
            let tracker = tracker.clone();
            let installed_label = installed_label.clone();
            let models_dir_async = models_dir.clone();

            glib::MainContext::default().spawn_local(async move {
                let res = rx.await;
                btn.set_sensitive(true);
                match res {
                    Ok(Ok((model_file, mmproj_file))) => {
                        let refresh_with_badge: Rc<dyn Fn()> = {
                            let refresh_installed = refresh_installed.clone();
                            let btn = btn.clone();
                            let installed_label = installed_label.clone();
                            Rc::new(move || {
                                btn.set_visible(false);
                                installed_label.set_visible(true);
                                refresh_installed();
                            })
                        };
                        start_hf_download(
                            &btn,
                            &controls,
                            &on_pause_handler,
                            &on_cancel_handler,
                            RECOMMENDED_STARTER_REPO,
                            &model_file.filename,
                            mmproj_file.as_ref().map(|m| m.filename.as_str()),
                            &db,
                            &dialog,
                            &refresh_with_badge,
                            &tracker,
                            models_dir_async,
                        );
                    }
                    Ok(Err(err)) => {
                        controls.container.set_visible(false);
                        dialog.add_toast(adw::Toast::new(&tr_f(
                            "Failed to fetch starter model: {err}",
                            &[("err", &err)],
                        )));
                    }
                    Err(_) => {
                        controls.container.set_visible(false);
                    }
                }
            });
        });
    }

    row.add_suffix(&controls.container);
    row.add_suffix(&downloading_label);
    row.add_suffix(&installed_label);
    row.add_suffix(&download_btn);
    row
}
