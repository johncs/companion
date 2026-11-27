//! Pending attachment chip state, file picker, and chip-strip rendering for the composer.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::gio;
use relm4::prelude::*;

use crate::core::types::StagedAttachment;
use crate::format::attachments::{AttachmentKind, classify_path, process_attachment_file};
use crate::i18n::gettext;
use crate::ui::widgets::{build_document_tile, build_image_thumbnail_button, spawn_blocking_ui};

use super::{ComposerModel, ComposerMsg, ComposerOutput};

#[derive(Clone, Debug)]
pub(super) struct PendingChip {
    pub(super) temp_id: String,
    pub(super) file_name: String,
    pub(super) is_loading: bool,
    pub(super) record: Option<StagedAttachment>,
}

#[derive(Default, Clone)]
pub(super) struct AttachmentQueue {
    chips: Vec<PendingChip>,
}

impl AttachmentQueue {
    pub(super) fn is_empty(&self) -> bool {
        self.chips.is_empty()
    }

    pub(super) fn has_loading(&self) -> bool {
        self.chips.iter().any(|c| c.is_loading)
    }

    pub(super) fn push_loading(&mut self, temp_id: String, file_name: String) {
        self.chips.push(PendingChip {
            temp_id,
            file_name,
            is_loading: true,
            record: None,
        });
    }

    pub(super) fn mark_loaded(&mut self, temp_id: &str, record: StagedAttachment) {
        if let Some(chip) = self.chips.iter_mut().find(|c| c.temp_id == temp_id) {
            chip.is_loading = false;
            chip.record = Some(record);
        }
    }

    pub(super) fn remove(&mut self, temp_id: &str) {
        self.chips.retain(|c| c.temp_id != temp_id);
    }

    pub(super) fn take_ready(&mut self) -> Vec<StagedAttachment> {
        let ready = self.chips.iter().filter_map(|c| c.record.clone()).collect();
        self.chips.clear();
        ready
    }

    pub(super) fn chips(&self) -> &[PendingChip] {
        &self.chips
    }
}

pub(super) fn open_attach_file_dialog(
    anchor: &gtk::TextView,
    sender: &ComponentSender<ComposerModel>,
) {
    let file_dialog = gtk::FileDialog::builder()
        .title(gettext("Attach Files"))
        .modal(true)
        .build();
    let window = anchor.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let sender_clone = sender.clone();

    file_dialog.open_multiple(window.as_ref(), None::<&gio::Cancellable>, move |res| {
        if let Ok(list) = res {
            let mut paths = Vec::new();
            for idx in 0..list.n_items() {
                if let Some(file) = list.item(idx).and_downcast::<gio::File>()
                    && let Some(p) = file.path()
                {
                    paths.push(p);
                }
            }
            if !paths.is_empty() {
                sender_clone.input(ComposerMsg::FilesDropped(paths));
            }
        }
    });
}

pub(super) fn classify_dropped_paths(
    paths: Vec<PathBuf>,
    rt_handle: &tokio::runtime::Handle,
    sender: &ComponentSender<ComposerModel>,
) {
    let sender_clone = sender.clone();
    spawn_blocking_ui(
        rt_handle,
        move || {
            let mut dirs = Vec::new();
            let mut files = Vec::new();
            for path in paths {
                if path.is_dir() {
                    dirs.push(crate::core::paths::canonicalize_or_self(path));
                } else if path.is_file() {
                    files.push(path);
                }
            }
            (dirs, files)
        },
        move |(dirs, files)| {
            if !dirs.is_empty() || !files.is_empty() {
                sender_clone.input(ComposerMsg::PathsClassified { dirs, files });
            }
        },
    );
}

pub(super) fn enqueue_classified_files(
    queue: &mut AttachmentQueue,
    paths: Vec<PathBuf>,
    has_mmproj: bool,
    chips_box: &gtk::Box,
    rt_handle: &tokio::runtime::Handle,
    sender: &ComponentSender<ComposerModel>,
) {
    for path in paths {
        if classify_path(&path) == AttachmentKind::Image && !has_mmproj {
            let _ = sender.output(ComposerOutput::ShowToast(gettext(
                "Active AI model has no vision projector (mmproj) configured in Settings → AI Models",
            )));
            continue;
        }

        let temp_id = uuid::Uuid::new_v4().to_string();
        let fallback = gettext("file");
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&fallback)
            .to_string();
        queue.push_loading(temp_id.clone(), file_name);
        rebuild_chips_box(chips_box, queue, sender);

        let sender_clone = sender.clone();
        spawn_blocking_ui(
            rt_handle,
            move || process_attachment_file(&path).map_err(|e| e.to_string()),
            move |result| {
                sender_clone.input(ComposerMsg::AttachmentLoaded { temp_id, result });
            },
        );
    }
}

pub(super) fn rebuild_chips_box(
    chips_box: &gtk::Box,
    queue: &AttachmentQueue,
    sender: &ComponentSender<ComposerModel>,
) {
    while let Some(child) = chips_box.first_child() {
        chips_box.remove(&child);
    }

    if queue.is_empty() {
        chips_box.set_visible(false);
        return;
    }
    chips_box.set_visible(true);

    for chip in queue.chips() {
        if chip.is_loading {
            chips_box.append(&build_loading_chip(&chip.file_name));
        } else if let Some(rec) = &chip.record {
            chips_box.append(&build_ready_chip(rec, &chip.temp_id, sender));
        }
    }
}

fn build_loading_chip(file_name: &str) -> gtk::Box {
    let pill = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .tooltip_text(file_name)
        .css_classes(["attachment-chip"])
        .build();
    let spinner = gtk::Spinner::new();
    spinner.start();
    pill.append(&spinner);
    pill.append(
        &gtk::Label::builder()
            .label(file_name)
            .css_classes(["caption"])
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(20)
            .build(),
    );
    pill
}

fn build_ready_chip(
    rec: &StagedAttachment,
    temp_id: &str,
    sender: &ComponentSender<ComposerModel>,
) -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.set_halign(gtk::Align::Start);
    overlay.set_valign(gtk::Align::Center);
    overlay.set_vexpand(false);
    if rec.mime_type.starts_with("image/") {
        overlay.set_child(Some(&build_image_thumbnail_button(
            &rec.file_path,
            &rec.file_name,
            84,
        )));
    } else {
        overlay.set_child(Some(&build_document_tile(
            &rec.file_name,
            &rec.mime_type,
            84,
        )));
    }
    let remove_btn = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(gettext("Remove attachment"))
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .css_classes(["thumb-remove-btn"])
        .build();
    let (tid, sender_clone) = (temp_id.to_string(), sender.clone());
    remove_btn
        .connect_clicked(move |_| sender_clone.input(ComposerMsg::RemoveAttachment(tid.clone())));
    overlay.add_overlay(&remove_btn);
    overlay
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_record(name: &str) -> StagedAttachment {
        StagedAttachment {
            file_name: name.to_string(),
            file_path: format!("/tmp/{name}"),
            mime_type: "text/plain".to_string(),
            byte_size: 2,
            extracted_text: Some("hi".to_string()),
        }
    }

    #[test]
    fn attachment_queue_tracks_loading_ready_and_removal() {
        let mut q = AttachmentQueue::default();
        assert!(q.is_empty());
        assert!(!q.has_loading());

        q.push_loading("t1".into(), "a.txt".into());
        q.push_loading("t2".into(), "b.txt".into());
        assert!(q.has_loading());

        q.mark_loaded("t1", dummy_record("a.txt"));
        assert!(q.has_loading());

        q.remove("t2");
        assert!(!q.has_loading());

        let ready = q.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].file_name, "a.txt");
        assert!(q.is_empty());
    }
}
