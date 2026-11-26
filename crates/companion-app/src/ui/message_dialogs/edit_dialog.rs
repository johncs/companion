//! `AdwDialog` modal to edit a user message and resend.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::i18n::gettext;

/// Present a GNOME `AdwDialog` to edit a user message (with a reset warning hint) and resend.
pub fn present_edit_message_dialog(
    parent: &impl IsA<gtk::Widget>,
    message_id: &str,
    initial_content: &str,
    on_save: Rc<dyn Fn(String, String)>,
) {
    let crate::ui::dialog_scaffold::DialogScaffold {
        dialog,
        toolbar,
        primary_btn: save_btn,
    } = crate::ui::dialog_scaffold::build_dialog_scaffold(
        &gettext("Edit Message"),
        580,
        380,
        &gettext("Save & Resend"),
        true,
    );

    let text_view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(14)
        .bottom_margin(14)
        .left_margin(16)
        .right_margin(16)
        .css_classes(["composer-input"])
        .build();
    text_view.buffer().set_text(initial_content);

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .min_content_height(240)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["card"])
        .child(&text_view)
        .build();

    let hint_box = build_edit_warning_hint();
    let content_box = crate::ui::widgets::dialog_content_box();
    content_box.append(&scroller);
    content_box.append(&hint_box);
    toolbar.set_content(Some(&content_box));

    wire_edit_dialog_actions(&dialog, &text_view, &save_btn, message_id, on_save);
    dialog.present(Some(parent));
    text_view.grab_focus();
}

fn wire_edit_dialog_actions(
    dialog: &adw::Dialog,
    text_view: &gtk::TextView,
    save_btn: &gtk::Button,
    message_id: &str,
    on_save: Rc<dyn Fn(String, String)>,
) {
    let save_btn_watch = save_btn.clone();
    text_view.buffer().connect_changed(move |buf| {
        let (s, e) = buf.bounds();
        save_btn_watch.set_sensitive(!buf.text(&s, &e, false).trim().is_empty());
    });
    let (msg_id, dialog_w, tv) = (
        message_id.to_string(),
        dialog.downgrade(),
        text_view.clone(),
    );
    let trigger_save: Rc<dyn Fn()> = Rc::new(move || {
        let buf = tv.buffer();
        let (s, e) = buf.bounds();
        let updated = buf.text(&s, &e, false).trim().to_string();
        if !updated.is_empty() {
            if let Some(d) = dialog_w.upgrade() {
                d.close();
            }
            on_save(msg_id.clone(), updated);
        }
    });
    let trigger_click = trigger_save.clone();
    save_btn.connect_clicked(move |_| trigger_click());
    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.connect_key_pressed(move |_, key, _, mods| {
        if (key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter)
            && mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            trigger_save();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    text_view.add_controller(key_ctrl);
}

fn build_edit_warning_hint() -> gtk::Box {
    let hint_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Start)
        .build();
    let hint_icon = gtk::Image::builder()
        .icon_name("dialog-warning-symbolic")
        .pixel_size(14)
        .css_classes(["dim-label"])
        .build();
    let hint_label = gtk::Label::builder()
        .label(gettext(
            "Editing this message will reset and remove all messages after it.",
        ))
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    hint_box.append(&hint_icon);
    hint_box.append(&hint_label);
    hint_box
}
