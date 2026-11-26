//! Hugging Face GGUF repository browser, smart quantization picker, and resumable downloader.

mod download_controls;
mod download_tracker;
mod repo_rows;
mod starter_models;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::runtime;
use crate::i18n::gettext;
use crate::models::download_manager::DownloadManager;
use crate::models::hardware::HardwareInfo;
use crate::models::huggingface::search_gguf_repos;
use crate::storage::Database;
use crate::ui::widgets::clear_preferences_group;

use repo_rows::build_hf_repo_row;

pub use download_controls::{DownloadControls, DownloadTracker};
pub use download_tracker::restore_pending_downloads;
pub use starter_models::build_starter_model_group;

/// Shared dependencies passed across Hugging Face browser builders.
#[derive(Clone)]
pub struct HfBrowserContext {
    pub dialog: adw::PreferencesDialog,
    pub db: Database,
    pub hw: HardwareInfo,
    pub download_manager: DownloadManager,
    pub tracker: DownloadTracker,
    pub refresh_installed: Rc<dyn Fn()>,
}

impl HfBrowserContext {
    pub(super) fn with_refresh(&self, refresh_installed: Rc<dyn Fn()>) -> Self {
        Self {
            dialog: self.dialog.clone(),
            db: self.db.clone(),
            hw: self.hw.clone(),
            download_manager: self.download_manager.clone(),
            tracker: self.tracker.clone(),
            refresh_installed,
        }
    }
}

/// Build the dedicated "Explore AI Models" navigation subpage for [`adw::PreferencesDialog`].
pub fn build_explore_subpage(ctx: &HfBrowserContext) -> adw::NavigationPage {
    build_explore_subpage_with_query(ctx, None)
}

/// Build the "Explore AI Models" subpage optionally pre-populated with an initial search query.
pub fn build_explore_subpage_with_query(
    ctx: &HfBrowserContext,
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
    let run_hf_search = build_search_runner(results_group.clone(), ctx.clone());

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
    ctx: HfBrowserContext,
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
        let ctx = ctx.clone();
        let search_weak_inner = search_weak_inner.clone();

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
                        results_group.add(&build_hf_repo_row(&repo, &ctx));
                    }
                }
                Ok(Err(err)) => {
                    let error_row = build_search_error_row(&err.to_string(), &query, &search_weak_inner);
                    results_group.add(&error_row);
                }
                Err(_) => {}
            }
        });
    });

    *search_weak.borrow_mut() = Some(func.clone());
    func
}

fn build_search_error_row(
    err_msg: &str,
    query: &str,
    search_weak: &Rc<RefCell<Option<Rc<dyn Fn(String)>>>>,
) -> adw::ActionRow {
    let error_row = adw::ActionRow::builder()
        .title(gettext("Could not reach Hugging Face"))
        .subtitle(err_msg)
        .build();
    error_row.add_prefix(&gtk::Image::from_icon_name("network-offline-symbolic"));
    let retry_btn = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text(gettext("Retry"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    let q_retry = query.to_string();
    let weak = search_weak.clone();
    retry_btn.connect_clicked(move |_| {
        if let Some(f) = weak.borrow().as_ref() {
            f(q_retry.clone());
        }
    });
    error_row.add_suffix(&retry_btn);
    error_row
}
