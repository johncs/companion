//! Reusable GTK4 and libadwaita widget helpers.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::i18n::gettext;

const MAX_CACHED_TEXTURES: usize = 128;

struct LruTextureCache<V> {
    capacity: usize,
    order: VecDeque<String>,
    map: HashMap<String, V>,
}

impl<V: Clone> LruTextureCache<V> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: VecDeque::with_capacity(capacity),
            map: HashMap::with_capacity(capacity),
        }
    }

    fn get(&mut self, key: &str) -> Option<V> {
        let val = self.map.get(key)?.clone();
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
            self.order.push_back(key.to_string());
        }
        Some(val)
    }

    fn insert(&mut self, key: String, val: V) {
        if let Some(pos) = self.order.iter().position(|k| k == &key) {
            self.order.remove(pos);
        } else if self.map.len() >= self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.map.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.map.insert(key, val);
    }
}

thread_local! {
    static TEXTURE_CACHE: RefCell<LruTextureCache<gdk::Texture>> =
        RefCell::new(LruTextureCache::new(MAX_CACHED_TEXTURES));
}

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

/// Build a rounded, aspect-ratio-preserving image thumbnail (decoded asynchronously off the main thread).
pub fn build_image_thumbnail(file_path: &str, max_size: i32) -> gtk::Widget {
    if let Some(tex) = TEXTURE_CACHE.with(|cache| cache.borrow_mut().get(file_path)) {
        let img = gtk::Image::from_paintable(Some(&tex));
        img.set_pixel_size(max_size);
        img.set_halign(gtk::Align::Start);
        img.set_valign(gtk::Align::Center);
        img.add_css_class("image-thumb");
        return img.upcast();
    }

    let img = gtk::Image::from_icon_name("image-x-generic-symbolic");
    img.set_pixel_size(max_size);
    img.set_halign(gtk::Align::Start);
    img.set_valign(gtk::Align::Center);
    img.add_css_class("image-thumb");

    let path_owned = file_path.to_string();
    let img_weak = img.downgrade();
    spawn_blocking_ui(
        move || {
            let path = PathBuf::from(&path_owned);
            let meta = std::fs::metadata(&path).ok()?;
            let _stamp = (
                meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                meta.len(),
            );
            gdk::Texture::from_file(&gio::File::for_path(&path))
                .ok()
                .map(|tex| (path_owned, tex))
        },
        move |loaded| {
            let Some(img) = img_weak.upgrade() else {
                return;
            };
            match loaded {
                Some((key, tex)) => {
                    TEXTURE_CACHE.with(|cache| cache.borrow_mut().insert(key, tex.clone()));
                    img.set_paintable(Some(&tex));
                }
                None => {
                    img.set_icon_name(Some("image-missing-symbolic"));
                }
            }
        },
    );

    img.upcast()
}

/// Build a minimal rounded document icon tile (e.g. for PDF, TXT, code) with extension badge and hover filename.
pub fn build_document_tile(file_name: &str, mime_type: &str, size: i32) -> gtk::Box {
    let tile = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .spacing(4)
        .tooltip_text(file_name)
        .css_classes(["doc-tile"])
        .build();
    tile.set_size_request(size, size);

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
        .unwrap_or_else(|| "FILE".to_string());
    let ext_label = gtk::Label::builder()
        .label(&ext)
        .css_classes(["caption-heading", "dim-label"])
        .margin_bottom(8)
        .build();
    tile.append(&ext_label);

    tile
}

/// Open a clean `AdwDialog` lightbox previewing an attached image at a larger size.
pub fn present_image_lightbox(parent: &impl IsA<gtk::Widget>, file_name: &str, file_path: &str) {
    let dialog = adw::Dialog::builder()
        .title(file_name)
        .content_width(780)
        .content_height(600)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let path = Path::new(file_path);
    if path.exists() {
        let picture = gtk::Picture::for_file(&gio::File::for_path(path));
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.set_margin_top(12);
        picture.set_margin_bottom(16);
        picture.set_margin_start(16);
        picture.set_margin_end(16);
        toolbar.set_content(Some(&picture));
    } else {
        let status = adw::StatusPage::builder()
            .icon_name("image-missing-symbolic")
            .title(gettext("Image Not Found"))
            .description(file_path)
            .build();
        toolbar.set_content(Some(&status));
    }

    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}

/// Run blocking work on the shared Tokio runtime and deliver its result on the GTK main loop.
pub fn spawn_blocking_ui<T, F, G>(work: F, on_done: G)
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    G: FnOnce(T) + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel::<T>();
    crate::core::runtime().spawn_blocking(move || {
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

#[cfg(test)]
mod tests {
    use super::LruTextureCache;

    #[test]
    fn lru_cache_promotes_hits_and_evicts_least_recently_used() {
        let mut cache = LruTextureCache::new(2);
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        assert_eq!(cache.get("a"), Some(1));

        cache.insert("c".to_string(), 3);
        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("a"), Some(1));
        assert_eq!(cache.get("c"), Some(3));
    }
}
