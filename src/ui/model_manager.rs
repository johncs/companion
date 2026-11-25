//! Models settings page: system hardware summary and installed GGUF management.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::core::runtime;

use crate::i18n::{gettext, tr_f};
use crate::models::gguf::{GgufMetadata, format_context_label, inspect_gguf_metadata};
use crate::models::hardware::HardwareInfo;
use crate::models::huggingface::{
    HfGgufFile, MmprojMatch, download_gguf_file, find_matching_mmproj, list_repo_gguf_files,
    remove_part_file, select_best_mmproj,
};
use crate::models::{format_bytes, friendly_model_name, managed_models_dir};
use crate::storage::{Database, ModelRecord};
use crate::ui::hf_browser::{
    DownloadControls, DownloadTracker, build_explore_subpage, build_explore_subpage_with_query,
    build_starter_model_group, restore_pending_downloads,
};
use crate::ui::icons;
use crate::ui::model_config_dialog::present_model_config_dialog;
use crate::ui::widgets::{
    clear_preferences_group, present_destructive_confirm, send_desktop_notification,
};

#[derive(Clone)]
struct InstalledModelInfo {
    record: ModelRecord,
    file_size: Option<u64>,
    gguf: GgufMetadata,
    mmproj_exists: bool,
    mmproj_gguf: Option<GgufMetadata>,
}

#[derive(Clone)]
struct ActiveDetailRows {
    config_btn: gtk::Button,
    details_expander: adw::ExpanderRow,
    file_row: adw::ActionRow,
    size_row: adw::ActionRow,
    quant_row: adw::ActionRow,
    ctx_row: adw::ActionRow,
    speed_row: adw::ActionRow,
    text_row: adw::ActionRow,
    thinking_row: adw::ActionRow,
    vision_row: adw::ActionRow,
    vision_action_btn: gtk::Button,
    audio_row: adw::ActionRow,
}

/// Build the "AI Models" [`adw::PreferencesPage`] for the Settings dialog.
pub fn build_models_page(
    dialog: &adw::PreferencesDialog,
    db: Database,
    hw: HardwareInfo,
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

    details_expander.add_row(&active_file_row);
    details_expander.add_row(&active_size_row);
    details_expander.add_row(&active_quant_row);
    details_expander.add_row(&active_ctx_row);
    details_expander.add_row(&active_speed_row);
    details_expander.add_row(&active_text_row);
    details_expander.add_row(&active_thinking_row);
    details_expander.add_row(&active_vision_row);
    details_expander.add_row(&active_audio_row);

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
        let installed_badges = installed_badges.clone();
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
    let starter_group = build_starter_model_group(&db, dialog, &starter_refresh_cb, &tracker);
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
    );
    *refresh_weak_for_active.borrow_mut() = Some(refresh_installed.clone());
    refresh_installed();
    restore_pending_downloads(&tracker, &db, dialog, &refresh_installed);

    let tracker_for_explore = tracker.clone();
    {
        let dialog = dialog.clone();
        let db = db.clone();
        let hw = hw.clone();
        let refresh_installed = refresh_installed.clone();
        explore_row.connect_activated(move |_| {
            let subpage = build_explore_subpage(
                &dialog,
                db.clone(),
                hw.clone(),
                refresh_installed.clone(),
                tracker_for_explore.clone(),
            );
            dialog.push_subpage(&subpage);
        });
    }

    {
        let on_vision_pause: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let on_vision_cancel: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

        {
            let on_vision_pause = on_vision_pause.clone();
            vision_controls.pause_btn.connect_clicked(move |_| {
                let cb = on_vision_pause.borrow().clone();
                if let Some(cb) = cb {
                    cb();
                }
            });
        }
        {
            let on_vision_cancel = on_vision_cancel.clone();
            vision_controls.cancel_btn.connect_clicked(move |_| {
                let cb = on_vision_cancel.borrow().clone();
                if let Some(cb) = cb {
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
                );
            }
        });
    }

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

fn build_property_row(title: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_css_class("property");
    row
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

        let (tx, rx) = tokio::sync::oneshot::channel::<Vec<InstalledModelInfo>>();
        let db_bg = db.clone();
        runtime().spawn_blocking(move || {
            let _ = db_bg.sync_managed_models(&managed_models_dir());
            let models = db_bg.list_models().unwrap_or_default();
            let items = stat_models_parallel(models);
            let _ = tx.send(items);
        });

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

        glib::MainContext::default().spawn_local(async move {
            let Ok(items) = rx.await else {
                return;
            };
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
                );
                installed_badges
                    .borrow_mut()
                    .push((info.record.id, badge));
                installed_group.add(&row);
            }
        });
    });

    *refresh_weak.borrow_mut() = Some(func.clone());
    func
}

fn sync_active_model_details(info: Option<&InstalledModelInfo>, rows: &ActiveDetailRows) {
    let Some(info) = info else {
        rows.config_btn.set_visible(false);
        rows.details_expander.set_visible(false);
        return;
    };

    let file_name = info
        .record
        .file_path
        .as_deref()
        .map(|p| {
            Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p)
        })
        .unwrap_or("—");
    let file_sub = if info.file_size.is_some() {
        file_name.to_string()
    } else {
        format!("{file_name} ({})", gettext("Missing file"))
    };
    rows.file_row
        .set_subtitle(&gtk::glib::markup_escape_text(&file_sub));
    rows.file_row
        .set_tooltip_text(info.record.file_path.as_deref());

    let size_sub = info
        .file_size
        .map(format_bytes)
        .unwrap_or_else(|| gettext("Missing file"));
    rows.size_row
        .set_subtitle(&gtk::glib::markup_escape_text(&size_sub));

    let quant_sub = info.record.quantization.as_deref().unwrap_or("—");
    rows.quant_row
        .set_subtitle(&gtk::glib::markup_escape_text(quant_sub));

    let ctx_sub = info
        .gguf
        .max_context
        .map(format_context_label)
        .unwrap_or_else(|| "—".to_string());
    rows.ctx_row
        .set_subtitle(&gtk::glib::markup_escape_text(&ctx_sub));

    let speed_sub = if let Some(tps) = info.record.avg_tokens_per_sec {
        tr_f("{tps} tok/s", &[("tps", &format!("{tps:.1}"))])
    } else {
        gettext("Unavailable")
    };
    rows.speed_row
        .set_subtitle(&gtk::glib::markup_escape_text(&speed_sub));

    rows.text_row
        .set_subtitle(&gtk::glib::markup_escape_text(&gettext("Active")));

    let thinking_supported = info.record.supports_thinking || info.gguf.supports_thinking;
    let thinking_sub = if thinking_supported {
        gettext("Active")
    } else {
        gettext("Unavailable")
    };
    rows.thinking_row
        .set_subtitle(&gtk::glib::markup_escape_text(&thinking_sub));

    let mut vision_action_label: Option<String> = None;
    let vision_sub = if info.record.mmproj_path.is_some() {
        if info.mmproj_exists {
            if info.mmproj_gguf.as_ref().is_some_and(|m| !m.is_clip) {
                vision_action_label = Some(if info.record.hf_repo.is_some() {
                    gettext("Install Vision")
                } else {
                    gettext("Enable Vision")
                });
                gettext("Invalid projector file")
            } else {
                gettext("Active")
            }
        } else {
            vision_action_label = Some(if info.record.hf_repo.is_some() {
                gettext("Install Vision")
            } else {
                gettext("Enable Vision")
            });
            gettext("Missing file")
        }
    } else if info.gguf.supports_vision {
        vision_action_label = Some(if info.record.hf_repo.is_some() {
            gettext("Install Vision")
        } else {
            gettext("Enable Vision")
        });
        if info.record.hf_repo.is_some() {
            gettext("Available (Requires mmproj)")
        } else {
            gettext("Available (Requires external mmproj file)")
        }
    } else {
        gettext("Unavailable")
    };
    rows.vision_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vision_sub));
    if let Some(label) = vision_action_label {
        rows.vision_action_btn.set_label(&label);
        rows.vision_action_btn.set_visible(true);
    } else {
        rows.vision_action_btn.set_visible(false);
    }

    let audio_supported = info.gguf.supports_audio
        || info.mmproj_gguf.as_ref().is_some_and(|m| m.supports_audio);
    let audio_sub = if audio_supported {
        gettext("Available (Coming soon)")
    } else {
        gettext("Unavailable")
    };
    rows.audio_row
        .set_subtitle(&gtk::glib::markup_escape_text(&audio_sub));

    rows.config_btn.set_visible(true);
    rows.details_expander.set_visible(true);
}

fn stat_models_parallel(models: Vec<ModelRecord>) -> Vec<InstalledModelInfo> {
    crate::core::parallel_chunks(&models, 4, |chunk| {
        chunk
            .iter()
            .map(|m| {
                let file_size = m
                    .file_path
                    .as_deref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|meta| meta.len());
                let gguf = m
                    .file_path
                    .as_deref()
                    .map(|p| inspect_gguf_metadata(Path::new(p)))
                    .unwrap_or_default();
                let mmproj_exists = m
                    .mmproj_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p).exists());
                let mmproj_gguf = m
                    .mmproj_path
                    .as_deref()
                    .filter(|_| mmproj_exists)
                    .map(|p| inspect_gguf_metadata(Path::new(p)));
                InstalledModelInfo {
                    record: m.clone(),
                    file_size,
                    gguf,
                    mmproj_exists,
                    mmproj_gguf,
                }
            })
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_installed_model_row(
    model: &ModelRecord,
    file_size: Option<u64>,
    is_active: bool,
    db: &Database,
    installed_group: &adw::PreferencesGroup,
    active_id_state: &Rc<RefCell<Option<String>>>,
    on_active_changed: &Rc<dyn Fn(Option<String>)>,
    dialog: &adw::PreferencesDialog,
    refresh_weak: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
) -> (adw::ActionRow, gtk::Label) {
    let size_str = file_size
        .map(format_bytes)
        .unwrap_or_else(|| gettext("Missing file"));

    let row = adw::ActionRow::builder()
        .title(&model.display_name)
        .subtitle(&size_str)
        .build();

    let active_badge = gtk::Label::builder()
        .label(&gettext("Active"))
        .valign(gtk::Align::Center)
        .margin_end(8)
        .css_classes(["dim-label", "caption-heading"])
        .visible(is_active)
        .build();
    row.add_suffix(&active_badge);

    let delete_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Remove model"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    {
        let db = db.clone();
        let installed_group = installed_group.clone();
        let active_id_state = active_id_state.clone();
        let on_active_changed = on_active_changed.clone();
        let dialog = dialog.clone();
        let refresh_weak = refresh_weak.clone();
        let model_clone = model.clone();

        delete_btn.connect_clicked(move |_| {
            let managed_dir = managed_models_dir();
            let is_managed_file = model_clone
                .file_path
                .as_deref()
                .is_some_and(|p| Path::new(p).starts_with(&managed_dir));

            let body = if is_managed_file {
                tr_f(
                    "Delete \"{name}\" and remove its downloaded files from disk?",
                    &[("name", &model_clone.display_name)],
                )
            } else {
                tr_f(
                    "Remove \"{name}\" from Companion? (Your imported .gguf file on disk will not be deleted.)",
                    &[("name", &model_clone.display_name)],
                )
            };

            let db = db.clone();
            let active_id_state = active_id_state.clone();
            let on_active_changed = on_active_changed.clone();
            let dialog = dialog.clone();
            let refresh_weak = refresh_weak.clone();
            let model_inner = model_clone.clone();

            present_destructive_confirm(
                &installed_group,
                &gettext("Remove AI Model?"),
                &body,
                &gettext("Remove"),
                move || {
                    let others: Vec<ModelRecord> = db
                        .list_models()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|m| m.id != model_inner.id)
                        .collect();
                    delete_managed_model_files(&managed_dir, &model_inner, &others);
                    let _ = db.delete_model(&model_inner.id);
                    if active_id_state.borrow().as_deref() == Some(&model_inner.id) {
                        *active_id_state.borrow_mut() = None;
                        on_active_changed(None);
                    }
                    dialog.add_toast(adw::Toast::new(&tr_f(
                        "Removed \"{name}\"",
                        &[("name", &model_inner.display_name)],
                    )));
                    if let Some(f) = refresh_weak.borrow().as_ref() {
                        f();
                    }
                },
            );
        });
    }

    row.add_suffix(&delete_btn);
    (row, active_badge)
}

fn remove_managed_entry(managed_dir: &Path, path: &Path, other_models: &[ModelRecord]) {
    if !path.starts_with(managed_dir) {
        return;
    }
    let path_in_use = |target: &Path| {
        other_models.iter().any(|m| {
            m.file_path.as_deref().is_some_and(|p| Path::new(p) == target)
                || m.mmproj_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p) == target)
        })
    };
    if let Some(parent) = path.parent()
        && parent != managed_dir
        && parent.starts_with(managed_dir)
    {
        let dir_in_use = other_models.iter().any(|m| {
            m.file_path
                .as_deref()
                .is_some_and(|p| Path::new(p).starts_with(parent))
                || m.mmproj_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p).starts_with(parent))
        });
        if !dir_in_use {
            let _ = std::fs::remove_dir_all(parent);
            return;
        }
    }
    if !path_in_use(path) {
        let _ = std::fs::remove_file(path);
    }
}

fn delete_managed_model_files(
    managed_dir: &Path,
    model: &ModelRecord,
    other_models: &[ModelRecord],
) {
    if let Some(path) = &model.file_path {
        remove_managed_entry(managed_dir, Path::new(path), other_models);
    }
    if let Some(mmproj) = &model.mmproj_path {
        remove_managed_entry(managed_dir, Path::new(mmproj), other_models);
    }
}

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

struct MmprojDownloadState {
    is_paused: bool,
    is_cancelled: bool,
    stop_flag: Arc<AtomicBool>,
    dest_path: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    resolved_file: Arc<std::sync::Mutex<Option<HfGgufFile>>>,
}

#[allow(clippy::too_many_arguments)]
fn start_mmproj_download(
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

#[allow(clippy::too_many_arguments)]
fn lookup_and_enable_vision(
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
                );
            }
            Ok(Ok(None)) => {
                present_manual_vision_guide(&dialog_ref, &db, &hw, model, &refresh, &tracker, None);
            }
            Ok(Err(err)) => {
                present_manual_vision_guide(
                    &dialog_ref,
                    &db,
                    &hw,
                    model,
                    &refresh,
                    &tracker,
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
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    runtime().spawn_blocking(move || {
                        if !inspect_gguf_metadata(&path).is_clip {
                            let _ = tx.send(Err(anyhow::anyhow!(gettext(
                                "Selected file is not a valid CLIP vision projector (mmproj)"
                            ))));
                            return;
                        }
                        updated.mmproj_path = Some(path.to_string_lossy().to_string());
                        let _ = tx.send(db.upsert_model(&updated));
                    });
                    glib::MainContext::default().spawn_local(async move {
                        match rx.await {
                            Ok(Ok(())) => {
                                refresh();
                                dialog_toast.add_toast(adw::Toast::new(&gettext(
                                    "Enabled Vision projector",
                                )));
                            }
                            Ok(Err(err)) => {
                                dialog_toast.add_toast(adw::Toast::new(&tr_f(
                                    "Failed to save: {err}",
                                    &[("err", &err.to_string())],
                                )));
                            }
                            Err(_) => {}
                        }
                    });
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
                Some(&query),
            );
            dialog_ref.push_subpage(&subpage);
        }
        _ => {}
    });

    alert.present(Some(dialog));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_model(id: &str, path: Option<String>) -> ModelRecord {
        ModelRecord {
            id: id.to_string(),
            display_name: id.to_string(),
            provider_type: "llama_cpp".to_string(),
            file_path: path,
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: false,
            default_params_json: "{}".to_string(),
            avg_tokens_per_sec: None,
        }
    }

    #[test]
    fn stat_models_parallel_reports_existing_and_missing_files() {
        let dir = std::env::temp_dir().join(format!("companion_mm_stat_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut models = Vec::new();
        for i in 0..5 {
            let p = dir.join(format!("m{i}.gguf"));
            std::fs::write(&p, vec![7u8; (i + 1) * 10]).unwrap();
            models.push(dummy_model(&format!("m{i}"), Some(p.to_string_lossy().to_string())));
        }
        models.push(dummy_model(
            "missing",
            Some(dir.join("gone.gguf").to_string_lossy().to_string()),
        ));

        let results = stat_models_parallel(models);
        assert_eq!(results.len(), 6);
        assert_eq!(results[0].file_size, Some(10));
        assert_eq!(results[4].file_size, Some(50));
        assert_eq!(results[5].file_size, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_managed_model_removes_repo_directory_and_vision_files() {
        let dir = std::env::temp_dir().join(format!("companion_mm_del_{}", uuid::Uuid::new_v4()));
        let repo_dir = dir.join("google--gemma-4-E4B");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let model_path = repo_dir.join("model.gguf");
        let mmproj_path = repo_dir.join("mmproj-BF16.gguf");
        std::fs::write(&model_path, b"model").unwrap();
        std::fs::write(&mmproj_path, b"vision").unwrap();

        let mut model = dummy_model("m1", Some(model_path.to_string_lossy().to_string()));
        model.mmproj_path = Some(mmproj_path.to_string_lossy().to_string());

        delete_managed_model_files(&dir, &model, &[]);
        assert!(!repo_dir.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
