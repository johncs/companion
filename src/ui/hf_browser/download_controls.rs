//! Reusable inline download controls and active-download list tracker.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::gettext;
use crate::models::download_manager::DownloadManager;

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

    pub fn is_repo_active(&self, mgr: &DownloadManager, repo_id: &str) -> bool {
        self.active_repos.borrow().contains(repo_id) || mgr.is_repo_active(repo_id)
    }

    /// Add an active download row and return `(DownloadControls, remove_callback)`.
    pub(super) fn track(
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
