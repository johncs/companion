//! Centralized icon name constants and symbolic icon loader.

use std::path::Path;

use gtk::gio;

pub const MODEL: &str = "companion-model-symbolic";
#[allow(dead_code)]
pub const THINKING: &str = "companion-thinking-symbolic";
pub const CHAT: &str = "companion-chat-symbolic";
pub const PROMPT: &str = "companion-prompt-symbolic";
pub const GLOBE: &str = "companion-globe-symbolic";

/// Build a symbolic [`gtk::Image`] that works both uninstalled (`cargo run`) and installed (Flatpak/Meson).
pub fn symbolic_image(icon_name: &str, pixel_size: i32) -> gtk::Image {
    let dev_icon = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/icons/hicolor/symbolic/actions")
        .join(format!("{icon_name}.svg"));

    if dev_icon.exists() {
        let paintable = gtk::IconPaintable::for_file(&gio::File::for_path(&dev_icon), pixel_size, 1);
        let img = gtk::Image::from_paintable(Some(&paintable));
        img.set_pixel_size(pixel_size);
        img
    } else {
        let img = gtk::Image::from_icon_name(icon_name);
        img.set_pixel_size(pixel_size);
        img
    }
}
