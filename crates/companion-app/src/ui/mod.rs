//! GTK4 / libadwaita UI components and dialogs.

pub mod chat_controls_dialog;
pub mod chat_view;
pub mod composer;
pub mod dialog_scaffold;
pub mod folder_permissions_dialog;
pub mod hf_browser;
pub mod icons;
pub mod media_widgets;
pub mod mention_picker;
pub mod message_dialogs;
pub mod message_object;
pub mod message_row;
pub mod model_config_dialog;
mod model_config_form_state;
pub mod model_manager;
pub mod permission_dialog;
pub mod preferences;
pub mod sidebar;
pub mod sidebar_rows;
pub mod widgets;
pub mod window;
mod window_actions;
mod window_settings;

use gtk::gdk;

/// Load Companion's custom CSS once for the default GDK display.
pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));

    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let icon_theme = gtk::IconTheme::for_display(&display);
        let dev_icons = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/icons");
        if dev_icons.exists() {
            icon_theme.add_search_path(&dev_icons);
            icon_theme.add_search_path(dev_icons.join("hicolor/symbolic/actions"));
        }
    }
}
