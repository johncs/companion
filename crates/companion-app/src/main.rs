//! Companion application entry point.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub(crate) use companion_agent as agent;
pub(crate) use companion_core as core;
pub(crate) use companion_core::i18n;
pub(crate) use companion_format as format;
pub(crate) use companion_inference as inference;
pub(crate) use companion_models as models;
pub(crate) use companion_storage as storage;
mod services;
mod ui;

use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use relm4::RelmApp;
use tracing_subscriber::EnvFilter;

use crate::i18n::{gettext, tr_f};
use crate::services::AppServices;

pub const APP_ID: &str = "io.github.companion.Companion";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("companion=info")),
        )
        .init();

    i18n::init();

    let paths = core::paths::AppPaths::from_data_dir(glib::user_data_dir());
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            tracing::error!("Tokio runtime initialization failed: {err}");
            show_startup_error(&paths.db_path, &err.to_string());
            return;
        }
    };

    match storage::Database::open(&paths.db_path) {
        Ok(db) => {
            let services = match AppServices::new(db, rt.handle().clone(), paths.clone()) {
                Ok(services) => Arc::new(services),
                Err(err) => {
                    tracing::error!("Service initialization failed: {err}");
                    show_startup_error(&paths.db_path, &err.to_string());
                    return;
                }
            };
            let app = RelmApp::new(APP_ID);
            app.run::<ui::window::MainWindow>(services);
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
