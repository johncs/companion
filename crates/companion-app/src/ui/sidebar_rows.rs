//! Row builders for the sidebar's Conversations list.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::ComponentSender;

use crate::i18n::{gettext, tr_f};
use crate::storage::Conversation;
use crate::ui::icons;
use crate::ui::sidebar::{SidebarModel, SidebarMsg};
use crate::ui::widgets::present_destructive_confirm;

pub const SIDEBAR_ICON_SIZE: i32 = 16;

pub fn new_nav_list() -> gtk::ListBox {
    gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .activate_on_single_click(true)
        .css_classes(["navigation-sidebar"])
        .build()
}

/// Build a selectable conversation row with a chat icon, optional spinner, and `⋯` action menu.
pub fn build_conversation_row(
    conv: &Conversation,
    is_active: bool,
    is_running: bool,
    sender: &ComponentSender<SidebarModel>,
) -> gtk::ListBoxRow {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    content.append(&build_row_leading_icon(is_running));
    content.append(
        &gtk::Label::builder()
            .label(&conv.title)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );

    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .activatable(true)
        .build();
    let menu_btn = build_conversation_menu_button(conv, row.upcast_ref(), sender);
    content.append(&menu_btn);
    attach_row_menu_behaviors(&row, &menu_btn, is_active);
    if is_active {
        row.add_css_class("sidebar-active-row");
    }
    // Store `conv.id` on the row so `ListBox::row-activated` handles both single-click and
    // keyboard Enter/Space once without duplicate per-row `GestureClick` signals.
    row.set_widget_name(&conv.id);
    row
}

fn build_row_leading_icon(is_running: bool) -> gtk::Widget {
    if is_running {
        gtk::Spinner::builder()
            .spinning(true)
            .width_request(SIDEBAR_ICON_SIZE)
            .height_request(SIDEBAR_ICON_SIZE)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Generating…"))
            .build()
            .upcast()
    } else {
        let chat_icon = icons::symbolic_image(icons::CHAT, SIDEBAR_ICON_SIZE);
        chat_icon.set_size_request(SIDEBAR_ICON_SIZE, SIDEBAR_ICON_SIZE);
        chat_icon.upcast()
    }
}

/// Reveal a row's `⋯` menu on hover/active/popover-open, and allow touch long-press or right-click to open it.
fn attach_row_menu_behaviors(
    row: &gtk::ListBoxRow,
    menu_btn: &gtk::MenuButton,
    always_visible: bool,
) {
    menu_btn.add_css_class("sidebar-row-menu");
    menu_btn.set_can_target(always_visible);
    let hovered = Rc::new(Cell::new(false));
    attach_hover_and_active_visibility(row, menu_btn, &hovered, always_visible);
    attach_popup_gestures(row, menu_btn);
}

fn attach_hover_and_active_visibility(
    row: &gtk::ListBoxRow,
    menu_btn: &gtk::MenuButton,
    hovered: &Rc<Cell<bool>>,
    always_visible: bool,
) {
    let motion = gtk::EventControllerMotion::new();
    let (h1, m1) = (hovered.clone(), menu_btn.clone());
    motion.connect_enter(move |_, _, _| {
        h1.set(true);
        m1.set_can_target(true);
    });
    let (h2, m2) = (hovered.clone(), menu_btn.clone());
    motion.connect_leave(move |_| {
        h2.set(false);
        if !always_visible && !m2.is_active() {
            m2.set_can_target(false);
        }
    });
    row.add_controller(motion);

    let h3 = hovered.clone();
    menu_btn.connect_active_notify(move |btn| {
        if btn.is_active() {
            btn.add_css_class("menu-open");
            btn.set_can_target(true);
        } else {
            btn.remove_css_class("menu-open");
            if !always_visible && !h3.get() {
                btn.set_can_target(false);
            }
        }
    });
}

fn attach_popup_gestures(row: &gtk::ListBoxRow, menu_btn: &gtk::MenuButton) {
    let long_press = gtk::GestureLongPress::new();
    let m1 = menu_btn.clone();
    long_press.connect_pressed(move |g, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        m1.popup();
    });
    row.add_controller(long_press);

    let right_click = gtk::GestureClick::builder().button(3).build();
    let m2 = menu_btn.clone();
    right_click.connect_pressed(move |g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        m2.popup();
    });
    row.add_controller(right_click);
}

fn build_conversation_menu_button(
    conv: &Conversation,
    row: &gtk::Widget,
    sender: &ComponentSender<SidebarModel>,
) -> gtk::MenuButton {
    let popover = gtk::Popover::new();
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(4)
        .margin_end(4)
        .build();

    let rename_btn = build_rename_menu_item(conv, row, &popover, sender);
    let duplicate_btn = build_duplicate_menu_item(&conv.id, &popover, sender);
    let delete_btn = build_delete_menu_item(conv, row, &popover, sender);
    box_.append(&rename_btn);
    box_.append(&duplicate_btn);
    box_.append(&delete_btn);
    wrap_more_menu(&gettext("Chat actions"), &popover, &box_)
}

fn build_rename_menu_item(
    conv: &Conversation,
    row: &gtk::Widget,
    popover: &gtk::Popover,
    sender: &ComponentSender<SidebarModel>,
) -> gtk::Button {
    let btn = build_popover_item(&gettext("Rename"));
    let (pop, sender, conv_id, title, row_ref) = (
        popover.clone(),
        sender.clone(),
        conv.id.clone(),
        conv.title.clone(),
        row.clone(),
    );
    btn.connect_clicked(move |_| {
        pop.popdown();
        let (s, id) = (sender.clone(), conv_id.clone());
        present_rename_dialog(
            &row_ref,
            &gettext("Rename Chat"),
            &title,
            move |new_title| {
                s.input(SidebarMsg::RenameConversation {
                    id: id.clone(),
                    title: new_title,
                });
            },
        );
    });
    btn
}

fn build_duplicate_menu_item(
    conv_id: &str,
    popover: &gtk::Popover,
    sender: &ComponentSender<SidebarModel>,
) -> gtk::Button {
    let btn = build_popover_item(&gettext("Duplicate"));
    let (pop, sender, id) = (popover.clone(), sender.clone(), conv_id.to_string());
    btn.connect_clicked(move |_| {
        pop.popdown();
        sender.input(SidebarMsg::DuplicateConversation(id.clone()));
    });
    btn
}

fn build_delete_menu_item(
    conv: &Conversation,
    row: &gtk::Widget,
    popover: &gtk::Popover,
    sender: &ComponentSender<SidebarModel>,
) -> gtk::Button {
    let btn = build_popover_item(&gettext("Delete"));
    btn.add_css_class("error");
    let (pop, sender, conv_id, title, row_ref) = (
        popover.clone(),
        sender.clone(),
        conv.id.clone(),
        conv.title.clone(),
        row.clone(),
    );
    btn.connect_clicked(move |_| {
        pop.popdown();
        let body = tr_f(
            "Delete \"{title}\" and all of its messages? This cannot be undone.",
            &[("title", &title)],
        );
        let (s, id) = (sender.clone(), conv_id.clone());
        present_destructive_confirm(
            &row_ref,
            &gettext("Delete Chat?"),
            &body,
            &gettext("Delete"),
            move || {
                s.input(SidebarMsg::DeleteConversation(id.clone()));
            },
        );
    });
    btn
}

fn wrap_more_menu(tooltip: &str, popover: &gtk::Popover, content: &gtk::Box) -> gtk::MenuButton {
    popover.set_child(Some(content));
    let icon = gtk::Image::builder()
        .icon_name("view-more-symbolic")
        .pixel_size(SIDEBAR_ICON_SIZE)
        .build();
    gtk::MenuButton::builder()
        .child(&icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .popover(popover)
        .css_classes(["flat", "dim-label"])
        .build()
}

fn build_popover_item(label: &str) -> gtk::Button {
    let lbl = gtk::Label::builder().label(label).xalign(0.0).build();
    let btn = gtk::Button::builder()
        .child(&lbl)
        .css_classes(["flat"])
        .build();
    btn.set_size_request(140, -1);
    btn
}

fn present_rename_dialog<F>(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    current_name: &str,
    on_save: F,
) where
    F: Fn(String) + 'static,
{
    let alert = adw::AlertDialog::builder().heading(heading).build();
    alert.add_response("cancel", &gettext("Cancel"));
    alert.add_response("save", &gettext("Rename"));
    alert.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("save"));

    let entry = gtk::Entry::builder()
        .text(current_name)
        .activates_default(true)
        .build();
    alert.set_extra_child(Some(&entry));

    let entry_clone = entry.clone();
    alert.connect_response(None, move |_, response| {
        if response == "save" {
            let trimmed = entry_clone.text().trim().to_string();
            if !trimmed.is_empty() {
                on_save(trimmed);
            }
        }
    });

    alert.present(Some(parent));
    entry.grab_focus();
    entry.select_region(0, -1);
}
