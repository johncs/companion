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

    if is_running {
        let spinner = gtk::Spinner::builder()
            .spinning(true)
            .width_request(SIDEBAR_ICON_SIZE)
            .height_request(SIDEBAR_ICON_SIZE)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Generating…"))
            .build();
        content.append(&spinner);
    } else {
        let chat_icon = icons::symbolic_image(icons::CHAT, SIDEBAR_ICON_SIZE);
        chat_icon.set_size_request(SIDEBAR_ICON_SIZE, SIDEBAR_ICON_SIZE);
        content.append(&chat_icon);
    }

    let title_lbl = gtk::Label::builder()
        .label(&conv.title)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();

    content.append(&title_lbl);

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

    {
        let sender = sender.clone();
        let conv_id = conv.id.clone();
        row.connect_activate(move |_| {
            sender.input(SidebarMsg::SelectConversation(conv_id.clone()));
        });
    }

    {
        let sender = sender.clone();
        let conv_id = conv.id.clone();
        let click = gtk::GestureClick::builder().button(1).build();
        click.set_propagation_phase(gtk::PropagationPhase::Bubble);
        click.connect_released(move |_, _, _, _| {
            sender.input(SidebarMsg::SelectConversation(conv_id.clone()));
        });
        row.add_controller(click);
    }

    row
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

    let motion = gtk::EventControllerMotion::new();
    {
        let hovered = hovered.clone();
        let menu_btn = menu_btn.clone();
        motion.connect_enter(move |_, _, _| {
            hovered.set(true);
            menu_btn.set_can_target(true);
        });
    }
    {
        let hovered = hovered.clone();
        let menu_btn = menu_btn.clone();
        motion.connect_leave(move |_| {
            hovered.set(false);
            if !always_visible && !menu_btn.is_active() {
                menu_btn.set_can_target(false);
            }
        });
    }
    row.add_controller(motion);

    {
        let hovered = hovered.clone();
        menu_btn.connect_active_notify(move |btn| {
            if btn.is_active() {
                btn.add_css_class("menu-open");
                btn.set_can_target(true);
            } else {
                btn.remove_css_class("menu-open");
                if !always_visible && !hovered.get() {
                    btn.set_can_target(false);
                }
            }
        });
    }

    let long_press = gtk::GestureLongPress::new();
    {
        let menu_btn = menu_btn.clone();
        long_press.connect_pressed(move |gesture, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            menu_btn.popup();
        });
    }
    row.add_controller(long_press);

    let right_click = gtk::GestureClick::builder().button(3).build();
    {
        let menu_btn = menu_btn.clone();
        right_click.connect_pressed(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            menu_btn.popup();
        });
    }
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

    let rename_btn = build_popover_item(&gettext("Rename"));
    {
        let popover = popover.clone();
        let sender = sender.clone();
        let conv_id = conv.id.clone();
        let current_title = conv.title.clone();
        let row_ref = row.clone();
        rename_btn.connect_clicked(move |_| {
            popover.popdown();
            present_rename_dialog(&row_ref, &gettext("Rename Chat"), &current_title, {
                let sender = sender.clone();
                let conv_id = conv_id.clone();
                move |new_title| {
                    sender.input(SidebarMsg::RenameConversation {
                        id: conv_id.clone(),
                        title: new_title,
                    });
                }
            });
        });
    }

    let duplicate_btn = build_popover_item(&gettext("Duplicate"));
    {
        let popover = popover.clone();
        let sender = sender.clone();
        let conv_id = conv.id.clone();
        duplicate_btn.connect_clicked(move |_| {
            popover.popdown();
            sender.input(SidebarMsg::DuplicateConversation(conv_id.clone()));
        });
    }

    let delete_btn = build_popover_item(&gettext("Delete"));
    delete_btn.add_css_class("error");
    {
        let popover = popover.clone();
        let sender = sender.clone();
        let conv_id = conv.id.clone();
        let conv_title = conv.title.clone();
        let row_ref = row.clone();
        delete_btn.connect_clicked(move |_| {
            popover.popdown();
            let body = tr_f(
                "Delete \"{title}\" and all of its messages? This cannot be undone.",
                &[("title", &conv_title)],
            );
            let sender = sender.clone();
            let conv_id = conv_id.clone();
            present_destructive_confirm(&row_ref, &gettext("Delete Chat?"), &body, &gettext("Delete"), move || {
                sender.input(SidebarMsg::DeleteConversation(conv_id.clone()));
            });
        });
    }

    box_.append(&rename_btn);
    box_.append(&duplicate_btn);
    box_.append(&delete_btn);
    wrap_more_menu(&gettext("Chat actions"), &popover, &box_)
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
    let lbl = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .build();
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
