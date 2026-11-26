//! Centralized icon name constants and symbolic icon loader.

pub const MODEL: &str = "companion-model-symbolic";
pub const CHAT: &str = "companion-chat-symbolic";
pub const PROMPT: &str = "companion-prompt-symbolic";
pub const GLOBE: &str = "companion-globe-symbolic";

/// Build a symbolic [`gtk::Image`] resolved via the active [`gtk::IconTheme`].
pub fn symbolic_image(icon_name: &str, pixel_size: i32) -> gtk::Image {
    let img = gtk::Image::from_icon_name(icon_name);
    img.set_pixel_size(pixel_size);
    img
}
