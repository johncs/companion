//! Binding message data, role styling, and attachment tiles onto recycled row widgets.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::core::types::MessageRole;
use crate::i18n::gettext;
use crate::storage::AttachmentRecord;
use crate::ui::message_object::{MessageEntry, MessageObject};
use crate::ui::widgets::{build_document_tile, build_image_thumbnail_button};

use super::RowWidgets;
use super::blocks::update_blocks_box;

pub(super) fn bind_entry(
    widgets: &RowWidgets,
    msg_obj: &MessageObject,
    assistant_name: &Rc<RefCell<String>>,
    user_name: &Rc<RefCell<Option<String>>>,
) {
    if let Ok(mut active) = widgets.active_msg_obj.try_borrow_mut() {
        *active = Some(msg_obj.clone());
    }
    msg_obj.try_with_entry(|entry| {
        let Ok(a_name) = assistant_name.try_borrow() else {
            return;
        };
        let Ok(u_name) = user_name.try_borrow() else {
            return;
        };
        bind_entry_inner(widgets, entry, &a_name, u_name.as_deref());
    });
}

fn bind_entry_inner(
    widgets: &RowWidgets,
    entry: &MessageEntry,
    assistant_name: &str,
    user_name: Option<&str>,
) {
    let is_user = entry.role == MessageRole::User;
    let same_message = *widgets.rendered_message_id.borrow() == entry.render_key;
    // Always refresh so live personalization name changes update visible rows.
    apply_role_style(widgets, is_user, assistant_name, user_name);
    sync_streaming_and_actions(widgets, entry, is_user);

    if !same_message {
        *widgets.rendered_message_id.borrow_mut() = entry.render_key.clone();
        widgets.rendered_blocks.borrow_mut().clear();
        bind_attachments(&widgets.attachments_box, &entry.attachments);
    }

    widgets
        .card
        .set_visible(!entry.blocks.is_empty() || !entry.attachments.is_empty());
    let is_dark = adw::StyleManager::default().is_dark();
    update_blocks_box(
        &widgets.blocks_box,
        &widgets.rendered_blocks,
        &widgets.rendered_is_dark,
        &entry.blocks,
        is_dark,
        entry.is_streaming,
    );
}

fn sync_streaming_and_actions(widgets: &RowWidgets, entry: &MessageEntry, is_user: bool) {
    widgets.avatar_icon.set_visible(!entry.is_streaming);
    widgets.streaming_spinner.set_visible(entry.is_streaming);
    widgets.streaming_spinner.set_spinning(entry.is_streaming);
    if entry.is_streaming {
        widgets
            .streaming_spinner
            .set_tooltip_text(entry.status_text.as_deref());
    }
    let has_text = !entry.is_streaming && !entry.content.trim().is_empty();
    if !entry.is_streaming {
        *widgets.copy_text.borrow_mut() = entry.content.clone();
        *widgets.edit_target.borrow_mut() = (entry.id.clone(), entry.content.clone());
    }
    widgets.copy_btn.set_visible(has_text);
    widgets.edit_btn.set_visible(is_user && !entry.is_streaming);
    widgets
        .delete_btn
        .set_visible(is_user && !entry.is_streaming);
    widgets
        .actions_box
        .set_visible(has_text || (is_user && !entry.is_streaming));
}

fn apply_role_style(
    widgets: &RowWidgets,
    is_user: bool,
    assistant_name: &str,
    user_name: Option<&str>,
) {
    if is_user {
        widgets.outer.set_halign(gtk::Align::End);
        widgets.card.remove_css_class("assistant-bubble");
        widgets.card.add_css_class("user-bubble");
        widgets.card.set_margin_start(72);
        widgets.card.set_margin_end(0);
        widgets.header_row.set_visible(true);
        widgets.header_row.set_halign(gtk::Align::End);
        widgets
            .avatar_icon
            .set_icon_name(Some("avatar-default-symbolic"));
        widgets
            .role_label
            .set_text(user_name.unwrap_or(&gettext("You")));
        widgets.actions_box.set_halign(gtk::Align::End);
        widgets.actions_box.set_margin_start(0);
    } else {
        widgets.outer.set_halign(gtk::Align::Fill);
        widgets.card.remove_css_class("user-bubble");
        widgets.card.add_css_class("assistant-bubble");
        widgets.card.set_margin_start(0);
        widgets.card.set_margin_end(0);
        widgets.header_row.set_visible(true);
        widgets.header_row.set_halign(gtk::Align::Fill);
        widgets
            .avatar_icon
            .set_icon_name(Some(crate::ui::icons::MODEL));
        widgets.actions_box.set_halign(gtk::Align::Start);
        widgets.actions_box.set_margin_start(12);
        widgets.role_label.set_text(assistant_name);
    }
}

fn bind_attachments(attachments_box: &gtk::FlowBox, attachments: &[AttachmentRecord]) {
    while let Some(child) = attachments_box.first_child() {
        attachments_box.remove(&child);
    }
    if attachments.is_empty() {
        attachments_box.set_visible(false);
        return;
    }

    attachments_box.set_visible(true);
    for att in attachments {
        if att.mime_type.starts_with("image/") {
            let thumb_btn = build_image_thumbnail_button(&att.file_path, &att.file_name, 168);
            attachments_box.insert(&thumb_btn, -1);
        } else {
            let tile = build_document_tile(&att.file_name, &att.mime_type, 72);
            attachments_box.insert(&tile, -1);
        }
    }
}
