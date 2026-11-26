//! Recycled `gtk::ListView` row factory for chat messages.

mod bind;
mod blocks;
mod widget;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::format::markdown::MarkdownBlock;
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
        list_item.set_child(Some(&MessageRowWidget::new(&clamp, widgets, theme_id)));
    });
    connect_factory_bind(&factory, assistant_name, user_name);
    connect_factory_unbind(&factory);
    factory
}

fn connect_factory_bind(
    factory: &gtk::SignalListItemFactory,
    assistant_name: Rc<RefCell<String>>,
    user_name: Rc<RefCell<Option<String>>>,
) {
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
        let (w_c, m_c, n_c, u_c) = (
            widgets.clone(),
            msg_obj.clone(),
            assistant_name.clone(),
            user_name.clone(),
        );
        let id = msg_obj.connect_local("updated", false, move |_| {
            bind_entry(&w_c, &m_c, &n_c, &u_c);
            None
        });
        row_widget.bind_signal(msg_obj, id);
    });
}

fn connect_factory_unbind(factory: &gtk::SignalListItemFactory) {
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
            // Recycled rows must not flash stale content or keep spinning offscreen.
            while let Some(child) = widgets.blocks_box.first_child() {
                widgets.blocks_box.remove(&child);
            }
            while let Some(child) = widgets.attachments_box.child_at_index(0) {
                widgets.attachments_box.remove(&child);
            }
            widgets.streaming_spinner.set_spinning(false);
        }
    });
}

fn build_row_widgets(
    clamp: &adw::Clamp,
    on_edit: &Rc<dyn Fn(String, String)>,
    on_delete: &Rc<dyn Fn(String)>,
) -> RowWidgets {
    let (outer, card, blocks_box, attachments_box) = build_row_containers();
    let active_msg_obj: Rc<RefCell<Option<MessageObject>>> = Rc::new(RefCell::new(None));
    let (header_row, avatar_icon, streaming_spinner, role_label) =
        build_header_row(&active_msg_obj);
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

fn build_row_containers() -> (gtk::Box, gtk::Box, gtk::Box, gtk::FlowBox) {
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
    (outer, card, blocks_box, attachments_box)
}

fn build_header_row(
    active_msg_obj: &Rc<RefCell<Option<MessageObject>>>,
) -> (gtk::Box, gtk::Image, gtk::Spinner, gtk::Label) {
    let header_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_bottom(4)
        .build();
    let (avatar_icon, streaming_spinner, role_label, role_btn_box) = build_header_role_box();
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

fn build_header_role_box() -> (gtk::Image, gtk::Spinner, gtk::Label, gtk::Box) {
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
    (avatar_icon, streaming_spinner, role_label, role_btn_box)
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
    let edit_btn = build_edit_action_btn(edit_target, on_edit);
    let copy_btn = build_copy_action_btn(copy_text);
    let delete_btn = build_delete_action_btn(edit_target, on_delete);
    actions_box.append(&edit_btn);
    actions_box.append(&delete_btn);
    actions_box.append(&copy_btn);
    (actions_box, edit_btn, delete_btn, copy_btn)
}

fn build_edit_action_btn(
    edit_target: &Rc<RefCell<(String, String)>>,
    on_edit: &Rc<dyn Fn(String, String)>,
) -> gtk::Button {
    let btn = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text(gettext("Edit message"))
        .css_classes(["flat", "circular", "message-copy-btn"])
        .focus_on_click(false)
        .visible(false)
        .build();
    let (edit_target_ref, on_edit_cb) = (edit_target.clone(), on_edit.clone());
    btn.connect_clicked(move |btn| {
        let (msg_id, content) = edit_target_ref.borrow().clone();
        if !msg_id.is_empty() {
            present_edit_message_dialog(btn, &msg_id, &content, on_edit_cb.clone());
        }
    });
    btn
}

fn build_copy_action_btn(copy_text: &Rc<RefCell<String>>) -> gtk::Button {
    let btn = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text(gettext("Copy message"))
        .css_classes(["flat", "circular", "message-copy-btn"])
        .focus_on_click(false)
        .build();
    let copy_text_ref = copy_text.clone();
    btn.connect_clicked(move |btn| {
        let text = copy_text_ref.borrow().clone();
        if !text.is_empty() {
            copy_text_with_feedback(btn, &text);
        }
    });
    btn
}

fn build_delete_action_btn(
    edit_target: &Rc<RefCell<(String, String)>>,
    on_delete: &Rc<dyn Fn(String)>,
) -> gtk::Button {
    let btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Delete message"))
        .css_classes(["flat", "circular", "message-copy-btn"])
        .focus_on_click(false)
        .visible(false)
        .build();
    let (edit_target_ref, on_delete_cb) = (edit_target.clone(), on_delete.clone());
    btn.connect_clicked(move |btn| {
        let (msg_id, _) = edit_target_ref.borrow().clone();
        if !msg_id.is_empty() {
            let cb = on_delete_cb.clone();
            present_destructive_confirm(
                btn,
                &gettext("Delete Message?"),
                &gettext("This message and all messages after it in this conversation will be permanently deleted."),
                &gettext("Delete"),
                move || cb(msg_id.clone()),
            );
        }
    });
    btn
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
        msg_obj.with_entry(|entry| {
            update_blocks_box(
                &blocks_box,
                &rendered_blocks,
                &rendered_is_dark,
                &entry.blocks,
                mgr.is_dark(),
                entry.is_streaming,
            );
        });
    })
}

#[cfg(test)]
mod tests {
    use super::blocks::{BlockUpdatePlan, plan_block_update};
    use crate::format::markdown::MarkdownBlock;

    #[test]
    fn rebind_after_unbind_onto_empty_blocks_rebuilds_all() {
        let incoming = vec![MarkdownBlock::Text("hello".into())];
        // Unbind clears `rendered_blocks` to empty `&[]`; binding a non-empty entry must rebuild.
        assert_eq!(
            plan_block_update(&[], false, &incoming, false, false),
            BlockUpdatePlan::RebuildAll
        );
        // Binding an empty entry onto an already-unbound row is a no-op.
        assert_eq!(
            plan_block_update(&[], false, &[], false, false),
            BlockUpdatePlan::Skip
        );
    }
}
