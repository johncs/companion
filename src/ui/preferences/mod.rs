//! Unified Settings dialog (`AdwPreferencesDialog`) with General, Personalization, Models, and Agent & MCP pages.

mod personalization_group;
mod storage_stats;
mod tools_group;
mod web_search_group;

use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::gettext;
use crate::models::hardware::probe_hardware;
use crate::models::managed_models_dir;
use crate::storage::Database;
use crate::ui::model_manager::build_models_page;

use personalization_group::build_personalization_groups;
use storage_stats::{
    build_hardware_group, build_open_folder_button, build_size_label, spawn_storage_size_refresh,
};
use tools_group::build_available_tools_group;
use web_search_group::build_web_search_group;

/// Present the unified Settings [`adw::PreferencesDialog`], optionally opening directly to `initial_page`
/// (`"general"`, `"personalization"`, `"models"`, or `"agent"`).
pub fn present_settings_dialog<F, P, W>(
    parent: &impl IsA<gtk::Widget>,
    db: Database,
    active_model_id: Option<String>,
    initial_page: &str,
    on_active_changed: F,
    on_personalization_changed: P,
    on_web_search_global_changed: W,
) where
    F: Fn(Option<String>) + 'static,
    P: Fn(String, Option<String>) + 'static,
    W: Fn(bool) + 'static,
{
    let dialog = adw::PreferencesDialog::builder()
        .title(gettext("Settings"))
        .content_width(820)
        .content_height(660)
        .build();

    let hw = probe_hardware();
    let general_page = build_general_page(&dialog, &db, &hw);

    let (identity_group, behavior_group, about_group) =
        build_personalization_groups(&db, on_personalization_changed);
    let personalization_page = adw::PreferencesPage::builder()
        .name("personalization")
        .title(gettext("Personalization"))
        .icon_name("avatar-default-symbolic")
        .build();
    personalization_page.add(&identity_group);
    personalization_page.add(&behavior_group);
    personalization_page.add(&about_group);

    let models_page = build_models_page(
        &dialog,
        db.clone(),
        hw,
        active_model_id,
        Rc::new(on_active_changed),
    );

    let agent_page = build_agent_page(&db, on_web_search_global_changed);

    dialog.add(&general_page);
    dialog.add(&personalization_page);
    dialog.add(&models_page);
    dialog.add(&agent_page);

    dialog.set_visible_page_name(initial_page);
    dialog.present(Some(parent));
}

fn build_general_page(
    dialog: &adw::PreferencesDialog,
    db: &Database,
    hw: &crate::models::hardware::HardwareInfo,
) -> adw::PreferencesPage {
    let general_page = adw::PreferencesPage::builder()
        .name("general")
        .title(gettext("General"))
        .icon_name("preferences-system-symbolic")
        .build();

    let appearance_group = adw::PreferencesGroup::builder()
        .title(gettext("Appearance"))
        .description(gettext("Companion follows your GNOME desktop theme and accent color."))
        .build();

    let style_row = adw::ComboRow::builder()
        .title(gettext("Color Scheme"))
        .subtitle(gettext("System default follows GNOME dark/light preference"))
        .model(&gtk::StringList::new(&[
            &gettext("System"),
            &gettext("Light"),
            &gettext("Dark"),
        ]))
        .build();

    style_row.set_selected(match adw::StyleManager::default().color_scheme() {
        adw::ColorScheme::ForceLight => 1,
        adw::ColorScheme::ForceDark => 2,
        _ => 0,
    });
    style_row.connect_selected_notify(|row| {
        let manager = adw::StyleManager::default();
        match row.selected() {
            1 => manager.set_color_scheme(adw::ColorScheme::ForceLight),
            2 => manager.set_color_scheme(adw::ColorScheme::ForceDark),
            _ => manager.set_color_scheme(adw::ColorScheme::Default),
        }
    });
    appearance_group.add(&style_row);

    let storage_group = build_storage_group(dialog, db);
    general_page.add(&appearance_group);
    general_page.add(&storage_group);
    general_page.add(&build_hardware_group(hw));
    general_page
}

fn build_storage_group(dialog: &adw::PreferencesDialog, db: &Database) -> adw::PreferencesGroup {
    let storage_group = adw::PreferencesGroup::builder()
        .title(gettext("Storage &amp; Data"))
        .build();

    let db_path = db.path().to_path_buf();
    let db_size_label = build_size_label();
    let db_row = adw::ActionRow::builder()
        .title(gettext("SQLite Database"))
        .subtitle(db_path.to_string_lossy().as_ref())
        .build();
    db_row.add_css_class("property");
    db_row.add_suffix(&db_size_label);
    db_row.add_suffix(&build_open_folder_button(dialog, db_path.clone(), true));
    storage_group.add(&db_row);

    let models_dir = managed_models_dir();
    let models_size_label = build_size_label();
    let models_dir_row = adw::ActionRow::builder()
        .title(gettext("Downloaded AI Model Folder"))
        .subtitle(models_dir.to_string_lossy().as_ref())
        .build();
    models_dir_row.add_css_class("property");
    models_dir_row.add_suffix(&models_size_label);
    models_dir_row.add_suffix(&build_open_folder_button(dialog, models_dir.clone(), false));
    storage_group.add(&models_dir_row);

    let total_row = adw::ActionRow::builder()
        .title(gettext("Total Allocated"))
        .subtitle(gettext("Calculating…"))
        .build();
    total_row.add_css_class("property");
    storage_group.add(&total_row);

    spawn_storage_size_refresh(
        total_row,
        db_size_label,
        models_size_label,
        db_path,
        models_dir,
    );
    storage_group
}

fn build_agent_page<W>(db: &Database, on_web_search_global_changed: W) -> adw::PreferencesPage
where
    W: Fn(bool) + 'static,
{
    let agent_page = adw::PreferencesPage::builder()
        .name("agent")
        .title(gettext("Agent & MCP"))
        .icon_name("applications-science-symbolic")
        .build();

    let (tools_group, update_web_tools) =
        build_available_tools_group(db.get_web_search_global_enabled().unwrap_or(false));
    let web_search_group = build_web_search_group(db, move |enabled| {
        update_web_tools(enabled);
        on_web_search_global_changed(enabled);
    });

    agent_page.add(&web_search_group);
    agent_page.add(&tools_group);

    let agent_group = adw::PreferencesGroup::builder()
        .title(gettext("External MCP Servers"))
        .description(gettext("External MCP server connections will be configured here."))
        .build();
    agent_page.add(&agent_group);
    agent_page
}
