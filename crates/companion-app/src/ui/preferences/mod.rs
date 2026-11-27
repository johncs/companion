//! Unified Settings dialog (`AdwPreferencesDialog`) with General, Personalization, Models, and Agent & MCP pages.

mod personalization_editors;
mod personalization_group;
mod storage_stats;
mod tools_group;
mod web_search_group;

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use crate::i18n::gettext;
use crate::models::hardware::HardwareInfo;
use crate::services::AppServices;
use crate::storage::Database;
use crate::ui::model_manager::build_models_page;

use personalization_group::build_personalization_groups;
use storage_stats::{
    build_hardware_group, build_open_folder_button, build_size_label, spawn_storage_size_refresh,
};
use tools_group::build_available_tools_group;
use web_search_group::build_web_search_group;

/// Initial page to display when opening the unified Settings dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPage {
    General,
    Models,
}

impl SettingsPage {
    fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Models => "models",
        }
    }
}

/// Configuration and callbacks for presenting the unified Settings dialog.
pub struct SettingsDialogConfig<F, P, W> {
    pub services: Arc<AppServices>,
    pub hardware_info: HardwareInfo,
    pub active_model_id: Option<String>,
    pub initial_page: SettingsPage,
    pub on_active_changed: F,
    pub on_personalization_changed: P,
    pub on_web_search_global_changed: W,
}

/// Present the unified Settings [`adw::PreferencesDialog`], optionally opening directly to `initial_page`.
pub fn present_settings_dialog<F, P, W>(
    parent: &impl IsA<gtk::Widget>,
    cfg: SettingsDialogConfig<F, P, W>,
) where
    F: Fn(Option<String>) + 'static,
    P: Fn(String, Option<String>) + 'static,
    W: Fn(bool) + 'static,
{
    let SettingsDialogConfig {
        services,
        hardware_info: hw,
        active_model_id,
        initial_page,
        on_active_changed,
        on_personalization_changed,
        on_web_search_global_changed,
    } = cfg;
    let dialog = adw::PreferencesDialog::builder()
        .title(gettext("Settings"))
        .content_width(820)
        .content_height(660)
        .build();

    let pages = [
        build_general_page(&dialog, &services, &hw),
        build_personalization_page(
            &services.db,
            &services.rt_handle,
            on_personalization_changed,
        ),
        build_models_page(
            &dialog,
            services.clone(),
            hw,
            active_model_id,
            Rc::new(on_active_changed),
        ),
        build_agent_page(
            &services.db,
            &services.rt_handle,
            services.web_search.clone(),
            on_web_search_global_changed,
        ),
    ];
    for page in &pages {
        dialog.add(page);
    }
    dialog.set_visible_page_name(initial_page.as_str());
    dialog.present(Some(parent));
}

fn build_personalization_page<P: Fn(String, Option<String>) + 'static>(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    on_personalization_changed: P,
) -> adw::PreferencesPage {
    let (identity_group, behavior_group, about_group) =
        build_personalization_groups(db, rt_handle, on_personalization_changed);
    let page = adw::PreferencesPage::builder()
        .name("personalization")
        .title(gettext("Personalization"))
        .icon_name("avatar-default-symbolic")
        .build();
    page.add(&identity_group);
    page.add(&behavior_group);
    page.add(&about_group);
    page
}

/// Apply a persisted color scheme setting (`system`, `light`, `dark`) to `adw::StyleManager`.
pub fn apply_color_scheme_setting(value: Option<&str>) {
    let scheme = match value {
        Some("light") => adw::ColorScheme::ForceLight,
        Some("dark") => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

fn build_general_page(
    dialog: &adw::PreferencesDialog,
    services: &AppServices,
    hw: &crate::models::hardware::HardwareInfo,
) -> adw::PreferencesPage {
    let db = &services.db;
    let general_page = adw::PreferencesPage::builder()
        .name("general")
        .title(gettext("General"))
        .icon_name("preferences-system-symbolic")
        .build();
    let appearance_group = build_appearance_group(db, &services.rt_handle);
    let storage_group =
        build_storage_group(dialog, db, &services.paths.models_dir, &services.rt_handle);
    general_page.add(&appearance_group);
    general_page.add(&storage_group);
    general_page.add(&build_hardware_group(hw));
    general_page
}

fn build_appearance_group(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Appearance"))
        .description(gettext(
            "Companion follows your GNOME desktop theme and accent color.",
        ))
        .build();
    let style_row = adw::ComboRow::builder()
        .title(gettext("Color Scheme"))
        .subtitle(gettext(
            "System default follows GNOME dark/light preference",
        ))
        .model(&gtk::StringList::new(&[
            &gettext("System"),
            &gettext("Light"),
            &gettext("Dark"),
        ]))
        .build();
    let initial_selected = match adw::StyleManager::default().color_scheme() {
        adw::ColorScheme::ForceLight => 1,
        adw::ColorScheme::ForceDark => 2,
        _ => 0,
    };
    style_row.set_selected(initial_selected);
    let (db_style, rt_style) = (db.clone(), rt_handle.clone());
    style_row.connect_selected_notify(move |row| {
        let (setting, scheme) = match row.selected() {
            1 => ("light", adw::ColorScheme::ForceLight),
            2 => ("dark", adw::ColorScheme::ForceDark),
            _ => ("system", adw::ColorScheme::Default),
        };
        adw::StyleManager::default().set_color_scheme(scheme);
        let db = db_style.clone();
        rt_style.spawn_blocking(move || {
            if let Err(err) = db.set_setting("color_scheme", Some(setting)) {
                tracing::error!("Failed to save color scheme preference: {err}");
            }
        });
    });
    group.add(&style_row);
    group
}

fn build_storage_group(
    dialog: &adw::PreferencesDialog,
    db: &Database,
    models_dir: &Path,
    rt_handle: &tokio::runtime::Handle,
) -> adw::PreferencesGroup {
    let storage_group = adw::PreferencesGroup::builder()
        .title(gettext("Storage &amp; Data"))
        .build();
    let db_path = db.path().to_path_buf();
    let db_size_label = build_size_label();
    let db_row = build_path_property_row(
        &gettext("SQLite Database"),
        &db_path,
        &db_size_label,
        dialog,
        true,
    );
    storage_group.add(&db_row);

    let models_dir = models_dir.to_path_buf();
    let models_size_label = build_size_label();
    let models_row = build_path_property_row(
        &gettext("Downloaded AI Model Folder"),
        &models_dir,
        &models_size_label,
        dialog,
        false,
    );
    storage_group.add(&models_row);

    let total_row = adw::ActionRow::builder()
        .title(gettext("Total Allocated"))
        .subtitle(gettext("Calculating…"))
        .build();
    total_row.add_css_class("property");
    storage_group.add(&total_row);
    spawn_storage_size_refresh(
        rt_handle,
        total_row,
        db_size_label,
        models_size_label,
        db_path,
        models_dir,
    );
    storage_group
}

fn build_path_property_row(
    title: &str,
    path: &Path,
    size_label: &gtk::Label,
    dialog: &adw::PreferencesDialog,
    is_file: bool,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(crate::format::markdown::escape_pango_markup(
            path.to_string_lossy().as_ref(),
        ))
        .build();
    row.add_css_class("property");
    row.add_suffix(size_label);
    row.add_suffix(&build_open_folder_button(
        dialog,
        path.to_path_buf(),
        is_file,
    ));
    row
}

fn build_agent_page<W>(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    web_search: std::sync::Arc<crate::agent::web_search::WebSearchService>,
    on_web_search_global_changed: W,
) -> adw::PreferencesPage
where
    W: Fn(bool) + 'static,
{
    let page_title = if cfg!(feature = "experimental-mcp") {
        gettext("Agent & MCP")
    } else {
        gettext("Agent & Tools")
    };
    let agent_page = adw::PreferencesPage::builder()
        .name("agent")
        .title(page_title)
        .icon_name("applications-science-symbolic")
        .build();

    let (tools_group, update_web_tools) = build_available_tools_group(web_search, false);
    let web_search_group = build_web_search_group(
        db,
        rt_handle,
        move |enabled| update_web_tools(enabled),
        on_web_search_global_changed,
    );

    agent_page.add(&web_search_group);
    agent_page.add(&tools_group);

    if cfg!(feature = "experimental-mcp") {
        let agent_group = adw::PreferencesGroup::builder()
            .title(gettext("External MCP Servers"))
            .description(gettext(
                "External MCP server connections will be configured here.",
            ))
            .build();
        agent_page.add(&agent_group);
    }
    agent_page
}
