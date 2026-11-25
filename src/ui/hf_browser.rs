//! Hugging Face GGUF repository browser, smart quantization picker, and resumable downloader.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use adw::prelude::*;
use gtk::glib;

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::hardware::{HardwareInfo, assess_model_fit};
use crate::models::huggingface::{
    HfGgufFile, HfRepoSummary, RECOMMENDED_STARTER_NAME, RECOMMENDED_STARTER_REPO,
    download_gguf_file, list_repo_gguf_files, remove_part_file, search_gguf_repos,
    select_starter_gguf,
};
use crate::models::{
    format_bytes, friendly_model_name, infer_quantization, managed_models_dir,
};
use crate::storage::{Database, PendingDownload};
use crate::ui::widgets::{
    clear_preferences_group, present_destructive_confirm, send_desktop_notification,
};

/// Reusable inline download controls: percentage label, progress bar, pause/resume, and cancel.
#[derive(Clone)]
pub struct DownloadControls {
    pub container: gtk::Box,
    pub pct_label: gtk::Label,
    pub progress: gtk::ProgressBar,
    pub pause_btn: gtk::Button,
    pub cancel_btn: gtk::Button,
}

impl Default for DownloadControls {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadControls {
    pub fn new() -> Self {
        let container = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();

        let pct_label = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .width_chars(4)
            .xalign(1.0)
            .build();

        let progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Center)
            .build();
        progress.set_size_request(100, -1);

        let pause_btn = gtk::Button::builder()
            .icon_name("media-playback-pause-symbolic")
            .tooltip_text(gettext("Pause download"))
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();

        let cancel_btn = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text(gettext("Cancel download"))
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();

        container.append(&pct_label);
        container.append(&progress);
        container.append(&pause_btn);
        container.append(&cancel_btn);

        Self {
            container,
            pct_label,
            progress,
            pause_btn,
            cancel_btn,
        }
    }

    pub fn set_progress(&self, done: u64, total: Option<u64>) {
        if let Some(t) = total
            && t > 0
        {
            let frac = (done as f64 / t as f64).clamp(0.0, 1.0);
            self.progress.set_fraction(frac);
            self.pct_label
                .set_text(&format!("{}%", (frac * 100.0).round() as u32));
        } else {
            self.progress.pulse();
            self.pct_label.set_text("");
        }
    }

    pub fn set_paused(&self, paused: bool) {
        if paused {
            self.pause_btn
                .set_icon_name("media-playback-start-symbolic");
            self.pause_btn
                .set_tooltip_text(Some(&gettext("Resume download")));
        } else {
            self.pause_btn
                .set_icon_name("media-playback-pause-symbolic");
            self.pause_btn
                .set_tooltip_text(Some(&gettext("Pause download")));
        }
    }

    pub fn reset_active(&self) {
        self.set_paused(false);
        self.pct_label.set_text("0%");
        self.progress.set_fraction(0.0);
        self.pause_btn.set_visible(true);
        self.cancel_btn.set_visible(true);
        self.container.set_visible(true);
    }
}

/// Tracks active Hugging Face downloads on the main AI Models page.
#[derive(Clone)]
pub struct DownloadTracker {
    group: adw::PreferencesGroup,
    active_count: Rc<RefCell<usize>>,
    active_repos: Rc<RefCell<HashSet<String>>>,
}

impl DownloadTracker {
    pub fn new(group: adw::PreferencesGroup) -> Self {
        Self {
            group,
            active_count: Rc::new(RefCell::new(0)),
            active_repos: Rc::new(RefCell::new(HashSet::new())),
        }
    }

    pub fn is_repo_active(&self, repo_id: &str) -> bool {
        if self.active_repos.borrow().contains(repo_id) {
            return true;
        }
        ACTIVE_DOWNLOADS
            .lock()
            .map(|m| m.keys().any(|(r, _)| r == repo_id))
            .unwrap_or(false)
    }

    /// Add an active download row and return `(DownloadControls, remove_callback)`.
    fn track(
        &self,
        repo_id: &str,
        title: &str,
        on_pause: Rc<dyn Fn()>,
        on_cancel: Rc<dyn Fn()>,
    ) -> (DownloadControls, Rc<dyn Fn()>) {
        let row = adw::ActionRow::builder().title(title).build();
        let controls = DownloadControls::new();
        controls.reset_active();

        controls.pause_btn.connect_clicked(move |_| on_pause());
        controls.cancel_btn.connect_clicked(move |_| on_cancel());

        row.add_suffix(&controls.container);
        self.group.add(&row);

        *self.active_count.borrow_mut() += 1;
        self.active_repos.borrow_mut().insert(repo_id.to_string());
        self.group.set_visible(true);

        let group = self.group.clone();
        let active_count = self.active_count.clone();
        let active_repos = self.active_repos.clone();
        let repo_owned = repo_id.to_string();
        let removed = Rc::new(RefCell::new(false));
        let finish: Rc<dyn Fn()> = Rc::new(move || {
            if *removed.borrow() {
                return;
            }
            *removed.borrow_mut() = true;
            group.remove(&row);
            active_repos.borrow_mut().remove(&repo_owned);
            let mut count = active_count.borrow_mut();
            *count = count.saturating_sub(1);
            if *count == 0 {
                group.set_visible(false);
            }
        });

        (controls, finish)
    }
}

/// Build the dedicated "Explore AI Models" navigation subpage for [`adw::PreferencesDialog`].
pub fn build_explore_subpage(
    dialog: &adw::PreferencesDialog,
    db: Database,
    hw: HardwareInfo,
    refresh_installed: Rc<dyn Fn()>,
    tracker: DownloadTracker,
) -> adw::NavigationPage {
    build_explore_subpage_with_query(dialog, db, hw, refresh_installed, tracker, None)
}

/// Build the "Explore AI Models" subpage optionally pre-populated with an initial search query.
pub fn build_explore_subpage_with_query(
    dialog: &adw::PreferencesDialog,
    db: Database,
    hw: HardwareInfo,
    refresh_installed: Rc<dyn Fn()>,
    tracker: DownloadTracker,
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

fn build_hf_repo_row(
    repo: &HfRepoSummary,
    hw: &HardwareInfo,
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
) -> adw::ExpanderRow {
    let expander = adw::ExpanderRow::builder()
        .title(&repo.id)
        .subtitle(tr_f(
            "{downloads} downloads · {likes} likes",
            &[
                ("downloads", &format_compact_count(repo.downloads)),
                ("likes", &format_compact_count(repo.likes)),
            ],
        ))
        .build();

    let placeholder = adw::ActionRow::builder()
        .title(gettext("Expand to view downloadable files…"))
        .sensitive(false)
        .build();
    expander.add_row(&placeholder);

    let loaded = Rc::new(RefCell::new(false));
    let repo_id = repo.id.clone();
    let hw = hw.clone();
    let db = db.clone();
    let dialog = dialog.clone();
    let refresh_installed = refresh_installed.clone();
    let tracker = tracker.clone();

    expander.connect_expanded_notify(move |exp| {
        if !exp.is_expanded() || *loaded.borrow() {
            return;
        }
        *loaded.borrow_mut() = true;
        placeholder.set_title(&gettext("Loading available files…"));

        let (tx, rx) = tokio::sync::oneshot::channel();
        let rid = repo_id.clone();
        runtime().spawn(async move {
            let _ = tx.send(list_repo_gguf_files(&rid).await);
        });

        let exp = exp.clone();
        let placeholder = placeholder.clone();
        let rid = repo_id.clone();
        let hw = hw.clone();
        let db = db.clone();
        let dialog = dialog.clone();
        let refresh_installed = refresh_installed.clone();
        let tracker = tracker.clone();

        let loaded_flag = loaded.clone();
        glib::MainContext::default().spawn_local(async move {
            match rx.await {
                Ok(Ok(files)) if files.is_empty() => {
                    placeholder.set_title(&gettext("No .gguf files in repository"));
                }
                Ok(Ok(files)) => {
                    exp.remove(&placeholder);
                    populate_repo_files(
                        &exp,
                        &rid,
                        files,
                        &hw,
                        &db,
                        &dialog,
                        &refresh_installed,
                        &tracker,
                    );
                }
                Ok(Err(err)) => {
                    *loaded_flag.borrow_mut() = false;
                    placeholder.set_title(&tr_f(
                        "Failed to load files (re-expand to retry): {err}",
                        &[("err", &err.to_string())],
                    ));
                }
                Err(_) => {
                    *loaded_flag.borrow_mut() = false;
                }
            }
        });
    });

    expander
}

#[allow(clippy::too_many_arguments)]
fn populate_repo_files(
    exp: &adw::ExpanderRow,
    repo_id: &str,
    files: Vec<HfGgufFile>,
    hw: &HardwareInfo,
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
) {
    let mut mmproj_file: Option<HfGgufFile> = None;
    let mut model_files: Vec<HfGgufFile> = Vec::with_capacity(files.len());
    for file in files {
        if file.filename.to_ascii_lowercase().contains("mmproj") {
            if mmproj_file.is_none() {
                mmproj_file = Some(file);
            }
        } else {
            model_files.push(file);
        }
    }

    if model_files.is_empty() {
        if let Some(mm) = mmproj_file {
            exp.add_row(&build_hf_file_row(
                repo_id,
                &mm,
                None,
                hw,
                db,
                dialog,
                refresh_installed,
                tracker,
            ));
        }
        return;
    }

    for file in model_files {
        exp.add_row(&build_hf_file_row(
            repo_id,
            &file,
            mmproj_file.as_ref(),
            hw,
            db,
            dialog,
            refresh_installed,
            tracker,
        ));
    }
}

/// Build a reusable "Recommended Starter Model" [`adw::PreferencesGroup`].
pub fn build_starter_model_group(
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Recommended Starter AI Model"))
        .build();
    group.add(&build_starter_model_row(
        db,
        dialog,
        refresh_installed,
        tracker,
    ));
    group
}

/// Build a 1-click starter model row (`Gemma 4 E4B`) with percentage, pause/resume, and cancel.
pub fn build_starter_model_row(
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
) -> adw::ActionRow {
    let default_sub = gettext("Fast, lightweight starter model with Text, Thinking, and Vision");
    let row = adw::ActionRow::builder()
        .title(RECOMMENDED_STARTER_NAME)
        .subtitle(&default_sub)
        .build();

    let resolved_starter: Rc<RefCell<Option<(HfGgufFile, Option<HfGgufFile>)>>> =
        Rc::new(RefCell::new(None));
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<Option<(HfGgufFile, Option<HfGgufFile>)>>();
        runtime().spawn(async move {
            let found = list_repo_gguf_files(RECOMMENDED_STARTER_REPO)
                .await
                .ok()
                .and_then(|files| select_starter_gguf(&files));
            let _ = tx.send(found);
        });
        let row_ref = row.clone();
        let resolved_ref = resolved_starter.clone();
        let friendly_line = default_sub.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Ok(Some((model_file, mmproj_file))) = rx.await {
                let mut lines = vec![
                    friendly_line,
                    tr_f(
                        "Model: {file} ({size})",
                        &[
                            ("file", &model_file.filename),
                            ("size", &format_bytes(model_file.size_bytes)),
                        ],
                    ),
                ];
                if let Some(mm) = &mmproj_file {
                    lines.push(tr_f(
                        "Vision: {file} ({size})",
                        &[
                            ("file", &mm.filename),
                            ("size", &format_bytes(mm.size_bytes)),
                        ],
                    ));
                }
                row_ref.set_subtitle(&gtk::glib::markup_escape_text(&lines.join("\n")));
                *resolved_ref.borrow_mut() = Some((model_file, mmproj_file));
            }
        });
    }

    let is_installed = db
        .list_models()
        .unwrap_or_default()
        .iter()
        .any(|m| {
            m.hf_repo.as_deref() == Some(RECOMMENDED_STARTER_REPO)
                && m.file_path
                    .as_deref()
                    .is_some_and(|p| std::path::Path::new(p).exists())
        });

    let is_downloading = tracker.is_repo_active(RECOMMENDED_STARTER_REPO);

    let controls = DownloadControls::new();
    let installed_label = gtk::Label::builder()
        .label(&gettext("Installed"))
        .valign(gtk::Align::Center)
        .css_classes(["dim-label", "caption-heading"])
        .visible(is_installed)
        .build();
    let downloading_label = gtk::Label::builder()
        .label(&gettext("Downloading"))
        .valign(gtk::Align::Center)
        .css_classes(["dim-label", "caption-heading"])
        .visible(!is_installed && is_downloading)
        .build();
    let download_btn = gtk::Button::builder()
        .label(&gettext("Download"))
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .visible(!is_installed && !is_downloading)
        .build();

    let on_pause_handler: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_cancel_handler: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    {
        let on_pause_handler = on_pause_handler.clone();
        controls.pause_btn.connect_clicked(move |_| {
            let cb = on_pause_handler.borrow().clone();
            if let Some(cb) = cb {
                cb();
            }
        });
    }
    {
        let on_cancel_handler = on_cancel_handler.clone();
        controls.cancel_btn.connect_clicked(move |_| {
            let cb = on_cancel_handler.borrow().clone();
            if let Some(cb) = cb {
                cb();
            }
        });
    }

    {
        let db = db.clone();
        let dialog = dialog.clone();
        let refresh_installed = refresh_installed.clone();
        let tracker = tracker.clone();
        let controls = controls.clone();
        let installed_label = installed_label.clone();
        let resolved_starter = resolved_starter.clone();

        download_btn.connect_clicked(move |btn| {
            if let Some((model_file, mmproj_file)) = resolved_starter.borrow().clone() {
                let refresh_with_badge: Rc<dyn Fn()> = {
                    let refresh_installed = refresh_installed.clone();
                    let btn = btn.clone();
                    let installed_label = installed_label.clone();
                    Rc::new(move || {
                        btn.set_visible(false);
                        installed_label.set_visible(true);
                        refresh_installed();
                    })
                };
                start_hf_download(
                    btn,
                    &controls,
                    &on_pause_handler,
                    &on_cancel_handler,
                    RECOMMENDED_STARTER_REPO,
                    &model_file.filename,
                    mmproj_file.as_ref().map(|m| m.filename.as_str()),
                    &db,
                    &dialog,
                    &refresh_with_badge,
                    &tracker,
                );
                return;
            }

            btn.set_sensitive(false);
            controls.pct_label.set_text("");
            controls.pause_btn.set_visible(false);
            controls.cancel_btn.set_visible(false);
            controls.container.set_visible(true);
            controls.progress.pulse();

            let (tx, rx) = tokio::sync::oneshot::channel::<
                Result<(HfGgufFile, Option<HfGgufFile>), String>,
            >();
            runtime().spawn(async move {
                let res = list_repo_gguf_files(RECOMMENDED_STARTER_REPO)
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|files| {
                        select_starter_gguf(&files).ok_or_else(|| {
                            gettext("No starter AI model found in repository")
                        })
                    });
                let _ = tx.send(res);
            });

            let btn = btn.clone();
            let controls = controls.clone();
            let on_pause_handler = on_pause_handler.clone();
            let on_cancel_handler = on_cancel_handler.clone();
            let db = db.clone();
            let dialog = dialog.clone();
            let refresh_installed = refresh_installed.clone();
            let tracker = tracker.clone();
            let installed_label = installed_label.clone();

            glib::MainContext::default().spawn_local(async move {
                let res = rx.await;
                btn.set_sensitive(true);
                match res {
                    Ok(Ok((model_file, mmproj_file))) => {
                        let refresh_with_badge: Rc<dyn Fn()> = {
                            let refresh_installed = refresh_installed.clone();
                            let btn = btn.clone();
                            let installed_label = installed_label.clone();
                            Rc::new(move || {
                                btn.set_visible(false);
                                installed_label.set_visible(true);
                                refresh_installed();
                            })
                        };
                        start_hf_download(
                            &btn,
                            &controls,
                            &on_pause_handler,
                            &on_cancel_handler,
                            RECOMMENDED_STARTER_REPO,
                            &model_file.filename,
                            mmproj_file.as_ref().map(|m| m.filename.as_str()),
                            &db,
                            &dialog,
                            &refresh_with_badge,
                            &tracker,
                        );
                    }
                    Ok(Err(err)) => {
                        controls.container.set_visible(false);
                        dialog.add_toast(adw::Toast::new(&tr_f(
                            "Failed to fetch starter model: {err}",
                            &[("err", &err)],
                        )));
                    }
                    Err(_) => {
                        controls.container.set_visible(false);
                    }
                }
            });
        });
    }

    row.add_suffix(&controls.container);
    row.add_suffix(&downloading_label);
    row.add_suffix(&installed_label);
    row.add_suffix(&download_btn);
    row
}

#[allow(clippy::too_many_arguments)]
fn build_hf_file_row(
    repo_id: &str,
    file: &HfGgufFile,
    mmproj_file: Option<&HfGgufFile>,
    hw: &HardwareInfo,
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
) -> adw::ActionRow {
    let quant = infer_quantization(&file.filename).unwrap_or_else(|| "GGUF".to_string());
    let fit = assess_model_fit(file.size_bytes, hw.total_ram_bytes);

    let mut subtitle_parts = vec![format_bytes(file.size_bytes), quant, fit.label()];
    if mmproj_file.is_some() {
        subtitle_parts.push(gettext("+ Vision"));
    }

    let row = adw::ActionRow::builder()
        .title(&file.filename)
        .subtitle(subtitle_parts.join(" · "))
        .build();

    let controls = DownloadControls::new();

    let download_btn = gtk::Button::builder()
        .icon_name("folder-download-symbolic")
        .tooltip_text(gettext("Download AI model"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();

    let on_pause_handler: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_cancel_handler: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    {
        let on_pause_handler = on_pause_handler.clone();
        controls.pause_btn.connect_clicked(move |_| {
            let cb = on_pause_handler.borrow().clone();
            if let Some(cb) = cb {
                cb();
            }
        });
    }
    {
        let on_cancel_handler = on_cancel_handler.clone();
        controls.cancel_btn.connect_clicked(move |_| {
            let cb = on_cancel_handler.borrow().clone();
            if let Some(cb) = cb {
                cb();
            }
        });
    }

    {
        let repo_id = repo_id.to_string();
        let filename = file.filename.clone();
        let mmproj_name = mmproj_file.map(|m| m.filename.clone());
        let db = db.clone();
        let dialog = dialog.clone();
        let refresh_installed = refresh_installed.clone();
        let tracker = tracker.clone();
        let controls = controls.clone();
        let on_pause_handler = on_pause_handler.clone();
        let on_cancel_handler = on_cancel_handler.clone();

        download_btn.connect_clicked(move |btn| {
            start_hf_download(
                btn,
                &controls,
                &on_pause_handler,
                &on_cancel_handler,
                &repo_id,
                &filename,
                mmproj_name.as_deref(),
                &db,
                &dialog,
                &refresh_installed,
                &tracker,
            );
        });
    }

    row.add_suffix(&controls.container);
    row.add_suffix(&download_btn);
    row
}

#[derive(Clone, Debug)]
struct DownloadSnapshot {
    done: u64,
    total: Option<u64>,
    is_paused: bool,
    finished: bool,
    cancelled: bool,
    error: Option<String>,
}

struct ActiveDownloadEntry {
    repo_id: String,
    filename: String,
    mmproj_filename: Option<String>,
    display_name: String,
    done: u64,
    total: Option<u64>,
    is_paused: bool,
    is_cancelled: bool,
    stop_flag: Arc<AtomicBool>,
    tx: tokio::sync::broadcast::Sender<DownloadSnapshot>,
}

type ActiveDownloadMap = HashMap<(String, String), Arc<Mutex<ActiveDownloadEntry>>>;

static ACTIVE_DOWNLOADS: LazyLock<Mutex<ActiveDownloadMap>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn emit_snapshot(entry: &ActiveDownloadEntry, finished: bool, error: Option<String>) {
    let _ = entry.tx.send(DownloadSnapshot {
        done: entry.done,
        total: entry.total,
        is_paused: entry.is_paused,
        finished,
        cancelled: entry.is_cancelled,
        error,
    });
}

fn launch_entry_task(entry_arc: Arc<Mutex<ActiveDownloadEntry>>, db: Database) {
    let (repo_id, filename, mmproj_name, display_name, stop_flag) = {
        let mut entry = entry_arc.lock().expect("active download mutex poisoned");
        let flag = Arc::new(AtomicBool::new(false));
        entry.stop_flag = flag.clone();
        entry.is_paused = false;
        emit_snapshot(&entry, false, None);
        (
            entry.repo_id.clone(),
            entry.filename.clone(),
            entry.mmproj_filename.clone(),
            entry.display_name.clone(),
            flag,
        )
    };

    let safe_repo = repo_id.replace('/', "--");
    let repo_dir = managed_models_dir().join(&safe_repo);
    let dest_path = repo_dir.join(&filename);
    let mmproj_dest = mmproj_name.as_deref().map(|m| repo_dir.join(m));

    runtime().spawn(async move {
        let done_main = Arc::new(AtomicU64::new(0));
        let total_main = Arc::new(AtomicU64::new(0));
        let done_mm = Arc::new(AtomicU64::new(0));
        let total_mm = Arc::new(AtomicU64::new(0));
        let saved_total = Arc::new(AtomicU64::new(0));

        let update_progress = {
            let entry_arc = entry_arc.clone();
            let db_t = db.clone();
            let rid_t = repo_id.clone();
            let fname_t = filename.clone();
            let saved_total = saved_total.clone();
            move |done: u64, total: Option<u64>| {
                if let Some(t) = total
                    && t > 0
                    && saved_total.swap(t, Ordering::Relaxed) != t
                {
                    let db_c = db_t.clone();
                    let r = rid_t.clone();
                    let f = fname_t.clone();
                    tokio::task::spawn_blocking(move || {
                        let _ = db_c.update_pending_download_total(&r, &f, t);
                    });
                }
                if let Ok(mut entry) = entry_arc.lock() {
                    entry.done = done;
                    if total.is_some() {
                        entry.total = total;
                    }
                    emit_snapshot(&entry, false, None);
                }
            }
        };

        let send_combined = {
            let dm = done_main.clone();
            let tm = total_main.clone();
            let dmm = done_mm.clone();
            let tmm = total_mm.clone();
            let update = update_progress.clone();
            move || {
                let done = dm
                    .load(Ordering::Relaxed)
                    .saturating_add(dmm.load(Ordering::Relaxed));
                let total = tm
                    .load(Ordering::Relaxed)
                    .saturating_add(tmm.load(Ordering::Relaxed));
                update(done, (total > 0).then_some(total));
            }
        };

        let res = if let (Some(mm_file), Some(mm_path)) = (&mmproj_name, mmproj_dest.clone()) {
            let main_fut = download_gguf_file(&repo_id, &filename, &dest_path, stop_flag.clone(), {
                let dm = done_main.clone();
                let tm = total_main.clone();
                let send = send_combined.clone();
                move |done, total| {
                    dm.store(done, Ordering::Relaxed);
                    if let Some(t) = total {
                        tm.store(t, Ordering::Relaxed);
                    }
                    send();
                }
            });
            let mm_fut = download_gguf_file(&repo_id, mm_file, &mm_path, stop_flag.clone(), {
                let dmm = done_mm.clone();
                let tmm = total_mm.clone();
                let send = send_combined;
                move |done, total| {
                    dmm.store(done, Ordering::Relaxed);
                    if let Some(t) = total {
                        tmm.store(t, Ordering::Relaxed);
                    }
                    send();
                }
            });
            let (r_main, r_mm) = tokio::join!(main_fut, mm_fut);
            r_main.and(r_mm)
        } else {
            download_gguf_file(&repo_id, &filename, &dest_path, stop_flag, update_progress).await
        };

        match res {
            Ok(()) => {
                let db_reg = db.clone();
                let dname_reg = display_name.clone();
                let dpath_reg = dest_path.clone();
                let mpath_reg = mmproj_dest.clone();
                let rid_reg = repo_id.clone();
                let fname_reg = filename.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(record) = db_reg.register_local_gguf(
                        Some(dname_reg),
                        &dpath_reg,
                        mpath_reg.as_deref(),
                        Some(rid_reg.clone()),
                    ) && db_reg.get_active_model_id().ok().flatten().is_none()
                    {
                        let _ = db_reg.set_active_model_id(Some(&record.id));
                    }
                    let _ = db_reg.remove_pending_download(&rid_reg, &fname_reg);
                })
                .await;

                if let Ok(mut map) = ACTIVE_DOWNLOADS.lock() {
                    map.remove(&(repo_id.clone(), filename.clone()));
                }
                if let Ok(entry) = entry_arc.lock() {
                    emit_snapshot(&entry, true, None);
                }
                let notif_id = format!("download-{repo_id}");
                let dname_notif = display_name.clone();
                glib::idle_add_once(move || {
                    send_desktop_notification(
                        &notif_id,
                        &gettext("Model Downloaded"),
                        &tr_f("{name} is ready to chat.", &[("name", &dname_notif)]),
                    );
                });
            }
            Err(err) => {
                let (is_paused, is_cancelled) = entry_arc
                    .lock()
                    .map(|e| (e.is_paused, e.is_cancelled))
                    .unwrap_or((false, false));

                if is_paused && !is_cancelled {
                    if let Ok(entry) = entry_arc.lock() {
                        emit_snapshot(&entry, false, None);
                    }
                    return;
                }

                if let Ok(mut map) = ACTIVE_DOWNLOADS.lock() {
                    map.remove(&(repo_id.clone(), filename.clone()));
                }

                if is_cancelled {
                    remove_part_file(&dest_path).await;
                    if let Some(mp) = mmproj_dest {
                        remove_part_file(&mp).await;
                    }
                    let db_c = db.clone();
                    let rid_c = repo_id.clone();
                    let fname_c = filename.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = db_c.remove_pending_download(&rid_c, &fname_c);
                    })
                    .await;
                    if let Ok(entry) = entry_arc.lock() {
                        emit_snapshot(&entry, true, None);
                    }
                    return;
                }

                let err_str = err.to_string();
                if let Ok(entry) = entry_arc.lock() {
                    emit_snapshot(&entry, true, Some(err_str.clone()));
                }
                let notif_id = format!("download-{repo_id}");
                glib::idle_add_once(move || {
                    send_desktop_notification(
                        &notif_id,
                        &gettext("Download Failed"),
                        &tr_f("{name}: {err}", &[("name", &display_name), ("err", &err_str)]),
                    );
                });
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn attach_entry_to_tracker(
    entry_arc: Arc<Mutex<ActiveDownloadEntry>>,
    db: &Database,
    tracker: &DownloadTracker,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    row_btn: Option<&gtk::Button>,
    row_controls: Option<&DownloadControls>,
    on_pause_handler: Option<&Rc<RefCell<Option<Rc<dyn Fn()>>>>>,
    on_cancel_handler: Option<&Rc<RefCell<Option<Rc<dyn Fn()>>>>>,
) {
    let (repo_id, filename, mmproj_name, display_name, init_done, init_total, init_paused, mut rx) = {
        let entry = entry_arc.lock().expect("active download mutex poisoned");
        (
            entry.repo_id.clone(),
            entry.filename.clone(),
            entry.mmproj_filename.clone(),
            entry.display_name.clone(),
            entry.done,
            entry.total,
            entry.is_paused,
            entry.tx.subscribe(),
        )
    };

    let on_pause: Rc<dyn Fn()> = {
        let entry_arc = entry_arc.clone();
        let db = db.clone();
        Rc::new(move || {
            let should_resume = {
                let Ok(mut entry) = entry_arc.lock() else {
                    return;
                };
                if !entry.is_paused {
                    entry.is_paused = true;
                    entry.stop_flag.store(true, Ordering::Relaxed);
                    emit_snapshot(&entry, false, None);
                    false
                } else {
                    true
                }
            };
            if should_resume {
                launch_entry_task(entry_arc.clone(), db.clone());
            }
        })
    };

    let on_cancel: Rc<dyn Fn()> = {
        let entry_arc = entry_arc.clone();
        let db = db.clone();
        let dialog_confirm = dialog.clone();
        let display_name_confirm = display_name.clone();
        let repo_id_c = repo_id.clone();
        let filename_c = filename.clone();
        let mmproj_c = mmproj_name.clone();
        Rc::new(move || {
            let entry_arc = entry_arc.clone();
            let db = db.clone();
            let repo_id_c = repo_id_c.clone();
            let filename_c = filename_c.clone();
            let mmproj_c = mmproj_c.clone();
            let body = tr_f(
                "Cancel downloading \"{name}\" and delete the partial download from disk?",
                &[("name", &display_name_confirm)],
            );

            present_destructive_confirm(
                &dialog_confirm,
                &gettext("Cancel Download?"),
                &body,
                &gettext("Cancel Download"),
                move || {
                    let is_still_active = ACTIVE_DOWNLOADS
                        .lock()
                        .map(|m| m.contains_key(&(repo_id_c.clone(), filename_c.clone())))
                        .unwrap_or(false);
                    if !is_still_active {
                        return;
                    }

                    let was_paused = {
                        let Ok(mut entry) = entry_arc.lock() else {
                            return;
                        };
                        let paused = entry.is_paused;
                        entry.is_cancelled = true;
                        entry.is_paused = false;
                        entry.stop_flag.store(true, Ordering::Relaxed);
                        paused
                    };

                    if was_paused {
                        if let Ok(mut map) = ACTIVE_DOWNLOADS.lock() {
                            map.remove(&(repo_id_c.clone(), filename_c.clone()));
                        }
                        if let Ok(entry) = entry_arc.lock() {
                            emit_snapshot(&entry, true, None);
                        }
                        let safe_repo = repo_id_c.replace('/', "--");
                        let repo_dir = managed_models_dir().join(&safe_repo);
                        let dpath = repo_dir.join(&filename_c);
                        let mpath = mmproj_c.as_deref().map(|m| repo_dir.join(m));
                        let db_c = db.clone();
                        let r = repo_id_c.clone();
                        let f = filename_c.clone();
                        runtime().spawn(async move {
                            remove_part_file(&dpath).await;
                            if let Some(mp) = mpath {
                                remove_part_file(&mp).await;
                            }
                            let _ = tokio::task::spawn_blocking(move || {
                                let _ = db_c.remove_pending_download(&r, &f);
                            })
                            .await;
                        });
                    }
                },
            );
        })
    };

    if let Some(p) = on_pause_handler {
        *p.borrow_mut() = Some(on_pause.clone());
    }
    if let Some(c) = on_cancel_handler {
        *c.borrow_mut() = Some(on_cancel.clone());
    }

    let (tracker_controls, finish_tracking) =
        tracker.track(&repo_id, &display_name, on_pause, on_cancel);
    tracker_controls.set_progress(init_done, init_total);
    tracker_controls.set_paused(init_paused);

    if let Some(c) = row_controls {
        c.reset_active();
        c.set_progress(init_done, init_total);
        c.set_paused(init_paused);
    }

    let row_btn_opt = row_btn.cloned();
    let row_controls_opt = row_controls.cloned();
    let on_pause_opt = on_pause_handler.cloned();
    let on_cancel_opt = on_cancel_handler.cloned();
    let dialog_toast = dialog.clone();
    let refresh_installed = refresh_installed.clone();

    glib::MainContext::default().spawn_local(async move {
        while let Ok(snap) = rx.recv().await {
            tracker_controls.set_progress(snap.done, snap.total);
            tracker_controls.set_paused(snap.is_paused);
            if let Some(c) = &row_controls_opt {
                c.set_progress(snap.done, snap.total);
                c.set_paused(snap.is_paused);
            }
            if snap.finished {
                finish_tracking();
                if let Some(p) = &on_pause_opt {
                    *p.borrow_mut() = None;
                }
                if let Some(c) = &on_cancel_opt {
                    *c.borrow_mut() = None;
                }
                if let Some(c) = &row_controls_opt {
                    c.container.set_visible(false);
                }
                if let Some(b) = &row_btn_opt {
                    b.set_visible(true);
                }
                if !snap.cancelled {
                    if let Some(err) = snap.error {
                        dialog_toast.add_toast(adw::Toast::new(&tr_f(
                            "Download stopped: {err}",
                            &[("err", &err)],
                        )));
                    } else {
                        refresh_installed();
                        dialog_toast.add_toast(adw::Toast::new(&tr_f(
                            "Downloaded \"{name}\"",
                            &[("name", &display_name)],
                        )));
                    }
                }
                break;
            }
        }
    });
}

/// Restore any active in-process downloads (after reopening Settings) and any paused downloads
/// from a previous app session whose `.gguf.part` files still exist on disk.
pub fn restore_pending_downloads(
    tracker: &DownloadTracker,
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
) {
    let active_entries: Vec<Arc<Mutex<ActiveDownloadEntry>>> = ACTIVE_DOWNLOADS
        .lock()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default();

    for entry_arc in active_entries {
        attach_entry_to_tracker(
            entry_arc,
            db,
            tracker,
            dialog,
            refresh_installed,
            None,
            None,
            None,
            None,
        );
    }

    let db_bg = db.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<Vec<(PendingDownload, u64)>>();
    runtime().spawn_blocking(move || {
        let pending = db_bg.list_pending_downloads().unwrap_or_default();
        let mut restorable = Vec::new();
        for item in pending {
            let already_active = ACTIVE_DOWNLOADS
                .lock()
                .map(|m| m.contains_key(&(item.repo_id.clone(), item.filename.clone())))
                .unwrap_or(false);
            if already_active {
                continue;
            }
            let safe_repo = item.repo_id.replace('/', "--");
            let repo_dir = managed_models_dir().join(&safe_repo);
            let main_part = repo_dir.join(&item.filename).with_extension("gguf.part");
            let main_done = repo_dir.join(&item.filename);
            let mm_part = item
                .mmproj_filename
                .as_deref()
                .map(|m| repo_dir.join(m).with_extension("gguf.part"));
            let mm_done = item.mmproj_filename.as_deref().map(|m| repo_dir.join(m));

            let has_part = main_part.exists() || mm_part.as_ref().is_some_and(|p| p.exists());
            if !has_part {
                let _ = db_bg.remove_pending_download(&item.repo_id, &item.filename);
                continue;
            }
            let part_bytes = |part: &std::path::Path, done: &std::path::Path| {
                std::fs::metadata(part)
                    .or_else(|_| std::fs::metadata(done))
                    .map(|m| m.len())
                    .unwrap_or(0)
            };
            let existing = part_bytes(&main_part, &main_done)
                + mm_part
                    .as_ref()
                    .zip(mm_done.as_ref())
                    .map(|(p, d)| part_bytes(p, d))
                    .unwrap_or(0);
            restorable.push((item, existing));
        }
        let _ = tx.send(restorable);
    });

    let tracker = tracker.clone();
    let db = db.clone();
    let dialog = dialog.clone();
    let refresh_installed = refresh_installed.clone();
    glib::MainContext::default().spawn_local(async move {
        let Ok(items) = rx.await else {
            return;
        };
        for (item, existing_bytes) in items {
            let key = (item.repo_id.clone(), item.filename.clone());
            let entry_arc = {
                let Ok(mut map) = ACTIVE_DOWNLOADS.lock() else {
                    continue;
                };
                if map.contains_key(&key) {
                    continue;
                }
                let (btx, _) = tokio::sync::broadcast::channel(32);
                let arc = Arc::new(Mutex::new(ActiveDownloadEntry {
                    repo_id: item.repo_id.clone(),
                    filename: item.filename.clone(),
                    mmproj_filename: item.mmproj_filename.clone(),
                    display_name: item.display_name.clone(),
                    done: existing_bytes,
                    total: item.total_bytes,
                    is_paused: true,
                    is_cancelled: false,
                    stop_flag: Arc::new(AtomicBool::new(false)),
                    tx: btx,
                }));
                map.insert(key, arc.clone());
                arc
            };
            attach_entry_to_tracker(
                entry_arc,
                &db,
                &tracker,
                &dialog,
                &refresh_installed,
                None,
                None,
                None,
                None,
            );
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn start_hf_download(
    btn: &gtk::Button,
    controls: &DownloadControls,
    on_pause_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_cancel_handler: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    repo_id: &str,
    filename: &str,
    mmproj_name: Option<&str>,
    db: &Database,
    dialog: &adw::PreferencesDialog,
    refresh_installed: &Rc<dyn Fn()>,
    tracker: &DownloadTracker,
) {
    btn.set_visible(false);
    controls.reset_active();

    let display_name = friendly_model_name(filename);
    let key = (repo_id.to_string(), filename.to_string());
    let (entry_arc, is_new) = {
        let Ok(mut map) = ACTIVE_DOWNLOADS.lock() else {
            return;
        };
        if let Some(existing) = map.get(&key) {
            (existing.clone(), false)
        } else {
            let (btx, _) = tokio::sync::broadcast::channel(32);
            let arc = Arc::new(Mutex::new(ActiveDownloadEntry {
                repo_id: repo_id.to_string(),
                filename: filename.to_string(),
                mmproj_filename: mmproj_name.map(str::to_string),
                display_name: display_name.clone(),
                done: 0,
                total: None,
                is_paused: false,
                is_cancelled: false,
                stop_flag: Arc::new(AtomicBool::new(false)),
                tx: btx,
            }));
            map.insert(key, arc.clone());
            (arc, true)
        }
    };

    if is_new {
        let db_pend = db.clone();
        let pend = PendingDownload {
            repo_id: repo_id.to_string(),
            filename: filename.to_string(),
            mmproj_filename: mmproj_name.map(str::to_string),
            display_name,
            total_bytes: None,
        };
        runtime().spawn_blocking(move || {
            let _ = db_pend.upsert_pending_download(pend);
        });
    }

    attach_entry_to_tracker(
        entry_arc.clone(),
        db,
        tracker,
        dialog,
        refresh_installed,
        Some(btn),
        Some(controls),
        Some(on_pause_handler),
        Some(on_cancel_handler),
    );

    if is_new {
        launch_entry_task(entry_arc, db.clone());
    }
}

fn format_compact_count(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if m < 10.0 {
            let s = format!("{m:.1}");
            format!("{}M", s.strip_suffix(".0").unwrap_or(&s))
        } else {
            format!("{}M", m.round() as u64)
        }
    } else if n >= 1_000 {
        let k = n as f64 / 1_000.0;
        if k < 10.0 {
            let s = format!("{k:.1}");
            format!("{}K", s.strip_suffix(".0").unwrap_or(&s))
        } else {
            format!("{}K", k.round() as u64)
        }
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_compact_download_and_like_counts() {
        assert_eq!(format_compact_count(842), "842");
        assert_eq!(format_compact_count(1_000), "1K");
        assert_eq!(format_compact_count(1_500), "1.5K");
        assert_eq!(format_compact_count(12_400), "12K");
        assert_eq!(format_compact_count(5_000_000), "5M");
        assert_eq!(format_compact_count(1_200_000), "1.2M");
    }
}
