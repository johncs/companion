//! GNOME `AdwDialog` modals for inspecting message metadata/thinking and editing messages.

mod edit_dialog;
mod info_dialog;

use adw::prelude::*;
use gtk::glib;

pub use edit_dialog::present_edit_message_dialog;
pub use info_dialog::present_message_info_dialog;

/// Copy `text` to the system clipboard and flash a checkmark icon on `btn` for 1.2s.
pub fn copy_text_with_feedback(btn: &gtk::Button, text: &str) {
    btn.display().clipboard().set_text(text);
    btn.set_icon_name("object-select-symbolic");
    let btn_weak = btn.downgrade();
    glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
        if let Some(b) = btn_weak.upgrade() {
            b.set_icon_name("edit-copy-symbolic");
        }
    });
}
