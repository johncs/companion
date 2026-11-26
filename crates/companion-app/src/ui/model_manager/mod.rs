//! Models settings page: active model selection, installed GGUF management, and Hugging Face explore.

mod active_details;
mod active_group;
mod view_model;
mod vision_download;
mod vision_setup;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use crate::i18n::{gettext, tr_f};
use crate::models::download_manager::DownloadManager;
use crate::models::hardware::HardwareInfo;
use crate::models::registry::ModelRegistryService;
use crate::services::AppServices;
use crate::storage::{Database, ModelRecord};
use crate::ui::hf_browser::{
    DownloadControls, DownloadTracker, HfBrowserContext, build_explore_subpage,
    build_starter_model_group, restore_pending_downloads,
};
use crate::ui::icons;
use crate::ui::widgets::spawn_blocking_ui;

use crate::models::inventory::{InstalledModelInfo, stat_models_parallel};
use active_details::ActiveDetailRows;
use active_group::{
    apply_installed_models_snapshot, build_active_group, build_explore_group,
    make_active_record_getter, wire_active_group_actions,
};
use vision_download::{VisionRowUi, start_mmproj_download};
use vision_setup::{VisionSetupCtx, lookup_and_enable_vision};

#[derive(Clone)]
struct ModelManagerState {
    dialog: adw::PreferencesDialog,
    services: Arc<AppServices>,
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
    services: Arc<AppServices>,
    hw: HardwareInfo,
    active_model_id: Option<String>,
    on_active_changed: Rc<dyn Fn(Option<String>)>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .name("models")
        .title(gettext("AI Models"))
        .icon_name(icons::MODEL)
        .build();
    let state = build_manager_state(dialog, services, hw, active_model_id, on_active_changed);
    let (refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let refresh_installed: Rc<dyn Fn()> = Rc::new(move || {
        let _ = refresh_tx.send(());
    });
    let get_active_record = make_active_record_getter(&state);

    let (active_group, active_model_row, active_rows, vision_controls) =
        setup_active_models_section(&page, &state, &refresh_installed, &get_active_record);
    let downloading_group = build_downloading_group();
    let hf_ctx =
        build_hf_browser_context(&state, downloading_group.clone(), refresh_installed.clone());
    let (starter_group, installed_group, explore_row) =
        add_models_page_groups(&page, &hf_ctx, &downloading_group);

    let closed = wire_dialog_close_flag(&state.dialog, &refresh_installed);
    spawn_refresh_loop(
        refresh_rx,
        RefreshLoopCtx {
            state: state.clone(),
            installed_group,
            active_model_row,
            active_rows: active_rows.clone(),
            starter_group,
            on_refresh: refresh_installed.clone(),
            closed,
        },
    );
    finish_models_page_setup(FinishModelsSetupCtx {
        state: &state,
        hf_ctx: &hf_ctx,
        explore_row: &explore_row,
        active_group: &active_group,
        page: &page,
        vision_controls: &vision_controls,
        vision_action_btn: &active_rows.vision_action_btn,
        refresh_installed: &refresh_installed,
        get_active_record: &get_active_record,
    });
    page
}

fn setup_active_models_section(
    page: &adw::PreferencesPage,
    state: &ModelManagerState,
    refresh_installed: &Rc<dyn Fn()>,
    get_active_record: &Rc<dyn Fn() -> Option<ModelRecord>>,
) -> (
    adw::PreferencesGroup,
    adw::ComboRow,
    ActiveDetailRows,
    DownloadControls,
) {
    let (active_group, active_model_row, active_rows, vision_controls, active_open_folder_btn) =
        build_active_group(&state.db);
    page.add(&active_group);
    wire_active_group_actions(
        state,
        &active_group,
        &active_model_row,
        &active_rows,
        &active_open_folder_btn,
        get_active_record,
        refresh_installed,
    );
    (active_group, active_model_row, active_rows, vision_controls)
}

struct FinishModelsSetupCtx<'a> {
    state: &'a ModelManagerState,
    hf_ctx: &'a HfBrowserContext,
    explore_row: &'a adw::ActionRow,
    active_group: &'a adw::PreferencesGroup,
    page: &'a adw::PreferencesPage,
    vision_controls: &'a DownloadControls,
    vision_action_btn: &'a gtk::Button,
    refresh_installed: &'a Rc<dyn Fn()>,
    get_active_record: &'a Rc<dyn Fn() -> Option<ModelRecord>>,
}

fn finish_models_page_setup(ctx: FinishModelsSetupCtx<'_>) {
    (ctx.refresh_installed)();
    restore_pending_downloads(ctx.hf_ctx);
    wire_explore_and_map(
        ctx.state,
        ctx.hf_ctx,
        ctx.explore_row,
        ctx.active_group,
        ctx.page,
    );
    wire_vision_controls(
        ctx.state,
        ctx.vision_controls,
        ctx.vision_action_btn,
        ctx.refresh_installed,
        ctx.hf_ctx,
        ctx.get_active_record,
    );
}

fn add_models_page_groups(
    page: &adw::PreferencesPage,
    hf_ctx: &HfBrowserContext,
    downloading_group: &adw::PreferencesGroup,
) -> (adw::PreferencesGroup, adw::PreferencesGroup, adw::ActionRow) {
    let starter_group = build_starter_model_group(hf_ctx);
    starter_group.set_visible(false);
    page.add(&starter_group);
    page.add(downloading_group);
    let installed_group = adw::PreferencesGroup::builder()
        .title(gettext("Installed AI Models"))
        .build();
    page.add(&installed_group);
    let explore_row = build_explore_group(page);
    (starter_group, installed_group, explore_row)
}

fn build_manager_state(
    dialog: &adw::PreferencesDialog,
    services: Arc<AppServices>,
    hw: HardwareInfo,
    active_model_id: Option<String>,
    on_active_changed: Rc<dyn Fn(Option<String>)>,
) -> ModelManagerState {
    ModelManagerState {
        dialog: dialog.clone(),
        db: services.db.clone(),
        models_dir: services.paths.models_dir.clone(),
        download_manager: (*services.download_manager).clone(),
        services,
        hw,
        active_id: Rc::new(RefCell::new(active_model_id)),
        cached_models: Rc::new(RefCell::new(Vec::new())),
        installed_badges: Rc::new(RefCell::new(Vec::new())),
        is_syncing_combo: Rc::new(Cell::new(false)),
        on_active_changed,
    }
}

fn build_downloading_group() -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder()
        .title(gettext("Downloading"))
        .description(gettext(
            "Downloads continue in the background if you close Settings, and pause automatically if you quit Companion.",
        ))
        .visible(false)
        .build()
}

fn build_hf_browser_context(
    state: &ModelManagerState,
    downloading_group: adw::PreferencesGroup,
    refresh_installed: Rc<dyn Fn()>,
) -> HfBrowserContext {
    HfBrowserContext {
        dialog: state.dialog.clone(),
        services: state.services.clone(),
        db: state.db.clone(),
        hw: state.hw.clone(),
        download_manager: state.download_manager.clone(),
        tracker: DownloadTracker::new(downloading_group),
        refresh_installed,
    }
}

fn wire_dialog_close_flag(dialog: &adw::PreferencesDialog, wake: &Rc<dyn Fn()>) -> Rc<Cell<bool>> {
    let closed = Rc::new(Cell::new(false));
    let (closed_c, wake_c) = (closed.clone(), wake.clone());
    dialog.connect_closed(move |_| {
        closed_c.set(true);
        wake_c();
    });
    closed
}

fn wire_explore_and_map(
    state: &ModelManagerState,
    hf_ctx: &HfBrowserContext,
    explore_row: &adw::ActionRow,
    active_group: &adw::PreferencesGroup,
    page: &adw::PreferencesPage,
) {
    let (hf_explore, dialog_explore) = (hf_ctx.clone(), state.dialog.clone());
    explore_row.connect_activated(move |_| {
        dialog_explore.push_subpage(&build_explore_subpage(&hf_explore));
    });
    let (db, active_group) = (state.db.clone(), active_group.clone());
    page.connect_map(move |_| {
        let name = db
            .get_assistant_name()
            .unwrap_or_else(|_| "Companion".to_string());
        active_group.set_description(Some(&tr_f(
            "Set the AI model your companion {name} uses. This setting is applied globally.",
            &[("name", &crate::format::markdown::escape_pango_markup(&name))],
        )));
    });
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
    let hf_ctx = hf_ctx.clone();
    let setup_ctx = VisionSetupCtx {
        dialog: state.dialog.clone(),
        db: state.db.clone(),
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
            start_mmproj_download(ui, &hf_ctx, model, repo, None);
        } else {
            lookup_and_enable_vision(ui, &setup_ctx, model);
        }
    });
}

/// Widgets and callbacks driven by the installed-models refresh loop.
struct RefreshLoopCtx {
    state: ModelManagerState,
    installed_group: adw::PreferencesGroup,
    active_model_row: adw::ComboRow,
    active_rows: ActiveDetailRows,
    starter_group: adw::PreferencesGroup,
    on_refresh: Rc<dyn Fn()>,
    closed: Rc<Cell<bool>>,
}

fn spawn_refresh_loop(mut rx: tokio::sync::mpsc::UnboundedReceiver<()>, ctx: RefreshLoopCtx) {
    // Hold only a weak handle to the refresh callback so this loop never keeps its own
    // channel sender alive (which would make `rx.recv()` wait forever after the dialog closes).
    let on_refresh_weak = Rc::downgrade(&ctx.on_refresh);
    let generation = Rc::new(Cell::new(0u64));
    glib::MainContext::default().spawn_local(async move {
        while rx.recv().await.is_some() {
            if ctx.closed.get() {
                break;
            }
            let Some(on_refresh) = on_refresh_weak.upgrade() else {
                break;
            };
            let current_gen = generation.get().wrapping_add(1);
            generation.set(current_gen);
            run_single_refresh_pass(&ctx, on_refresh, &generation, current_gen);
        }
    });
}

fn run_single_refresh_pass(
    ctx: &RefreshLoopCtx,
    on_refresh: Rc<dyn Fn()>,
    generation: &Rc<Cell<u64>>,
    current_gen: u64,
) {
    let (db_bg, models_dir_bg, gguf_cache_bg) = (
        ctx.state.db.clone(),
        ctx.state.models_dir.clone(),
        ctx.state.services.gguf_cache.clone(),
    );
    let (state_ui, installed_group, active_model_row, active_rows, starter_group, gen_ref) = (
        ctx.state.clone(),
        ctx.installed_group.clone(),
        ctx.active_model_row.clone(),
        ctx.active_rows.clone(),
        ctx.starter_group.clone(),
        generation.clone(),
    );
    spawn_blocking_ui(
        &ctx.state.services.rt_handle,
        move || {
            let _ = ModelRegistryService::sync_managed_models_with_cache(
                &db_bg,
                &models_dir_bg,
                &gguf_cache_bg,
            );
            stat_models_parallel(db_bg.list_models().unwrap_or_default())
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
