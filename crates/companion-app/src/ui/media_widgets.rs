//! Cached image thumbnail builders and lightbox preview dialog.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::i18n::gettext;

const MAX_CACHED_TEXTURES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TextureKey {
    path: String,
    mtime_secs: u64,
    file_len: u64,
}

struct LruTextureCache<V> {
    capacity: usize,
    order: VecDeque<TextureKey>,
    map: HashMap<TextureKey, V>,
    latest_by_path: HashMap<String, TextureKey>,
}

impl<V: Clone> LruTextureCache<V> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: VecDeque::with_capacity(capacity),
            map: HashMap::with_capacity(capacity),
            latest_by_path: HashMap::with_capacity(capacity),
        }
    }

    fn get_by_path(&mut self, path: &str) -> Option<V> {
        let key = self.latest_by_path.get(path)?.clone();
        let val = self.map.get(&key)?.clone();
        if let Some(pos) = self.order.iter().position(|k| k == &key) {
            self.order.remove(pos);
            self.order.push_back(key);
        }
        Some(val)
    }

    fn insert(&mut self, key: TextureKey, val: V) {
        if let Some(prev_key) = self.latest_by_path.insert(key.path.clone(), key.clone())
            && prev_key != key
        {
            self.map.remove(&prev_key);
            if let Some(pos) = self.order.iter().position(|k| k == &prev_key) {
                self.order.remove(pos);
            }
        }
        if let Some(pos) = self.order.iter().position(|k| k == &key) {
            self.order.remove(pos);
        } else if self.map.len() >= self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.map.remove(&oldest);
            if self.latest_by_path.get(&oldest.path) == Some(&oldest) {
                self.latest_by_path.remove(&oldest.path);
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, val);
    }
}

thread_local! {
    static TEXTURE_CACHE: RefCell<LruTextureCache<gdk::Texture>> =
        RefCell::new(LruTextureCache::new(MAX_CACHED_TEXTURES));
}

/// Build a rounded, aspect-ratio-preserving image thumbnail (decoded asynchronously off the main thread).
pub fn build_image_thumbnail(file_path: &str, max_size: i32) -> gtk::Widget {
    if let Some(tex) = TEXTURE_CACHE.with(|cache| cache.borrow_mut().get_by_path(file_path)) {
        let img = gtk::Image::from_paintable(Some(&tex));
        style_thumb_image(&img, max_size);
        return img.upcast();
    }
    let img = gtk::Image::from_icon_name("image-x-generic-symbolic");
    style_thumb_image(&img, max_size);

    let (path_owned, img_weak) = (file_path.to_string(), img.downgrade());
    glib::MainContext::default().spawn_local(async move {
        let loaded = decode_texture_async(path_owned).await;
        let Some(img) = img_weak.upgrade() else {
            return;
        };
        match loaded {
            Some((key, tex)) => {
                TEXTURE_CACHE.with(|cache| cache.borrow_mut().insert(key, tex.clone()));
                img.set_paintable(Some(&tex));
            }
            None => img.set_icon_name(Some("image-missing-symbolic")),
        }
    });
    img.upcast()
}

fn style_thumb_image(img: &gtk::Image, max_size: i32) {
    img.set_pixel_size(max_size);
    img.set_halign(gtk::Align::Start);
    img.set_valign(gtk::Align::Center);
    img.add_css_class("image-thumb");
}

async fn decode_texture_async(path_owned: String) -> Option<(TextureKey, gdk::Texture)> {
    gio::spawn_blocking(move || {
        let path = PathBuf::from(&path_owned);
        let (mtime_secs, file_len) = crate::core::paths::file_mtime_and_len(&path)?;
        let key = TextureKey {
            path: path_owned,
            mtime_secs,
            file_len,
        };
        gdk::Texture::from_file(&gio::File::for_path(&path))
            .ok()
            .map(|tex| (key, tex))
    })
    .await
    .ok()
    .flatten()
}

/// Build a clickable image thumbnail button that opens [`present_image_lightbox`] when clicked.
pub fn build_image_thumbnail_button(file_path: &str, file_name: &str, size: i32) -> gtk::Button {
    let thumb = build_image_thumbnail(file_path, size);
    let btn = gtk::Button::builder()
        .child(&thumb)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .tooltip_text(file_name)
        .css_classes(["flat", "thumb-btn"])
        .build();
    let fname = file_name.to_string();
    let fpath = file_path.to_string();
    btn.connect_clicked(move |b| {
        present_image_lightbox(b, &fname, &fpath);
    });
    btn
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
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    toolbar.set_content(Some(&spinner));

    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));

    let path_str = file_path.to_string();
    let toolbar_weak = toolbar.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let check_path = path_str.clone();
        let exists = gio::spawn_blocking(move || Path::new(&check_path).exists())
            .await
            .unwrap_or(false);
        let Some(toolbar) = toolbar_weak.upgrade() else {
            return;
        };
        if exists {
            let picture = gtk::Picture::for_file(&gio::File::for_path(Path::new(&path_str)));
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
                .description(&path_str)
                .build();
            toolbar.set_content(Some(&status));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{LruTextureCache, TextureKey};

    fn key(path: &str, mtime_secs: u64, file_len: u64) -> TextureKey {
        TextureKey {
            path: path.to_string(),
            mtime_secs,
            file_len,
        }
    }

    #[test]
    fn lru_cache_promotes_hits_and_evicts_least_recently_used() {
        let mut cache = LruTextureCache::new(2);
        cache.insert(key("a", 1, 10), 1);
        cache.insert(key("b", 1, 20), 2);
        assert_eq!(cache.get_by_path("a"), Some(1));

        cache.insert(key("c", 1, 30), 3);
        assert_eq!(cache.get_by_path("b"), None);
        assert_eq!(cache.get_by_path("a"), Some(1));
        assert_eq!(cache.get_by_path("c"), Some(3));

        // Updating a path with a new (mtime, len) stamp supersedes the old entry.
        cache.insert(key("a", 2, 15), 99);
        assert_eq!(cache.get_by_path("a"), Some(99));
    }
}
