//! Active model detail rows syncing and installed model list row construction.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::{gettext, tr_f};
use crate::models::gguf::format_context_label;
use crate::models::{format_bytes, managed_models_dir};
use crate::storage::{Database, ModelRecord};
use crate::ui::widgets::present_destructive_confirm;

use super::inventory::{InstalledModelInfo, delete_managed_model_files};

#[derive(Clone)]
pub(super) struct ActiveDetailRows {
    pub(super) config_btn: gtk::Button,
    pub(super) details_expander: adw::ExpanderRow,
    pub(super) file_row: adw::ActionRow,
    pub(super) size_row: adw::ActionRow,
    pub(super) quant_row: adw::ActionRow,
    pub(super) ctx_row: adw::ActionRow,
    pub(super) speed_row: adw::ActionRow,
    pub(super) text_row: adw::ActionRow,
    pub(super) thinking_row: adw::ActionRow,
    pub(super) vision_row: adw::ActionRow,
    pub(super) vision_action_btn: gtk::Button,
    pub(super) audio_row: adw::ActionRow,
}

pub(super) fn build_property_row(title: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_css_class("property");
    row
}

pub(super) fn sync_active_model_details(info: Option<&InstalledModelInfo>, rows: &ActiveDetailRows) {
    let Some(info) = info else {
        rows.config_btn.set_visible(false);
        rows.details_expander.set_visible(false);
        return;
    };

    let file_name = info
        .record
        .file_path
        .as_deref()
        .map(|p| {
            Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p)
        })
        .unwrap_or("—");
    let file_sub = if info.file_size.is_some() {
        file_name.to_string()
    } else {
        format!("{file_name} ({})", gettext("Missing file"))
    };
    rows.file_row
        .set_subtitle(&gtk::glib::markup_escape_text(&file_sub));
    rows.file_row
        .set_tooltip_text(info.record.file_path.as_deref());

    let size_sub = info
        .file_size
        .map(format_bytes)
        .unwrap_or_else(|| gettext("Missing file"));
    rows.size_row
        .set_subtitle(&gtk::glib::markup_escape_text(&size_sub));

    let quant_sub = info.record.quantization.as_deref().unwrap_or("—");
    rows.quant_row
        .set_subtitle(&gtk::glib::markup_escape_text(quant_sub));

    let ctx_sub = info
        .gguf
        .max_context
        .map(format_context_label)
        .unwrap_or_else(|| "—".to_string());
    rows.ctx_row
        .set_subtitle(&gtk::glib::markup_escape_text(&ctx_sub));

    let speed_sub = if let Some(tps) = info.record.avg_tokens_per_sec {
        tr_f("{tps} tok/s", &[("tps", &format!("{tps:.1}"))])
    } else {
        gettext("Unavailable")
    };
    rows.speed_row
        .set_subtitle(&gtk::glib::markup_escape_text(&speed_sub));

    rows.text_row
        .set_subtitle(&gtk::glib::markup_escape_text(&gettext("Active")));

    let thinking_supported = info.record.supports_thinking || info.gguf.supports_thinking;
    let thinking_sub = if thinking_supported {
        gettext("Active")
    } else {
        gettext("Unavailable")
    };
    rows.thinking_row
        .set_subtitle(&gtk::glib::markup_escape_text(&thinking_sub));

    let mut vision_action_label: Option<String> = None;
    let vision_sub = if info.record.mmproj_path.is_some() {
        if info.mmproj_exists {
            if info.mmproj_gguf.as_ref().is_some_and(|m| !m.is_clip) {
                vision_action_label = Some(if info.record.hf_repo.is_some() {
                    gettext("Install Vision")
                } else {
                    gettext("Enable Vision")
                });
                gettext("Invalid projector file")
            } else {
                gettext("Active")
            }
        } else {
            vision_action_label = Some(if info.record.hf_repo.is_some() {
                gettext("Install Vision")
            } else {
                gettext("Enable Vision")
            });
            gettext("Missing file")
        }
    } else if info.gguf.supports_vision {
        vision_action_label = Some(if info.record.hf_repo.is_some() {
            gettext("Install Vision")
        } else {
            gettext("Enable Vision")
        });
        if info.record.hf_repo.is_some() {
            gettext("Available (Requires mmproj)")
        } else {
            gettext("Available (Requires external mmproj file)")
        }
    } else {
        gettext("Unavailable")
    };
    rows.vision_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vision_sub));
    if let Some(label) = vision_action_label {
        rows.vision_action_btn.set_label(&label);
        rows.vision_action_btn.set_visible(true);
    } else {
        rows.vision_action_btn.set_visible(false);
    }

    let audio_supported = info.gguf.supports_audio
        || info.mmproj_gguf.as_ref().is_some_and(|m| m.supports_audio);
    let audio_sub = if audio_supported {
        gettext("Available (Coming soon)")
    } else {
        gettext("Unavailable")
    };
    rows.audio_row
        .set_subtitle(&gtk::glib::markup_escape_text(&audio_sub));

    rows.config_btn.set_visible(true);
    rows.details_expander.set_visible(true);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_installed_model_row(
    model: &ModelRecord,
    file_size: Option<u64>,
    is_active: bool,
    db: &Database,
    installed_group: &adw::PreferencesGroup,
    active_id_state: &Rc<RefCell<Option<String>>>,
    on_active_changed: &Rc<dyn Fn(Option<String>)>,
    dialog: &adw::PreferencesDialog,
    refresh_weak: &Rc<RefCell<Option<Rc<dyn Fn()>>>>,
) -> (adw::ActionRow, gtk::Label) {
    let size_str = file_size
        .map(format_bytes)
        .unwrap_or_else(|| gettext("Missing file"));

    let row = adw::ActionRow::builder()
        .title(&model.display_name)
        .subtitle(&size_str)
        .build();

    let active_badge = gtk::Label::builder()
        .label(&gettext("Active"))
        .valign(gtk::Align::Center)
        .margin_end(8)
        .css_classes(["dim-label", "caption-heading"])
        .visible(is_active)
        .build();
    row.add_suffix(&active_badge);

    let delete_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Remove model"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    {
        let db = db.clone();
        let installed_group = installed_group.clone();
        let active_id_state = active_id_state.clone();
        let on_active_changed = on_active_changed.clone();
        let dialog = dialog.clone();
        let refresh_weak = refresh_weak.clone();
        let model_clone = model.clone();

        delete_btn.connect_clicked(move |_| {
            let managed_dir = managed_models_dir();
            let is_managed_file = model_clone
                .file_path
                .as_deref()
                .is_some_and(|p| Path::new(p).starts_with(&managed_dir));

            let body = if is_managed_file {
                tr_f(
                    "Delete \"{name}\" and remove its downloaded files from disk?",
                    &[("name", &model_clone.display_name)],
                )
            } else {
                tr_f(
                    "Remove \"{name}\" from Companion? (Your imported .gguf file on disk will not be deleted.)",
                    &[("name", &model_clone.display_name)],
                )
            };

            let db = db.clone();
            let active_id_state = active_id_state.clone();
            let on_active_changed = on_active_changed.clone();
            let dialog = dialog.clone();
            let refresh_weak = refresh_weak.clone();
            let model_inner = model_clone.clone();

            present_destructive_confirm(
                &installed_group,
                &gettext("Remove AI Model?"),
                &body,
                &gettext("Remove"),
                move || {
                    let others: Vec<ModelRecord> = db
                        .list_models()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|m| m.id != model_inner.id)
                        .collect();
                    delete_managed_model_files(&managed_dir, &model_inner, &others);
                    let _ = db.delete_model(&model_inner.id);
                    if active_id_state.borrow().as_deref() == Some(&model_inner.id) {
                        *active_id_state.borrow_mut() = None;
                        on_active_changed(None);
                    }
                    dialog.add_toast(adw::Toast::new(&tr_f(
                        "Removed \"{name}\"",
                        &[("name", &model_inner.display_name)],
                    )));
                    if let Some(f) = refresh_weak.borrow().as_ref() {
                        f();
                    }
                },
            );
        });
    }

    row.add_suffix(&delete_btn);
    (row, active_badge)
}
