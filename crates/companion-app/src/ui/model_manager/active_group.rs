use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::{gettext, tr_f};
use crate::models::friendly_model_name;
use crate::storage::{Database, ModelRecord};
use crate::ui::hf_browser::DownloadControls;
use crate::ui::model_config_dialog::present_model_config_dialog;
use crate::ui::widgets::clear_preferences_group;

use super::ModelManagerState;
use super::active_details::{
    ActiveDetailRows, InstalledRowCtx, build_installed_model_row, build_property_row,
    sync_active_model_details,
};
use crate::models::inventory::InstalledModelInfo;

pub(super) fn build_active_group(
    db: &Database,
) -> (
    adw::PreferencesGroup,
    adw::ComboRow,
    ActiveDetailRows,
    DownloadControls,
    gtk::Button,
) {
    let active_group = build_active_group_header(db);
    let active_config_btn = build_active_config_btn();
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

    let (active_rows, vision_controls) = build_active_detail_rows(
        &active_config_btn,
        &active_open_folder_btn,
        details_expander,
    );
    active_group.add(&active_model_row);
    active_group.add(&active_rows.details_expander);

    (
        active_group,
        active_model_row,
        active_rows,
        vision_controls,
        active_open_folder_btn,
    )
}

fn build_active_group_header(db: &Database) -> adw::PreferencesGroup {
    let assistant_name = db
        .get_assistant_name()
        .unwrap_or_else(|_| "Companion".to_string());
    adw::PreferencesGroup::builder()
        .title(gettext("Active AI Model"))
        .description(tr_f(
            "Set the AI model your companion {name} uses. This setting is applied globally.",
            &[(
                "name",
                &crate::format::markdown::escape_pango_markup(&assistant_name),
            )],
        ))
        .build()
}

fn build_active_config_btn() -> gtk::Button {
    gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text(gettext(
            "Model settings (context, GPU layers, vision projector)",
        ))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .visible(false)
        .build()
}

fn build_active_detail_rows(
    config_btn: &gtk::Button,
    open_folder_btn: &gtk::Button,
    details_expander: adw::ExpanderRow,
) -> (ActiveDetailRows, DownloadControls) {
    let file_row = build_property_row(&gettext("File"));
    file_row.add_suffix(open_folder_btn);
    file_row.add_suffix(config_btn);
    let size_row = build_property_row(&gettext("Size"));
    let quant_row = build_property_row(&gettext("Quantization"));
    let ctx_row = build_property_row(&gettext("Max Context"));
    let speed_row = build_property_row(&gettext("Average Speed"));
    let text_row = build_property_row(&gettext("Text Generation"));
    let thinking_row = build_property_row(&gettext("Thinking"));
    let (vision_row, vision_controls, vision_action_btn) = build_vision_detail_row();
    let audio_row = build_property_row(&gettext("Audio"));

    for row in [
        &file_row,
        &size_row,
        &quant_row,
        &ctx_row,
        &speed_row,
        &text_row,
        &thinking_row,
        &vision_row,
        &audio_row,
    ] {
        details_expander.add_row(row);
    }
    let rows = ActiveDetailRows {
        config_btn: config_btn.clone(),
        details_expander,
        file_row,
        size_row,
        quant_row,
        ctx_row,
        speed_row,
        text_row,
        thinking_row,
        vision_row,
        vision_action_btn,
        audio_row,
    };
    (rows, vision_controls)
}

fn build_vision_detail_row() -> (adw::ActionRow, DownloadControls, gtk::Button) {
    let vision_row = build_property_row(&gettext("Vision"));
    let vision_controls = DownloadControls::new();
    let vision_action_btn = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    vision_row.add_suffix(&vision_controls.container);
    vision_row.add_suffix(&vision_action_btn);
    (vision_row, vision_controls, vision_action_btn)
}

pub(super) fn build_explore_group(page: &adw::PreferencesPage) -> adw::ActionRow {
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
    explore_row
}

pub(super) fn make_active_record_getter(
    state: &ModelManagerState,
) -> Rc<dyn Fn() -> Option<ModelRecord>> {
    let cached_models = state.cached_models.clone();
    let active_id = state.active_id.clone();
    Rc::new(move || {
        let aid = active_id.borrow().clone();
        let list = cached_models.borrow();
        aid.as_deref()
            .and_then(|id| list.iter().find(|i| i.record.id == id))
            .or_else(|| list.first())
            .map(|i| i.record.clone())
    })
}

pub(super) fn wire_active_group_actions(
    state: &ModelManagerState,
    active_group: &adw::PreferencesGroup,
    active_model_row: &adw::ComboRow,
    active_rows: &ActiveDetailRows,
    open_folder_btn: &gtk::Button,
    get_active_record: &Rc<dyn Fn() -> Option<ModelRecord>>,
    on_refresh: &Rc<dyn Fn()>,
) {
    wire_config_and_folder_buttons(
        state,
        active_group,
        &active_rows.config_btn,
        open_folder_btn,
        get_active_record,
        on_refresh,
    );
    let state = state.clone();
    let active_rows = active_rows.clone();
    active_model_row.connect_selected_notify(move |row| {
        if state.is_syncing_combo.get() {
            return;
        }
        let selected_info = state
            .cached_models
            .borrow()
            .get(row.selected() as usize)
            .cloned();
        if let Some(info) = selected_info {
            sync_active_model_details(Some(&info), &active_rows);
            let new_id = Some(info.record.id);
            for (id, badge) in state.installed_badges.borrow().iter() {
                badge.set_visible(Some(id.as_str()) == new_id.as_deref());
            }
            if *state.active_id.borrow() != new_id {
                *state.active_id.borrow_mut() = new_id.clone();
                (state.on_active_changed)(new_id);
            }
        }
    });
}

fn wire_config_and_folder_buttons(
    state: &ModelManagerState,
    active_group: &adw::PreferencesGroup,
    config_btn: &gtk::Button,
    open_folder_btn: &gtk::Button,
    get_active_record: &Rc<dyn Fn() -> Option<ModelRecord>>,
    on_refresh: &Rc<dyn Fn()>,
) {
    let (dialog, db, rt_handle, gguf_cache, active_group, get_active, on_refresh) = (
        state.dialog.clone(),
        state.db.clone(),
        state.services.rt_handle.clone(),
        state.services.gguf_cache.clone(),
        active_group.clone(),
        get_active_record.clone(),
        on_refresh.clone(),
    );
    config_btn.connect_clicked(move |_| {
        if let Some(m) = get_active() {
            present_model_config_dialog(
                &active_group,
                db.clone(),
                rt_handle.clone(),
                gguf_cache.clone(),
                m,
                dialog.clone(),
                on_refresh.clone(),
            );
        }
    });

    let (dialog, get_active) = (state.dialog.clone(), get_active_record.clone());
    open_folder_btn.connect_clicked(move |_| {
        let Some(path) = get_active().and_then(|m| m.file_path) else {
            return;
        };
        let dialog_toast = dialog.clone();
        crate::ui::widgets::open_path_with_launcher(
            &dialog,
            std::path::Path::new(&path),
            true,
            move |err| {
                dialog_toast.add_toast(adw::Toast::new(&tr_f(
                    "Failed to open folder: {err}",
                    &[("err", &err)],
                )));
            },
        );
    });
}

pub(super) fn apply_installed_models_snapshot(
    state: &ModelManagerState,
    installed_group: &adw::PreferencesGroup,
    active_model_row: &adw::ComboRow,
    active_rows: &ActiveDetailRows,
    starter_group: &adw::PreferencesGroup,
    on_refresh: &Rc<dyn Fn()>,
    items: Vec<InstalledModelInfo>,
) {
    let effective_active_id =
        sync_active_combo_snapshot(state, active_model_row, active_rows, &items);

    clear_preferences_group(installed_group);
    state.installed_badges.borrow_mut().clear();
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

    let row_ctx = InstalledRowCtx {
        db: state.db.clone(),
        installed_group: installed_group.clone(),
        active_id_state: state.active_id.clone(),
        on_active_changed: state.on_active_changed.clone(),
        dialog: state.dialog.clone(),
        on_refresh: on_refresh.clone(),
        models_dir: state.models_dir.clone(),
        rt_handle: state.services.rt_handle.clone(),
    };
    for info in items {
        let is_active = effective_active_id.as_deref() == Some(info.record.id.as_str());
        let (row, badge) =
            build_installed_model_row(&info.record, info.file_size, is_active, &row_ctx);
        state
            .installed_badges
            .borrow_mut()
            .push((info.record.id, badge));
        installed_group.add(&row);
    }
}

fn sync_active_combo_snapshot(
    state: &ModelManagerState,
    active_model_row: &adw::ComboRow,
    active_rows: &ActiveDetailRows,
    items: &[InstalledModelInfo],
) -> Option<String> {
    state.is_syncing_combo.set(true);
    let effective_active_id = if items.is_empty() {
        if state.active_id.borrow().is_some() {
            *state.active_id.borrow_mut() = None;
            (state.on_active_changed)(None);
        }
        state.cached_models.borrow_mut().clear();
        active_model_row.set_model(Some(&gtk::StringList::new(&[gettext(
            "No local models installed",
        )
        .as_str()])));
        active_model_row.set_selected(0);
        active_model_row.set_sensitive(false);
        sync_active_model_details(None, active_rows);
        None
    } else {
        let names: Vec<String> = items
            .iter()
            .map(|info| friendly_model_name(&info.record.display_name))
            .collect();
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let selected_idx = state
            .active_id
            .borrow()
            .as_deref()
            .and_then(|id| items.iter().position(|x| x.record.id == id))
            .unwrap_or(0);
        let active_id = items.get(selected_idx).map(|x| x.record.id.clone());
        if *state.active_id.borrow() != active_id {
            *state.active_id.borrow_mut() = active_id.clone();
            (state.on_active_changed)(active_id.clone());
        }
        *state.cached_models.borrow_mut() = items.to_vec();
        active_model_row.set_model(Some(&gtk::StringList::new(&name_refs)));
        active_model_row.set_selected(selected_idx as u32);
        active_model_row.set_sensitive(true);
        sync_active_model_details(items.get(selected_idx), active_rows);
        active_id
    };
    state.is_syncing_combo.set(false);
    effective_active_id
}
