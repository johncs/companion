//! Recycled `gtk::ListView` row factory for chat messages.

mod bind;
mod blocks;
mod widget;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::markdown::MarkdownBlock;
use crate::i18n::gettext;
use crate::ui::message_dialogs::{
    copy_text_with_feedback, present_edit_message_dialog, present_message_info_dialog,
};
use crate::ui::message_object::MessageObject;
use crate::ui::widgets::present_destructive_confirm;

use bind::bind_entry;
use blocks::update_blocks_box;
use widget::MessageRowWidget;

#[derive(Clone)]
pub(super) struct RowWidgets {
    pub(super) outer: gtk::Box,
    pub(super) card: gtk::Box,
    pub(super) header_row: gtk::Box,
    pub(super) avatar_icon: gtk::Image,
    pub(super) role_label: gtk::Label,
    pub(super) attachments_box: gtk::FlowBox,
    pub(super) active_msg_obj: Rc<RefCell<Option<MessageObject>>>,
    pub(super) blocks_box: gtk::Box,
    pub(super) streaming_spinner: gtk::Spinner,
    pub(super) actions_box: gtk::Box,
    pub(super) edit_btn: gtk::Button,
    pub(super) delete_btn: gtk::Button,
    pub(super) edit_target: Rc<RefCell<(String, String)>>,
    pub(super) copy_btn: gtk::Button,
    pub(super) copy_text: Rc<RefCell<String>>,
    pub(super) rendered_message_id: Rc<RefCell<String>>,
    pub(super) rendered_blocks: Rc<RefCell<Vec<MarkdownBlock>>>,
    pub(super) rendered_is_dark: Rc<Cell<bool>>,
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
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        list_item.set_activatable(false);
        list_item.set_selectable(false);

        let clamp = adw::Clamp::builder()
            .maximum_size(780)
            .tightening_threshold(560)
            .build();
        let widgets = build_row_widgets(&clamp, &on_edit, &on_delete);
        let theme_id = attach_theme_listener(&widgets);
        let row_widget = MessageRowWidget::new(&clamp, widgets, theme_id);
        list_item.set_child(Some(&row_widget));
    });

    {
        let assistant_name = assistant_name.clone();
        let user_name = user_name.clone();
        factory.connect_bind(move |_, obj| {
            let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(msg_obj) = list_item.item().and_downcast::<MessageObject>() else {
                return;
            };
            let Some(row_widget) = list_item.child().and_downcast::<MessageRowWidget>() else {
                return;
            };
            let Some(widgets) = row_widget.row_widgets() else {
                return;
            };

            bind_entry(widgets, &msg_obj, &assistant_name, &user_name);

            let widgets_clone = widgets.clone();
            let msg_clone = msg_obj.clone();
            let name_clone = assistant_name.clone();
            let uname_clone = user_name.clone();
            let id = msg_obj.connect_local("updated", false, move |_| {
                bind_entry(&widgets_clone, &msg_clone, &name_clone, &uname_clone);
                None
            });
            row_widget.bind_signal(msg_obj, id);
        });
    }

    factory.connect_unbind(|_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row_widget) = list_item.child().and_downcast::<MessageRowWidget>() else {
            return;
        };
        row_widget.unbind_signal();
        if let Some(widgets) = row_widget.row_widgets() {
            *widgets.active_msg_obj.borrow_mut() = None;
            widgets.rendered_message_id.borrow_mut().clear();
            widgets.rendered_blocks.borrow_mut().clear();
        }
    });

    factory
}

fn build_row_widgets(
    clamp: &adw::Clamp,
    on_edit: &Rc<dyn Fn(String, String)>,
    on_delete: &Rc<dyn Fn(String)>,
) -> RowWidgets {
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

    let active_msg_obj: Rc<RefCell<Option<MessageObject>>> = Rc::new(RefCell::new(None));
    let (header_row, avatar_icon, streaming_spinner, role_label) =
        build_header_row(&active_msg_obj);

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

    let edit_target = Rc::new(RefCell::new((String::new(), String::new())));
    let copy_text = Rc::new(RefCell::new(String::new()));
    let (actions_box, edit_btn, delete_btn, copy_btn) =
        build_actions_box(&edit_target, &copy_text, on_edit, on_delete);

    card.append(&attachments_box);
    card.append(&blocks_box);
    outer.append(&header_row);
    outer.append(&card);
    outer.append(&actions_box);
    clamp.set_child(Some(&outer));

    RowWidgets {
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
        rendered_is_dark: Rc::new(Cell::new(adw::StyleManager::default().is_dark())),
    }
}

fn build_header_row(
    active_msg_obj: &Rc<RefCell<Option<MessageObject>>>,
) -> (gtk::Box, gtk::Image, gtk::Spinner, gtk::Label) {
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

    let msg_obj_ref = active_msg_obj.clone();
    role_btn.connect_clicked(move |btn| {
        if let Some(obj) = msg_obj_ref.borrow().clone() {
            present_message_info_dialog(btn, obj);
        }
    });

    header_row.append(&role_btn);
    (header_row, avatar_icon, streaming_spinner, role_label)
}

fn build_actions_box(
    edit_target: &Rc<RefCell<(String, String)>>,
    copy_text: &Rc<RefCell<String>>,
    on_edit: &Rc<dyn Fn(String, String)>,
    on_delete: &Rc<dyn Fn(String)>,
) -> (gtk::Box, gtk::Button, gtk::Button, gtk::Button) {
    let actions_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .margin_top(2)
        .css_classes(["message-actions"])
        .build();

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
    (actions_box, edit_btn, delete_btn, copy_btn)
}

fn attach_theme_listener(widgets: &RowWidgets) -> glib::SignalHandlerId {
    let active_msg_obj = widgets.active_msg_obj.clone();
    let rendered_blocks = widgets.rendered_blocks.clone();
    let rendered_is_dark = widgets.rendered_is_dark.clone();
    let blocks_weak = widgets.blocks_box.downgrade();
    adw::StyleManager::default().connect_dark_notify(move |mgr| {
        let Some(blocks_box) = blocks_weak.upgrade() else {
            return;
        };
        let Some(msg_obj) = active_msg_obj.borrow().clone() else {
            return;
        };
        let entry = msg_obj.entry();
        update_blocks_box(
            &blocks_box,
            &rendered_blocks,
            &rendered_is_dark,
            &entry.blocks,
            mgr.is_dark(),
            entry.is_streaming,
        );
    })
}
