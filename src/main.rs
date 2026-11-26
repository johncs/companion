//! Companion application entry point.

mod agent;
mod core;
mod i18n;
mod inference;
mod models;
mod storage;
mod ui;

use adw::prelude::*;
use gtk::glib;
use relm4::RelmApp;
use tracing_subscriber::EnvFilter;

use crate::i18n::{gettext, tr_f};

pub const APP_ID: &str = "io.github.companion.Companion";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("companion=info")),
        )
        .init();

    i18n::init();

    let paths = core::paths::AppPaths::from_data_dir(glib::user_data_dir());
    match storage::Database::open(&paths.db_path) {
        Ok(db) => {
            if db.get_setting("user_name").ok().flatten().is_none() {
                let real = glib::real_name().to_string_lossy().trim().to_string();
                if !real.is_empty() && real != "Unknown" {
                    let _ = db.set_user_name(&real);
                }
            }
            if let Err(err) = models::registry::ModelRegistryService::sync_managed_models(&db, &paths.models_dir) {
                tracing::warn!("Failed to synchronize managed models: {err}");
            }
            let app = RelmApp::new(APP_ID);
            app.run::<ui::window::MainWindow>((db, paths));
        }
        Err(err) => {
            tracing::error!("Database initialization failed: {err}");
            show_startup_error(&paths.db_path, &err.to_string());
        }
    }
}

fn show_startup_error(db_path: &std::path::Path, error: &str) {
    let app = adw::Application::builder().application_id(APP_ID).build();
    let msg = tr_f(
        "Could not open database at:\n{path}\n\n{error}",
        &[("path", &db_path.display().to_string()), ("error", error)],
    );
    app.connect_activate(move |app| {
        let status = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title(gettext("Unable to Start Companion"))
            .description(&msg)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&status));

        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("Companion")
            .default_width(520)
            .default_height(360)
            .content(&toolbar)
            .build();
        win.present();
    });
    app.run();
}
