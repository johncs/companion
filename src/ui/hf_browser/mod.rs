//! Hugging Face GGUF repository browser, smart quantization picker, and resumable downloader.

mod download_controls;
mod download_state;
mod download_tracker;
mod download_worker;
mod repo_rows;
mod starter_models;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::runtime;
use crate::i18n::gettext;
use crate::models::hardware::HardwareInfo;
use crate::models::huggingface::search_gguf_repos;
use crate::storage::Database;
use crate::ui::widgets::clear_preferences_group;

use repo_rows::build_hf_repo_row;

pub use download_controls::{DownloadControls, DownloadTracker};
pub use download_tracker::restore_pending_downloads;
pub use starter_models::build_starter_model_group;

/// Build the dedicated "Explore AI Models" navigation subpage for [`adw::PreferencesDialog`].
pub fn build_explore_subpage(
    dialog: &adw::PreferencesDialog,
    db: Database,
    hw: HardwareInfo,
    refresh_installed: Rc<dyn Fn()>,
    tracker: DownloadTracker,
    models_dir: PathBuf,
) -> adw::NavigationPage {
    build_explore_subpage_with_query(
        dialog,
        db,
        hw,
        refresh_installed,
        tracker,
        models_dir,
        None,
    )
}

/// Build the "Explore AI Models" subpage optionally pre-populated with an initial search query.
pub fn build_explore_subpage_with_query(
    dialog: &adw::PreferencesDialog,
    db: Database,
    hw: HardwareInfo,
    refresh_installed: Rc<dyn Fn()>,
    tracker: DownloadTracker,
    models_dir: PathBuf,
    initial_query: Option<&str>,
) -> adw::NavigationPage {
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&adw::HeaderBar::new());

    let page = adw::PreferencesPage::new();

    let discover_group = adw::PreferencesGroup::builder()
        .title(gettext("Discover on Hugging Face"))
        .description(gettext("Search and download AI models directly into Companion."))
        .build();

    let search_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Search AI models (e.g. Gemma, Qwen, Llama)…"))
        .hexpand(true)
        .build();
    let search_btn = gtk::Button::builder()
        .label(gettext("Search"))
        .css_classes(["suggested-action"])
        .build();
    search_row.append(&search_entry);
    search_row.append(&search_btn);
    discover_group.add(&search_row);

    let results_group = adw::PreferencesGroup::new();

    let run_hf_search = build_search_runner(
        results_group.clone(),
        dialog.clone(),
        db,
        hw,
        refresh_installed,
        tracker,
        models_dir,
    );

    page.add(&discover_group);
    page.add(&results_group);
    toolbar_view.set_content(Some(&page));

    {
        let run = run_hf_search.clone();
        let entry = search_entry.clone();
        search_btn.connect_clicked(move |_| run(entry.text().to_string()));
    }
    {
        let run = run_hf_search.clone();
        search_entry.connect_activate(move |entry| run(entry.text().to_string()));
    }

    let init_q = initial_query.unwrap_or("").trim().to_string();
    if !init_q.is_empty() {
        search_entry.set_text(&init_q);
    }
    run_hf_search(init_q);

    adw::NavigationPage::builder()
        .title(gettext("Explore AI Models"))
        .tag("explore-models")
        .child(&toolbar_view)
        .build()
}

fn build_search_runner(
    results_group: adw::PreferencesGroup,
    dialog: adw::PreferencesDialog,
    db: Database,
    hw: HardwareInfo,
    refresh_installed: Rc<dyn Fn()>,
    tracker: DownloadTracker,
    models_dir: PathBuf,
) -> Rc<dyn Fn(String)> {
    let search_weak: Rc<RefCell<Option<Rc<dyn Fn(String)>>>> = Rc::new(RefCell::new(None));
    let search_weak_inner = search_weak.clone();

    let func: Rc<dyn Fn(String)> = Rc::new(move |query: String| {
        clear_preferences_group(&results_group);
        let loading = adw::ActionRow::builder()
            .title(gettext("Searching Hugging Face…"))
            .sensitive(false)
            .build();
        results_group.add(&loading);

        let (tx, rx) = tokio::sync::oneshot::channel();
        let query_for_task = query.clone();
        runtime().spawn(async move {
            let _ = tx.send(search_gguf_repos(&query_for_task, 12).await);
        });

        let results_group = results_group.clone();
        let db = db.clone();
        let hw = hw.clone();
        let dialog = dialog.clone();
        let refresh_installed = refresh_installed.clone();
        let tracker = tracker.clone();
        let search_weak_inner = search_weak_inner.clone();
        let models_dir = models_dir.clone();

        glib::MainContext::default().spawn_local(async move {
            let res = rx.await;
            clear_preferences_group(&results_group);
            match res {
                Ok(Ok(repos)) if repos.is_empty() => {
                    let empty = adw::ActionRow::builder()
                        .title(gettext("No AI model repositories found"))
                        .sensitive(false)
                        .build();
                    results_group.add(&empty);
                }
                Ok(Ok(repos)) => {
                    for repo in repos {
                        results_group.add(&build_hf_repo_row(
                            &repo,
                            &hw,
                            &db,
                            &dialog,
                            &refresh_installed,
                            &tracker,
                            models_dir.clone(),
                        ));
                    }
                }
                Ok(Err(err)) => {
                    let error_row = adw::ActionRow::builder()
                        .title(gettext("Could not reach Hugging Face"))
                        .subtitle(err.to_string())
                        .build();
                    error_row.add_prefix(&gtk::Image::from_icon_name("network-offline-symbolic"));
                    let retry_btn = gtk::Button::builder()
                        .icon_name("view-refresh-symbolic")
                        .tooltip_text(gettext("Retry"))
                        .valign(gtk::Align::Center)
                        .css_classes(["flat", "circular"])
                        .build();
                    let q_retry = query.clone();
                    retry_btn.connect_clicked(move |_| {
                        if let Some(f) = search_weak_inner.borrow().as_ref() {
                            f(q_retry.clone());
                        }
                    });
                    error_row.add_suffix(&retry_btn);
                    results_group.add(&error_row);
                }
                Err(_) => {}
            }
        });
    });

    *search_weak.borrow_mut() = Some(func.clone());
    func
}
