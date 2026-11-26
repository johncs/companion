//! Recommended starter model group and 1-click download row.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::format::markdown::escape_pango_markup;
use crate::i18n::{gettext, tr_f};
use crate::models::format_bytes;
use crate::models::huggingface::{
    HfGgufFile, RECOMMENDED_STARTER_NAME, RECOMMENDED_STARTER_REPO, select_starter_gguf,
};

use super::HfBrowserContext;
use super::download_controls::DownloadControls;
use super::download_tracker::{RowDownloadUi, start_hf_download};

type ResolvedStarter = Rc<RefCell<Option<(HfGgufFile, Option<HfGgufFile>)>>>;

/// Build a reusable "Recommended Starter Model" [`adw::PreferencesGroup`].
pub fn build_starter_model_group(ctx: &HfBrowserContext) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Recommended Starter AI Model"))
        .build();
    group.add(&build_starter_model_row(ctx));
    group
}

/// Build a 1-click starter model row (`Gemma 4 E4B`) with percentage, pause/resume, and cancel.
pub fn build_starter_model_row(ctx: &HfBrowserContext) -> adw::ActionRow {
    let default_sub = gettext("Fast, lightweight starter model with Text, Thinking, and Vision");
    let row = adw::ActionRow::builder()
        .title(RECOMMENDED_STARTER_NAME)
        .subtitle(&default_sub)
        .build();

    let resolved_starter: ResolvedStarter = Rc::new(RefCell::new(None));
    let is_installed = is_starter_installed(&ctx.db);
    let is_downloading = ctx
        .tracker
        .is_repo_active(&ctx.download_manager, RECOMMENDED_STARTER_REPO);

    let lookup_started = Rc::new(std::cell::Cell::new(false));
    {
        let services_map = ctx.services.clone();
        let default_sub = default_sub.clone();
        let resolved_starter = resolved_starter.clone();
        row.connect_map(move |r| {
            if !lookup_started.get() {
                lookup_started.set(true);
                spawn_starter_metadata_lookup(r, &services_map, &default_sub, &resolved_starter);
            }
        });
    }

    let controls = DownloadControls::new();
    let installed_label = build_status_badge(&gettext("Installed"), is_installed);
    let downloading_label =
        build_status_badge(&gettext("Downloading"), !is_installed && is_downloading);
    let download_btn = gtk::Button::builder()
        .label(gettext("Download"))
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .visible(!is_installed && !is_downloading)
        .build();

    let ui = StarterUi {
        controls: controls.clone(),
        installed_label: installed_label.clone(),
        downloading_label: downloading_label.clone(),
    };
    wire_starter_download_button(&download_btn, ui, &resolved_starter, ctx);

    row.add_suffix(&controls.container);
    row.add_suffix(&downloading_label);
    row.add_suffix(&installed_label);
    row.add_suffix(&download_btn);
    row
}

fn spawn_starter_metadata_lookup(
    row: &adw::ActionRow,
    services: &crate::services::AppServices,
    default_sub: &str,
    resolved_starter: &ResolvedStarter,
) {
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<(HfGgufFile, Option<HfGgufFile>)>>();
    let hf_client = services.hf_client.clone();
    services.rt_handle.spawn(async move {
        let found = hf_client
            .list_repo_gguf_files(RECOMMENDED_STARTER_REPO)
            .await
            .ok()
            .and_then(|files| select_starter_gguf(&files));
        let _ = tx.send(found);
    });
    let row_ref = row.clone();
    let resolved_ref = resolved_starter.clone();
    let friendly_line = default_sub.to_string();
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
            row_ref.set_subtitle(&escape_pango_markup(&lines.join("\n")));
            *resolved_ref.borrow_mut() = Some((model_file, mmproj_file));
        }
    });
}

fn is_starter_installed(db: &crate::storage::Database) -> bool {
    db.list_models().unwrap_or_default().iter().any(|m| {
        m.hf_repo.as_deref() == Some(RECOMMENDED_STARTER_REPO)
            && m.file_path
                .as_deref()
                .is_some_and(|p| std::path::Path::new(p).exists())
    })
}

fn build_status_badge(label: &str, visible: bool) -> gtk::Label {
    gtk::Label::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .css_classes(["dim-label", "caption-heading"])
        .visible(visible)
        .build()
}

#[derive(Clone)]
struct StarterUi {
    controls: DownloadControls,
    installed_label: gtk::Label,
    downloading_label: gtk::Label,
}

fn wire_starter_download_button(
    download_btn: &gtk::Button,
    ui: StarterUi,
    resolved_starter: &ResolvedStarter,
    ctx: &HfBrowserContext,
) {
    let ctx = ctx.clone();
    let resolved_starter = resolved_starter.clone();

    download_btn.connect_clicked(move |btn| {
        if let Some((model_file, mmproj_file)) = resolved_starter.borrow().clone() {
            launch_starter_download(btn, &ui, &ctx, model_file, mmproj_file);
            return;
        }

        let pulsing = start_starter_pulse(&ui.controls);
        let rx = spawn_starter_gguf_lookup(&ctx.services.hf_client, &ctx.services.rt_handle);
        let (btn, ui, ctx) = (btn.clone(), ui.clone(), ctx.clone());
        glib::MainContext::default().spawn_local(async move {
            let res = rx.await;
            pulsing.set(false);
            btn.set_sensitive(true);
            match res {
                Ok(Ok((model_file, mmproj_file))) => {
                    launch_starter_download(&btn, &ui, &ctx, model_file, mmproj_file)
                }
                Ok(Err(err)) => {
                    ui.controls.container.set_visible(false);
                    ctx.dialog.add_toast(adw::Toast::new(&tr_f(
                        "Failed to fetch starter model: {err}",
                        &[("err", &err)],
                    )));
                }
                Err(_) => ui.controls.container.set_visible(false),
            }
        });
    });
}

fn spawn_starter_gguf_lookup(
    hf_client: &std::sync::Arc<crate::models::huggingface::HfClient>,
    rt_handle: &tokio::runtime::Handle,
) -> tokio::sync::oneshot::Receiver<Result<(HfGgufFile, Option<HfGgufFile>), String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let hf_client = hf_client.clone();
    rt_handle.spawn(async move {
        let res = hf_client
            .list_repo_gguf_files(RECOMMENDED_STARTER_REPO)
            .await
            .map_err(|e| e.to_string())
            .and_then(|files| {
                select_starter_gguf(&files)
                    .ok_or_else(|| gettext("No starter AI model found in repository"))
            });
        let _ = tx.send(res);
    });
    rx
}

fn start_starter_pulse(controls: &DownloadControls) -> Rc<std::cell::Cell<bool>> {
    controls.pct_label.set_text("");
    controls.pause_btn.set_visible(false);
    controls.cancel_btn.set_visible(false);
    controls.container.set_visible(true);
    controls.progress.pulse();

    let pulsing = Rc::new(std::cell::Cell::new(true));
    let pulsing_timer = pulsing.clone();
    let progress_timer = controls.progress.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if !pulsing_timer.get() {
            return glib::ControlFlow::Break;
        }
        progress_timer.pulse();
        glib::ControlFlow::Continue
    });
    pulsing
}

fn launch_starter_download(
    btn: &gtk::Button,
    ui: &StarterUi,
    ctx: &HfBrowserContext,
    model_file: HfGgufFile,
    mmproj_file: Option<HfGgufFile>,
) {
    let refresh_with_badge: Rc<dyn Fn()> = {
        let refresh_installed = ctx.refresh_installed.clone();
        let btn = btn.clone();
        let installed_label = ui.installed_label.clone();
        let downloading_label = ui.downloading_label.clone();
        Rc::new(move || {
            btn.set_visible(false);
            downloading_label.set_visible(false);
            installed_label.set_visible(true);
            refresh_installed();
        })
    };
    let badge_ctx = ctx.with_refresh(refresh_with_badge);
    start_hf_download(
        &badge_ctx,
        RECOMMENDED_STARTER_REPO,
        &model_file.filename,
        mmproj_file.as_ref().map(|m| m.filename.as_str()),
        RowDownloadUi {
            btn: btn.clone(),
            controls: ui.controls.clone(),
        },
    );
}
