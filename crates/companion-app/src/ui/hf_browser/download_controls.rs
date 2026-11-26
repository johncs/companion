//! Reusable inline download controls and active-download list tracker.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tokio::sync::broadcast;

use crate::i18n::{gettext, tr_f};
use crate::models::download_manager::{
    DownloadKey, DownloadManager, DownloadSnapshot, DownloadStatus,
};
use crate::ui::widgets::present_destructive_confirm;

#[derive(Clone)]
struct BoundDownload {
    manager: DownloadManager,
    key: DownloadKey,
    display_name: String,
    dialog: adw::PreferencesDialog,
}

/// Reusable inline download controls: percentage label, progress bar, pause/resume, and cancel.
#[derive(Clone)]
pub struct DownloadControls {
    pub container: gtk::Box,
    pub pct_label: gtk::Label,
    pub progress: gtk::ProgressBar,
    pub pause_btn: gtk::Button,
    pub cancel_btn: gtk::Button,
    bound: Rc<RefCell<Option<BoundDownload>>>,
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

        let pause_btn =
            build_circle_icon_btn("media-playback-pause-symbolic", &gettext("Pause download"));
        let cancel_btn =
            build_circle_icon_btn("process-stop-symbolic", &gettext("Cancel download"));

        container.append(&pct_label);
        container.append(&progress);
        container.append(&pause_btn);
        container.append(&cancel_btn);

        let bound: Rc<RefCell<Option<BoundDownload>>> = Rc::new(RefCell::new(None));
        wire_pause_button(&pause_btn, &bound);
        wire_cancel_button(&cancel_btn, &bound);

        Self {
            container,
            pct_label,
            progress,
            pause_btn,
            cancel_btn,
            bound,
        }
    }

    pub fn bind_download(
        &self,
        manager: &DownloadManager,
        key: DownloadKey,
        display_name: String,
        dialog: &adw::PreferencesDialog,
    ) {
        *self.bound.borrow_mut() = Some(BoundDownload {
            manager: manager.clone(),
            key,
            display_name,
            dialog: dialog.clone(),
        });
    }

    pub fn unbind(&self) {
        *self.bound.borrow_mut() = None;
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
    active_keys: Rc<RefCell<HashSet<DownloadKey>>>,
}

impl DownloadTracker {
    pub fn new(group: adw::PreferencesGroup) -> Self {
        Self {
            group,
            active_count: Rc::new(RefCell::new(0)),
            active_keys: Rc::new(RefCell::new(HashSet::new())),
        }
    }

    pub fn is_repo_active(&self, mgr: &DownloadManager, repo_id: &str) -> bool {
        self.active_keys.borrow().iter().any(|(r, _)| r == repo_id) || mgr.is_repo_active(repo_id)
    }

    /// Add an active download row and return `(DownloadControls, remove_callback)`.
    pub(crate) fn track(
        &self,
        manager: &DownloadManager,
        key: &DownloadKey,
        title: &str,
        dialog: &adw::PreferencesDialog,
    ) -> (DownloadControls, Rc<dyn Fn()>) {
        let escaped_title = crate::format::markdown::escape_pango_markup(title);
        let row = adw::ActionRow::builder().title(&escaped_title).build();
        let controls = DownloadControls::new();
        controls.reset_active();
        controls.bind_download(manager, key.clone(), title.to_string(), dialog);

        row.add_suffix(&controls.container);
        self.group.add(&row);

        *self.active_count.borrow_mut() += 1;
        self.active_keys.borrow_mut().insert(key.clone());
        self.group.set_visible(true);

        let group = self.group.clone();
        let active_count = self.active_count.clone();
        let active_keys = self.active_keys.clone();
        let key_owned = key.clone();
        let removed = Rc::new(RefCell::new(false));
        let controls_finish = controls.clone();
        let finish: Rc<dyn Fn()> = Rc::new(move || {
            if *removed.borrow() {
                return;
            }
            *removed.borrow_mut() = true;
            controls_finish.unbind();
            group.remove(&row);
            active_keys.borrow_mut().remove(&key_owned);
            let mut count = active_count.borrow_mut();
            *count = count.saturating_sub(1);
            if *count == 0 {
                group.set_visible(false);
            }
        });

        (controls, finish)
    }
}

/// Drive one or two [`DownloadControls`] from a [`DownloadManager`] snapshot stream until terminal status
/// or until `dialog` is closed.
pub fn subscribe_download_lifecycle<F>(
    initial: Option<DownloadSnapshot>,
    rx: broadcast::Receiver<DownloadSnapshot>,
    primary: DownloadControls,
    secondary: Option<DownloadControls>,
    dialog: Option<&adw::PreferencesDialog>,
    on_terminal: F,
) where
    F: FnOnce(DownloadStatus) + 'static,
{
    if let Some(init) = initial {
        apply_download_snapshot(&primary, secondary.as_ref(), &init);
    }
    let closed = Rc::new(std::cell::Cell::new(false));
    if let Some(d) = dialog {
        let closed = closed.clone();
        d.connect_closed(move |_| closed.set(true));
    }
    glib::MainContext::default().spawn_local(async move {
        let status = drive_snapshot_stream(
            rx,
            || closed.get(),
            |snap| apply_download_snapshot(&primary, secondary.as_ref(), snap),
        )
        .await;
        if let Some(status) = status {
            if let Some(sec) = &secondary {
                sec.unbind();
            }
            on_terminal(status);
        }
    });
}

async fn drive_snapshot_stream<C, S>(
    mut rx: broadcast::Receiver<DownloadSnapshot>,
    is_closed: C,
    mut on_snapshot: S,
) -> Option<DownloadStatus>
where
    C: Fn() -> bool,
    S: FnMut(&DownloadSnapshot),
{
    loop {
        if is_closed() {
            return None;
        }
        let snap = match rx.recv().await {
            Ok(snap) => snap,
            // Broadcast buffer overflowed: skip stale snapshots and keep following the stream
            // so progress bars never freeze and terminal states are never missed.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        if is_closed() {
            return None;
        }
        on_snapshot(&snap);
        if snap.status.is_terminal() {
            return Some(snap.status);
        }
    }
}

fn apply_download_snapshot(
    primary: &DownloadControls,
    secondary: Option<&DownloadControls>,
    snap: &DownloadSnapshot,
) {
    primary.set_progress(snap.done, snap.total);
    primary.set_paused(snap.status.is_paused());
    if let Some(sec) = secondary {
        sec.set_progress(snap.done, snap.total);
        sec.set_paused(snap.status.is_paused());
    }
}

fn build_circle_icon_btn(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build()
}

fn wire_pause_button(pause_btn: &gtk::Button, bound: &Rc<RefCell<Option<BoundDownload>>>) {
    let bound_ref = bound.clone();
    pause_btn.connect_clicked(move |_| {
        let Some(b) = bound_ref.borrow().clone() else {
            return;
        };
        let is_paused = b
            .manager
            .active_downloads()
            .into_iter()
            .find(|s| s.key == b.key)
            .is_some_and(|s| s.snapshot.status.is_paused());
        if is_paused {
            b.manager.resume(&b.key);
        } else {
            b.manager.pause(&b.key);
        }
    });
}

fn wire_cancel_button(cancel_btn: &gtk::Button, bound: &Rc<RefCell<Option<BoundDownload>>>) {
    let bound_ref = bound.clone();
    cancel_btn.connect_clicked(move |_| {
        let Some(b) = bound_ref.borrow().clone() else {
            return;
        };
        let body = tr_f(
            "Cancel downloading \"{name}\" and delete the partial download from disk?",
            &[("name", &b.display_name)],
        );
        let (mgr, key) = (b.manager.clone(), b.key.clone());
        present_destructive_confirm(
            &b.dialog,
            &gettext("Cancel Download?"),
            &body,
            &gettext("Cancel Download"),
            move || mgr.cancel(&key),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snap(done: u64, status: DownloadStatus) -> DownloadSnapshot {
        DownloadSnapshot {
            done,
            total: Some(100),
            status,
        }
    }

    #[tokio::test]
    async fn download_stream_recovers_from_lag_and_reaches_terminal() {
        let (tx, rx) = broadcast::channel(1);
        let _ = tx.send(make_snap(10, DownloadStatus::Active));
        let _ = tx.send(make_snap(50, DownloadStatus::Active));
        let _ = tx.send(make_snap(100, DownloadStatus::Completed));

        let mut seen = Vec::new();
        let terminal = drive_snapshot_stream(rx, || false, |s| seen.push(s.done)).await;
        assert_eq!(terminal, Some(DownloadStatus::Completed));
        assert_eq!(seen, vec![100]);
    }
}
