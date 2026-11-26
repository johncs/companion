//! Models settings page: active model selection, installed GGUF management, and Hugging Face explore.

mod active_details;
mod active_group;
mod inventory;
mod view_model;
mod vision_download;
mod vision_setup;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::i18n::{gettext, tr_f};
use crate::models::download_manager::DownloadManager;
use crate::models::hardware::HardwareInfo;
use crate::models::registry::ModelRegistryService;
use crate::storage::{Database, ModelRecord};
use crate::ui::hf_browser::{
    DownloadControls, DownloadTracker, HfBrowserContext, build_explore_subpage,
    build_starter_model_group, restore_pending_downloads,
};
use crate::ui::icons;
use crate::ui::widgets::spawn_blocking_ui;

use active_details::ActiveDetailRows;
use active_group::{
    apply_installed_models_snapshot, build_active_group, build_explore_group,
    make_active_record_getter, wire_active_group_actions,
};
use inventory::{InstalledModelInfo, stat_models_parallel};
use vision_download::{VisionRowUi, start_mmproj_download};
use vision_setup::{VisionSetupCtx, lookup_and_enable_vision};

#[derive(Clone)]
struct ModelManagerState {
    dialog: adw::PreferencesDialog,
    db: Database,
    hw: HardwareInfo,
    models_dir: PathBuf,
    download_manager: DownloadManager,
    active_id: Rc<RefCell<Option<String>>>,
    cached_models: Rc<RefCell<Vec<InstalledModelInfo>>>,
    installed_badges: Rc<RefCell<Vec<(String, gtk::Label)>>>,
    is_syncing_combo: Rc<Cell<bool>>,
    on_active_changed: Rc<dyn Fn(Option<String>)>,
}

/// Build the "AI Models" [`adw::PreferencesPage`] for the Settings dialog.
pub fn build_models_page(
    dialog: &adw::PreferencesDialog,
    db: Database,
    hw: HardwareInfo,
    models_dir: PathBuf,
    download_manager: DownloadManager,
    active_model_id: Option<String>,
    on_active_changed: Rc<dyn Fn(Option<String>)>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .name("models")
        .title(gettext("AI Models"))
        .icon_name(icons::MODEL)
        .build();

    let state = ModelManagerState {
        dialog: dialog.clone(),
        db: db.clone(),
        hw,
        models_dir,
        download_manager,
        active_id: Rc::new(RefCell::new(active_model_id)),
        cached_models: Rc::new(RefCell::new(Vec::new())),
        installed_badges: Rc::new(RefCell::new(Vec::new())),
        is_syncing_combo: Rc::new(Cell::new(false)),
        on_active_changed,
    };

    let (refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let refresh_installed: Rc<dyn Fn()> = Rc::new(move || {
        let _ = refresh_tx.send(());
    });

    let (active_group, active_model_row, active_rows, vision_controls, active_open_folder_btn) =
        build_active_group(&db);
    page.add(&active_group);

    let get_active_record = make_active_record_getter(&state);
    wire_active_group_actions(
        &state,
        &active_group,
        &active_model_row,
        &active_rows,
        &active_open_folder_btn,
        &get_active_record,
        &refresh_installed,
    );

    let downloading_group = adw::PreferencesGroup::builder()
        .title(gettext("Downloading"))
        .description(gettext(
            "Downloads continue in the background if you close Settings, and pause automatically if you quit Companion.",
        ))
        .visible(false)
        .build();
    let tracker = DownloadTracker::new(downloading_group.clone());

    let hf_ctx = HfBrowserContext {
        dialog: state.dialog.clone(),
        db: state.db.clone(),
        hw: state.hw.clone(),
        download_manager: state.download_manager.clone(),
        tracker,
        refresh_installed: refresh_installed.clone(),
    };

    let starter_group = build_starter_model_group(&hf_ctx);
    starter_group.set_visible(false);
    page.add(&starter_group);
    page.add(&downloading_group);

    let installed_group = adw::PreferencesGroup::builder()
        .title(gettext("Installed AI Models"))
        .build();
    page.add(&installed_group);

    let explore_row = build_explore_group(&page);

    spawn_refresh_loop(
        refresh_rx,
        &state,
        &installed_group,
        &active_model_row,
        &active_rows,
        &starter_group,
        &refresh_installed,
    );
    refresh_installed();
    restore_pending_downloads(&hf_ctx);

    {
        let hf_explore = hf_ctx.clone();
        let dialog_explore = state.dialog.clone();
        explore_row.connect_activated(move |_| {
            let subpage = build_explore_subpage(&hf_explore);
            dialog_explore.push_subpage(&subpage);
        });
    }

    wire_vision_controls(
        &state,
        &vision_controls,
        &active_rows.vision_action_btn,
        &refresh_installed,
        &hf_ctx,
        &get_active_record,
    );

    {
        let db = state.db.clone();
        let active_group = active_group.clone();
        page.connect_map(move |_| {
            let name = db
                .get_assistant_name()
                .unwrap_or_else(|_| "Companion".to_string());
            active_group.set_description(Some(&tr_f(
                "Set the AI model your companion {name} uses. This setting is applied globally.",
                &[("name", &name)],
            )));
        });
    }

    page
}

fn wire_vision_controls(
    state: &ModelManagerState,
    vision_controls: &DownloadControls,
    vision_action_btn: &gtk::Button,
    refresh_installed: &Rc<dyn Fn()>,
    hf_ctx: &HfBrowserContext,
    get_active_record: &Rc<dyn Fn() -> Option<ModelRecord>>,
) {
    let state = state.clone();
    let refresh_installed = refresh_installed.clone();
    let vision_controls = vision_controls.clone();
    let get_active = get_active_record.clone();
    let setup_ctx = VisionSetupCtx {
        dialog: state.dialog.clone(),
        db: state.db.clone(),
        download_manager: state.download_manager.clone(),
        refresh_installed: refresh_installed.clone(),
        hf_ctx: hf_ctx.clone(),
    };

    vision_action_btn.connect_clicked(move |btn| {
        let Some(model) = get_active() else {
            return;
        };
        let ui = VisionRowUi {
            btn: btn.clone(),
            controls: vision_controls.clone(),
        };
        if let Some(repo) = model.hf_repo.clone().filter(|r| !r.trim().is_empty()) {
            start_mmproj_download(
                ui,
                &state.dialog,
                &state.download_manager,
                model,
                repo,
                None,
                &refresh_installed,
            );
        } else {
            lookup_and_enable_vision(ui, &setup_ctx, model);
        }
    });
}

fn spawn_refresh_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    state: &ModelManagerState,
    installed_group: &adw::PreferencesGroup,
    active_model_row: &adw::ComboRow,
    active_rows: &ActiveDetailRows,
    starter_group: &adw::PreferencesGroup,
    on_refresh: &Rc<dyn Fn()>,
) {
    let state = state.clone();
    let installed_group = installed_group.clone();
    let active_model_row = active_model_row.clone();
    let active_rows = active_rows.clone();
    let starter_group = starter_group.clone();
    let on_refresh = on_refresh.clone();
    let generation = Rc::new(Cell::new(0u64));

    glib::MainContext::default().spawn_local(async move {
        while rx.recv().await.is_some() {
            let current_gen = generation.get().wrapping_add(1);
            generation.set(current_gen);

            let db_bg = state.db.clone();
            let models_dir_bg = state.models_dir.clone();
            let state_ui = state.clone();
            let installed_group = installed_group.clone();
            let active_model_row = active_model_row.clone();
            let active_rows = active_rows.clone();
            let starter_group = starter_group.clone();
            let on_refresh = on_refresh.clone();
            let gen_ref = generation.clone();

            spawn_blocking_ui(
                move || {
                    let _ = ModelRegistryService::sync_managed_models(&db_bg, &models_dir_bg);
                    let models = db_bg.list_models().unwrap_or_default();
                    stat_models_parallel(models)
                },
                move |items| {
                    if gen_ref.get() != current_gen {
                        return;
                    }
                    apply_installed_models_snapshot(
                        &state_ui,
                        &installed_group,
                        &active_model_row,
                        &active_rows,
                        &starter_group,
                        &on_refresh,
                        items,
                    );
                },
            );
        }
    });
}
