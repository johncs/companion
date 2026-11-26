//! Models settings page: active model selection, installed GGUF management, and Hugging Face explore.

mod active_details;
mod inventory;
mod vision_download;
mod vision_setup;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::i18n::{gettext, tr_f};
use crate::models::friendly_model_name;
use crate::models::hardware::HardwareInfo;
use crate::storage::{Database, ModelRecord};
use crate::ui::hf_browser::{
    DownloadControls, DownloadTracker, build_explore_subpage, build_starter_model_group,
    restore_pending_downloads,
};
use crate::ui::icons;
use crate::ui::model_config_dialog::present_model_config_dialog;
use crate::ui::widgets::{clear_preferences_group, spawn_blocking_ui};

use active_details::{
    ActiveDetailRows, build_installed_model_row, build_property_row, sync_active_model_details,
};
use inventory::{InstalledModelInfo, stat_models_parallel};
use vision_download::start_mmproj_download;
use vision_setup::lookup_and_enable_vision;

/// Build the "AI Models" [`adw::PreferencesPage`] for the Settings dialog.
pub fn build_models_page(
    dialog: &adw::PreferencesDialog,
    db: Database,
    hw: HardwareInfo,
    models_dir: PathBuf,
    active_model_id: Option<String>,
    on_active_changed: Rc<dyn Fn(Option<String>)>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .name("models")
        .title(gettext("AI Models"))
        .icon_name(icons::MODEL)
        .build();

    let active_id_state = Rc::new(RefCell::new(active_model_id));
    let cached_models: Rc<RefCell<Vec<InstalledModelInfo>>> = Rc::new(RefCell::new(Vec::new()));
    let is_syncing_combo = Rc::new(Cell::new(false));
    let refresh_weak_for_active: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let installed_badges: Rc<RefCell<Vec<(String, gtk::Label)>>> = Rc::new(RefCell::new(Vec::new()));

    let assistant_name = db
        .get_assistant_name()
        .unwrap_or_else(|_| "Companion".to_string());
    let active_group = adw::PreferencesGroup::builder()
        .title(gettext("Active AI Model"))
        .description(tr_f(
            "Set the AI model your companion {name} uses. This setting is applied globally.",
            &[("name", &assistant_name)],
        ))
        .build();

    let active_config_btn = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text(gettext("Model settings (context, GPU layers, vision projector)"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .visible(false)
        .build();
    let active_model_row = adw::ComboRow::builder()
        .title(gettext("Selected Model"))
        .sensitive(false)
        .build();
    let details_expander = adw::ExpanderRow::builder()
        .title(gettext("Model Details"))
        .expanded(false)
        .visible(false)
        .build();
    let active_open_folder_btn = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text(gettext("Open in File Manager"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();

    let active_file_row = build_property_row(&gettext("File"));
    active_file_row.add_suffix(&active_open_folder_btn);
    active_file_row.add_suffix(&active_config_btn);
    let active_size_row = build_property_row(&gettext("Size"));
    let active_quant_row = build_property_row(&gettext("Quantization"));
    let active_ctx_row = build_property_row(&gettext("Max Context"));
    let active_speed_row = build_property_row(&gettext("Average Speed"));
    let active_text_row = build_property_row(&gettext("Text Generation"));
    let active_thinking_row = build_property_row(&gettext("Thinking"));
    let active_vision_row = build_property_row(&gettext("Vision"));
    let vision_controls = DownloadControls::new();
    let vision_action_btn = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    active_vision_row.add_suffix(&vision_controls.container);
    active_vision_row.add_suffix(&vision_action_btn);
    let active_audio_row = build_property_row(&gettext("Audio"));

    for row in [
        &active_file_row,
        &active_size_row,
        &active_quant_row,
        &active_ctx_row,
        &active_speed_row,
        &active_text_row,
        &active_thinking_row,
        &active_vision_row,
        &active_audio_row,
    ] {
        details_expander.add_row(row);
    }

    let active_rows = ActiveDetailRows {
        config_btn: active_config_btn.clone(),
        details_expander: details_expander.clone(),
        file_row: active_file_row,
        size_row: active_size_row,
        quant_row: active_quant_row,
        ctx_row: active_ctx_row,
        speed_row: active_speed_row,
        text_row: active_text_row,
        thinking_row: active_thinking_row,
        vision_row: active_vision_row,
        vision_action_btn: vision_action_btn.clone(),
        audio_row: active_audio_row,
    };

    active_group.add(&active_model_row);
    active_group.add(&details_expander);
    page.add(&active_group);

    let get_active_record: Rc<dyn Fn() -> Option<ModelRecord>> = {
        let cached_models = cached_models.clone();
        let active_id_state = active_id_state.clone();
        Rc::new(move || {
            let active_id = active_id_state.borrow().clone();
            let list = cached_models.borrow();
            active_id
                .as_deref()
                .and_then(|id| list.iter().find(|i| i.record.id == id))
                .or_else(|| list.first())
                .map(|i| i.record.clone())
        })
    };

    {
        let dialog = dialog.clone();
        let db = db.clone();
        let active_group = active_group.clone();
        let get_active_record = get_active_record.clone();
        let refresh_weak = refresh_weak_for_active.clone();
        active_config_btn.connect_clicked(move |_| {
            if let Some(m) = get_active_record() {
                present_model_config_dialog(
                    &active_group,
                    db.clone(),
                    m,
                    dialog.clone(),
                    refresh_weak.clone(),
                );
            }
        });
    }

    {
        let dialog = dialog.clone();
        let get_active_record = get_active_record.clone();
        active_open_folder_btn.connect_clicked(move |_| {
            let Some(path) = get_active_record().and_then(|m| m.file_path) else {
                return;
            };
            let file = gio::File::for_path(&path);
            let launcher = gtk::FileLauncher::new(Some(&file));
            let window = dialog.root().and_downcast::<gtk::Window>();
            let dialog_toast = dialog.clone();
            launcher.open_containing_folder(
                window.as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    if let Err(err) = res {
                        dialog_toast.add_toast(adw::Toast::new(&tr_f(
                            "Failed to open folder: {err}",
                            &[("err", &err.to_string())],
                        )));
                    }
                },
            );
        });
    }

    {
        let active_id_state = active_id_state.clone();
        let cached_models = cached_models.clone();
        let installed_badges = installed_badges.clone();
        let is_syncing_combo = is_syncing_combo.clone();
        let on_active_changed = on_active_changed.clone();
        let active_rows = active_rows.clone();
        active_model_row.connect_selected_notify(move |row| {
            if is_syncing_combo.get() {
                return;
            }
            let selected_info = cached_models
                .borrow()
                .get(row.selected() as usize)
                .cloned();
            if let Some(info) = selected_info {
                sync_active_model_details(Some(&info), &active_rows);
                let new_id = Some(info.record.id);
                for (id, badge) in installed_badges.borrow().iter() {
                    badge.set_visible(Some(id.as_str()) == new_id.as_deref());
                }
                if *active_id_state.borrow() != new_id {
                    *active_id_state.borrow_mut() = new_id.clone();
                    on_active_changed(new_id);
                }
            }
        });
    }

    let downloading_group = adw::PreferencesGroup::builder()
        .title(gettext("Downloading"))
        .description(gettext(
            "Downloads continue in the background if you close Settings, and pause automatically if you quit Companion.",
        ))
        .visible(false)
        .build();
    let tracker = DownloadTracker::new(downloading_group.clone());

    let starter_refresh_cb: Rc<dyn Fn()> = {
        let rw = refresh_weak_for_active.clone();
        Rc::new(move || {
            if let Some(f) = rw.borrow().as_ref() {
                f();
            }
        })
    };
    let starter_group = build_starter_model_group(
        &db,
        dialog,
        &starter_refresh_cb,
        &tracker,
        models_dir.clone(),
    );
    starter_group.set_visible(false);
    page.add(&starter_group);
    page.add(&downloading_group);

    let installed_group = adw::PreferencesGroup::builder()
        .title(gettext("Installed AI Models"))
        .build();
    page.add(&installed_group);

    let explore_group = adw::PreferencesGroup::builder()
        .title(gettext("Explore"))
        .build();
    let explore_row = adw::ActionRow::builder()
        .title(gettext("Explore &amp; Download AI Models"))
        .activatable(true)
        .build();
    explore_row.add_prefix(&gtk::Image::from_icon_name("system-search-symbolic"));
    explore_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    explore_group.add(&explore_row);
    page.add(&explore_group);

    let refresh_installed = build_refresh_installed_callback(
        dialog,
        &db,
        &installed_group,
        &active_model_row,
        &active_rows,
        &cached_models,
        &installed_badges,
        &is_syncing_combo,
        &active_id_state,
        &on_active_changed,
        &starter_group,
        models_dir.clone(),
    );
    *refresh_weak_for_active.borrow_mut() = Some(refresh_installed.clone());
    refresh_installed();
    restore_pending_downloads(&tracker, &db, dialog, &refresh_installed, models_dir.clone());

    let tracker_for_explore = tracker.clone();
    {
        let dialog = dialog.clone();
        let db = db.clone();
        let hw = hw.clone();
        let refresh_installed = refresh_installed.clone();
        let models_dir = models_dir.clone();
        explore_row.connect_activated(move |_| {
            let subpage = build_explore_subpage(
                &dialog,
                db.clone(),
                hw.clone(),
                refresh_installed.clone(),
                tracker_for_explore.clone(),
                models_dir.clone(),
            );
            dialog.push_subpage(&subpage);
        });
    }

    wire_vision_controls(
        &vision_controls,
        &vision_action_btn,
        dialog,
        &db,
        &hw,
        &refresh_installed,
        &tracker,
        &get_active_record,
        models_dir,
    );

    {
        let db = db.clone();
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

#[allow(clippy::too_many_arguments)]
fn wire_vision_controls(
    vision_controls: &DownloadControls,
    vision_action_btn: &gtk::Button,
    dialog: &adw::PreferencesDialog,
    db: &Database,
    hw: &HardwareInfo,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
    get_active_record: &Rc<dyn Fn() -> Option<ModelRecord>>,
    models_dir: PathBuf,
) {
    let on_vision_pause: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_vision_cancel: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    {
        let on_vision_pause = on_vision_pause.clone();
        vision_controls.pause_btn.connect_clicked(move |_| {
            if let Some(cb) = on_vision_pause.borrow().clone() {
                cb();
            }
        });
    }
    {
        let on_vision_cancel = on_vision_cancel.clone();
        vision_controls.cancel_btn.connect_clicked(move |_| {
            if let Some(cb) = on_vision_cancel.borrow().clone() {
                cb();
            }
        });
    }

    let dialog = dialog.clone();
    let db = db.clone();
    let hw = hw.clone();
    let refresh_installed = refresh_installed.clone();
    let tracker = tracker.clone();
    let vision_controls = vision_controls.clone();
    let get_active_record = get_active_record.clone();
    vision_action_btn.connect_clicked(move |btn| {
        let Some(model) = get_active_record() else {
            return;
        };
        if let Some(repo) = model.hf_repo.clone().filter(|r| !r.trim().is_empty()) {
            start_mmproj_download(
                btn,
                &vision_controls,
                &on_vision_pause,
                &on_vision_cancel,
                &dialog,
                &db,
                model,
                repo,
                None,
                &refresh_installed,
                models_dir.clone(),
            );
        } else {
            lookup_and_enable_vision(
                btn,
                &vision_controls,
                &on_vision_pause,
                &on_vision_cancel,
                &dialog,
                &db,
                &hw,
                model,
                &refresh_installed,
                &tracker,
                models_dir.clone(),
            );
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn build_refresh_installed_callback(
    dialog: &adw::PreferencesDialog,
    db: &Database,
    installed_group: &adw::PreferencesGroup,
    active_model_row: &adw::ComboRow,
    active_rows: &ActiveDetailRows,
    cached_models: &Rc<RefCell<Vec<InstalledModelInfo>>>,
    installed_badges: &Rc<RefCell<Vec<(String, gtk::Label)>>>,
    is_syncing_combo: &Rc<Cell<bool>>,
    active_id_state: &Rc<RefCell<Option<String>>>,
    on_active_changed: &Rc<dyn Fn(Option<String>)>,
    starter_group: &adw::PreferencesGroup,
    models_dir: PathBuf,
) -> Rc<dyn Fn()> {
    let db = db.clone();
    let installed_group = installed_group.clone();
    let active_model_row = active_model_row.clone();
    let active_rows = active_rows.clone();
    let cached_models = cached_models.clone();
    let installed_badges = installed_badges.clone();
    let is_syncing_combo = is_syncing_combo.clone();
    let active_id_state = active_id_state.clone();
    let on_active_changed = on_active_changed.clone();
    let starter_group = starter_group.clone();
    let dialog = dialog.clone();
    let refresh_weak: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let refresh_weak_inner = refresh_weak.clone();
    let generation = Rc::new(Cell::new(0u64));

    let func: Rc<dyn Fn()> = Rc::new(move || {
        let current_gen = generation.get().wrapping_add(1);
        generation.set(current_gen);

        let db_bg = db.clone();
        let installed_group = installed_group.clone();
        let active_model_row = active_model_row.clone();
        let active_rows = active_rows.clone();
        let cached_models = cached_models.clone();
        let installed_badges = installed_badges.clone();
        let is_syncing_combo = is_syncing_combo.clone();
        let db = db.clone();
        let active_id_state = active_id_state.clone();
        let on_active_changed = on_active_changed.clone();
        let starter_group = starter_group.clone();
        let dialog = dialog.clone();
        let refresh_weak_inner = refresh_weak_inner.clone();
        let generation = generation.clone();
        let models_dir_bg = models_dir.clone();
        let models_dir_ui = models_dir.clone();

        spawn_blocking_ui(
            move || {
                let _ = crate::models::registry::ModelRegistryService::sync_managed_models(
                    &db_bg,
                    &models_dir_bg,
                );
                let models = db_bg.list_models().unwrap_or_default();
                stat_models_parallel(models)
            },
            move |items| {
                if generation.get() != current_gen {
                    return;
                }

                let effective_active_id: Option<String>;
                is_syncing_combo.set(true);
                if items.is_empty() {
                    effective_active_id = None;
                    if active_id_state.borrow().is_some() {
                        *active_id_state.borrow_mut() = None;
                        on_active_changed(None);
                    }
                    cached_models.borrow_mut().clear();
                    let empty_label = gettext("No local models installed");
                    active_model_row.set_model(Some(&gtk::StringList::new(&[empty_label.as_str()])));
                    active_model_row.set_selected(0);
                    active_model_row.set_sensitive(false);
                    sync_active_model_details(None, &active_rows);
                } else {
                    let names: Vec<String> = items
                        .iter()
                        .map(|info| friendly_model_name(&info.record.display_name))
                        .collect();
                    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    let selected_idx = active_id_state
                        .borrow()
                        .as_deref()
                        .and_then(|id| items.iter().position(|x| x.record.id == id))
                        .unwrap_or(0);
                    effective_active_id = items.get(selected_idx).map(|x| x.record.id.clone());
                    if *active_id_state.borrow() != effective_active_id {
                        *active_id_state.borrow_mut() = effective_active_id.clone();
                        on_active_changed(effective_active_id.clone());
                    }
                    *cached_models.borrow_mut() = items.clone();
                    active_model_row.set_model(Some(&gtk::StringList::new(&name_refs)));
                    active_model_row.set_selected(selected_idx as u32);
                    active_model_row.set_sensitive(true);
                    sync_active_model_details(items.get(selected_idx), &active_rows);
                }
                is_syncing_combo.set(false);

                clear_preferences_group(&installed_group);
                installed_badges.borrow_mut().clear();
                starter_group.set_visible(items.is_empty());
                if items.is_empty() {
                    let empty = adw::ActionRow::builder()
                        .title(gettext("No local models yet"))
                        .subtitle(gettext(
                            "Download the Recommended Starter AI Model above or explore Hugging Face below.",
                        ))
                        .sensitive(false)
                        .build();
                    installed_group.add(&empty);
                    return;
                }

                for info in items {
                    let is_active = effective_active_id.as_deref() == Some(info.record.id.as_str());
                    let (row, badge) = build_installed_model_row(
                        &info.record,
                        info.file_size,
                        is_active,
                        &db,
                        &installed_group,
                        &active_id_state,
                        &on_active_changed,
                        &dialog,
                        &refresh_weak_inner,
                        models_dir_ui.clone(),
                    );
                    installed_badges
                        .borrow_mut()
                        .push((info.record.id, badge));
                    installed_group.add(&row);
                }
            },
        );
    });

    *refresh_weak.borrow_mut() = Some(func.clone());
    func
}
