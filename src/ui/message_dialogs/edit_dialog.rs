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
    let dialog = adw::Dialog::builder()
        .title(gettext("Edit Message"))
        .content_width(580)
        .content_height(380)
        .build();

    let cancel_btn = gtk::Button::builder().label(gettext("Cancel")).build();
    let save_btn = gtk::Button::builder()
        .label(gettext("Save & Resend"))
        .css_classes(["suggested-action"])
        .build();

    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);

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
        .min_content_height(260)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["card"])
        .child(&text_view)
        .build();

    let hint_box = build_edit_warning_hint();
    let content_box = crate::ui::widgets::dialog_content_box();
    content_box.append(&scroller);
    content_box.append(&hint_box);

    let outer_scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content_box)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&outer_scroller));
    dialog.set_child(Some(&toolbar));

    let save_btn_watch = save_btn.clone();
    text_view.buffer().connect_changed(move |buf| {
        let (s, e) = buf.bounds();
        save_btn_watch.set_sensitive(!buf.text(&s, &e, false).trim().is_empty());
    });

    let dialog_cancel = dialog.clone();
    cancel_btn.connect_clicked(move |_| {
        dialog_cancel.close();
    });

    let msg_id = message_id.to_string();
    let trigger_save: Rc<dyn Fn()> = {
        let dialog = dialog.clone();
        let tv = text_view.clone();
        Rc::new(move || {
            let buf = tv.buffer();
            let (s, e) = buf.bounds();
            let updated = buf.text(&s, &e, false).trim().to_string();
            if !updated.is_empty() {
                dialog.close();
                on_save(msg_id.clone(), updated);
            }
        })
    };

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

    dialog.present(Some(parent));
    text_view.grab_focus();
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
