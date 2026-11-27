//! Reusable GTK4 and libadwaita widget helpers.

use std::path::Path;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::gettext;

pub use super::media_widgets::build_image_thumbnail_button;

/// Remove all child widgets from a [`gtk::Box`].
pub fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// Remove all rows from a [`gtk::ListBox`].
pub fn clear_list_box(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

/// Send a native GNOME desktop notification via the active [`gio::Application`].
pub fn send_desktop_notification(id: &str, title: &str, body: &str) {
    if let Some(app) = gio::Application::default() {
        let notification = gio::Notification::new(title);
        notification.set_body(Some(body));
        app.send_notification(Some(id), &notification);
    }
}

/// Standard GNOME HIG content container for preferences-style dialogs (24px margins and spacing).
pub fn dialog_content_box() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build()
}

/// Remove all rows from an [`adw::PreferencesGroup`].
pub fn clear_preferences_group(group: &adw::PreferencesGroup) {
    let mut children = Vec::new();
    let mut current = group.first_child();
    while let Some(widget) = current {
        current = widget.next_sibling();
        children.push(widget);
    }
    for widget in children {
        if let Some(listbox) = find_listbox(&widget) {
            while let Some(row) = listbox.first_child() {
                group.remove(&row);
            }
        }
    }
}

/// Present a standard destructive confirmation [`adw::AlertDialog`].
pub fn present_destructive_confirm<F>(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    confirm_label: &str,
    on_confirm: F,
) where
    F: Fn() + 'static,
{
    let alert = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    alert.add_response("cancel", &gettext("Cancel"));
    alert.add_response("confirm", confirm_label);
    alert.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("cancel"));

    alert.connect_response(None, move |_, response| {
        if response == "confirm" {
            on_confirm();
        }
    });
    alert.present(Some(parent));
}

/// Launch a directory or reveal a file's containing folder using `gtk::FileLauncher`.
pub fn open_path_with_launcher<F>(
    parent: &impl IsA<gtk::Widget>,
    path: &Path,
    is_file: bool,
    on_error: F,
) where
    F: FnOnce(String) + 'static,
{
    let file = gio::File::for_path(path);
    let launcher = gtk::FileLauncher::new(Some(&file));
    let window = parent.root().and_downcast::<gtk::Window>();
    let cb = std::cell::RefCell::new(Some(on_error));
    let handle_result = move |res: Result<(), gtk::glib::Error>| {
        if let Err(err) = res
            && let Some(f) = cb.borrow_mut().take()
        {
            f(err.to_string());
        }
    };
    if is_file {
        launcher.open_containing_folder(window.as_ref(), None::<&gio::Cancellable>, handle_result);
    } else {
        launcher.launch(window.as_ref(), None::<&gio::Cancellable>, handle_result);
    }
}

/// Build a minimal rounded document icon tile (e.g. for PDF, TXT, code) with extension badge and hover filename.
pub fn build_document_tile(file_name: &str, mime_type: &str, size: i32) -> gtk::Box {
    let tile = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .spacing(4)
        .focusable(true)
        .tooltip_text(file_name)
        .css_classes(["doc-tile"])
        .build();
    tile.set_size_request(size, size);
    tile.update_property(&[gtk::accessible::Property::Label(&crate::i18n::tr_f(
        "Document: {name}",
        &[("name", file_name)],
    ))]);

    let icon_name = if mime_type == "application/pdf" {
        "x-office-document-symbolic"
    } else {
        "text-x-generic-symbolic"
    };
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size((size / 2).clamp(24, 36));
    icon.set_margin_top(12);
    icon.add_css_class("dim-label");
    tile.append(&icon);

    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_else(|| gettext("FILE"));
    let ext_label = gtk::Label::builder()
        .label(&ext)
        .css_classes(["caption-heading", "dim-label"])
        .margin_bottom(8)
        .build();
    tile.append(&ext_label);

    tile
}

/// Run blocking work on the injected Tokio runtime and deliver its result on the GTK main loop.
pub fn spawn_blocking_ui<T, F, G>(rt: &tokio::runtime::Handle, work: F, on_done: G)
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    G: FnOnce(T) + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel::<T>();
    rt.spawn_blocking(move || {
        let _ = tx.send(work());
    });
    glib::MainContext::default().spawn_local(async move {
        if let Ok(res) = rx.await {
            on_done(res);
        }
    });
}

fn find_listbox(root: &gtk::Widget) -> Option<gtk::ListBox> {
    if let Ok(lb) = root.clone().downcast::<gtk::ListBox>() {
        return Some(lb);
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_listbox(&widget) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}
