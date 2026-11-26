//! Helper widgets for the sidebar component.

use adw::prelude::*;

use crate::i18n::gettext;
use crate::ui::sidebar_rows::SIDEBAR_ICON_SIZE;

pub(super) fn build_section_box(title: &str, body: &impl IsA<gtk::Widget>) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_start(8)
        .margin_end(4)
        .build();
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["heading", "dim-label"])
        .build();
    header.append(&label);
    section.append(&header);
    section.append(body);
    section
}

pub(super) fn build_sidebar_empty_row(text: &str) -> gtk::ListBoxRow {
    let lbl = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .margin_start(4)
        .css_classes(["dim-label", "caption"])
        .build();
    gtk::ListBoxRow::builder()
        .child(&lbl)
        .activatable(false)
        .selectable(false)
        .sensitive(false)
        .build()
}

pub(super) fn build_sidebar_loading_row(text: &str) -> gtk::ListBoxRow {
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(4)
        .build();
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .width_request(SIDEBAR_ICON_SIZE)
        .height_request(SIDEBAR_ICON_SIZE)
        .valign(gtk::Align::Center)
        .build();
    let lbl = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    box_.append(&spinner);
    box_.append(&lbl);
    gtk::ListBoxRow::builder()
        .child(&box_)
        .activatable(false)
        .selectable(false)
        .build()
}

pub(super) fn build_sidebar_error_row(text: &str, on_retry: impl Fn() + 'static) -> gtk::ListBoxRow {
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(4)
        .build();
    let icon = gtk::Image::builder()
        .icon_name("dialog-warning-symbolic")
        .pixel_size(SIDEBAR_ICON_SIZE)
        .css_classes(["warning"])
        .build();
    let lbl = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "caption"])
        .build();
    let retry_btn = gtk::Button::builder()
        .child(
            &gtk::Image::builder()
                .icon_name("view-refresh-symbolic")
                .pixel_size(SIDEBAR_ICON_SIZE)
                .build(),
        )
        .tooltip_text(gettext("Retry"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    retry_btn.connect_clicked(move |_| on_retry());
    box_.append(&icon);
    box_.append(&lbl);
    box_.append(&retry_btn);
    gtk::ListBoxRow::builder()
        .child(&box_)
        .activatable(false)
        .selectable(false)
        .build()
}
