//! Recycled `gtk::ListView` row factory for chat messages.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::markdown::MarkdownBlock;
use crate::i18n::gettext;
use crate::ui::message_dialogs::{
    copy_text_with_feedback, present_edit_message_dialog, present_message_info_dialog,
};
use crate::ui::message_object::MessageObject;
use crate::ui::widgets::{
    build_document_tile, build_image_thumbnail, clear_box, present_destructive_confirm,
    present_image_lightbox,
};

#[derive(Clone)]
struct RowWidgets {
    outer: gtk::Box,
    card: gtk::Box,
    header_row: gtk::Box,
    avatar_icon: gtk::Image,
    role_label: gtk::Label,
    attachments_box: gtk::FlowBox,
    active_msg_obj: Rc<RefCell<Option<MessageObject>>>,
    blocks_box: gtk::Box,
    streaming_spinner: gtk::Spinner,
    actions_box: gtk::Box,
    edit_btn: gtk::Button,
    delete_btn: gtk::Button,
    edit_target: Rc<RefCell<(String, String)>>,
    copy_btn: gtk::Button,
    copy_text: Rc<RefCell<String>>,
    rendered_message_id: Rc<RefCell<String>>,
    rendered_blocks: Rc<RefCell<Vec<MarkdownBlock>>>,
}

type SignalSlot = Rc<RefCell<Option<(MessageObject, glib::SignalHandlerId)>>>;

const ROW_WIDGETS_KEY: &str = "row-widgets";
const SIGNAL_SLOT_KEY: &str = "signal-slot";

fn get_row_widgets(clamp: &adw::Clamp) -> Option<RowWidgets> {
    unsafe { clamp.data::<RowWidgets>(ROW_WIDGETS_KEY) }.map(|ptr| unsafe { ptr.as_ref().clone() })
}

fn get_signal_slot(clamp: &adw::Clamp) -> Option<SignalSlot> {
    unsafe { clamp.data::<SignalSlot>(SIGNAL_SLOT_KEY) }.map(|ptr| unsafe { ptr.as_ref().clone() })
}

fn disconnect_slot(slot: &SignalSlot) {
    if let Some((prev_obj, prev_id)) = slot.borrow_mut().take() {
        prev_obj.disconnect(prev_id);
    }
}

/// Build the [`gtk::SignalListItemFactory`] used by the virtualized chat `ListView`.
pub fn build_message_list_factory(
    assistant_name: Rc<RefCell<String>>,
    user_name: Rc<RefCell<Option<String>>>,
    on_edit: Rc<dyn Fn(String, String)>,
    on_delete: Rc<dyn Fn(String)>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_, obj| {
        let list_item = obj
            .downcast_ref::<gtk::ListItem>()
            .expect("ListItem expected");
        list_item.set_activatable(false);
        list_item.set_selectable(false);

        let clamp = adw::Clamp::builder()
            .maximum_size(780)
            .tightening_threshold(560)
            .build();

        let outer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(16)
            .margin_end(16)
            .css_classes(["message-row"])
            .build();

        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .css_classes(["message-card"])
            .build();

        let header_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_bottom(4)
            .build();

        let avatar_icon = gtk::Image::builder()
            .icon_name(crate::ui::icons::MODEL)
            .pixel_size(14)
            .width_request(14)
            .height_request(14)
            .css_classes(["dim-label"])
            .build();

        let streaming_spinner = gtk::Spinner::builder()
            .width_request(14)
            .height_request(14)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .visible(false)
            .spinning(false)
            .build();

        let role_label = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["caption-heading", "dim-label"])
            .build();

        let role_btn_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        role_btn_box.append(&avatar_icon);
        role_btn_box.append(&streaming_spinner);
        role_btn_box.append(&role_label);

        let role_btn = gtk::Button::builder()
            .child(&role_btn_box)
            .tooltip_text(gettext("Message info"))
            .css_classes(["flat", "thinking-chip-btn"])
            .valign(gtk::Align::Center)
            .focus_on_click(false)
            .build();

        let active_msg_obj: Rc<RefCell<Option<MessageObject>>> = Rc::new(RefCell::new(None));
        {
            let msg_obj_ref = active_msg_obj.clone();
            role_btn.connect_clicked(move |btn| {
                if let Some(obj) = msg_obj_ref.borrow().clone() {
                    present_message_info_dialog(btn, obj);
                }
            });
        }

        header_row.append(&role_btn);

        let blocks_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .build();

        let attachments_box = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .halign(gtk::Align::Start)
            .row_spacing(4)
            .column_spacing(6)
            .visible(false)
            .build();

        let actions_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_top(2)
            .css_classes(["message-actions"])
            .build();

        let edit_target = Rc::new(RefCell::new((String::new(), String::new())));
        let edit_btn = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text(gettext("Edit message"))
            .css_classes(["flat", "circular", "message-copy-btn"])
            .focus_on_click(false)
            .visible(false)
            .build();
        {
            let edit_target_ref = edit_target.clone();
            let on_edit_cb = on_edit.clone();
            edit_btn.connect_clicked(move |btn| {
                let (msg_id, content) = edit_target_ref.borrow().clone();
                if !msg_id.is_empty() {
                    present_edit_message_dialog(btn, &msg_id, &content, on_edit_cb.clone());
                }
            });
        }

        let copy_text = Rc::new(RefCell::new(String::new()));
        let copy_btn = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text(gettext("Copy message"))
            .css_classes(["flat", "circular", "message-copy-btn"])
            .focus_on_click(false)
            .build();
        {
            let copy_text_ref = copy_text.clone();
            copy_btn.connect_clicked(move |btn| {
                let text = copy_text_ref.borrow().clone();
                if !text.is_empty() {
                    copy_text_with_feedback(btn, &text);
                }
            });
        }

        let delete_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(gettext("Delete message"))
            .css_classes(["flat", "circular", "message-copy-btn"])
            .focus_on_click(false)
            .visible(false)
            .build();
        {
            let edit_target_ref = edit_target.clone();
            let on_delete_cb = on_delete.clone();
            delete_btn.connect_clicked(move |btn| {
                let (msg_id, _) = edit_target_ref.borrow().clone();
                if !msg_id.is_empty() {
                    let cb = on_delete_cb.clone();
                    present_destructive_confirm(
                        btn,
                        &gettext("Delete Message?"),
                        &gettext("This message and all messages after it in this conversation will be permanently deleted."),
                        &gettext("Delete"),
                        move || {
                            cb(msg_id.clone());
                        },
                    );
                }
            });
        }

        actions_box.append(&edit_btn);
        actions_box.append(&delete_btn);
        actions_box.append(&copy_btn);

        card.append(&attachments_box);
        card.append(&blocks_box);
        outer.append(&header_row);
        outer.append(&card);
        outer.append(&actions_box);
        clamp.set_child(Some(&outer));

        let widgets = RowWidgets {
            outer,
            card,
            header_row,
            avatar_icon,
            role_label,
            attachments_box,
            active_msg_obj,
            blocks_box,
            streaming_spinner,
            actions_box,
            edit_btn,
            delete_btn,
            edit_target,
            copy_btn,
            copy_text,
            rendered_message_id: Rc::new(RefCell::new(String::new())),
            rendered_blocks: Rc::new(RefCell::new(Vec::new())),
        };

        let handler_slot: SignalSlot = Rc::new(RefCell::new(None));

        unsafe {
            clamp.set_data(ROW_WIDGETS_KEY, widgets);
            clamp.set_data(SIGNAL_SLOT_KEY, handler_slot);
        }

        list_item.set_child(Some(&clamp));
    });

    {
        let assistant_name = assistant_name.clone();
        let user_name = user_name.clone();
        factory.connect_bind(move |_, obj| {
            let list_item = obj
                .downcast_ref::<gtk::ListItem>()
                .expect("ListItem expected");
            let Some(msg_obj) = list_item.item().and_downcast::<MessageObject>() else {
                return;
            };
            let Some(clamp) = list_item.child().and_downcast::<adw::Clamp>() else {
                return;
            };
            let Some(widgets) = get_row_widgets(&clamp) else {
                return;
            };

            bind_entry(&widgets, &msg_obj, &assistant_name, &user_name);

            if let Some(slot) = get_signal_slot(&clamp) {
                disconnect_slot(&slot);
                let widgets_clone = widgets.clone();
                let msg_clone = msg_obj.clone();
                let name_clone = assistant_name.clone();
                let uname_clone = user_name.clone();
                let id = msg_obj.connect_local("updated", false, move |_| {
                    bind_entry(&widgets_clone, &msg_clone, &name_clone, &uname_clone);
                    None
                });
                *slot.borrow_mut() = Some((msg_obj, id));
            }
        });
    }

    factory.connect_unbind(|_, obj| {
        let list_item = obj
            .downcast_ref::<gtk::ListItem>()
            .expect("ListItem expected");
        let Some(clamp) = list_item.child().and_downcast::<adw::Clamp>() else {
            return;
        };
        if let Some(slot) = get_signal_slot(&clamp) {
            disconnect_slot(&slot);
        }
        if let Some(widgets) = get_row_widgets(&clamp) {
            widgets.rendered_message_id.borrow_mut().clear();
            widgets.rendered_blocks.borrow_mut().clear();
        }
    });

    factory
}

fn bind_entry(
    widgets: &RowWidgets,
    msg_obj: &MessageObject,
    assistant_name: &Rc<RefCell<String>>,
    user_name: &Rc<RefCell<Option<String>>>,
) {
    *widgets.active_msg_obj.borrow_mut() = Some(msg_obj.clone());
    msg_obj.with_entry(|entry| {
        bind_entry_inner(
            widgets,
            entry,
            &assistant_name.borrow(),
            user_name.borrow().as_deref(),
        );
    });
}

fn bind_entry_inner(
    widgets: &RowWidgets,
    entry: &crate::ui::message_object::MessageEntry,
    assistant_name: &str,
    user_name: Option<&str>,
) {
    let is_user = entry.role == "user";
    let same_message = *widgets.rendered_message_id.borrow() == entry.id;

    if !same_message {
        if is_user {
            widgets.outer.set_halign(gtk::Align::End);
            widgets.card.remove_css_class("assistant-bubble");
            widgets.card.add_css_class("user-bubble");
            widgets.card.set_margin_start(72);
            widgets.card.set_margin_end(0);
            widgets.header_row.set_visible(true);
            widgets.header_row.set_halign(gtk::Align::End);
            widgets.avatar_icon.set_icon_name(Some("avatar-default-symbolic"));
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
    widgets.delete_btn.set_visible(is_user && !entry.is_streaming);
    widgets
        .actions_box
        .set_visible(has_text || (is_user && !entry.is_streaming));

    if !same_message {
        *widgets.rendered_message_id.borrow_mut() = entry.id.clone();
        widgets.rendered_blocks.borrow_mut().clear();

        while let Some(child) = widgets.attachments_box.first_child() {
            widgets.attachments_box.remove(&child);
        }
        if entry.attachments.is_empty() {
            widgets.attachments_box.set_visible(false);
        } else {
            widgets.attachments_box.set_visible(true);
            for att in &entry.attachments {
                if att.mime_type.starts_with("image/") {
                    let thumb = build_image_thumbnail(&att.file_path, 168);
                    let thumb_btn = gtk::Button::builder()
                        .child(&thumb)
                        .halign(gtk::Align::Start)
                        .valign(gtk::Align::Center)
                        .tooltip_text(&att.file_name)
                        .css_classes(["flat", "thumb-btn"])
                        .build();
                    let fname = att.file_name.clone();
                    let fpath = att.file_path.clone();
                    let box_ref = widgets.attachments_box.clone();
                    thumb_btn.connect_clicked(move |_| {
                        present_image_lightbox(&box_ref, &fname, &fpath);
                    });
                    widgets.attachments_box.insert(&thumb_btn, -1);
                } else {
                    let tile = build_document_tile(&att.file_name, &att.mime_type, 72);
                    widgets.attachments_box.insert(&tile, -1);
                }
            }
        }
    }

    let has_card_content = !entry.blocks.is_empty() || !entry.attachments.is_empty();
    widgets.card.set_visible(has_card_content);

    let is_dark = adw::StyleManager::default().is_dark();
    update_blocks_box(
        &widgets.blocks_box,
        &widgets.rendered_blocks,
        &entry.blocks,
        is_dark,
        entry.is_streaming,
    );
}

fn update_blocks_box(
    blocks_box: &gtk::Box,
    rendered_cache: &Rc<RefCell<Vec<MarkdownBlock>>>,
    new_blocks: &[MarkdownBlock],
    is_dark: bool,
    is_streaming: bool,
) {
    let prev = rendered_cache.borrow();
    if prev.as_slice() == new_blocks {
        return;
    }

    // Fast path during streaming: if all prior blocks match, only update/replace the tail.
    let prefix_len = prev.len().saturating_sub(1);
    if !prev.is_empty()
        && new_blocks.len() >= prev.len()
        && prev[..prefix_len] == new_blocks[..prefix_len]
    {
        if new_blocks.len() == prev.len()
            && let (Some(MarkdownBlock::Text(new_markup)), Some(last_widget)) =
                (new_blocks.last(), blocks_box.last_child())
            && let Ok(label) = last_widget.downcast::<gtk::Label>()
        {
            apply_markup_to_label(&label, new_markup);
            drop(prev);
            *rendered_cache.borrow_mut() = new_blocks.to_vec();
            return;
        }

        if is_streaming
            && new_blocks.len() == prev.len()
            && let (
                Some(MarkdownBlock::Code { language: prev_lang, .. }),
                Some(MarkdownBlock::Code {
                    language: new_lang,
                    light_pango_markup,
                    dark_pango_markup,
                    ..
                }),
                Some(last_widget),
            ) = (prev.last(), new_blocks.last(), blocks_box.last_child())
            && prev_lang == new_lang
            && let Some(code_box) = last_widget.downcast_ref::<gtk::Box>()
            && let Some(scroller) = code_box.last_child().and_downcast::<gtk::ScrolledWindow>()
            && let Some(code_label) = scroller.child().and_downcast::<gtk::Label>()
        {
            let markup = if is_dark {
                dark_pango_markup
            } else {
                light_pango_markup
            };
            code_label.set_markup(markup);
            drop(prev);
            *rendered_cache.borrow_mut() = new_blocks.to_vec();
            return;
        }

        if let Some(last_widget) = blocks_box.last_child() {
            blocks_box.remove(&last_widget);
        }
        for block in &new_blocks[prefix_len..] {
            blocks_box.append(&build_block_widget(block, is_dark));
        }
        drop(prev);
        *rendered_cache.borrow_mut() = new_blocks.to_vec();
        return;
    }

    drop(prev);
    clear_box(blocks_box);
    for block in new_blocks {
        blocks_box.append(&build_block_widget(block, is_dark));
    }
    *rendered_cache.borrow_mut() = new_blocks.to_vec();
}

fn build_block_widget(block: &MarkdownBlock, is_dark: bool) -> gtk::Widget {
    match block {
        MarkdownBlock::Text(markup) => build_markup_label(markup).upcast(),
        MarkdownBlock::Quote(markup) => {
            let quote_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .css_classes(["markdown-blockquote"])
                .build();
            let label = build_markup_label(markup);
            label.add_css_class("dim-label");
            quote_box.append(&label);
            quote_box.upcast()
        }
        MarkdownBlock::Rule => gtk::Separator::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_top(6)
            .margin_bottom(6)
            .build()
            .upcast(),
        MarkdownBlock::Table { headers, rows } => build_markdown_table(headers, rows).upcast(),
        MarkdownBlock::Code {
            language,
            code,
            light_pango_markup,
            dark_pango_markup,
        } => {
            let markup = if is_dark {
                dark_pango_markup
            } else {
                light_pango_markup
            };
            build_code_block(language.as_deref(), code, markup).upcast()
        }
    }
}

fn apply_markup_to_label(label: &gtk::Label, markup: &str) {
    if let Ok((attrs, text, _)) = gtk::pango::parse_markup(markup, '\0') {
        attrs.insert(gtk::pango::AttrFloat::new_line_height(1.42));
        label.set_text(&text);
        label.set_attributes(Some(&attrs));
    } else {
        let attrs = gtk::pango::AttrList::new();
        attrs.insert(gtk::pango::AttrFloat::new_line_height(1.42));
        label.set_text(markup);
        label.set_attributes(Some(&attrs));
    }
}

fn build_markup_label(markup: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .natural_wrap_mode(gtk::NaturalWrapMode::None)
        .selectable(true)
        .build();
    apply_markup_to_label(&label, markup);
    label
}

fn build_markdown_table(headers: &[String], rows: &[Vec<String>]) -> gtk::ScrolledWindow {
    let grid = gtk::Grid::builder()
        .column_spacing(0)
        .row_spacing(0)
        .build();

    for (col_idx, header) in headers.iter().enumerate() {
        let cell = build_markup_label(header);
        cell.set_margin_top(8);
        cell.set_margin_bottom(8);
        cell.set_margin_start(12);
        cell.set_margin_end(12);
        cell.add_css_class("heading");
        let wrapper = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["markdown-table-header"])
            .build();
        wrapper.append(&cell);
        grid.attach(&wrapper, col_idx as i32, 0, 1, 1);
    }

    let row_offset = if headers.is_empty() { 0 } else { 1 };
    for (row_idx, row) in rows.iter().enumerate() {
        for (col_idx, cell_markup) in row.iter().enumerate() {
            let cell = build_markup_label(cell_markup);
            cell.set_margin_top(6);
            cell.set_margin_bottom(6);
            cell.set_margin_start(12);
            cell.set_margin_end(12);
            let wrapper = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .css_classes(["markdown-table-cell"])
                .build();
            wrapper.append(&cell);
            grid.attach(&wrapper, col_idx as i32, (row_idx + row_offset) as i32, 1, 1);
        }
    }

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["markdown-table-card"])
        .child(&grid)
        .build()
}

fn build_code_block(language: Option<&str>, raw_code: &str, pango_markup: &str) -> gtk::Box {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["code-card"])
        .build();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(14)
        .margin_end(8)
        .build();

    let lang_label = gtk::Label::builder()
        .label(language.unwrap_or("text"))
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["caption-heading", "dim-label"])
        .build();

    let copy_btn = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text(gettext("Copy code"))
        .css_classes(["flat", "circular"])
        .build();
    {
        let code_owned = raw_code.to_string();
        copy_btn.connect_clicked(move |btn| {
            copy_text_with_feedback(btn, &code_owned);
        });
    }

    header.append(&lang_label);
    header.append(&copy_btn);
    container.append(&header);
    container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let code_label = gtk::Label::builder()
        .xalign(0.0)
        .selectable(true)
        .use_markup(true)
        .margin_top(10)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .css_classes(["monospace"])
        .build();
    code_label.set_markup(pango_markup);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&code_label)
        .build();

    container.append(&scroller);
    container
}
