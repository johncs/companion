//! Shared `AdwDialog` + `ToolbarView` + `HeaderBar` scaffold for modal action dialogs.

use adw::prelude::*;

use crate::i18n::gettext;

/// Standard modal dialog parts returned by [`build_dialog_scaffold`].
pub struct DialogScaffold {
    pub dialog: adw::Dialog,
    pub toolbar: adw::ToolbarView,
    pub primary_btn: gtk::Button,
}

/// Build an [`adw::Dialog`] with a `HeaderBar` (`Cancel` + suggested primary button) and weak-ref cancel close.
pub fn build_dialog_scaffold(
    title: &str,
    width: i32,
    height: i32,
    primary_label: &str,
    primary_initially_sensitive: bool,
) -> DialogScaffold {
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(width)
        .content_height(height)
        .build();

    let cancel_btn = gtk::Button::with_label(&gettext("Cancel"));
    let primary_btn = gtk::Button::with_label(primary_label);
    primary_btn.add_css_class("suggested-action");
    primary_btn.set_sensitive(primary_initially_sensitive);

    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel_btn);
    header.pack_end(&primary_btn);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    dialog.set_child(Some(&toolbar));

    let dialog_cancel = dialog.downgrade();
    cancel_btn.connect_clicked(move |_| {
        if let Some(d) = dialog_cancel.upgrade() {
            d.close();
        }
    });

    DialogScaffold {
        dialog,
        toolbar,
        primary_btn,
    }
}
