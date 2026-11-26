//! Resumable `mmproj` vision projector download with pause/resume/cancel controls.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gtk::glib;

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::huggingface::{
    HfGgufFile, download_gguf_file, list_repo_gguf_files, remove_part_file, select_best_mmproj,
};
use crate::models::{friendly_model_name, managed_models_dir};
use crate::storage::{Database, ModelRecord};
use crate::ui::hf_browser::DownloadControls;
use crate::ui::widgets::{present_destructive_confirm, send_desktop_notification};

struct MmprojDownloadState {
    is_paused: bool,
    is_cancelled: bool,
    stop_flag: Arc<AtomicBool>,
    dest_path: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    resolved_file: Arc<std::sync::Mutex<Option<HfGgufFile>>>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn start_mmproj_download(
    btn: &gtk::Button,
    controls: &DownloadControls,
    on_pause_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_cancel_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    dialog: &adw::PreferencesDialog,
    db: &Database,
    model: ModelRecord,
    repo_id: String,
    preselected_file: Option<HfGgufFile>,
    refresh_installed: &Rc<dyn Fn()>,
) {
    btn.set_visible(false);
    controls.reset_active();

    let safe_repo = repo_id.replace('/', "--");
    let initial_dest = preselected_file
        .as_ref()
        .map(|f| managed_models_dir().join(&safe_repo).join(&f.filename));

    let state = Rc::new(RefCell::new(MmprojDownloadState {
        is_paused: false,
        is_cancelled: false,
        stop_flag: Arc::new(AtomicBool::new(false)),
        dest_path: Arc::new(std::sync::Mutex::new(initial_dest)),
        resolved_file: Arc::new(std::sync::Mutex::new(preselected_file)),
    }));

    let launch_attempt: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    {
        let launch_attempt_weak = launch_attempt.clone();
        let state = state.clone();
        let btn = btn.clone();
        let controls = controls.clone();
        let on_pause_handler = on_pause_handler.clone();
        let on_cancel_handler = on_cancel_handler.clone();
        let dialog_toast = dialog.clone();
        let db = db.clone();
        let model = model.clone();
        let repo_id = repo_id.clone();
        let refresh = refresh_installed.clone();
        let model_name = friendly_model_name(&model.display_name);

        *launch_attempt.borrow_mut() = Some(Rc::new(move || {
            let stop_flag = Arc::new(AtomicBool::new(false));
            state.borrow_mut().stop_flag = stop_flag.clone();

            let (prog_tx, mut prog_rx) =
                tokio::sync::mpsc::unbounded_channel::<(u64, Option<u64>)>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

            let db_bg = db.clone();
            let repo_bg = repo_id.clone();
            let mut updated = model.clone();
            let dest_shared = state.borrow().dest_path.clone();
            let resolved_shared = state.borrow().resolved_file.clone();

            runtime().spawn(async move {
                let cached_file = resolved_shared.lock().ok().and_then(|g| g.clone());
                let mmproj_file = match cached_file {
                    Some(file) => file,
                    None => {
                        let files = match list_repo_gguf_files(&repo_bg).await {
                            Ok(f) => f,
                            Err(err) => {
                                let _ = done_tx.send(Err(err.to_string()));
                                return;
                            }
                        };
                        let Some(file) = select_best_mmproj(&files) else {
                            let _ = done_tx.send(Err(gettext(
                                "No mmproj vision projector file was found in this repository",
                            )));
                            return;
                        };
                        if let Ok(mut g) = resolved_shared.lock() {
                            *g = Some(file.clone());
                        }
                        file
                    }
                };

                let safe_repo = repo_bg.replace('/', "--");
                let dest_path = managed_models_dir()
                    .join(&safe_repo)
                    .join(&mmproj_file.filename);
                if let Ok(mut g) = dest_shared.lock() {
                    *g = Some(dest_path.clone());
                }

                if let Err(err) = download_gguf_file(
                    &repo_bg,
                    &mmproj_file.filename,
                    &dest_path,
                    stop_flag,
                    move |done, total| {
                        let _ = prog_tx.send((done, total));
                    },
                )
                .await
                {
                    let _ = done_tx.send(Err(err.to_string()));
                    return;
                }

                updated.mmproj_path = Some(dest_path.to_string_lossy().to_string());
                let save_res = tokio::task::spawn_blocking(move || db_bg.upsert_model(&updated))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()));
                let _ = done_tx.send(save_res);
            });

            {
                let controls = controls.clone();
                glib::MainContext::default().spawn_local(async move {
                    while let Some((done, total)) = prog_rx.recv().await {
                        controls.set_progress(done, total);
                    }
                });
            }

            {
                let state = state.clone();
                let btn = btn.clone();
                let controls = controls.clone();
                let on_pause_handler = on_pause_handler.clone();
                let on_cancel_handler = on_cancel_handler.clone();
                let dialog_toast = dialog_toast.clone();
                let refresh = refresh.clone();
                let model_name = model_name.clone();
                let model_id = model.id.clone();
                let launch_attempt_weak = launch_attempt_weak.clone();

                glib::MainContext::default().spawn_local(async move {
                    let res = done_rx.await;
                    let (is_paused, is_cancelled) = {
                        let st = state.borrow();
                        (st.is_paused, st.is_cancelled)
                    };

                    if is_paused && !is_cancelled {
                        return;
                    }

                    *launch_attempt_weak.borrow_mut() = None;
                    *on_pause_handler.borrow_mut() = None;
                    *on_cancel_handler.borrow_mut() = None;
                    controls.container.set_visible(false);
                    btn.set_visible(true);
                    btn.set_sensitive(true);

                    if is_cancelled {
                        let dest_shared = state.borrow().dest_path.clone();
                        runtime().spawn(async move {
                            let path = dest_shared.lock().ok().and_then(|g| g.clone());
                            if let Some(p) = path {
                                remove_part_file(&p).await;
                            }
                        });
                        return;
                    }

                    match res {
                        Ok(Ok(())) => {
                            refresh();
                            dialog_toast.add_toast(adw::Toast::new(&tr_f(
                                "Enabled Vision for \"{name}\"",
                                &[("name", &model_name)],
                            )));
                            send_desktop_notification(
                                &format!("vision-{model_id}"),
                                &gettext("Vision Enabled"),
                                &tr_f(
                                    "Vision projector for {name} is ready.",
                                    &[("name", &model_name)],
                                ),
                            );
                        }
                        Ok(Err(err)) => {
                            dialog_toast.add_toast(adw::Toast::new(&tr_f(
                                "Failed to install Vision: {err}",
                                &[("err", &err)],
                            )));
                            send_desktop_notification(
                                &format!("vision-{model_id}"),
                                &gettext("Vision Download Failed"),
                                &tr_f("{name}: {err}", &[("name", &model_name), ("err", &err)]),
                            );
                        }
                        Err(_) => {}
                    }
                });
            }
        }));
    }

    let on_pause: Rc<dyn Fn()> = {
        let state = state.clone();
        let controls = controls.clone();
        let launch_attempt = launch_attempt.clone();
        Rc::new(move || {
            let currently_paused = state.borrow().is_paused;
            if !currently_paused {
                state.borrow_mut().is_paused = true;
                state.borrow().stop_flag.store(true, Ordering::Relaxed);
                controls.set_paused(true);
            } else {
                state.borrow_mut().is_paused = false;
                controls.set_paused(false);
                let launch = launch_attempt.borrow().clone();
                if let Some(launch) = launch {
                    launch();
                }
            }
        })
    };

    let on_cancel: Rc<dyn Fn()> = {
        let state = state.clone();
        let btn = btn.clone();
        let controls = controls.clone();
        let on_pause_handler = on_pause_handler.clone();
        let on_cancel_handler = on_cancel_handler.clone();
        let launch_attempt = launch_attempt.clone();
        let dialog_confirm = dialog.clone();
        let model_name_confirm = friendly_model_name(&model.display_name);
        Rc::new(move || {
            let state = state.clone();
            let btn = btn.clone();
            let controls = controls.clone();
            let on_pause_handler = on_pause_handler.clone();
            let on_cancel_handler = on_cancel_handler.clone();
            let launch_attempt = launch_attempt.clone();
            let body = tr_f(
                "Cancel downloading the vision projector for \"{name}\" and delete the partial download?",
                &[("name", &model_name_confirm)],
            );

            present_destructive_confirm(
                &dialog_confirm,
                &gettext("Cancel Download?"),
                &body,
                &gettext("Cancel Download"),
                move || {
                    if launch_attempt.borrow().is_none() {
                        return;
                    }
                    let was_paused = state.borrow().is_paused;
                    {
                        let mut st = state.borrow_mut();
                        st.is_cancelled = true;
                        st.is_paused = false;
                        st.stop_flag.store(true, Ordering::Relaxed);
                    }
                    if was_paused {
                        *launch_attempt.borrow_mut() = None;
                        *on_pause_handler.borrow_mut() = None;
                        *on_cancel_handler.borrow_mut() = None;
                        controls.container.set_visible(false);
                        btn.set_visible(true);
                        btn.set_sensitive(true);
                        let dest_shared = state.borrow().dest_path.clone();
                        runtime().spawn(async move {
                            let path = dest_shared.lock().ok().and_then(|g| g.clone());
                            if let Some(p) = path {
                                remove_part_file(&p).await;
                            }
                        });
                    }
                },
            );
        })
    };

    *on_pause_handler.borrow_mut() = Some(on_pause);
    *on_cancel_handler.borrow_mut() = Some(on_cancel);

    let launch = launch_attempt.borrow().clone();
    if let Some(launch) = launch {
        launch();
    }
}
